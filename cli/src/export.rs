//! `canvas export <card_id> [-o file]`: writes the card as one standalone
//! HTML page (canvasd's `GET /api/cards/:id/export`) to `<card_id>.html` or
//! the named file, and prints `{"path", "cards": 1, "warnings"}`.
//!
//! `canvas export --all [--session <id>] [-o file.zip]` lists the cards
//! canvasd holds (`/api/state`), or one session's, exports each through that
//! same route one at a time, and writes a zip (`canvas-export.zip` by
//! default): `<card_id>.html` per card plus `index.html`, newest first by
//! `at`, linking each. It prints `{"path", "cards", "warnings"}` with each
//! warning's `card_id`.
//!
//! A warning (an image gone from disk, a CDN asset that failed, a card
//! evicted mid-run) never fails an export; only an unreachable daemon, an
//! unknown card or session or an unwritable file does.

use std::collections::HashMap;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

use canvas_core::export::{self as card_export, Exported};
use canvas_core::html::{card_title, escape_attr, escape_text};
use canvas_core::{Card, ExportWarning, ExportWarningKind, Session};
use serde::Serialize;
use zip::write::SimpleFileOptions;
use zip::{DateTime, ZipWriter};

use crate::client;

const DEFAULT_ZIP: &str = "canvas-export.zip";

#[derive(Debug, PartialEq, Eq)]
pub enum Args {
    Card {
        card_id: String,
        out: Option<String>,
    },
    All {
        session: Option<String>,
        out: Option<String>,
    },
}

pub fn parse_args(args: &[String]) -> Result<Args, String> {
    let mut card_id = None;
    let mut out = None;
    let mut all = false;
    let mut session = None;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-o" | "--out" => out = Some(it.next().ok_or("-o needs a file")?.clone()),
            "--all" => all = true,
            "--session" => session = Some(it.next().ok_or("--session needs a session id")?.clone()),
            flag if flag.starts_with('-') => return Err(format!("unknown flag {flag}")),
            id if card_id.is_none() => card_id = Some(id.to_string()),
            extra => return Err(format!("unexpected argument {extra}")),
        }
    }
    match (all, card_id) {
        (true, None) => Ok(Args::All { session, out }),
        (true, Some(id)) => Err(format!("--all takes no card id, got {id}")),
        (false, _) if session.is_some() => Err("--session needs --all".to_string()),
        (false, card_id) => Ok(Args::Card {
            card_id: card_id.ok_or("canvas export needs a card id or --all")?,
            out,
        }),
    }
}

pub fn run(args: Args) -> Result<(), String> {
    match args {
        Args::Card { card_id, out } => run_card(&card_id, out),
        Args::All { session, out } => run_all(session.as_deref(), out),
    }
}

fn run_card(card_id: &str, out: Option<String>) -> Result<(), String> {
    let result = match card_export::fetch(card_id)? {
        Exported::Page(result) => result,
        Exported::Gone => return Err("canvasd returned HTTP 404 (no card with that id)".into()),
        Exported::Failed(e) => return Err(e),
    };
    let path = out
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(format!("{card_id}.html")));
    std::fs::write(&path, &result.html)
        .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    let path = absolute(&path);
    canvas_core::log::info(
        "export",
        &[
            ("card", &card_id),
            ("path", &path.display()),
            ("warnings", &result.warnings.len()),
        ],
    );
    println!(
        "{}",
        serde_json::json!({ "path": path, "cards": 1, "warnings": result.warnings })
    );
    Ok(())
}

fn run_all(session: Option<&str>, out: Option<String>) -> Result<(), String> {
    let stream = client::get_stream()?;
    if let Some(id) = session {
        if !stream.sessions.iter().any(|s| s.id == id) {
            return Err(format!("canvasd holds no session with id {id}"));
        }
    }
    let mut cards: Vec<Card> = stream
        .cards
        .into_iter()
        .filter(|card| session.is_none_or(|id| card.session_id == id))
        .collect();
    cards.sort_by(|a, b| b.at.cmp(&a.at));

    let path = PathBuf::from(out.unwrap_or_else(|| DEFAULT_ZIP.to_string()));
    // Written beside the target and renamed into place, so a run that fails
    // part way leaves whatever was at `path` untouched.
    let mut part = path.clone().into_os_string();
    part.push(".part");
    let part = PathBuf::from(part);
    let file = File::create(&part).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    let written = write_zip(file, &cards, &stream.sessions).and_then(|done| {
        std::fs::rename(&part, &path)
            .map(|()| done)
            .map_err(|e| ZipFailure::Write(e.to_string()))
    });
    let (exported, warnings) = match written {
        Ok(done) => done,
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            return Err(e.message(&path));
        }
    };

    let path = absolute(&path);
    canvas_core::log::info(
        "export all",
        &[
            ("session", &session.unwrap_or("-")),
            ("cards", &exported),
            ("path", &path.display()),
            ("warnings", &warnings.len()),
        ],
    );
    println!(
        "{}",
        serde_json::json!({ "path": path, "cards": exported, "warnings": warnings })
    );
    Ok(())
}

enum ZipFailure {
    Canvasd(String),
    Write(String),
}

impl ZipFailure {
    fn message(&self, path: &Path) -> String {
        match self {
            ZipFailure::Canvasd(e) => e.clone(),
            ZipFailure::Write(e) => format!("cannot write {}: {e}", path.display()),
        }
    }
}

/// One warning of an `--all` export, tagged with the card it came from.
#[derive(Debug, Serialize)]
struct CardWarning {
    card_id: String,
    #[serde(flatten)]
    warning: ExportWarning,
}

/// Exports `cards` (newest first) into the zip one at a time, then writes
/// the index of those that exported. Returns how many did, and the warnings,
/// each tagged with its card's id. A card canvasd no longer holds, or whose
/// export it answers with an error, is a warning and gets no page.
fn write_zip(
    file: File,
    cards: &[Card],
    sessions: &[Session],
) -> Result<(usize, Vec<CardWarning>), ZipFailure> {
    let write_err = |e: &dyn std::fmt::Display| ZipFailure::Write(e.to_string());
    let mut zip = ZipWriter::new(file);
    let mut warnings = Vec::new();
    let mut exported = Vec::new();
    for card in cards {
        let tag = |warning| CardWarning {
            card_id: card.id.clone(),
            warning,
        };
        let skipped = |kind, reason| {
            tag(ExportWarning {
                kind,
                target: card.id.clone(),
                reason,
            })
        };
        let result = match card_export::fetch(&card.id).map_err(ZipFailure::Canvasd)? {
            Exported::Page(result) => result,
            // Evicted (or deleted) between the listing and its export.
            Exported::Gone => {
                warnings.push(skipped(
                    ExportWarningKind::CardGone,
                    "canvasd no longer holds this card".to_string(),
                ));
                continue;
            }
            Exported::Failed(e) => {
                warnings.push(skipped(ExportWarningKind::ExportFailed, e));
                continue;
            }
        };
        warnings.extend(result.warnings.into_iter().map(tag));
        zip.start_file(format!("{}.html", card.id), file_options(&card.at))
            .map_err(|e| write_err(&e))?;
        zip.write_all(result.html.as_bytes())
            .map_err(|e| write_err(&e))?;
        exported.push(card);
    }

    let names: HashMap<&str, &str> = sessions
        .iter()
        .map(|s| (s.id.as_str(), s.name.as_str()))
        .collect();
    let newest = exported.first().map_or("", |card| card.at.as_str());
    zip.start_file("index.html", file_options(newest))
        .map_err(|e| write_err(&e))?;
    zip.write_all(index_html(&exported, &names).as_bytes())
        .map_err(|e| write_err(&e))?;
    zip.finish().map_err(|e| write_err(&e))?;
    Ok((exported.len(), warnings))
}

/// Stored (uncompressed) entries, timestamped with the card's `at`.
fn file_options(at: &str) -> SimpleFileOptions {
    let options = SimpleFileOptions::default();
    match zip_time(at) {
        Some(time) => options.last_modified_time(time),
        None => options,
    }
}

/// An RFC 3339 time as a zip timestamp, which readers take as local time.
fn zip_time(at: &str) -> Option<DateTime> {
    use chrono::{Datelike, Timelike};
    let local = chrono::DateTime::parse_from_rfc3339(at)
        .ok()?
        .with_timezone(&chrono::Local);
    DateTime::from_date_and_time(
        u16::try_from(local.year()).ok()?,
        local.month() as u8,
        local.day() as u8,
        local.hour() as u8,
        local.minute() as u8,
        local.second() as u8,
    )
    .ok()
}

/// The zip's front page: one row per card, newest first, linking its page.
/// Self-contained, and the reader's light/dark setting picks the palette.
fn index_html(cards: &[&Card], sessions: &HashMap<&str, &str>) -> String {
    let mut rows = String::new();
    for card in cards {
        let session = sessions
            .get(card.session_id.as_str())
            .copied()
            .unwrap_or(&card.session_id);
        let label = card_title(&card.html);
        rows.push_str(&format!(
            "<li><a href=\"{href}\">{label}</a><span class=\"meta\"><span>{session}</span><time datetime=\"{at}\">{when}</time></span></li>\n",
            href = escape_attr(&format!("{}.html", card.id)),
            label = escape_text(&label),
            session = escape_text(session),
            at = escape_attr(&card.at),
            when = escape_text(&utc_minute(&card.at)),
        ));
    }
    let count = match cards.len() {
        0 => "No posts to export".to_string(),
        1 => "1 post".to_string(),
        n => format!("{n} posts, newest first"),
    };
    format!(
        r#"<!doctype html>
<html><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Canvas export</title>
<style>
:root {{ color-scheme: light dark; --bg: #fafaf9; --ink: #1c1917; --muted: #78716c; --rule: #e7e5e4; --link: #1d4ed8; }}
@media (prefers-color-scheme: dark) {{
  :root {{ --bg: #1c1917; --ink: #f5f5f4; --muted: #a8a29e; --rule: #44403c; --link: #93c5fd; }}
}}
body {{ margin: 0; background: var(--bg); color: var(--ink); font: 15px/1.5 -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; }}
main {{ max-width: 760px; margin: 0 auto; padding: 32px 16px; }}
h1 {{ font-size: 22px; margin: 0 0 4px; }}
p {{ margin: 0 0 24px; color: var(--muted); }}
ol {{ list-style: none; margin: 0; padding: 0; }}
li {{ display: flex; flex-wrap: wrap; justify-content: space-between; gap: 4px 16px; padding: 10px 0; border-top: 1px solid var(--rule); }}
a {{ color: var(--link); text-decoration: none; overflow-wrap: anywhere; }}
a:hover {{ text-decoration: underline; }}
.meta {{ display: flex; gap: 12px; color: var(--muted); font-size: 13px; white-space: nowrap; }}
</style></head>
<body><main>
<h1>Canvas export</h1>
<p>{count}</p>
<ol>
{rows}</ol>
</main>
<script>
for (const t of document.querySelectorAll('time')) {{
  const d = new Date(t.dateTime);
  if (!isNaN(d)) t.textContent = d.toLocaleString([], {{ dateStyle: 'medium', timeStyle: 'short' }});
}}
</script>
</body></html>
"#
    )
}

/// `2026-10-05T14:03:12…+00:00` as `2026-10-05 14:03 UTC`, the text a
/// reader without scripts sees; anything else is shown as given.
fn utc_minute(at: &str) -> String {
    match (at.get(0..10), at.get(11..16)) {
        (Some(day), Some(minute)) if at.ends_with("+00:00") || at.ends_with('Z') => {
            format!("{day} {minute} UTC")
        }
        _ => at.to_string(),
    }
}

fn absolute(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_id_and_out() {
        assert_eq!(
            parse_args(&args(&["c1", "-o", "x.html"])),
            Ok(Args::Card {
                card_id: "c1".into(),
                out: Some("x.html".into())
            })
        );
        assert_eq!(
            parse_args(&args(&["c1"])),
            Ok(Args::Card {
                card_id: "c1".into(),
                out: None
            })
        );
        assert!(parse_args(&args(&[])).is_err());
        assert!(parse_args(&args(&["c1", "--zip"])).is_err());
        assert!(parse_args(&args(&["c1", "-o"])).is_err());
    }

    #[test]
    fn parses_all_with_session() {
        assert_eq!(
            parse_args(&args(&["--all", "--session", "s1", "-o", "a.zip"])),
            Ok(Args::All {
                session: Some("s1".into()),
                out: Some("a.zip".into())
            })
        );
        assert!(parse_args(&args(&["--all", "c1"])).is_err());
        assert!(parse_args(&args(&["c1", "--session", "s1"])).is_err());
        assert!(parse_args(&args(&["--all", "--session"])).is_err());
    }

    #[test]
    fn zip_time_reads_rfc3339() {
        use chrono::{Datelike, Timelike};
        let local = chrono::DateTime::parse_from_rfc3339("2026-10-05T14:03:12.123+00:00")
            .unwrap()
            .with_timezone(&chrono::Local);
        let time = zip_time("2026-10-05T14:03:12.123+00:00").unwrap();
        assert_eq!(
            (time.day(), time.hour(), time.minute(), time.second()),
            (
                local.day() as u8,
                local.hour() as u8,
                local.minute() as u8,
                12
            )
        );
        assert_eq!(zip_time("garbage"), None);
        assert_eq!(utc_minute("2026-10-05T14:03:12Z"), "2026-10-05 14:03 UTC");
    }
}
