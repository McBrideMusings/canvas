//! Pin refresh: runs a pin's `--refresh` command on its interval and delivers
//! the JSON it prints the way `canvas data` does.

use std::collections::{HashMap, HashSet};
use std::process::Stdio;
use std::time::{Duration, Instant};

use canvas_core::{Card, Refresh};

use crate::routes::MAX_DATA_BYTES;
use crate::state::{AppState, CanvasEvent};

/// Most stderr kept from a run; only its first line is used.
const MAX_STDERR_BYTES: usize = 64 * 1024;

/// A run that takes longer than this is killed and counts as a failure.
pub const REFRESH_TIMEOUT: Duration = Duration::from_secs(60);

/// Failures double the wait between runs, up to this many seconds.
const MAX_BACKOFF_SECS: u64 = 600;

/// How often the daemon looks for pins whose refresh is due.
const TICK_INTERVAL: Duration = Duration::from_secs(1);

/// What the scheduler remembers about one card's refresh loop. Dropped when
/// the card stops refreshing (unpinned, session ended, removed).
pub struct Slot {
    generation: u64,
    command: String,
    every_secs: u64,
    next_due: Instant,
    failures: u32,
    running: bool,
}

/// One run handed out by [`AppState::refresh_tick`].
struct Job {
    card_id: String,
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
    /// Starts every refresh that is due. A run is spawned and left to finish
    /// on its own; one pin never has two running at once.
    pub async fn refresh_tick(&self) {
        let now = Instant::now();
        let mut jobs = Vec::new();
        {
            let inner = self.inner.read().await;
            let mut slots = self.refreshes.lock().unwrap_or_else(|e| e.into_inner());
            let mut live = HashSet::new();
            for card in &inner.cards {
                let Some(refresh) = card.pin.as_ref().and_then(|p| p.refresh.as_ref()) else {
                    continue;
                };
                let Some(session) = inner
                    .sessions
                    .get(&card.session_id)
                    .filter(|s| s.ended_at.is_none())
                else {
                    continue;
                };
                live.insert(card.id.clone());
                let stale = slots.get(&card.id).is_none_or(|s| {
                    s.command != refresh.command || s.every_secs != refresh.every_secs
                });
                if stale {
                    let generation = self.next_generation();
                    slots.insert(
                        card.id.clone(),
                        Slot {
                            generation,
                            command: refresh.command.clone(),
                            every_secs: refresh.every_secs,
                            next_due: now,
                            failures: 0,
                            running: false,
                        },
                    );
                }
                let slot = slots.get_mut(&card.id).expect("slot inserted above");
                if !slot.running && now >= slot.next_due {
                    slot.running = true;
                    jobs.push(Job {
                        card_id: card.id.clone(),
                        generation: slot.generation,
                        command: refresh.command.clone(),
                        cwd: session.cwd.clone(),
                    });
                }
            }
            slots.retain(|id, _| live.contains(id));
        }
        for job in jobs {
            let state = self.clone();
            tokio::spawn(async move {
                let result = run_command(&job.command, &job.cwd).await;
                state.finish_refresh(&job, result).await;
            });
        }
    }

    /// Delivers a run's result, unless the pin was replaced, unpinned or its
    /// session ended while the command ran.
    async fn finish_refresh(&self, job: &Job, result: Result<serde_json::Value, String>) {
        let mut inner = self.inner.write().await;
        {
            let mut slots = self.refreshes.lock().unwrap_or_else(|e| e.into_inner());
            let Some(slot) = slots
                .get_mut(&job.card_id)
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
            slot.next_due =
                Instant::now() + Duration::from_secs(delay_secs(slot.every_secs, slot.failures));
        }
        let Some(card) = inner.cards.iter().find(|c| c.id == job.card_id) else {
            return;
        };
        let live = inner
            .sessions
            .get(&card.session_id)
            .is_some_and(|s| s.ended_at.is_none());
        if !live || card.pin.as_ref().and_then(|p| p.refresh.as_ref()).is_none() {
            return;
        }
        let mut card: Card = card.clone();
        let pin = card.pin.as_mut().expect("checked above");
        let next_error = result.as_ref().err().cloned();
        if let Some(error) = &next_error {
            canvas_core::log::warn(
                "pin refresh failed",
                &[("card", &job.card_id), ("error", error)],
            );
        }
        if let Ok(value) = result {
            inner.data.insert(job.card_id.clone(), value.clone());
            self.publish(CanvasEvent::CardData {
                id: job.card_id.clone(),
                value,
            });
        }
        if pin.refresh_error != next_error {
            pin.refresh_error = next_error;
            inner.upsert_card(card.clone());
            self.publish(CanvasEvent::CardUpserted(card));
        }
    }

    /// How many cards have a refresh loop going.
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

/// The refresh a post may carry: at least [`canvas_core::MIN_REFRESH_SECS`].
pub fn check(refresh: &Refresh) -> Result<(), String> {
    if refresh.command.trim().is_empty() {
        return Err("a refresh needs a command".to_string());
    }
    if refresh.every_secs < canvas_core::MIN_REFRESH_SECS {
        return Err(format!(
            "a refresh must run at least every {} seconds",
            canvas_core::MIN_REFRESH_SECS
        ));
    }
    Ok(())
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
