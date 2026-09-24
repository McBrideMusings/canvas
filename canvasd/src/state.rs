use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use canvas_core::{Session, TurnCard};
use tokio::sync::{broadcast, RwLock};

pub const CARD_RING_CAPACITY: usize = 500;

#[derive(Debug, Clone)]
pub enum CanvasEvent {
    SessionUpserted(Session),
    CardUpserted(TurnCard),
}

pub struct Inner {
    pub sessions: HashMap<String, Session>,
    /// Front = newest.
    pub cards: VecDeque<TurnCard>,
}

#[derive(Clone)]
pub struct AppState {
    pub inner: Arc<RwLock<Inner>>,
    pub events: broadcast::Sender<CanvasEvent>,
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
        }
    }

    pub fn publish(&self, event: CanvasEvent) {
        // No receivers (e.g. no SSE clients connected) is not an error.
        let _ = self.events.send(event);
    }
}

impl Inner {
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
