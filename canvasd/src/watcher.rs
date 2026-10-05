//! Watches artifact folders and reloads an artifact when its files change.
//!
//! Each artifact id has one watched path: its owned folder, or the folder or
//! HTML file it links. Paths can nest or repeat (two links into one repo), so
//! a write belongs to every artifact whose path holds it. Writes are gathered
//! per artifact until [`QUIET`] passes with no write (or [`MAX_WAIT`] since the
//! burst's first write), then the artifact's `updatedAt` is stamped and
//! `artifact-upserted` goes out once; the viewer reloads the open pane when
//! `updatedAt` changes. A burst that leaves the folder's
//! [`fingerprint`](crate::artifacts::fingerprint) as it was last stamped
//! (a `put`, which stamps itself) publishes nothing.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use notify::{EventKind, RecursiveMode, Watcher as _};
use tokio::time::Instant;

use crate::artifact_routes::save_and_publish;
use crate::state::AppState;

/// How long a folder must go without a write before its burst ends.
pub const QUIET: Duration = Duration::from_millis(200);
/// The longest a burst of continuous writes waits before it reloads anyway.
pub const MAX_WAIT: Duration = Duration::from_secs(2);

/// The paths being watched, by artifact id. Until [`spawn_artifact_watcher`]
/// starts the OS watcher, `watch` only records the path.
#[derive(Default)]
pub struct Watcher {
    inner: std::sync::Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    os: Option<notify::RecommendedWatcher>,
    /// Canonical paths, since the OS reports events under those. Two ids may
    /// hold the same path; the OS watch stays until neither does.
    paths: HashMap<String, PathBuf>,
    /// Artifacts a `put` is copying into, with how many puts: their bursts
    /// wait until the last put has stamped, however long the copy takes.
    held: HashMap<String, usize>,
}

impl Inner {
    /// Stops the OS watch on `path` unless another artifact still holds it.
    fn release(&mut self, path: &Path) {
        if self.paths.values().any(|p| p == path) {
            return;
        }
        if let Some(os) = self.os.as_mut() {
            let _ = os.unwatch(path);
        }
    }
}

/// While alive, the artifact's bursts don't end; see [`Watcher::hold`].
pub struct Hold {
    watcher: Arc<Watcher>,
    id: String,
}

impl Drop for Hold {
    fn drop(&mut self) {
        let mut inner = self.watcher.lock();
        if let Some(n) = inner.held.get_mut(&self.id) {
            *n -= 1;
            if *n == 0 {
                inner.held.remove(&self.id);
            }
        }
    }
}

impl Watcher {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Watches `path` recursively on behalf of artifact `id`, replacing any
    /// path `id` had.
    pub fn watch(&self, id: &str, path: &Path) {
        let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let mut inner = self.lock();
        if let Some(old) = inner.paths.insert(id.to_string(), path.clone()) {
            inner.release(&old);
        }
        if let Some(os) = inner.os.as_mut() {
            if let Err(e) = os.watch(&path, RecursiveMode::Recursive) {
                canvas_core::log::warn(
                    "artifact folder not watched",
                    &[("id", &id), ("path", &path.display()), ("error", &e)],
                );
            }
        }
    }

    /// Stops watching artifact `id`'s path.
    pub fn unwatch(&self, id: &str) {
        let mut inner = self.lock();
        if let Some(path) = inner.paths.remove(id) {
            inner.release(&path);
        }
    }

    /// Keeps `id`'s bursts pending until the returned guard drops, so the
    /// writes of a `put` end in one burst after the put has stamped, which
    /// then matches the put's fingerprint and publishes nothing.
    pub fn hold(self: &Arc<Self>, id: &str) -> Hold {
        *self.lock().held.entry(id.to_string()).or_default() += 1;
        Hold {
            watcher: Arc::clone(self),
            id: id.to_string(),
        }
    }

    fn is_held(&self, id: &str) -> bool {
        self.lock().held.contains_key(id)
    }

    /// Every artifact whose watched path holds `path`. Each checks its own
    /// fingerprint, so an outer artifact reloads for a write inside a nested
    /// one only when its own files changed too, which they did.
    fn owners(&self, path: &Path) -> Vec<String> {
        self.lock()
            .paths
            .iter()
            .filter(|(_, root)| path.starts_with(root))
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// Hands the watcher the OS watcher and watches every path recorded so far.
    fn start(&self, mut os: notify::RecommendedWatcher) -> usize {
        let mut inner = self.lock();
        for (id, path) in &inner.paths {
            if let Err(e) = os.watch(path, RecursiveMode::Recursive) {
                canvas_core::log::warn(
                    "artifact folder not watched",
                    &[("id", id), ("path", &path.display()), ("error", &e)],
                );
            }
        }
        inner.os = Some(os);
        inner.paths.len()
    }
}

/// Starts watching every artifact's folder, and every folder `watch`ed from
/// here on, and runs the loop that turns each burst of writes into one reload.
pub fn spawn_artifact_watcher(state: AppState) {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<PathBuf>();
    let os = notify::recommended_watcher(move |res: notify::Result<notify::Event>| match res {
        Ok(event) if !matches!(event.kind, EventKind::Access(_)) => {
            for path in event.paths {
                let _ = tx.send(path);
            }
        }
        Ok(_) => {}
        Err(e) => canvas_core::log::warn("artifact watcher error", &[("error", &e)]),
    });
    let os = match os {
        Ok(os) => os,
        Err(e) => {
            canvas_core::log::error("artifact watcher not started", &[("error", &e)]);
            return;
        }
    };
    tokio::spawn(async move {
        {
            let artifacts = state.artifacts.read().await;
            for (id, record) in &artifacts.records {
                if let Some(root) = artifacts.source_path(record) {
                    state.watcher.watch(id, &root);
                }
            }
        }
        let watched = state.watcher.start(os);
        canvas_core::log::info("artifact watcher started", &[("watched", &watched)]);

        // Per artifact: the burst's first write, its latest, and how many.
        let mut pending: HashMap<String, (Instant, Instant, usize)> = HashMap::new();
        loop {
            let due = pending
                .values()
                .map(|(first, last, _)| (*last + QUIET).min(*first + MAX_WAIT))
                .min();
            let wait = async {
                match due {
                    Some(at) => tokio::time::sleep_until(at).await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                path = rx.recv() => {
                    let Some(path) = path else { break };
                    let now = Instant::now();
                    for id in state.watcher.owners(&path) {
                        let burst = pending.entry(id).or_insert((now, now, 0));
                        burst.1 = now;
                        burst.2 += 1;
                    }
                }
                () = wait => {
                    let now = Instant::now();
                    let ready: Vec<(String, usize)> = pending
                        .iter()
                        .filter(|(_, (first, last, _))| now >= *last + QUIET || now >= *first + MAX_WAIT)
                        .map(|(id, (_, _, writes))| (id.clone(), *writes))
                        .collect();
                    for (id, writes) in ready {
                        if state.watcher.is_held(&id) {
                            // Look again once a quiet period has passed.
                            if let Some(burst) = pending.get_mut(&id) {
                                *burst = (now, now, burst.2);
                            }
                            continue;
                        }
                        pending.remove(&id);
                        files_changed(&state, &id, writes).await;
                    }
                }
            }
        }
    });
}

/// One burst ended: stamps `updatedAt` and tells every viewer, unless the
/// files still match the fingerprint of their last stamp. A linked path that
/// disappeared is a change too: the view it publishes says `sourceMissing`.
async fn files_changed(state: &AppState, id: &str, writes: usize) {
    let Some(folder) = state.artifacts.read().await.source_path_of(id) else {
        return;
    };
    let Ok(print) =
        tokio::task::spawn_blocking(move || crate::artifacts::fingerprint(&folder)).await
    else {
        return;
    };
    let mut artifacts = state.artifacts.write().await;
    if artifacts.fingerprints.get(id) == Some(&print) {
        canvas_core::log::info(
            "artifact files unchanged since last stamp",
            &[("id", &id), ("writes", &writes)],
        );
        return;
    }
    let Some(record) = artifacts.records.get_mut(id) else {
        return;
    };
    record.updated_at = chrono::Utc::now().to_rfc3339();
    artifacts.fingerprints.insert(id.to_string(), print);
    if let Ok(view) = save_and_publish(state, &artifacts, id).await {
        canvas_core::log::info(
            "artifact files changed",
            &[
                ("id", &id),
                ("writes", &writes),
                ("updated_at", &view.artifact.updated_at),
                ("source_missing", &view.source_missing),
            ],
        );
    }
}
