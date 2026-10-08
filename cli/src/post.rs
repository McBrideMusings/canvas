//! `canvas post` — lets an agent drop rich HTML into its own turn's card.
//!
//! Every other CLI path (the `hook` subcommands) must never disturb the
//! Claude session it's attached to, so failures there are swallowed and the
//! process exits 0. `post` is the deliberate exception: an agent runs it on
//! purpose and needs to know if it failed, so every failure here prints
//! exactly one line to stderr and the process exits non-zero.

use std::io::Read;

use canvas_core::PostRequest;

use crate::agent::{self, AgentAdapter};
use crate::client;
use crate::format::{self, Format};
use crate::inline_css;
use crate::scan;

/// The parsed `canvas post` arguments: the positional path (or `-`/absent
/// for stdin), an explicit `--format md|text|html`, an optional
/// `--update <card_id>` to replace an existing card instead of creating one,
/// and `--focus` to bring the card into view in every open viewer.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct PostArgs<'a> {
    pub arg: Option<&'a str>,
    pub format_flag: Option<&'a str>,
    pub update_id: Option<&'a str>,
    pub focus: bool,
}

/// A malformed `canvas post` argument list. The caller prints the flag it
/// names, when it names one, then the usage line, and exits 2.
#[derive(Debug, PartialEq, Eq)]
pub struct UsageError(pub Option<String>);

/// Parses the arguments to `canvas post` (everything after `post` itself).
/// Each flag may appear before or after the single positional path/`-`.
/// `--focus` takes no value. Returns `Err` on a usage error: an unknown
/// flag (named in the error), a flag with no following value, a flag given
/// twice, or a second positional argument. Does not validate the flag values
/// themselves — `run` does.
pub fn parse_args(rest: &[String]) -> Result<PostArgs<'_>, UsageError> {
    let mut parsed = PostArgs::default();
    let mut i = 0;
    while i < rest.len() {
        let flag = match rest[i].as_str() {
            "--focus" if !parsed.focus => {
                parsed.focus = true;
                i += 1;
                continue;
            }
            "--format" => &mut parsed.format_flag,
            "--update" => &mut parsed.update_id,
            other if other.starts_with("--") => {
                return Err(UsageError(Some(format!("unknown flag {other}"))))
            }
            other if parsed.arg.is_none() => {
                parsed.arg = Some(other);
                i += 1;
                continue;
            }
            _ => return Err(UsageError(None)),
        };
        match (flag.is_some(), rest.get(i + 1)) {
            (false, Some(v)) => *flag = Some(v.as_str()),
            _ => return Err(UsageError(None)),
        }
        i += 2;
    }
    Ok(parsed)
}

/// The bytes a post's `<link rel="stylesheet" href>` inlines: `/canvas.css`
/// is Canvas's own stylesheet, the copy compiled into this binary (the one
/// canvasd serves), and any other path a local file.
fn read_stylesheet(path: &str) -> Option<Vec<u8>> {
    if path == "/canvas.css" {
        return Some(canvasd::viewer::canvas_css().into_bytes());
    }
    std::fs::read(path).ok()
}

/// Runs `canvas post` for parsed `args`. `--update` replaces that card's
/// content in place instead of creating a new one — the session/cwd that
/// created it aren't touched.
pub fn run(args: PostArgs) -> Result<(), String> {
    let format = resolve_format(args.arg, args.format_flag)?;
    let input = read_html(args.arg, std::io::stdin())?;
    validate(&input)?;
    let converted = format::convert(&input, format);
    let inlined = inline_css::inline_stylesheets(&converted, read_stylesheet);
    let home = std::env::var("HOME").ok();
    let exists = |path: &str| std::path::Path::new(path).exists();
    let mut scanned = scan::scan(&inlined.html, home.as_deref(), exists);
    if format == Format::Text {
        scanned.hints = scan::text_hints(&input, home.as_deref(), exists);
    }
    for warning in inlined.warnings.iter().chain(&scanned.warnings) {
        eprintln!("{warning}");
    }
    for hint in &scanned.hints {
        eprintln!("canvas post: {hint}");
    }
    let card = match args.update_id {
        Some(id) => client::update_card(id, scanned.html, scanned.images, scanned.targets)?,
        None => {
            let (adapter, session_id) = session()?;
            let cwd = current_dir()?;
            client::post_explicit(PostRequest {
                session_id,
                cwd,
                agent: adapter.agent(),
                html: scanned.html,
                images: scanned.images,
                targets: scanned.targets,
                pid: adapter.pid(),
            })?
        }
    };
    for hint in &scanned.hints {
        canvas_core::log::info("post hint", &[("card", &card.id), ("hint", hint)]);
    }
    let mut output = serde_json::json!({
        "card_id": card.id,
        "images": card.images,
        "targets": card.targets,
        "hints": scanned.hints,
    });
    if args.focus {
        // The card exists either way, so a focus that reached nobody is a
        // warning, not a failure an agent would answer by posting again.
        match client::focus_card(&card.id) {
            Ok(viewers) => {
                if viewers == 0 {
                    eprintln!("{}", crate::focus::NO_VIEWER);
                }
                output["viewers"] = viewers.into();
            }
            Err(e) => eprintln!("posted, but could not focus the card: {e}"),
        }
    }
    println!("{output}");
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
    fn a_link_to_canvas_css_inlines_the_canvas_stylesheet() {
        let html = r#"<link rel="stylesheet" href="/canvas.css"><p>x</p>"#;
        let r = inline_css::inline_stylesheets(html, read_stylesheet);
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
        assert!(r.html.starts_with("<style>"), "{}", r.html);
        assert!(r.html.contains("--color-text:"), "the tokens");
        assert!(r.html.contains("h1 {"), "the base styles");
        assert!(r.html.ends_with("</style><p>x</p>"));
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
    fn focus_takes_no_value_and_combines_with_update() {
        let rest = args(&["--focus", "--update", "c1", "a.md"]);
        assert_eq!(
            parse_args(&rest).unwrap(),
            PostArgs {
                arg: Some("a.md"),
                update_id: Some("c1"),
                focus: true,
                ..Default::default()
            }
        );
        assert!(parse_args(&args(&["--focus", "--focus", "a.md"])).is_err());
    }

    #[test]
    fn an_unknown_flag_is_named() {
        assert_eq!(
            parse_args(&args(&["a.md", "--pin", "status"])),
            Err(UsageError(Some("unknown flag --pin".to_string())))
        );
    }

    #[test]
    fn a_second_positional_is_an_error() {
        assert!(parse_args(&args(&["a.md", "b.md"])).is_err());
    }
}
