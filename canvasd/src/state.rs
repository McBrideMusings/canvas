use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use canvas_core::{Card, PinScope, Session};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, RwLock};

use crate::profiles::ProfilesConfig;
use crate::store::Store;

pub const CARD_RING_CAPACITY: usize = 500;

/// A state change. Also the line format of the persisted stream.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", content = "data", rename_all = "snake_case")]
pub enum CanvasEvent {
    SessionUpserted(Session),
    CardUpserted(Card),
    CardRemoved(String),
    SessionRemoved(String),
    /// A value pushed into a card's running script (`canvas data`). Sent to
    /// viewers only: never written to the persisted stream.
    CardData {
        id: String,
        value: serde_json::Value,
    },
}

#[derive(Default)]
pub struct Inner {
    pub sessions: HashMap<String, Session>,
    /// Front = newest.
    pub cards: VecDeque<Card>,
    /// One reply value per card id (canvas-17z), last write wins. In-memory
    /// only, like the rest of `Inner` on a restart — a reply is part of a
    /// live interaction with a running session, not history worth carrying
    /// across a daemon restart the way a card's own content is.
    pub replies: HashMap<String, serde_json::Value>,
    /// The latest value `canvas data` pushed into each card, last write wins.
    /// In-memory only: the viewer replays it into a card whose iframe reloads
    /// (an update, a theme change), and the next push after a daemon restart
    /// refills it.
    pub data: HashMap<String, serde_json::Value>,
}

#[derive(Clone)]
pub struct AppState {
    pub inner: Arc<RwLock<Inner>>,
    pub events: broadcast::Sender<CanvasEvent>,
    pub profiles: Arc<RwLock<ProfilesConfig>>,
    /// One refresh loop per refreshing card; see [`crate::refresh`].
    pub(crate) refreshes: Arc<std::sync::Mutex<crate::refresh::Slots>>,
    pub(crate) generation: Arc<std::sync::atomic::AtomicU64>,
    store: Option<Store>,
    data_dir: Option<std::path::PathBuf>,
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

impl AppState {
    pub fn new() -> Self {
        let (tx, _rx) = broadcast::channel(1024);
        AppState {
            inner: Arc::new(RwLock::new(Inner {
                sessions: HashMap::new(),
                cards: VecDeque::new(),
                replies: HashMap::new(),
                data: HashMap::new(),
            })),
            events: tx,
            profiles: Arc::new(RwLock::new(ProfilesConfig::default())),
            refreshes: Arc::default(),
            generation: Arc::default(),
            store: None,
            data_dir: None,
        }
    }

    /// State backed by `dir/stream.jsonl` for sessions and cards, and
    /// `dir/profiles.json` for named-profile overrides: reloads both on start.
    pub async fn open(dir: &std::path::Path) -> Self {
        let (store, inner) = Store::open(dir);
        let profiles = crate::profiles::load(dir).await;
        let (tx, _rx) = broadcast::channel(1024);
        AppState {
            inner: Arc::new(RwLock::new(inner)),
            events: tx,
            profiles: Arc::new(RwLock::new(profiles)),
            refreshes: Arc::default(),
            generation: Arc::default(),
            store: Some(store),
            data_dir: Some(dir.to_path_buf()),
        }
    }

    /// Persist the current profiles config to `profiles.json`. No-op
    /// without a data dir (e.g. `AppState::new()` in tests).
    pub async fn save_profiles(&self) {
        if let Some(dir) = &self.data_dir {
            let config = self.profiles.read().await;
            crate::profiles::save(dir, &config).await;
        }
    }

    /// Block until every change published so far is on disk. No-op without a store.
    pub fn flush_store(&self) {
        if let Some(store) = &self.store {
            store.flush();
        }
    }

    /// Ends a session the way the session-end route does: stamps `ended_at`,
    /// publishes it, and releases the session's session-scoped pins back to the
    /// feed. A repo-scoped pin stays held. `None` when the id names no session.
    pub fn end_session(&self, inner: &mut Inner, id: &str) -> Option<Session> {
        let session = inner.sessions.get_mut(id)?;
        session.ended_at = Some(chrono::Utc::now().to_rfc3339());
        let session = session.clone();
        self.publish(CanvasEvent::SessionUpserted(session.clone()));
        let released: Vec<Card> = inner
            .cards
            .iter()
            .filter(|c| {
                c.session_id == id && c.pin.as_ref().is_some_and(|p| p.scope == PinScope::Session)
            })
            .cloned()
            .collect();
        for mut card in released {
            card.pin = None;
            inner.upsert_card(card.clone());
            self.publish(CanvasEvent::CardUpserted(card));
        }
        Some(session)
    }

    /// Ends every running session whose agent process `is_alive` rejects.
    /// Returns how many it ended. A session with no pid is never swept.
    pub async fn sweep_dead_sessions(&self, is_alive: impl Fn(u32) -> bool) -> usize {
        let mut inner = self.inner.write().await;
        let dead: Vec<String> = inner
            .sessions
            .values()
            .filter(|s| s.ended_at.is_none() && s.pid.is_some_and(|pid| !is_alive(pid)))
            .map(|s| s.id.clone())
            .collect();
        for id in &dead {
            eprintln!("canvasd: agent process of session {id} is gone; ending it");
            self.end_session(&mut inner, id);
        }
        dead.len()
    }

    pub fn publish(&self, event: CanvasEvent) {
        if let Some(store) = &self.store {
            if !matches!(event, CanvasEvent::CardData { .. }) {
                store.append(&event);
            }
        }
        // No receivers (e.g. no SSE clients connected) is not an error.
        let _ = self.events.send(event);
    }
}

/// Whether a process with this pid exists (`kill(pid, 0)`). A process owned by
/// another user answers `EPERM`, which still means it is alive.
pub fn process_alive(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return true;
    };
    // SAFETY: signal 0 only checks that the process exists.
    unsafe {
        libc::kill(pid, 0) == 0
            || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
    }
}

/// How often the daemon checks that each session's agent process still runs.
pub const LIVENESS_SWEEP_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);

/// Runs [`AppState::sweep_dead_sessions`] every [`LIVENESS_SWEEP_INTERVAL`].
pub fn spawn_liveness_sweep(state: AppState) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(LIVENESS_SWEEP_INTERVAL);
        loop {
            tick.tick().await;
            state.sweep_dead_sessions(process_alive).await;
        }
    });
}

/// What a pin slot is unique within: the session's GitHub repo, else its cwd.
pub fn pin_key(session: &Session) -> &str {
    session.repo.as_deref().unwrap_or(&session.cwd)
}

impl Inner {
    /// Apply a change the way the handlers made it.
    pub fn apply(&mut self, event: CanvasEvent) {
        match event {
            CanvasEvent::SessionUpserted(session) => {
                self.sessions.insert(session.id.clone(), session);
            }
            CanvasEvent::CardUpserted(card) => self.upsert_card(card),
            CanvasEvent::CardRemoved(id) => {
                self.cards.retain(|c| c.id != id);
                self.forget(&id);
            }
            CanvasEvent::SessionRemoved(id) => {
                self.sessions.remove(&id);
                let (removed, kept): (VecDeque<_>, VecDeque<_>) =
                    self.cards.drain(..).partition(|c| c.session_id == id);
                self.cards = kept;
                for card in removed {
                    self.forget(&card.id);
                }
            }
            CanvasEvent::CardData { id, value } => {
                if self.cards.iter().any(|c| c.id == id) {
                    self.data.insert(id, value);
                }
            }
        }
    }

    /// Drop what is held beside a card that no longer exists.
    fn forget(&mut self, card_id: &str) {
        self.replies.remove(card_id);
        self.data.remove(card_id);
    }

    /// Drop cards last touched before `cutoff` (never a pinned one), then every
    /// session with no card left: a session exists only once it has posted,
    /// however recently it started or ended.
    pub fn prune_before(&mut self, cutoff: DateTime<Utc>) {
        let keep = |c: &Card| {
            c.pin.is_some()
                || DateTime::parse_from_rfc3339(&c.at)
                    .map_or(true, |t| t.with_timezone(&Utc) >= cutoff)
        };
        let stale: Vec<String> = self
            .cards
            .iter()
            .filter(|c| !keep(c))
            .map(|c| c.id.clone())
            .collect();
        for id in &stale {
            self.forget(id);
        }
        self.cards.retain(keep);
        let cards = &self.cards;
        self.sessions
            .retain(|id, _| cards.iter().any(|c| &c.session_id == id));
    }

    /// Insert or replace a card, moving it to the front either way — an
    /// update is a fresh touch, same as a new post, so it competes for
    /// ring space on the same terms and doesn't sit wherever it originally
    /// landed waiting to be evicted despite being current.
    pub fn upsert_card(&mut self, card: Card) {
        if let Some(idx) = self.cards.iter().position(|c| c.id == card.id) {
            self.cards.remove(idx);
        }
        self.push_card(card);
    }

    /// Push a new card at the front, evicting the oldest unpinned card while
    /// the ring is over capacity. A pinned card is never evicted, but it counts
    /// toward the capacity, so it leaves less room for unpinned cards.
    pub fn push_card(&mut self, card: Card) {
        self.cards.push_front(card);
        while self.cards.len() > CARD_RING_CAPACITY {
            // Index 0 is the card just pushed; it is never the one evicted.
            let Some(idx) = self.cards.iter().rposition(|c| c.pin.is_none()) else {
                break;
            };
            if idx == 0 {
                break;
            }
            if let Some(evicted) = self.cards.remove(idx) {
                self.forget(&evicted.id);
            }
        }
    }

    /// The card holding `slot` under `key` (see [`pin_key`]).
    pub fn pinned_in(&self, key: &str, slot: &str) -> Option<&Card> {
        self.cards.iter().find(|c| {
            c.pin.as_ref().is_some_and(|p| p.slot == slot)
                && self
                    .sessions
                    .get(&c.session_id)
                    .is_some_and(|s| pin_key(s) == key)
        })
    }
}
