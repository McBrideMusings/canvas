//! Append-only JSONL log of every state change, so a restart keeps the stream.
//!
//! One line per `CanvasEvent`. A dedicated OS thread owns the file and drains a
//! channel, so a request never waits on disk.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::mpsc;

use chrono::{DateTime, Duration, Utc};

use crate::state::{CanvasEvent, Inner};

pub const STREAM_FILE: &str = "stream.jsonl";
const RETENTION_HOURS: i64 = 24;

enum Msg {
    Line(String),
    Flush(mpsc::SyncSender<()>),
}

#[derive(Clone)]
pub struct Store {
    tx: mpsc::Sender<Msg>,
}

impl Store {
    /// Rebuild state from `dir/stream.jsonl`, rewrite the file as a compacted
    /// snapshot of that state, and start the appender thread.
    pub fn open(dir: &Path) -> (Store, Inner) {
        let path = dir.join(STREAM_FILE);
        if let Err(e) = fs::create_dir_all(dir) {
            canvas_core::log::error(
                "cannot create data dir",
                &[("path", &dir.display()), ("error", &e)],
            );
        }
        let inner = load(&path, Utc::now());
        canvas_core::log::info(
            "stream reloaded",
            &[
                ("path", &path.display()),
                ("sessions", &inner.sessions.len()),
                ("cards", &inner.cards.len()),
            ],
        );
        if let Err(e) = write_snapshot(&path, &inner) {
            canvas_core::log::error(
                "cannot compact stream",
                &[("path", &path.display()), ("error", &e)],
            );
        }

        let (tx, rx) = mpsc::channel::<Msg>();
        std::thread::spawn(move || {
            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .map_err(|e| {
                    canvas_core::log::error(
                        "cannot open stream",
                        &[("path", &path.display()), ("error", &e)],
                    )
                })
                .ok();
            for msg in rx {
                match msg {
                    Msg::Line(line) => {
                        if let Some(f) = file.as_mut() {
                            if let Err(e) = f.write_all(line.as_bytes()) {
                                canvas_core::log::error("stream write failed", &[("error", &e)]);
                            }
                        }
                    }
                    Msg::Flush(done) => {
                        let _ = done.send(());
                    }
                }
            }
        });
        (Store { tx }, inner)
    }

    pub fn append(&self, event: &CanvasEvent) {
        match serde_json::to_string(event) {
            Ok(mut line) => {
                line.push('\n');
                let _ = self.tx.send(Msg::Line(line));
            }
            Err(e) => canvas_core::log::error("stream serialize failed", &[("error", &e)]),
        }
    }

    /// Block until every line appended so far has been written.
    pub fn flush(&self) {
        let (done_tx, done_rx) = mpsc::sync_channel(0);
        if self.tx.send(Msg::Flush(done_tx)).is_ok() {
            let _ = done_rx.recv();
        }
    }
}

fn load(path: &Path, now: DateTime<Utc>) -> Inner {
    let mut inner = Inner::default();
    let Ok(bytes) = fs::read(path) else {
        return inner;
    };
    // Lossy so a crash that tore a multi-byte character cannot fail the read.
    let text = String::from_utf8_lossy(&bytes);
    let mut skipped = 0;
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        match serde_json::from_str::<CanvasEvent>(line) {
            Ok(event) => inner.apply(event),
            Err(_) => skipped += 1,
        }
    }
    if skipped > 0 {
        canvas_core::log::warn(
            "skipped unparsable stream lines",
            &[("count", &skipped), ("path", &path.display())],
        );
    }
    // A card stored before canvasd refused HTML that freezes Canvas.app's
    // WebKit would freeze every viewer that loads it.
    inner.cards.retain(|card| {
        let freezes = canvas_core::html::webkit_freeze(&card.html).is_some();
        if freezes {
            canvas_core::log::warn(
                "card dropped on reload: it freezes WebKit",
                &[("card", &card.id)],
            );
        }
        !freezes
    });
    inner.prune_before(now - Duration::hours(RETENTION_HOURS));
    inner
}

/// Write `stream.jsonl` as sessions, then cards oldest-first, via a temp file
/// in the same directory and a rename.
fn write_snapshot(path: &Path, inner: &Inner) -> std::io::Result<()> {
    let mut out = String::new();
    let events = inner
        .sessions
        .values()
        .cloned()
        .map(CanvasEvent::SessionUpserted)
        .chain(
            inner
                .cards
                .iter()
                .rev()
                .cloned()
                .map(CanvasEvent::CardUpserted),
        );
    for event in events {
        out.push_str(&serde_json::to_string(&event).map_err(std::io::Error::other)?);
        out.push('\n');
    }
    let tmp = path.with_extension("jsonl.tmp");
    fs::write(&tmp, out)?;
    fs::rename(&tmp, path)
}
