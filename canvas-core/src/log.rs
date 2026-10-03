//! Timestamped log files for canvasd, the CLI and Canvas.app, one file per
//! process in `logs/` under the data dir, rotated by size.
//!
//! A line is `<UTC time> <process> <LEVEL> <message> key=value ...`. Nothing
//! is written until [`init`] names the process, so a test that builds the
//! router in-process never touches the real data dir. Every write failure is
//! swallowed: a log line must never change what a command prints or how it
//! exits.

use std::fmt::Display;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::OnceLock;

/// A file rotates once it reaches this size.
pub const MAX_BYTES: u64 = 5 * 1024 * 1024;
/// Rotated files kept beside the live one: `daemon.log.1` .. `daemon.log.3`.
pub const KEEP: u32 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Process {
    Daemon,
    Cli,
    App,
}

impl Process {
    pub fn name(self) -> &'static str {
        match self {
            Process::Daemon => "daemon",
            Process::Cli => "cli",
            Process::App => "app",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Warn,
    Error,
}

impl Level {
    fn name(self) -> &'static str {
        match self {
            Level::Info => "INFO",
            Level::Warn => "WARN",
            Level::Error => "ERROR",
        }
    }
}

static PROCESS: OnceLock<Process> = OnceLock::new();
/// Set by [`init_background`]: lines go to a writer thread instead of being
/// appended by the caller.
static WRITER: OnceLock<mpsc::Sender<Msg>> = OnceLock::new();

enum Msg {
    Line(PathBuf, String),
    Flush(mpsc::SyncSender<()>),
}

/// Names the process every later line comes from; the caller appends each
/// line itself. Only the first `init` or `init_background` call counts.
pub fn init(process: Process) {
    let _ = PROCESS.set(process);
}

/// Like [`init`], but every line is handed to one writer thread, so a caller on
/// an async executor or holding a lock never waits on the disk. For the
/// long-lived daemon; a short-lived process would exit with lines unwritten.
pub fn init_background(process: Process) {
    if PROCESS.set(process).is_err() {
        return;
    }
    let (tx, rx) = mpsc::channel::<Msg>();
    let spawned = std::thread::Builder::new()
        .name("canvas-log".to_string())
        .spawn(move || {
            for msg in rx {
                match msg {
                    Msg::Line(dir, line) => {
                        let _ = append(&dir, process.name(), &line, MAX_BYTES);
                    }
                    Msg::Flush(done) => {
                        let _ = done.send(());
                    }
                }
            }
        });
    if spawned.is_ok() {
        let _ = WRITER.set(tx);
    }
}

/// Blocks until the writer thread has written every line sent so far, for a
/// process about to exit. Returns at once without a writer thread.
pub fn flush() {
    if let Some(writer) = WRITER.get() {
        let (done_tx, done_rx) = mpsc::sync_channel(0);
        if writer.send(Msg::Flush(done_tx)).is_ok() {
            let _ = done_rx.recv();
        }
    }
}

/// The most of an error response's body a log line quotes.
pub const MAX_ERROR_BYTES: usize = 2048;

/// A 4xx/5xx body as an `error` field value: lossy UTF-8, cut to
/// [`MAX_ERROR_BYTES`], trimmed; `None` when nothing is left.
pub fn error_text(body: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(&body[..body.len().min(MAX_ERROR_BYTES)]);
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// `logs/` under the data dir, absolute even when `CANVAS_DATA_DIR` is
/// relative.
pub fn logs_dir() -> Option<PathBuf> {
    let dir = crate::paths::data_dir()?.join("logs");
    if dir.is_absolute() {
        Some(dir)
    } else {
        Some(std::env::current_dir().ok()?.join(dir))
    }
}

pub fn info(message: &str, fields: &[(&str, &dyn Display)]) {
    write(Level::Info, message, fields);
}

pub fn warn(message: &str, fields: &[(&str, &dyn Display)]) {
    write(Level::Warn, message, fields);
}

pub fn error(message: &str, fields: &[(&str, &dyn Display)]) {
    write(Level::Error, message, fields);
}

pub fn write(level: Level, message: &str, fields: &[(&str, &dyn Display)]) {
    let Some(process) = PROCESS.get() else {
        return;
    };
    let Some(dir) = logs_dir() else {
        return;
    };
    let line = format_line(
        &chrono::Utc::now()
            .format("%Y-%m-%dT%H:%M:%S%.3fZ")
            .to_string(),
        *process,
        level,
        message,
        fields,
    );
    match WRITER.get() {
        Some(writer) => {
            let _ = writer.send(Msg::Line(dir, line));
        }
        None => {
            let _ = append(&dir, process.name(), &line, MAX_BYTES);
        }
    }
}

fn format_line(
    time: &str,
    process: Process,
    level: Level,
    message: &str,
    fields: &[(&str, &dyn Display)],
) -> String {
    let mut line = format!(
        "{time} {} {} {}",
        process.name(),
        level.name(),
        one_line(message)
    );
    for (key, value) in fields {
        line.push(' ');
        line.push_str(key);
        line.push('=');
        line.push_str(&quote(&value.to_string()));
    }
    line.push('\n');
    line
}

fn one_line(text: &str) -> String {
    text.replace('\r', "\\r").replace('\n', "\\n")
}

/// Leaves a plain value bare and wraps anything with a space, quote, `=` or
/// line break in double quotes, so a line still splits on spaces.
fn quote(value: &str) -> String {
    let plain = !value.is_empty()
        && !value
            .chars()
            .any(|c| c.is_whitespace() || c == '"' || c == '=' || c.is_control());
    if plain {
        return value.to_string();
    }
    let mut out = String::from("\"");
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Appends `line` to `dir/<name>.log`, first rotating the file when it has
/// reached `max_bytes`. One `write` call per line, so lines from processes
/// appending at once never interleave.
fn append(dir: &Path, name: &str, line: &str, max_bytes: u64) -> std::io::Result<()> {
    fs::create_dir_all(dir)?;
    let path = dir.join(format!("{name}.log"));
    if fs::metadata(&path).is_ok_and(|m| m.len() >= max_bytes) {
        for n in (1..KEEP).rev() {
            let _ = fs::rename(
                dir.join(format!("{name}.log.{n}")),
                dir.join(format!("{name}.log.{}", n + 1)),
            );
        }
        // Another process may have rotated first; the line still goes in.
        let _ = fs::rename(&path, dir.join(format!("{name}.log.1")));
    }
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?
        .write_all(line.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_carries_time_process_level_message_and_quoted_fields() {
        let line = format_line(
            "2026-10-02T19:18:00.000Z",
            Process::Daemon,
            Level::Warn,
            "request",
            &[("path", &"/api/posts"), ("error", &"no \"card\"\nhere")],
        );
        assert_eq!(
            line,
            "2026-10-02T19:18:00.000Z daemon WARN request path=/api/posts error=\"no \\\"card\\\"\\nhere\"\n"
        );
    }

    #[test]
    fn a_full_file_rotates_and_only_keep_files_survive() {
        let dir = std::env::temp_dir().join(format!("canvas-log-rotate-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        for i in 0..6 {
            append(&dir, "cli", &format!("line {i}\n"), 1).unwrap();
        }
        let read = |name: &str| fs::read_to_string(dir.join(name)).unwrap();
        assert_eq!(read("cli.log"), "line 5\n");
        assert_eq!(read("cli.log.1"), "line 4\n");
        assert_eq!(read("cli.log.3"), "line 2\n");
        assert!(!dir.join("cli.log.4").exists());
        let _ = fs::remove_dir_all(&dir);
    }
}
