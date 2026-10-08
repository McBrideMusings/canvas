use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use canvas_core::{Card, Session};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, RwLock};

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
    /// Asks every open viewer to bring a card into view (`canvas focus`).
    /// Sent to viewers only: never written to the persisted stream.
    CardFocus(String),
    /// Asks a viewer to capture a card as rendered (`canvas snapshot`); the
    /// viewer answers on `POST /api/snapshots/:request`. Sent to viewers
    /// only: never written to the persisted stream.
    CardSnapshot {
        id: String,
        request: String,
    },
    /// Asks every open viewer to switch to this theme (`canvas theme`), as a
    /// click on the title-bar button would. Sent to viewers only: never
    /// written to the persisted stream.
    ThemeSet(Theme),
    /// An artifact was created or its files changed. Sent to viewers only:
    /// artifacts persist in `artifacts.json`, not the stream.
    ArtifactUpserted(Box<canvas_core::ArtifactView>),
    /// An artifact was deleted. Sent to viewers only.
    ArtifactRemoved(String),
    /// A value pushed into an artifact's widget and page (`canvas data
    /// art-…` or its refresh command). Sent to viewers only.
    ArtifactData {
        id: String,
        value: serde_json::Value,
    },
    /// Asks every open viewer to switch to the Artifacts page and open this
    /// artifact (`canvas focus art-…`). Sent to viewers only.
    ArtifactFocus(String),
    /// Asks every open viewer to rebuild this artifact's pane at its entry
    /// page, forgetting where its page last was (`canvas artifact reset`).
    /// Sent to viewers only.
    ArtifactReset(String),
    /// Asks a viewer to capture an artifact's pane as rendered (`canvas
    /// snapshot art-…`); answered like `CardSnapshot`. Sent to viewers only.
    ArtifactSnapshot {
        id: String,
        request: String,
    },
    /// Asks every open viewer to change an artifact's pane the way the
    /// person would by hand (`canvas artifact pane`). Sent to viewers only.
    ArtifactPane {
        id: String,
        action: PaneAction,
    },
}

/// One change to an artifact's pane: a size for its frame (clamped to the
/// window by the viewer), back to its declared size, or in and out of full
/// window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "lowercase")]
pub enum PaneAction {
    Resize { width: u32, height: u32 },
    Reset,
    Full,
    Exit,
}

/// What a viewer reports its artifact pane showing (`PUT /api/artifact-pane`):
/// the open artifact, the frame's size in CSS pixels, whether it fills the
/// window, and the size the person chose for it, if any.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PaneReport {
    pub id: String,
    pub width: u32,
    pub height: u32,
    pub full: bool,
    pub chosen: Option<PaneSize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneSize {
    pub width: u32,
    pub height: u32,
}

/// The viewer's light or dark palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    Light,
    Dark,
}

impl Theme {
    pub fn as_str(self) -> &'static str {
        match self {
            Theme::Light => "light",
            Theme::Dark => "dark",
        }
    }
}

/// What a viewer sent back for one `canvas snapshot` request.
#[derive(Debug)]
pub enum SnapshotReply {
    /// `clipped` when the card ran past the window, so the PNG stops at its edge.
    Png { png: Vec<u8>, clipped: bool },
    /// Why the viewer could not capture the card, in its own words.
    Failed(String),
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
    /// Every artifact record; see [`crate::artifacts`].
    pub artifacts: Arc<RwLock<crate::artifacts::Artifacts>>,
    /// The artifact folders being watched; see [`crate::watcher`].
    pub watcher: Arc<crate::watcher::Watcher>,
    /// One refresh loop per refreshing artifact; see [`crate::refresh`].
    pub(crate) refreshes: Arc<std::sync::Mutex<crate::refresh::Slots>>,
    pub(crate) generation: Arc<std::sync::atomic::AtomicU64>,
    /// Snapshot requests waiting for a viewer's answer, by request id.
    pub(crate) snapshots:
        Arc<std::sync::Mutex<HashMap<String, tokio::sync::oneshot::Sender<SnapshotReply>>>>,
    /// The theme a viewer last reported showing (`PUT /api/theme`), in
    /// memory only; `None` until a viewer reports one.
    pub(crate) viewer_theme: Arc<std::sync::Mutex<Option<Theme>>>,
    /// The artifact pane a viewer last reported showing, in memory only;
    /// `None` until a viewer reports one.
    pub(crate) viewer_pane: Arc<std::sync::Mutex<Option<PaneReport>>>,
    /// The git roots sessions have posted from, newest first, as
    /// `projects.json` holds them; see [`AppState::record_project`].
    pub(crate) projects: Arc<std::sync::Mutex<Vec<canvas_core::instructions::SeenProject>>>,
    /// Held across every instruction-layer write, so two requests never share
    /// `write_atomic`'s temp file name.
    pub(crate) layer_writes: Arc<tokio::sync::Mutex<()>>,
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
            artifacts: Arc::default(),
            watcher: Arc::default(),
            refreshes: Arc::default(),
            generation: Arc::default(),
            snapshots: Arc::default(),
            viewer_theme: Arc::default(),
            viewer_pane: Arc::default(),
            projects: Arc::default(),
            layer_writes: Arc::default(),
            store: None,
            data_dir: None,
        }
    }

    /// State backed by `dir/stream.jsonl` for sessions and cards, and
    /// `dir/artifacts.json` and `dir/projects.json`: reloads them on start.
    pub async fn open(dir: &std::path::Path) -> Self {
        let (store, inner) = Store::open(dir);
        let artifacts = crate::artifacts::Artifacts::load(dir).await;
        let projects = canvas_core::instructions::seen_projects(dir);
        let (tx, _rx) = broadcast::channel(1024);
        AppState {
            inner: Arc::new(RwLock::new(inner)),
            events: tx,
            artifacts: Arc::new(RwLock::new(artifacts)),
            watcher: Arc::default(),
            refreshes: Arc::default(),
            generation: Arc::default(),
            snapshots: Arc::default(),
            viewer_theme: Arc::default(),
            viewer_pane: Arc::default(),
            projects: Arc::new(std::sync::Mutex::new(projects)),
            layer_writes: Arc::default(),
            store: Some(store),
            data_dir: Some(dir.to_path_buf()),
        }
    }

    /// The data dir this state persists to; `None` for `AppState::new()`.
    pub fn data_dir(&self) -> Option<&std::path::Path> {
        self.data_dir.as_deref()
    }

    /// The projects recorded so far, newest first.
    pub fn seen_projects(&self) -> Vec<canvas_core::instructions::SeenProject> {
        self.projects
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Records `cwd`'s git root as seen now, moving it to the front, and
    /// rewrites `projects.json` (kept only in memory without a data dir). A
    /// `cwd` outside git records nothing.
    pub fn record_project(&self, cwd: &str) {
        let Some(root) = canvas_core::instructions::git_root(std::path::Path::new(cwd)) else {
            return;
        };
        let name = root
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| root.display().to_string());
        let mut projects = self.projects.lock().unwrap_or_else(|e| e.into_inner());
        projects.retain(|p| p.root != root);
        projects.insert(
            0,
            canvas_core::instructions::SeenProject {
                root: root.clone(),
                name,
                last_seen: chrono::Utc::now().to_rfc3339(),
            },
        );
        let Some(dir) = &self.data_dir else {
            return;
        };
        let path = dir.join(canvas_core::instructions::PROJECTS_FILE);
        let written = serde_json::to_vec_pretty(&*projects)
            .map_err(|e| e.to_string())
            .and_then(|json| {
                canvas_core::instructions::write_atomic(&path, &json).map_err(|e| e.to_string())
            });
        match written {
            Ok(()) => canvas_core::log::info("project recorded", &[("root", &root.display())]),
            Err(e) => canvas_core::log::error(
                "project record failed",
                &[("root", &root.display()), ("error", &e)],
            ),
        }
    }

    /// Block until every change published so far is on disk. No-op without a store.
    pub fn flush_store(&self) {
        if let Some(store) = &self.store {
            store.flush();
        }
    }

    /// Ends a session the way the session-end route does: stamps `ended_at`
    /// and publishes it. `None` when the id names no session.
    pub fn end_session(&self, inner: &mut Inner, id: &str) -> Option<Session> {
        let session = inner.sessions.get_mut(id)?;
        session.ended_at = Some(chrono::Utc::now().to_rfc3339());
        let session = session.clone();
        self.publish(CanvasEvent::SessionUpserted(session.clone()));
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
            canvas_core::log::info("agent process gone; ending session", &[("session", id)]);
            self.end_session(&mut inner, id);
        }
        dead.len()
    }

    /// Persists `event` (unless it is viewer-only) and sends it to every
    /// connected viewer. Returns how many viewers it reached: each open
    /// `/api/events` stream holds one receiver. None connected is not an error.
    pub fn publish(&self, event: CanvasEvent) -> usize {
        if let Some(store) = &self.store {
            if !matches!(
                event,
                CanvasEvent::CardData { .. }
                    | CanvasEvent::CardFocus(_)
                    | CanvasEvent::CardSnapshot { .. }
                    | CanvasEvent::ThemeSet(_)
                    | CanvasEvent::ArtifactUpserted(_)
                    | CanvasEvent::ArtifactRemoved(_)
                    | CanvasEvent::ArtifactData { .. }
                    | CanvasEvent::ArtifactFocus(_)
                    | CanvasEvent::ArtifactReset(_)
                    | CanvasEvent::ArtifactSnapshot { .. }
                    | CanvasEvent::ArtifactPane { .. }
            ) {
                store.append(&event);
            }
        }
        self.events.send(event).unwrap_or(0)
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
            CanvasEvent::CardFocus(_)
            | CanvasEvent::CardSnapshot { .. }
            | CanvasEvent::ThemeSet(_)
            | CanvasEvent::ArtifactUpserted(_)
            | CanvasEvent::ArtifactRemoved(_)
            | CanvasEvent::ArtifactData { .. }
            | CanvasEvent::ArtifactFocus(_)
            | CanvasEvent::ArtifactReset(_)
            | CanvasEvent::ArtifactSnapshot { .. }
            | CanvasEvent::ArtifactPane { .. } => {}
        }
    }

    /// Drop what is held beside a card that no longer exists.
    fn forget(&mut self, card_id: &str) {
        self.replies.remove(card_id);
        self.data.remove(card_id);
    }

    /// Drop cards last touched before `cutoff`, then every session with no
    /// card left: a session exists only once it has posted, however recently
    /// it started or ended.
    pub fn prune_before(&mut self, cutoff: DateTime<Utc>) {
        let keep = |c: &Card| {
            DateTime::parse_from_rfc3339(c.touched_at())
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

    /// Push a new card at the front, evicting the oldest while the ring is
    /// over capacity.
    pub fn push_card(&mut self, card: Card) {
        self.cards.push_front(card);
        while self.cards.len() > CARD_RING_CAPACITY {
            if let Some(evicted) = self.cards.pop_back() {
                self.forget(&evicted.id);
            }
        }
    }
}
