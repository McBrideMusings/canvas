//! Artifact refresh: runs an artifact's `--refresh` command on its interval,
//! in the directory it was set from, while the agent process that set it is
//! alive, and delivers the JSON it prints the way `canvas data` does.

use std::collections::{HashMap, HashSet};
use std::process::Stdio;
use std::time::{Duration, Instant};

use canvas_core::RefreshError;

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

/// What the scheduler remembers about one artifact's refresh loop. Dropped
/// when the artifact stops refreshing (deleted, or the process that set the
/// refresh is gone).
pub struct Slot {
    generation: u64,
    command: String,
    every_secs: u64,
    cwd: String,
    next_due: Instant,
    failures: u32,
    running: bool,
}

/// One run handed out by [`AppState::refresh_tick`].
struct Job {
    id: String,
    generation: u64,
    command: String,
    cwd: String,
}

/// Seconds to wait after a run: the interval, doubled per consecutive
/// failure, capped at 10 minutes (never below the interval itself).
pub fn delay_secs(every_secs: u64, failures: u32) -> u64 {
    every_secs
        .saturating_mul(1u64 << failures.min(20))
        .min(MAX_BACKOFF_SECS)
        .max(every_secs)
}

impl AppState {
    /// Starts every refresh that is due, for each artifact whose refresh was
    /// set by a process `is_alive` accepts (or by none). A run is spawned and
    /// left to finish on its own; one artifact never has two running at once.
    pub async fn refresh_tick_with(&self, is_alive: impl Fn(u32) -> bool) {
        let now = Instant::now();
        let mut jobs = Vec::new();
        let mut gone = Vec::new();
        {
            let artifacts = self.artifacts.read().await;
            let mut slots = self.refreshes.lock().unwrap_or_else(|e| e.into_inner());
            let mut live = HashSet::new();
            for record in artifacts.records.values() {
                let Some(refresh) = record.refresh.as_ref() else {
                    continue;
                };
                if refresh.pid.is_some_and(|pid| !is_alive(pid)) {
                    // Nothing will retry it, so its error no longer applies.
                    if artifacts.refresh_errors.contains_key(&record.id) {
                        gone.push(record.id.clone());
                    }
                    continue;
                }
                live.insert(record.id.clone());
                let stale = slots.get(&record.id).is_none_or(|s| {
                    s.command != refresh.command
                        || s.every_secs != refresh.every_secs
                        || s.cwd != refresh.cwd
                });
                if stale {
                    let generation = self.next_generation();
                    slots.insert(
                        record.id.clone(),
                        Slot {
                            generation,
                            command: refresh.command.clone(),
                            every_secs: refresh.every_secs,
                            cwd: refresh.cwd.clone(),
                            next_due: now,
                            failures: 0,
                            running: false,
                        },
                    );
                }
                let slot = slots.get_mut(&record.id).expect("slot inserted above");
                if !slot.running && now >= slot.next_due {
                    slot.running = true;
                    jobs.push(Job {
                        id: record.id.clone(),
                        generation: slot.generation,
                        command: refresh.command.clone(),
                        cwd: refresh.cwd.clone(),
                    });
                }
            }
            slots.retain(|id, _| live.contains(id));
        }
        if !gone.is_empty() {
            let mut artifacts = self.artifacts.write().await;
            for id in gone {
                if artifacts.refresh_errors.remove(&id).is_none() {
                    continue;
                }
                if let Some(record) = artifacts.records.get(&id) {
                    let view = artifacts.view(record);
                    self.publish(CanvasEvent::ArtifactUpserted(Box::new(view)));
                }
            }
        }
        for job in jobs {
            let state = self.clone();
            tokio::spawn(async move {
                let result = run_command(&job.command, &job.cwd).await;
                state.finish_refresh(&job, result).await;
            });
        }
    }

    /// [`Self::refresh_tick_with`] against the real process table.
    pub async fn refresh_tick(&self) {
        self.refresh_tick_with(crate::state::process_alive).await;
    }

    /// Delivers a run's result, unless the refresh was replaced, or the
    /// artifact deleted, while the command ran. A success pushes its value
    /// and clears the error; a failure keeps the last value and records the
    /// error with when the next run is due.
    async fn finish_refresh(&self, job: &Job, result: Result<serde_json::Value, String>) {
        let mut artifacts = self.artifacts.write().await;
        let wait = {
            let mut slots = self.refreshes.lock().unwrap_or_else(|e| e.into_inner());
            let Some(slot) = slots
                .get_mut(&job.id)
                .filter(|s| s.generation == job.generation)
            else {
                return;
            };
            slot.running = false;
            slot.failures = if result.is_ok() {
                0
            } else {
                slot.failures.saturating_add(1)
            };
            let wait = delay_secs(slot.every_secs, slot.failures);
            slot.next_due = Instant::now() + Duration::from_secs(wait);
            wait
        };
        if !artifacts.records.contains_key(&job.id) {
            return;
        }
        let had_error = artifacts.refresh_errors.contains_key(&job.id);
        match result {
            Ok(value) => {
                artifacts.data.insert(job.id.clone(), value.clone());
                self.publish(CanvasEvent::ArtifactData {
                    id: job.id.clone(),
                    value,
                });
                if !had_error {
                    return;
                }
                artifacts.refresh_errors.remove(&job.id);
                canvas_core::log::info("artifact refresh recovered", &[("id", &job.id)]);
            }
            Err(message) => {
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

    /// How many artifacts have a refresh loop going.
    pub fn refreshing_count(&self) -> usize {
        self.refreshes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .len()
    }

    fn next_generation(&self) -> u64 {
        self.generation
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }
}

/// Runs `command` with `sh -c` in `cwd`: its stdout must be one JSON value.
/// The error is the first stderr line, else the reason.
pub async fn run_command(command: &str, cwd: &str) -> Result<serde_json::Value, String> {
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
    serde_json::from_slice(&out).map_err(|_| "output is not JSON".to_string())
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

/// Scheduler state kept beside the cards.
pub type Slots = HashMap<String, Slot>;
