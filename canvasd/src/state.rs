use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use canvas_core::{Card, Session};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, RwLock};

use crate::guidance::GuidanceConfig;
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
}

#[derive(Default)]
pub struct Inner {
    pub sessions: HashMap<String, Session>,
    /// Front = newest.
    pub cards: VecDeque<Card>,
}

#[derive(Clone)]
pub struct AppState {
    pub inner: Arc<RwLock<Inner>>,
    pub events: broadcast::Sender<CanvasEvent>,
    pub guidance: Arc<RwLock<GuidanceConfig>>,
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
            })),
            events: tx,
            guidance: Arc::new(RwLock::new(GuidanceConfig::default())),
            store: None,
            data_dir: None,
        }
    }

    /// State backed by `dir/stream.jsonl` for sessions and cards, and
    /// `dir/guidance.json` for guidance overrides: reloads both on start.
    pub async fn open(dir: &std::path::Path) -> Self {
        let (store, inner) = Store::open(dir);
        let guidance = crate::guidance::load(dir).await;
        let (tx, _rx) = broadcast::channel(1024);
        AppState {
            inner: Arc::new(RwLock::new(inner)),
            events: tx,
            guidance: Arc::new(RwLock::new(guidance)),
            store: Some(store),
            data_dir: Some(dir.to_path_buf()),
        }
    }

    /// Persist the current guidance config to `guidance.json`. No-op
    /// without a data dir (e.g. `AppState::new()` in tests).
    pub async fn save_guidance(&self) {
        if let Some(dir) = &self.data_dir {
            let config = self.guidance.read().await;
            crate::guidance::save(dir, &config).await;
        }
    }

    /// Block until every change published so far is on disk. No-op without a store.
    pub fn flush_store(&self) {
        if let Some(store) = &self.store {
            store.flush();
        }
    }

    pub fn publish(&self, event: CanvasEvent) {
        if let Some(store) = &self.store {
            store.append(&event);
        }
        // No receivers (e.g. no SSE clients connected) is not an error.
        let _ = self.events.send(event);
    }
}

impl Inner {
    /// Apply a change the way the handlers made it.
    pub fn apply(&mut self, event: CanvasEvent) {
        match event {
            CanvasEvent::SessionUpserted(session) => {
                self.sessions.insert(session.id.clone(), session);
            }
            CanvasEvent::CardUpserted(card) => self.upsert_card(card),
            CanvasEvent::CardRemoved(id) => self.cards.retain(|c| c.id != id),
            CanvasEvent::SessionRemoved(id) => {
                self.sessions.remove(&id);
                self.cards.retain(|c| c.session_id != id);
            }
        }
    }

    /// Drop cards last touched before `cutoff`, then sessions with no card left
    /// whose own last timestamp (end, else start) is also before it.
    pub fn prune_before(&mut self, cutoff: DateTime<Utc>) {
        let fresh = |ts: &str| {
            DateTime::parse_from_rfc3339(ts).map_or(true, |t| t.with_timezone(&Utc) >= cutoff)
        };
        self.cards.retain(|c| fresh(&c.at));
        let cards = &self.cards;
        self.sessions.retain(|id, s| {
            cards.iter().any(|c| &c.session_id == id)
                || fresh(s.ended_at.as_deref().unwrap_or(&s.started_at))
        });
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

    /// Push a new card at the front, evicting the oldest if the ring is full.
    pub fn push_card(&mut self, card: Card) {
        self.cards.push_front(card);
        while self.cards.len() > CARD_RING_CAPACITY {
            self.cards.pop_back();
        }
    }
}
