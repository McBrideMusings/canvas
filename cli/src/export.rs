//! `canvas export <card_id> [-o file]`: writes the card as one standalone
//! HTML page (canvasd's `GET /api/cards/:id/export`) to `<card_id>.html` or
//! the named file, and prints `{"path", "cards": 1, "warnings"}`. A warning
//! (an image gone from disk) never fails the export; only an unreachable
//! daemon, an unknown card or an unwritable file does.

use std::path::{Path, PathBuf};

use crate::client;

#[derive(Debug, PartialEq, Eq)]
pub struct Args {
    pub card_id: String,
    pub out: Option<String>,
}

pub fn parse_args(args: &[String]) -> Result<Args, String> {
    let mut card_id = None;
    let mut out = None;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-o" | "--out" => out = Some(it.next().ok_or("-o needs a file")?.clone()),
            flag if flag.starts_with('-') => return Err(format!("unknown flag {flag}")),
            id if card_id.is_none() => card_id = Some(id.to_string()),
            extra => return Err(format!("unexpected argument {extra}")),
        }
    }
    Ok(Args {
        card_id: card_id.ok_or("canvas export needs a card id")?,
        out,
    })
}

pub fn run(args: Args) -> Result<(), String> {
    let result = client::export_card(&args.card_id)?;
    let path = args
        .out
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(format!("{}.html", args.card_id)));
    std::fs::write(&path, &result.html)
        .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    let path = absolute(&path);
    canvas_core::log::info(
        "export",
        &[
            ("card", &args.card_id),
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
            Ok(Args {
                card_id: "c1".into(),
                out: Some("x.html".into())
            })
        );
        assert_eq!(
            parse_args(&args(&["c1"])),
            Ok(Args {
                card_id: "c1".into(),
                out: None
            })
        );
        assert!(parse_args(&args(&[])).is_err());
        assert!(parse_args(&args(&["c1", "--zip"])).is_err());
        assert!(parse_args(&args(&["c1", "-o"])).is_err());
    }
}
