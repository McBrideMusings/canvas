//! `canvas post` — lets an agent drop rich HTML into its own turn's card.
//!
//! Every other CLI path (the `hook` subcommands) must never disturb the
//! Claude session it's attached to, so failures there are swallowed and the
//! process exits 0. `post` is the deliberate exception: an agent runs it on
//! purpose and needs to know if it failed, so every failure here prints
//! exactly one line to stderr and the process exits non-zero.

use std::io::Read;

use crate::{client, pid};

pub fn run(arg: Option<&str>) -> Result<(), String> {
    let html = read_html(arg, std::io::stdin())?;
    validate(&html)?;
    let claude_pid =
        pid::claude_pid().ok_or_else(|| "could not resolve the calling claude pid".to_string())?;
    client::post_explicit(claude_pid, html)
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
        let err = read_html(Some("/definitely/does/not/exist.html"), Cursor::new(Vec::new()))
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
}
