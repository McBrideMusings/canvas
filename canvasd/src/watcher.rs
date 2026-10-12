//! Watches artifact folders and reloads an artifact when its files change.
//!
//! Each artifact id has one watched path: its owned folder, or the folder or
//! HTML file it links. Paths can nest or repeat (two links into one repo), so
//! a write belongs to every artifact whose path holds it. Writes are gathered
//! per artifact until [`QUIET`] passes with no write (or [`MAX_WAIT`] since the
//! burst's first write), then the artifact's `updatedAt` is stamped and
//! `artifact-upserted` goes out once, listing the paths the burst wrote; the
//! viewer tells the open page when `updatedAt` changes. A burst that leaves the folder's
//! [`fingerprint`](crate::artifacts::fingerprint) as it was last stamped
//! (a `put`, which stamps itself) publishes nothing. Writes under an artifact's `canvas-data/` folder (a
//! page's saved state), and to any path with an
//! [`ignored_name`](crate::artifacts::ignored_name) in it (`.DS_Store`, a swap
//! file, a linked repo's `.git`, `target` or `node_modules`), belong to no burst.
//!
//! The OS refuses to watch a path that doesn't exist (a link whose folder is
//! gone when canvasd starts), so a path whose watch failed is tried again
//! every [`RETRY_EVERY`]; once it is watched, it reloads as a burst would.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use notify::{EventKind, RecursiveMode, Watcher as _};
use tokio::time::Instant;

use crate::artifact_routes::save_and_publish;
use crate::artifacts::ignored_name;
use crate::provenance::{Action, Actor};
use crate::state::AppState;

/// How long a folder must go without a write before its burst ends.
pub const QUIET: Duration = Duration::from_millis(200);
/// The longest a burst of continuous writes waits before it reloads anyway.
pub const MAX_WAIT: Duration = Duration::from_secs(2);
/// How often a path whose OS watch failed is tried again.
pub const RETRY_EVERY: Duration = Duration::from_secs(1);

/// The paths being watched, by artifact id. Until [`spawn_artifact_watcher`]
/// starts the OS watcher, `watch` only records the path.
///
/// Starting an FSEvents stream can block for seconds on a busy machine, so
/// the OS watcher sits in its own mutex, taken before `inner` and held only by
/// the calls that change what the OS watches; those run on the blocking pool.
/// `inner` is never held across an OS call, so [`Watcher::owners`] and the
/// hold checks never wait behind one.
#[derive(Default)]
pub struct Watcher {
    os: std::sync::Mutex<Option<notify::RecommendedWatcher>>,
    inner: std::sync::Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    /// Canonical paths, since the OS reports events under those. Two ids may
    /// hold the same path; the OS watch stays until neither does.
    paths: HashMap<String, PathBuf>,
    /// Artifacts a `put` is copying into, with how many puts: their bursts
    /// wait until the last put has stamped, however long the copy takes.
    held: HashMap<String, usize>,
    /// Artifacts whose path has no OS watch because watching it failed, with
    /// the last error logged for it, so a retry logs only a new error.
    failed: HashMap<String, String>,
}

type OsGuard<'a> = std::sync::MutexGuard<'a, Option<notify::RecommendedWatcher>>;

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

    fn lock_os(&self) -> OsGuard<'_> {
        self.os.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Watches `path` recursively on behalf of artifact `id`, replacing any
    /// path `id` had. A path the OS can't watch yet is retried every
    /// [`RETRY_EVERY`].
    pub async fn watch(self: &Arc<Self>, id: &str, path: &Path) {
        let (this, id, path) = (Arc::clone(self), id.to_string(), path.to_path_buf());
        let task = tokio::task::spawn_blocking(move || this.watch_blocking(&id, &path));
        if let Err(e) = task.await {
            canvas_core::log::error("artifact folder watch failed", &[("error", &e)]);
        }
    }

    fn watch_blocking(&self, id: &str, path: &Path) {
        let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let mut os = self.lock_os();
        let old = {
            let mut inner = self.lock();
            let old = inner.paths.insert(id.to_string(), path.clone());
            if old.as_ref() == Some(&path) && !inner.failed.contains_key(id) {
                return;
            }
            old
        };
        self.watch_os(&mut os, id, &path);
        if let Some(old) = old {
            self.release(&mut os, &old);
        }
    }

    /// Stops watching artifact `id`'s path.
    pub async fn unwatch(self: &Arc<Self>, id: &str) {
        let (this, id) = (Arc::clone(self), id.to_string());
        let task = tokio::task::spawn_blocking(move || this.unwatch_blocking(&id));
        if let Err(e) = task.await {
            canvas_core::log::error("artifact folder unwatch failed", &[("error", &e)]);
        }
    }

    fn unwatch_blocking(&self, id: &str) {
        let mut os = self.lock_os();
        let path = {
            let mut inner = self.lock();
            inner.failed.remove(id);
            inner.paths.remove(id)
        };
        if let Some(path) = path {
            self.release(&mut os, &path);
        }
    }

    /// Stops the OS watch on `path` unless another artifact still holds it.
    fn release(&self, os: &mut OsGuard<'_>, path: &Path) {
        if self.lock().paths.values().any(|p| p == path) {
            return;
        }
        if let Some(os) = os.as_mut() {
            let _ = os.unwatch(path);
        }
    }

    /// Asks the OS to watch `path` for `id`, recording a failure for retry.
    fn watch_os(&self, os: &mut OsGuard<'_>, id: &str, path: &Path) {
        let Some(os) = os.as_mut() else { return };
        let result = os.watch(path, RecursiveMode::Recursive);
        let mut inner = self.lock();
        match result {
            Ok(()) => {
                inner.failed.remove(id);
            }
            Err(e) => {
                canvas_core::log::warn(
                    "artifact folder not watched",
                    &[("id", &id), ("path", &path.display()), ("error", &e)],
                );
                inner.failed.insert(id.to_string(), e.to_string());
            }
        }
    }

    /// Tries again to watch every path whose OS watch failed, and returns the
    /// ids now watched. A path still missing is skipped without asking the OS.
    /// The OS lock is taken per path, so a `watch` or `unwatch` waits behind
    /// at most one FSEvents start.
    fn retry_failed(&self) -> Vec<String> {
        let due: Vec<(String, PathBuf)> = {
            let inner = self.lock();
            inner
                .failed
                .keys()
                .filter_map(|id| Some((id.clone(), inner.paths.get(id)?.clone())))
                .collect()
        };
        let mut watched = Vec::new();
        for (id, path) in due {
            // Paths are recorded uncanonicalized while missing; the OS reports
            // events under the canonical path, which `owners` matches against.
            let Ok(canonical) = path.canonicalize() else {
                continue;
            };
            let mut os = self.lock_os();
            let Some(os) = os.as_mut() else {
                return watched;
            };
            // Only calls holding `os` change `paths` and `failed`: check that
            // a `watch` or `unwatch` since the list was read left `id` as is.
            {
                let inner = self.lock();
                if !inner.failed.contains_key(&id) || inner.paths.get(&id) != Some(&path) {
                    continue;
                }
            }
            let started = std::time::Instant::now();
            let result = os.watch(&canonical, RecursiveMode::Recursive);
            let ms = started.elapsed().as_millis();
            let mut inner = self.lock();
            match result {
                Ok(()) => {
                    canvas_core::log::info(
                        "artifact folder watched on retry",
                        &[("id", &id), ("path", &canonical.display()), ("ms", &ms)],
                    );
                    inner.failed.remove(&id);
                    inner.paths.insert(id.clone(), canonical);
                    watched.push(id);
                }
                Err(e) => {
                    let error = e.to_string();
                    let Some(last_error) = inner.failed.get_mut(&id) else {
                        continue;
                    };
                    if *last_error != error {
                        canvas_core::log::warn(
                            "artifact folder retry failed",
                            &[
                                ("id", &id),
                                ("path", &path.display()),
                                ("error", &error),
                                ("ms", &ms),
                            ],
                        );
                        *last_error = error;
                    }
                }
            }
        }
        watched
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

    /// Every artifact whose watched path holds `path`, with `path` relative
    /// to that artifact's (a linked HTML file is its own name; a watched
    /// folder itself is no path). Each checks its own fingerprint, so an
    /// outer artifact reloads for a write inside a nested one only when its
    /// own files changed too, which they did.
    fn owners(&self, path: &Path) -> Vec<(String, Option<String>)> {
        self.lock()
            .paths
            .iter()
            .filter_map(|(id, root)| {
                let rel = path.strip_prefix(root).ok()?;
                // Scratch files, `.git` and build output are not the page.
                if rel.components().any(|c| ignored_name(c.as_os_str())) {
                    return None;
                }
                // A page's saved state is not a change to the page.
                if rel
                    .components()
                    .next()
                    .is_some_and(|c| c.as_os_str() == crate::artifact_state::DIR)
                    && root.is_dir()
                {
                    return None;
                }
                let rel = if rel.as_os_str().is_empty() {
                    root.is_file()
                        .then(|| root.file_name())
                        .flatten()
                        .map(Path::new)
                } else {
                    Some(rel)
                };
                Some((id.clone(), rel.map(|r| r.to_string_lossy().into_owned())))
            })
            .collect()
    }

    /// Hands the watcher the OS watcher and watches every path recorded so
    /// far, returning how many the OS took (the rest are retried).
    fn start(&self, os: notify::RecommendedWatcher) -> usize {
        let mut guard = self.lock_os();
        *guard = Some(os);
        let paths: Vec<(String, PathBuf)> = self
            .lock()
            .paths
            .iter()
            .map(|(id, path)| (id.clone(), path.clone()))
            .collect();
        for (id, path) in &paths {
            self.watch_os(&mut guard, id, path);
        }
        paths.len() - self.lock().failed.len()
    }
}

/// Starts watching every artifact's folder, and every folder `watch`ed from
/// here on, and runs the loop that turns each burst of writes into one reload.
/// The receiver gets how many paths the OS took once the OS watcher has
/// started; it closes empty when the OS watcher can't be created.
pub fn spawn_artifact_watcher(state: AppState) -> tokio::sync::oneshot::Receiver<usize> {
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
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
            return started_rx;
        }
    };
    tokio::spawn(async move {
        {
            let artifacts = state.artifacts.read().await;
            for (id, record) in &artifacts.records {
                if let Some(root) = artifacts.source_path(record) {
                    state.watcher.watch(id, &root).await;
                }
            }
        }
        let watcher = Arc::clone(&state.watcher);
        let Ok(watched) = tokio::task::spawn_blocking(move || watcher.start(os)).await else {
            canvas_core::log::error(
                "artifact watcher not started",
                &[("error", &"start panicked")],
            );
            return;
        };
        canvas_core::log::info("artifact watcher started", &[("watched", &watched)]);
        let _ = started_tx.send(watched);
        let mut retried = spawn_retry(Arc::clone(&state.watcher));

        // Per artifact: the burst's first write, its latest, how many, and
        // the paths written, relative to the artifact's.
        let mut pending: HashMap<String, Burst> = HashMap::new();
        loop {
            let due = pending
                .values()
                .map(|b| (b.last + QUIET).min(b.first + MAX_WAIT))
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
                    for (id, rel) in state.watcher.owners(&path) {
                        let burst = pending.entry(id).or_insert_with(|| Burst::new(now));
                        burst.last = now;
                        burst.writes += 1;
                        if let Some(rel) = rel {
                            burst.paths.insert(rel);
                        }
                    }
                }
                Some(id) = retried.recv() => {
                    // A burst with no writes: what changed before the watch
                    // began is in the fingerprint, so it stamps once if
                    // anything did, and waits out a hold like any burst.
                    pending.entry(id).or_insert_with(|| Burst::new(Instant::now()));
                }
                () = wait => {
                    let now = Instant::now();
                    let ready: Vec<String> = pending
                        .iter()
                        .filter(|(_, b)| now >= b.last + QUIET || now >= b.first + MAX_WAIT)
                        .map(|(id, _)| id.clone())
                        .collect();
                    for id in ready {
                        if state.watcher.is_held(&id) {
                            // Look again once a quiet period has passed.
                            if let Some(burst) = pending.get_mut(&id) {
                                burst.first = now;
                                burst.last = now;
                            }
                            continue;
                        }
                        if let Some(burst) = pending.remove(&id) {
                            files_changed(&state, &id, burst).await;
                        }
                    }
                }
            }
        }
    });
    started_rx
}

/// Retries every failed OS watch each [`RETRY_EVERY`] on the blocking pool,
/// apart from the burst loop, which keeps gathering writes while a slow
/// FSEvents start runs, and sends it each id the OS now watches.
fn spawn_retry(watcher: Arc<Watcher>) -> tokio::sync::mpsc::UnboundedReceiver<String> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        let mut retry = tokio::time::interval(RETRY_EVERY);
        retry.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        while !tx.is_closed() {
            retry.tick().await;
            if watcher.lock().failed.is_empty() {
                continue;
            }
            let watcher = Arc::clone(&watcher);
            let Ok(ids) = tokio::task::spawn_blocking(move || watcher.retry_failed()).await else {
                continue;
            };
            for id in ids {
                let _ = tx.send(id);
            }
        }
    });
    rx
}

/// Whether a path a burst saw names one of the page's files. A folder is not
/// one, nor is a name with no extension that is gone again by the burst's end
/// (vim's `4913` probe); a removed file with an extension is a change the
/// page needs. [`Watcher::owners`] has already dropped [`ignored_name`]s.
fn page_file(folder: &Path, rel: &str) -> bool {
    let path = Path::new(rel);
    let full = if folder.is_file() {
        folder.to_path_buf()
    } else {
        folder.join(path)
    };
    match std::fs::metadata(&full) {
        Ok(meta) => !meta.is_dir(),
        Err(_) => path.extension().is_some(),
    }
}

/// One artifact's writes, gathered until it goes quiet.
struct Burst {
    first: Instant,
    last: Instant,
    writes: usize,
    paths: BTreeSet<String>,
}

impl Burst {
    fn new(now: Instant) -> Self {
        Self {
            first: now,
            last: now,
            writes: 0,
            paths: BTreeSet::new(),
        }
    }
}

/// One burst ended: stamps `updatedAt` and tells every viewer which paths it
/// wrote, unless the files still match the fingerprint of their last stamp.
/// A linked path that disappeared is a change too: the view it publishes says
/// `sourceMissing`. A burst a retried watch started wrote nothing it saw, so
/// it names no paths.
async fn files_changed(state: &AppState, id: &str, burst: Burst) {
    let writes = burst.writes;
    let Some(folder) = state.artifacts.read().await.source_path_of(id) else {
        return;
    };
    let root = folder.clone();
    let Ok(print) = tokio::task::spawn_blocking(move || crate::artifacts::fingerprint(&root)).await
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
    let paths: Vec<String> = burst
        .paths
        .into_iter()
        .filter(|rel| page_file(&folder, rel))
        .collect();
    let listed = paths.len();
    let changed = (!paths.is_empty()).then_some(paths);
    if let Ok(view) = save_and_publish(state, &artifacts, id, changed).await {
        artifacts
            .log_action(id, Action::Change, &Actor::default())
            .await;
        canvas_core::log::info(
            "artifact files changed",
            &[
                ("id", &id),
                ("writes", &writes),
                ("paths", &listed),
                ("updated_at", &view.artifact.updated_at),
                ("source_missing", &view.source_missing),
            ],
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_under_ignored_names_belong_to_no_artifact() {
        let watcher = Watcher::default();
        let root = std::env::temp_dir().join(format!("canvasd-owners-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        watcher.lock().paths.insert("art-1".into(), root.clone());
        for rel in [
            ".git/index",
            "target/debug/x.o",
            "node_modules/a/b.js",
            "css/.DS_Store",
            "index.html~",
        ] {
            assert!(
                watcher.owners(&root.join(rel)).is_empty(),
                "{rel} started a burst"
            );
        }
        assert_eq!(
            watcher.owners(&root.join("css/site.css")),
            [("art-1".to_string(), Some("css/site.css".to_string()))]
        );
        // Only the path below the artifact counts: a root inside `target` still reloads.
        let nested = root.join("target/site");
        std::fs::create_dir_all(&nested).unwrap();
        watcher.lock().paths.insert("art-2".into(), nested.clone());
        assert_eq!(
            watcher.owners(&nested.join("index.html")),
            [("art-2".to_string(), Some("index.html".to_string()))]
        );
        std::fs::remove_dir_all(&root).unwrap();
    }
}
