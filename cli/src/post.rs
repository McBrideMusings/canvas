//! `canvas post` — lets an agent drop rich HTML into its own turn's card.
//!
//! Every other CLI path (the `hook` subcommands) must never disturb the
//! Claude session it's attached to, so failures there are swallowed and the
//! process exits 0. `post` is the deliberate exception: an agent runs it on
//! purpose and needs to know if it failed, so every failure here prints
//! exactly one line to stderr and the process exits non-zero.

use std::io::Read;

use canvas_core::{Pin, PinScope, PostRequest, Refresh, MIN_REFRESH_SECS};

/// How often `--refresh` runs when `--every` is not given.
const DEFAULT_REFRESH_SECS: u64 = 30;

use crate::agent::{self, AgentAdapter};
use crate::client;
use crate::format::{self, Format};
use crate::inline_css;
use crate::scan;

/// The parsed `canvas post` arguments: the positional path (or `-`/absent
/// for stdin), an explicit `--format md|text|html`, an optional
/// `--update <card_id>` to replace an existing card instead of creating one,
/// and `--pin <slot>` with its `--pin-scope session|repo`, `--widget <file>`
/// and `--refresh <cmd> [--every <secs>]` to hold the card in a pin slot.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct PostArgs<'a> {
    pub arg: Option<&'a str>,
    pub format_flag: Option<&'a str>,
    pub update_id: Option<&'a str>,
    pub pin: Option<&'a str>,
    pub pin_scope: Option<&'a str>,
    pub widget: Option<&'a str>,
    pub refresh: Option<&'a str>,
    pub every: Option<&'a str>,
}

/// A malformed `canvas post` argument list. Carries no detail: the caller
/// always responds by printing the usage line and exiting 2.
#[derive(Debug, PartialEq, Eq)]
pub struct UsageError;

/// Parses the arguments to `canvas post` (everything after `post` itself).
/// Each flag may appear before or after the single positional path/`-`.
/// Returns `Err` on a usage error: a flag with no following value, a flag
/// given twice, a second positional argument, `--pin-scope`/`--widget`/
/// `--refresh`/`--every` without `--pin`, or `--every` without `--refresh`. Does not validate the flag values themselves — `run` does.
pub fn parse_args(rest: &[String]) -> Result<PostArgs<'_>, UsageError> {
    let mut parsed = PostArgs::default();
    let mut i = 0;
    while i < rest.len() {
        let flag = match rest[i].as_str() {
            "--format" => &mut parsed.format_flag,
            "--update" => &mut parsed.update_id,
            "--pin" => &mut parsed.pin,
            "--pin-scope" => &mut parsed.pin_scope,
            "--widget" => &mut parsed.widget,
            "--refresh" => &mut parsed.refresh,
            "--every" => &mut parsed.every,
            other if parsed.arg.is_none() => {
                parsed.arg = Some(other);
                i += 1;
                continue;
            }
            _ => return Err(UsageError),
        };
        match (flag.is_some(), rest.get(i + 1)) {
            (false, Some(v)) => *flag = Some(v.as_str()),
            _ => return Err(UsageError),
        }
        i += 2;
    }
    let pin_only = parsed.pin_scope.is_some()
        || parsed.widget.is_some()
        || parsed.refresh.is_some()
        || parsed.every.is_some();
    if parsed.pin.is_none() && pin_only {
        return Err(UsageError);
    }
    if parsed.every.is_some() && parsed.refresh.is_none() {
        return Err(UsageError);
    }
    Ok(parsed)
}

/// Runs `canvas post` for parsed `args`. `--update` replaces that card's
/// content in place instead of creating a new one — the session/cwd that
/// created it aren't touched.
pub fn run(args: PostArgs) -> Result<(), String> {
    let format = resolve_format(args.arg, args.format_flag)?;
    let input = read_html(args.arg, std::io::stdin())?;
    validate(&input)?;
    let converted = format::convert(&input, format);
    let inlined = inline_css::inline_stylesheets(&converted, |path| std::fs::read(path).ok());
    let scanned = scan::scan(&inlined.html, |path| std::path::Path::new(path).exists());
    for warning in inlined.warnings.iter().chain(&scanned.warnings) {
        eprintln!("{warning}");
    }
    let card = match args.update_id {
        Some(id) => {
            if args.pin.is_some() {
                return Err("--pin cannot be combined with --update".to_string());
            }
            client::update_card(id, scanned.html, scanned.images, scanned.targets)?
        }
        None => {
            let pin = build_pin(&args)?;
            let (adapter, session_id) = session()?;
            let cwd = current_dir()?;
            client::post_explicit(PostRequest {
                session_id,
                cwd,
                agent: adapter.agent(),
                html: scanned.html,
                images: scanned.images,
                targets: scanned.targets,
                pin,
                pid: adapter.pid(),
            })?
        }
    };
    println!(
        "{}",
        serde_json::json!({
            "card_id": card.id,
            "images": card.images,
            "targets": card.targets,
        })
    );
    Ok(())
}

/// The slot `--pin` names, with its scope (default session) and the widget
/// file converted like a card body. A widget is a small tile: unlike the card
/// it carries no local images or clickable links.
fn build_pin(args: &PostArgs) -> Result<Option<Pin>, String> {
    let Some(slot) = args.pin else {
        return Ok(None);
    };
    if slot.trim().is_empty() {
        return Err("--pin needs a slot name".to_string());
    }
    let scope = match args.pin_scope {
        None | Some("session") => PinScope::Session,
        Some("repo") => PinScope::Repo,
        Some(other) => {
            return Err(format!(
                "unknown --pin-scope {other:?} (expected session or repo)"
            ))
        }
    };
    let widget_html = match args.widget {
        Some(path) => {
            let widget =
                std::fs::read_to_string(path).map_err(|e| format!("could not read {path}: {e}"))?;
            if widget.trim().is_empty() {
                return Err("no HTML in the --widget file (empty input)".to_string());
            }
            let format = resolve_format(Some(path), None)?;
            Some(format::convert(&widget, format))
        }
        None => None,
    };
    let refresh = match args.refresh {
        Some(command) => {
            if command.trim().is_empty() {
                return Err("--refresh needs a command".to_string());
            }
            let every_secs = match args.every {
                None => DEFAULT_REFRESH_SECS,
                Some(v) => v
                    .parse::<u64>()
                    .map_err(|_| format!("--every needs a whole number of seconds, got {v:?}"))?,
            };
            if every_secs < MIN_REFRESH_SECS {
                return Err(format!(
                    "--every must be at least {MIN_REFRESH_SECS} seconds"
                ));
            }
            Some(Refresh {
                command: command.to_string(),
                every_secs,
            })
        }
        None => None,
    };
    Ok(Some(Pin {
        slot: slot.to_string(),
        scope,
        widget_html,
        refresh,
        refresh_error: None,
    }))
}

/// `--format`, else the file extension, else Markdown (including for stdin
/// and an unrecognised extension).
fn resolve_format(arg: Option<&str>, format_flag: Option<&str>) -> Result<Format, String> {
    if let Some(value) = format_flag {
        return Format::parse(value)
            .ok_or_else(|| format!("unknown --format {value:?} (expected md, text or html)"));
    }
    let from_ext = match arg {
        Some(path) if path != "-" => Format::from_extension(path),
        _ => None,
    };
    Ok(from_ext.unwrap_or(Format::Markdown))
}

/// The agent this shell runs under and its session id, read from the
/// environment variable that agent's adapter names.
fn session() -> Result<(&'static dyn AgentAdapter, String), String> {
    agent::from_env().ok_or_else(|| {
        format!(
            "canvas post must run inside a coding-agent session (one of {} must be set)",
            agent::session_envs().join(", ")
        )
    })
}

fn current_dir() -> Result<String, String> {
    std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .map_err(|e| format!("could not resolve the current directory: {e}"))
}

/// A file argument reads that file; no argument, or `-`, reads `stdin`.
pub(crate) fn read_html<R: Read>(arg: Option<&str>, mut stdin: R) -> Result<String, String> {
    match arg {
        Some(path) if path != "-" => {
            std::fs::read_to_string(path).map_err(|e| format!("could not read {path}: {e}"))
        }
        _ => {
            let mut buf = String::new();
            stdin
                .read_to_string(&mut buf)
                .map_err(|e| format!("could not read stdin: {e}"))?;
            Ok(buf)
        }
    }
}

fn validate(html: &str) -> Result<(), String> {
    if html.trim().is_empty() {
        Err("no HTML to post (empty input)".to_string())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn file_argument_is_read_from_disk() {
        let dir = std::env::temp_dir().join(format!("canvas-post-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("post.html");
        std::fs::write(&path, "<p>hi</p>").unwrap();

        let html = read_html(Some(path.to_str().unwrap()), Cursor::new(Vec::new())).unwrap();
        assert_eq!(html, "<p>hi</p>");
    }

    #[test]
    fn missing_file_argument_is_an_error() {
        let err = read_html(
            Some("/definitely/does/not/exist.html"),
            Cursor::new(Vec::new()),
        )
        .unwrap_err();
        assert!(err.contains("/definitely/does/not/exist.html"));
    }

    #[test]
    fn no_argument_or_dash_reads_stdin() {
        let html = read_html(None, Cursor::new(b"<p>from stdin</p>".to_vec())).unwrap();
        assert_eq!(html, "<p>from stdin</p>");

        let html = read_html(Some("-"), Cursor::new(b"<p>dash</p>".to_vec())).unwrap();
        assert_eq!(html, "<p>dash</p>");
    }

    #[test]
    fn empty_or_whitespace_only_html_is_rejected() {
        assert!(validate("").is_err());
        assert!(validate("   \n\t").is_err());
        assert!(validate("<p>ok</p>").is_ok());
    }

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn one_positional_with_no_flag() {
        let rest = args(&["file.md"]);
        let parsed = parse_args(&rest).unwrap();
        assert_eq!(
            parsed,
            PostArgs {
                arg: Some("file.md"),
                ..Default::default()
            }
        );
    }

    #[test]
    fn format_before_positional() {
        let rest = args(&["--format", "html", "file.md"]);
        let parsed = parse_args(&rest).unwrap();
        assert_eq!(
            parsed,
            PostArgs {
                arg: Some("file.md"),
                format_flag: Some("html"),
                ..Default::default()
            }
        );
    }

    #[test]
    fn format_after_positional() {
        let rest = args(&["file.md", "--format", "html"]);
        let parsed = parse_args(&rest).unwrap();
        assert_eq!(
            parsed,
            PostArgs {
                arg: Some("file.md"),
                format_flag: Some("html"),
                ..Default::default()
            }
        );
    }

    #[test]
    fn format_missing_its_value_is_an_error() {
        assert!(parse_args(&args(&["file.md", "--format"])).is_err());
    }

    #[test]
    fn duplicate_format_is_an_error() {
        assert!(parse_args(&args(&["--format", "md", "--format", "html", "x.md"])).is_err());
    }

    #[test]
    fn update_flag_is_parsed() {
        let rest = args(&["--update", "card-123", "file.md"]);
        let parsed = parse_args(&rest).unwrap();
        assert_eq!(
            parsed,
            PostArgs {
                arg: Some("file.md"),
                update_id: Some("card-123"),
                ..Default::default()
            }
        );
    }

    #[test]
    fn duplicate_update_is_an_error() {
        assert!(parse_args(&args(&["--update", "a", "--update", "b", "x.md"])).is_err());
    }

    #[test]
    fn update_missing_its_value_is_an_error() {
        assert!(parse_args(&args(&["file.md", "--update"])).is_err());
    }

    #[test]
    fn pin_flags_are_parsed() {
        let rest = args(&[
            "a.md",
            "--pin",
            "status",
            "--pin-scope",
            "repo",
            "--widget",
            "w.html",
        ]);
        assert_eq!(
            parse_args(&rest).unwrap(),
            PostArgs {
                arg: Some("a.md"),
                pin: Some("status"),
                pin_scope: Some("repo"),
                widget: Some("w.html"),
                ..Default::default()
            }
        );
    }

    #[test]
    fn pin_scope_or_widget_without_a_pin_is_an_error() {
        assert!(parse_args(&args(&["a.md", "--pin-scope", "repo"])).is_err());
        assert!(parse_args(&args(&["a.md", "--widget", "w.html"])).is_err());
    }

    #[test]
    fn a_bad_pin_scope_names_the_value() {
        let rest = args(&["--pin", "s", "--pin-scope", "global"]);
        let err = build_pin(&parse_args(&rest).unwrap()).unwrap_err();
        assert!(err.contains("\"global\""), "{err}");
    }

    #[test]
    fn refresh_builds_a_pin_with_the_given_interval() {
        let rest = args(&["--pin", "s", "--refresh", "date", "--every", "7"]);
        let pin = build_pin(&parse_args(&rest).unwrap()).unwrap().unwrap();
        assert_eq!(
            pin.refresh,
            Some(Refresh {
                command: "date".to_string(),
                every_secs: 7
            })
        );
    }

    #[test]
    fn an_interval_under_the_minimum_is_an_error() {
        let rest = args(&["--pin", "s", "--refresh", "date", "--every", "2"]);
        let err = build_pin(&parse_args(&rest).unwrap()).unwrap_err();
        assert_eq!(err, "--every must be at least 5 seconds");
    }

    #[test]
    fn refresh_without_a_pin_or_every_without_refresh_is_an_error() {
        assert!(parse_args(&args(&["a.md", "--refresh", "date"])).is_err());
        assert!(parse_args(&args(&["a.md", "--pin", "s", "--every", "9"])).is_err());
    }

    #[test]
    fn a_second_positional_is_an_error() {
        assert!(parse_args(&args(&["a.md", "b.md"])).is_err());
    }
}
