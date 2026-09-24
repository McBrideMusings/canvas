use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use canvas_core::{Session, TurnCard};
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
    CardUpserted(TurnCard),
    CardRemoved(String),
    SessionRemoved(String),
}

#[derive(Default)]
pub struct Inner {
    pub sessions: HashMap<String, Session>,
    /// Front = newest.
    pub cards: VecDeque<TurnCard>,
}

#[derive(Clone)]
pub struct AppState {
    pub inner: Arc<RwLock<Inner>>,
    pub events: broadcast::Sender<CanvasEvent>,
    store: Option<Store>,
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
            store: None,
        }
    }

    /// State backed by `dir/stream.jsonl`: reloads the last 24 hours from it
    /// and appends every later change.
    pub fn open(dir: &std::path::Path) -> Self {
        let (store, inner) = Store::open(dir);
        let (tx, _rx) = broadcast::channel(1024);
        AppState {
            inner: Arc::new(RwLock::new(inner)),
            events: tx,
            store: Some(store),
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
            CanvasEvent::CardUpserted(card) => match self.cards.iter().position(|c| c.id == card.id) {
                Some(idx) => self.cards[idx] = card,
                None => self.push_card(card),
            },
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

    /// Push a new card at the front, evicting the oldest if the ring is full.
    pub fn push_card(&mut self, card: TurnCard) {
        self.cards.push_front(card);
        while self.cards.len() > CARD_RING_CAPACITY {
            self.cards.pop_back();
        }
    }

    /// Find the open card for a session, if any.
    pub fn open_card_index(&self, session_id: &str) -> Option<usize> {
        self.cards
            .iter()
            .position(|c| c.session_id == session_id && c.open)
    }
}
