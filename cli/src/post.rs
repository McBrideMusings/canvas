//! `canvas post` — lets an agent drop rich HTML into its own turn's card.
//!
//! Every other CLI path (the `hook` subcommands) must never disturb the
//! Claude session it's attached to, so failures there are swallowed and the
//! process exits 0. `post` is the deliberate exception: an agent runs it on
//! purpose and needs to know if it failed, so every failure here prints
//! exactly one line to stderr and the process exits non-zero.

use std::io::Read;

use crate::client;
use crate::format::{self, Format};
use crate::scan;

/// The parsed `canvas post` arguments: the positional path (or `-`/absent
/// for stdin) and an explicit `--format md|text|html`, when given.
#[derive(Debug, PartialEq, Eq)]
pub struct PostArgs<'a> {
    pub arg: Option<&'a str>,
    pub format_flag: Option<&'a str>,
}

/// A malformed `canvas post` argument list. Carries no detail: the caller
/// always responds by printing the usage line and exiting 2.
#[derive(Debug, PartialEq, Eq)]
pub struct UsageError;

/// Parses the arguments to `canvas post` (everything after `post` itself).
/// `--format <value>` may appear before or after the single positional
/// path/`-`. Returns `Err` on a usage error: `--format` with no following
/// value, `--format` given twice, or a second positional argument. Does not
/// validate the format value itself — `resolve_format` does that.
pub fn parse_args(rest: &[String]) -> Result<PostArgs<'_>, UsageError> {
    let mut arg: Option<&str> = None;
    let mut format_flag: Option<&str> = None;
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--format" => {
                if format_flag.is_some() {
                    return Err(UsageError);
                }
                match rest.get(i + 1) {
                    Some(v) => {
                        format_flag = Some(v.as_str());
                        i += 2;
                    }
                    None => return Err(UsageError),
                }
            }
            other if arg.is_none() => {
                arg = Some(other);
                i += 1;
            }
            _ => return Err(UsageError),
        }
    }
    Ok(PostArgs { arg, format_flag })
}

/// `arg` is the file path (or `-`/absent for stdin); `format_flag` is an
/// explicit `--format md|text|html`, when given.
pub fn run(arg: Option<&str>, format_flag: Option<&str>) -> Result<(), String> {
    let format = resolve_format(arg, format_flag)?;
    let input = read_html(arg, std::io::stdin())?;
    validate(&input)?;
    let converted = format::convert(&input, format);
    let scanned = scan::scan(&converted, |path| std::path::Path::new(path).exists());
    for warning in &scanned.warnings {
        eprintln!("{warning}");
    }
    let session_id = session_id()?;
    let cwd = current_dir()?;
    let card = client::post_explicit(
        &session_id,
        &cwd,
        scanned.html,
        scanned.images,
        scanned.targets,
    )?;
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

/// Claude Code sets `CLAUDE_CODE_SESSION_ID` in every Bash tool shell, equal
/// to the `session_id` the hooks get on stdin. There is no other reliable
/// way for a plain shell command to learn which session it's running in.
fn session_id() -> Result<String, String> {
    std::env::var("CLAUDE_CODE_SESSION_ID").map_err(|_| {
        "canvas post must run inside a Claude Code session (CLAUDE_CODE_SESSION_ID is not set)"
            .to_string()
    })
}

fn current_dir() -> Result<String, String> {
    std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .map_err(|e| format!("could not resolve the current directory: {e}"))
}

/// A file argument reads that file; no argument, or `-`, reads `stdin`.
fn read_html<R: Read>(arg: Option<&str>, mut stdin: R) -> Result<String, String> {
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
                format_flag: None,
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
    fn a_second_positional_is_an_error() {
        assert!(parse_args(&args(&["a.md", "b.md"])).is_err());
    }
}
