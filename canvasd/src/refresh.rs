//! Artifact refresh: runs an artifact's `--refresh` command on its interval,
//! in the directory it was set from, while a visible Canvas window shows the
//! artifact (or always, when it is marked so), and delivers the JSON it
//! prints the way `canvas data` does. A run that prints nothing skips: the
//! last value stays.

use std::collections::{HashMap, HashSet};
use std::process::Stdio;
use std::time::{Duration, Instant};

use canvas_core::RefreshError;

use crate::artifacts::Artifacts;
use crate::routes::MAX_DATA_BYTES;
use crate::state::{AppState, CanvasEvent};

/// Most stderr kept from a run; only its first line is used.
const MAX_STDERR_BYTES: usize = 64 * 1024;

/// A run that takes longer than this is killed and counts as a failure.
pub const REFRESH_TIMEOUT: Duration = Duration::from_secs(60);

/// Failures double the wait between runs, up to this many seconds.
const MAX_BACKOFF_SECS: u64 = 600;

/// How often the daemon looks for artifacts whose refresh is due.
const TICK_INTERVAL: Duration = Duration::from_secs(1);

/// What the scheduler remembers about one artifact's refresh loop, kept
/// while the artifact has a refresh, watched or not. A changed command,
/// interval or directory starts a new one.
pub struct Slot {
    generation: u64,
    command: String,
    every_secs: u64,
    cwd: String,
    /// When a run may start next. One that comes due while nothing watches
    /// the artifact waits; a window starting to show it sets this from the
    /// shown value's age.
    next_due: Instant,
    failures: u32,
    running: bool,
    /// Whether a live watch named the artifact at the last tick.
    was_watched: bool,
    last_run_at: Option<String>,
}

/// Why a run started, as its log line names it.
#[derive(Clone, Copy)]
enum Reason {
    /// A window shows the artifact.
    Watched,
    /// A window started showing it with a stale or missing value.
    FirstWatch,
    /// Nothing shows it; the refresh is marked `always`.
    Always,
}

impl Reason {
    fn as_str(self) -> &'static str {
        match self {
            Reason::Watched => "watched",
            Reason::FirstWatch => "first-watch",
            Reason::Always => "always",
        }
    }
}

/// One run handed out by [`AppState::refresh_tick_at`].
struct Job {
    id: String,
    generation: u64,
    command: String,
    cwd: String,
}

/// What a run came to: a value to deliver, a skip (exit 0 and nothing on
/// stdout), or the failure's text.
#[derive(Debug, PartialEq)]
pub enum Outcome {
    Value(serde_json::Value),
    Skip,
    Failed(String),
}

/// Seconds to wait after a run: the interval, doubled per consecutive
/// failure, capped at 10 minutes (never below the interval itself).
pub fn delay_secs(every_secs: u64, failures: u32) -> u64 {
    every_secs
        .saturating_mul(1u64 << failures.min(20))
        .min(MAX_BACKOFF_SECS)
        .max(every_secs)
}

impl Artifacts {
    /// Whether `id`'s refresh command is running now.
    pub fn refreshing(&self, id: &str) -> bool {
        self.slots.get(id).is_some_and(|s| s.running)
    }

    /// When `id`'s refresh command last finished.
    pub fn last_run_at(&self, id: &str) -> Option<String> {
        self.slots.get(id).and_then(|s| s.last_run_at.clone())
    }

    /// How many artifacts have a refresh loop.
    pub fn refresh_count(&self) -> usize {
        self.slots.len()
    }
}

impl AppState {
    /// Starts every refresh due at `now` whose artifact a live watch report
    /// names, or that is marked `always`. A run is spawned and left to finish
    /// on its own; one artifact never has two running at once. Each start
    /// tells the viewers, so the page header reads "Refreshing…".
    pub async fn refresh_tick_at(&self, now: Instant) {
        let mut jobs = Vec::new();
        {
            let mut guard = self.artifacts.write().await;
            let artifacts = &mut *guard;
            let watched = artifacts.watched_ids(now);
            let mut live = HashSet::new();
            for record in artifacts.records.values() {
                let Some(refresh) = record.refresh.as_ref() else {
                    continue;
                };
                live.insert(record.id.clone());
                let stale = artifacts.slots.get(&record.id).is_none_or(|s| {
                    s.command != refresh.command
                        || s.every_secs != refresh.every_secs
                        || s.cwd != refresh.cwd
                });
                if stale {
                    artifacts.generation += 1;
                    artifacts.slots.insert(
                        record.id.clone(),
                        Slot {
                            generation: artifacts.generation,
                            command: refresh.command.clone(),
                            every_secs: refresh.every_secs,
                            cwd: refresh.cwd.clone(),
                            next_due: now,
                            failures: 0,
                            running: false,
                            was_watched: false,
                            last_run_at: None,
                        },
                    );
                }
                let slot = artifacts
                    .slots
                    .get_mut(&record.id)
                    .expect("slot inserted above");
                let is_watched = watched.contains(&record.id);
                let first_watch = is_watched && !slot.was_watched;
                slot.was_watched = is_watched;
                // A window started showing it: run at once when the value is
                // missing or older than the interval, else when it turns so.
                // A failure's backoff still holds.
                if first_watch && !refresh.always && slot.failures == 0 {
                    let every = Duration::from_secs(slot.every_secs);
                    slot.next_due = match artifacts.data_instant.get(&record.id) {
                        Some(at) if now.saturating_duration_since(*at) < every => *at + every,
                        _ => now,
                    };
                }
                let reason = match (is_watched, first_watch, refresh.always) {
                    (true, true, _) => Reason::FirstWatch,
                    (true, false, _) => Reason::Watched,
                    (false, _, true) => Reason::Always,
                    (false, _, false) => continue,
                };
                if slot.running || now < slot.next_due {
                    continue;
                }
                slot.running = true;
                canvas_core::log::info(
                    "artifact refresh started",
                    &[("id", &record.id), ("reason", &reason.as_str())],
                );
                jobs.push(Job {
                    id: record.id.clone(),
                    generation: slot.generation,
                    command: refresh.command.clone(),
                    cwd: refresh.cwd.clone(),
                });
            }
            artifacts.slots.retain(|id, _| live.contains(id));
            for job in &jobs {
                if let Some(record) = artifacts.records.get(&job.id) {
                    let view = artifacts.view(record);
                    self.publish(CanvasEvent::ArtifactUpserted(Box::new(view)));
                }
            }
        }
        for job in jobs {
            let state = self.clone();
            tokio::spawn(async move {
                let outcome = run_command(&job.command, &job.cwd).await;
                state.finish_refresh(&job, outcome).await;
            });
        }
    }

    /// [`Self::refresh_tick_at`] now.
    pub async fn refresh_tick(&self) {
        self.refresh_tick_at(Instant::now()).await;
    }

    /// Keeps one viewer window's watch report; a report naming a different
    /// set or visibility than the window's last is logged.
    pub async fn record_watch(&self, report: canvas_core::WatchReport) {
        let ids = report.artifacts.join(",");
        let changed = self.artifacts.write().await.record_watch(
            report.window.clone(),
            report.visible,
            report.artifacts,
            Instant::now(),
        );
        if changed {
            canvas_core::log::info(
                "artifact watch",
                &[
                    ("window", &report.window),
                    ("visible", &report.visible),
                    ("ids", &ids),
                ],
            );
        }
    }

    /// How many viewers can see a viewer-only event that reached `reached`
    /// event streams: the windows whose live watch report says visible, never
    /// more than the streams open. A hidden window keeps its stream, so
    /// `reached` alone counts it.
    pub async fn visible_viewers(&self, reached: usize) -> usize {
        let visible = self.artifacts.read().await.visible_windows(Instant::now());
        visible.min(reached)
    }

    /// Delivers a run's outcome, unless the refresh was stopped or replaced,
    /// or the artifact deleted, while the command ran. A value is pushed and clears
    /// the error; a skip keeps the last value; a failure keeps the last value
    /// and records the error with when the next run is due. Either way the
    /// viewers hear that the run ended.
    async fn finish_refresh(&self, job: &Job, outcome: Outcome) {
        let mut artifacts = self.artifacts.write().await;
        let wait = {
            let Some(slot) = artifacts
                .slots
                .get_mut(&job.id)
                .filter(|s| s.generation == job.generation)
            else {
                return;
            };
            slot.running = false;
            slot.last_run_at = Some(chrono::Utc::now().to_rfc3339());
            slot.failures = match outcome {
                Outcome::Failed(_) => slot.failures.saturating_add(1),
                _ => 0,
            };
            let wait = delay_secs(slot.every_secs, slot.failures);
            slot.next_due = Instant::now() + Duration::from_secs(wait);
            wait
        };
        let Some(record) = artifacts.records.get(&job.id) else {
            return;
        };
        let current = record
            .refresh
            .as_ref()
            .is_some_and(|r| r.command == job.command && r.cwd == job.cwd);
        if !current {
            // Replaced while it ran: its outcome belongs to the old command,
            // but the viewers still hear that the run ended.
            let view = artifacts.view(record);
            self.publish(CanvasEvent::ArtifactUpserted(Box::new(view)));
            return;
        }
        match outcome {
            Outcome::Value(value) => {
                artifacts.set_data(&job.id, Some(value.clone()));
                self.publish(CanvasEvent::ArtifactData {
                    id: job.id.clone(),
                    value,
                });
                if artifacts.refresh_errors.remove(&job.id).is_some() {
                    canvas_core::log::info("artifact refresh recovered", &[("id", &job.id)]);
                }
            }
            Outcome::Skip => {
                canvas_core::log::info("artifact refresh skipped", &[("id", &job.id)]);
                if artifacts.refresh_errors.remove(&job.id).is_some() {
                    canvas_core::log::info("artifact refresh recovered", &[("id", &job.id)]);
                }
            }
            Outcome::Failed(message) => {
                let now = chrono::Utc::now();
                let retry_at = now + chrono::Duration::seconds(wait as i64);
                canvas_core::log::warn(
                    "artifact refresh failed",
                    &[("id", &job.id), ("error", &message), ("retry_secs", &wait)],
                );
                artifacts.refresh_errors.insert(
                    job.id.clone(),
                    RefreshError {
                        message,
                        at: now.to_rfc3339(),
                        retry_at: retry_at.to_rfc3339(),
                    },
                );
            }
        }
        if let Some(record) = artifacts.records.get(&job.id) {
            let view = artifacts.view(record);
            self.publish(CanvasEvent::ArtifactUpserted(Box::new(view)));
        }
    }
}

/// Runs `command` with `sh -c` in `cwd`: its stdout must be one JSON value,
/// or nothing but whitespace for a skip. A failure's text is the first stderr
/// line, else the reason.
pub async fn run_command(command: &str, cwd: &str) -> Outcome {
    match run(command, cwd).await {
        Ok(out) if out.iter().all(u8::is_ascii_whitespace) => Outcome::Skip,
        Ok(out) => match serde_json::from_slice(&out) {
            Ok(value) => Outcome::Value(value),
            Err(_) => Outcome::Failed("output is not JSON".to_string()),
        },
        Err(message) => Outcome::Failed(message),
    }
}

/// Runs `command` to its exit and answers its stdout, or why it failed.
async fn run(command: &str, cwd: &str) -> Result<Vec<u8>, String> {
    let mut cmd = tokio::process::Command::new("sh");
    cmd.arg("-c")
        .arg(command)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .process_group(0);
    let mut child = cmd.spawn().map_err(|e| format!("could not run: {e}"))?;
    let pid = child.id();
    let kill_group = || {
        if let Some(pid) = pid.and_then(|p| libc::pid_t::try_from(p).ok()) {
            // SAFETY: kills the run's own process group (it leads one).
            unsafe {
                libc::kill(-pid, libc::SIGKILL);
            }
        }
    };
    let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
        return Err("could not run: no output pipes".to_string());
    };
    // Reads stop at the cap, so a runaway command cannot grow the daemon.
    let read = async {
        let out = read_capped(stdout, MAX_DATA_BYTES + 1);
        let err = read_capped(stderr, MAX_STDERR_BYTES);
        let (out, err) = tokio::join!(out, err);
        if out.len() > MAX_DATA_BYTES {
            return Err(format!("output is over {} KB", MAX_DATA_BYTES / 1024));
        }
        let status = child
            .wait()
            .await
            .map_err(|e| format!("could not run: {e}"))?;
        Ok((out, err, status))
    };
    let (out, err, status) = match tokio::time::timeout(REFRESH_TIMEOUT, read).await {
        Ok(Ok(done)) => done,
        Ok(Err(reason)) => {
            kill_group();
            return Err(reason);
        }
        Err(_) => {
            kill_group();
            return Err(format!("timed out after {}s", REFRESH_TIMEOUT.as_secs()));
        }
    };
    if !status.success() {
        let stderr = String::from_utf8_lossy(&err);
        return Err(match stderr.lines().find(|l| !l.trim().is_empty()) {
            Some(line) => line.trim().to_string(),
            None => match status.code() {
                Some(code) => format!("exited with status {code}"),
                None => "killed by a signal".to_string(),
            },
        });
    }
    Ok(out)
}

/// Reads up to `cap` bytes, then stops (the rest is left unread).
async fn read_capped(pipe: impl tokio::io::AsyncRead + Unpin, cap: usize) -> Vec<u8> {
    use tokio::io::AsyncReadExt;
    let mut buf = Vec::new();
    let _ = pipe.take(cap as u64).read_to_end(&mut buf).await;
    buf
}

/// Runs [`AppState::refresh_tick`] every second.
pub fn spawn_refresh_loop(state: AppState) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(TICK_INTERVAL);
        loop {
            tick.tick().await;
            state.refresh_tick().await;
        }
    });
}

/// Each refreshing artifact's [`Slot`], by id.
pub type Slots = HashMap<String, Slot>;
