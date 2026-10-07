//! `canvas artifact new|put|relink|list|show|delete|log|pane`: artifacts are
//! folders of files canvasd keeps until someone deletes them, or a folder or
//! HTML file of the person's that an artifact links to, shown on the Artifacts
//! page as real web pages. Every verb prints canvasd's JSON on one line and,
//! like `canvas post`, fails loudly: one stderr line, non-zero exit.
//! `--focus` on `new` or `put` then switches every open viewer to the
//! artifact and adds `viewers` to the JSON. `--widget <file>` on either sets
//! the small HTML its row on the Artifacts page shows, and `--refresh '<cmd>'
//! [--every <secs>]` a command canvasd runs in this shell's directory, while
//! this shell's agent lives, whose JSON output reaches the widget and page as
//! `canvas data` does.
//!
//! `state <id> [<key>|--clear]` prints what the page saved with
//! `canvas-state-set` as one JSON object, or one key's value, or deletes it all.
//!
//! `pane <id> --size WxH|--reset|--full|--exit` changes the open viewers' pane
//! for that artifact as the person would by hand, printing `{"viewers": N}`
//! and failing when N is 0; bare `pane` prints the pane a viewer last reported
//! showing, failing when no open viewer has reported one.

use canvas_core::unix_http::percent_encode;
use canvas_core::{Refresh, MIN_REFRESH_SECS};

use crate::client;
use crate::format::{self, Format};

/// How often `--refresh` runs when `--every` is not given.
const DEFAULT_REFRESH_SECS: u64 = 30;

#[derive(Debug)]
pub struct UsageError;

/// The `--widget`, `--refresh` and `--every` values `new` or `put` was given.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Extras {
    pub widget: Option<String>,
    pub refresh: Option<String>,
    pub every: Option<String>,
}

pub enum Command {
    New {
        title: Option<String>,
        link: Option<String>,
        extras: Extras,
        focus: bool,
    },
    Put {
        id: String,
        source: String,
        extras: Extras,
        focus: bool,
    },
    Relink {
        id: String,
        link: String,
    },
    List,
    Show {
        id: String,
    },
    Delete {
        id: String,
    },
    Log {
        id: String,
    },
    Pane {
        id: String,
        action: serde_json::Value,
    },
    PaneShown,
    /// `state <id> [<key>|--clear]`: the values a page saved with
    /// `canvas-state-set`.
    State {
        id: String,
        what: StateWhat,
    },
}

#[derive(Debug, PartialEq, Eq)]
pub enum StateWhat {
    All,
    Key(String),
    Clear,
}

/// The stderr line when no viewer was connected to receive a pane change.
pub const NO_VIEWER: &str = "no Canvas viewer is open to change the pane";

/// The stderr line when no open viewer has reported its pane.
pub const NOT_REPORTED: &str = "no open Canvas viewer has reported an artifact pane";

pub fn parse_args(args: &[String]) -> Result<Command, UsageError> {
    let mut words: Vec<&str> = args.iter().map(String::as_str).collect();
    // `--focus` takes no value and goes anywhere after `new` or `put`.
    let focus = match words.iter().filter(|w| **w == "--focus").count() {
        0 => false,
        1 if matches!(words.first(), Some(&"new" | &"put")) => {
            words.retain(|w| *w != "--focus");
            true
        }
        _ => return Err(UsageError),
    };
    let extras = if matches!(words.first(), Some(&"new" | &"put")) {
        Extras {
            widget: take_flag(&mut words, "--widget")?,
            refresh: take_flag(&mut words, "--refresh")?,
            every: take_flag(&mut words, "--every")?,
        }
    } else {
        Extras::default()
    };
    if extras.every.is_some() && extras.refresh.is_none() {
        return Err(UsageError);
    }
    match words.as_slice() {
        ["new", flags @ ..] => {
            let (mut title, mut link) = (None, None);
            for pair in flags.chunks(2) {
                match pair {
                    ["--title", value] if title.is_none() => title = Some(value.to_string()),
                    ["--link", value] if link.is_none() => link = Some(value.to_string()),
                    _ => return Err(UsageError),
                }
            }
            Ok(Command::New {
                title,
                link,
                extras,
                focus,
            })
        }
        ["put", id, source] => Ok(Command::Put {
            id: id.to_string(),
            source: source.to_string(),
            extras,
            focus,
        }),
        ["relink", id, link] => Ok(Command::Relink {
            id: id.to_string(),
            link: link.to_string(),
        }),
        ["list"] => Ok(Command::List),
        ["show", id] => Ok(Command::Show { id: id.to_string() }),
        ["delete", id] => Ok(Command::Delete { id: id.to_string() }),
        ["log", id] => Ok(Command::Log { id: id.to_string() }),
        ["state", id] => Ok(Command::State {
            id: id.to_string(),
            what: StateWhat::All,
        }),
        ["state", id, "--clear"] => Ok(Command::State {
            id: id.to_string(),
            what: StateWhat::Clear,
        }),
        ["state", id, key] if !key.starts_with('-') => Ok(Command::State {
            id: id.to_string(),
            what: StateWhat::Key(key.to_string()),
        }),
        ["pane"] => Ok(Command::PaneShown),
        ["pane", id, flag @ ..] => Ok(Command::Pane {
            id: id.to_string(),
            action: parse_pane_action(flag)?,
        }),
        _ => Err(UsageError),
    }
}

/// Removes `flag` and the value after it from anywhere after the verb.
/// Given twice, or with no value, is a usage error.
fn take_flag(words: &mut Vec<&str>, flag: &str) -> Result<Option<String>, UsageError> {
    let Some(at) = words.iter().skip(1).position(|w| *w == flag).map(|i| i + 1) else {
        return Ok(None);
    };
    if at + 1 >= words.len() {
        return Err(UsageError);
    }
    let value = words.remove(at + 1).to_string();
    words.remove(at);
    if words.iter().skip(1).any(|w| *w == flag) {
        return Err(UsageError);
    }
    Ok(Some(value))
}

/// The request fields `extras` adds: the widget file converted like a card
/// body (by its extension, else Markdown), and the refresh with its interval
/// and this shell's directory, which the command runs in.
fn extras_json(extras: Extras) -> Result<serde_json::Map<String, serde_json::Value>, String> {
    let mut out = serde_json::Map::new();
    if let Some(path) = extras.widget {
        let text =
            std::fs::read_to_string(&path).map_err(|e| format!("could not read {path}: {e}"))?;
        if text.trim().is_empty() {
            return Err("no HTML in the --widget file (empty input)".to_string());
        }
        let format = Format::from_extension(&path).unwrap_or(Format::Markdown);
        out.insert("widget_html".into(), format::convert(&text, format).into());
    }
    if let Some(command) = extras.refresh {
        if command.trim().is_empty() {
            return Err("--refresh needs a command".to_string());
        }
        let every_secs = match extras.every {
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
        let refresh = Refresh {
            command,
            every_secs,
        };
        let cwd = std::env::current_dir()
            .map_err(|e| format!("could not resolve the current directory: {e}"))?;
        out.insert(
            "refresh".into(),
            serde_json::to_value(refresh).map_err(|e| e.to_string())?,
        );
        out.insert("cwd".into(), cwd.to_string_lossy().into_owned().into());
    }
    Ok(out)
}

fn parse_pane_action(flag: &[&str]) -> Result<serde_json::Value, UsageError> {
    match flag {
        ["--size", size] => {
            let (w, h) = size.split_once(['x', 'X', '×']).ok_or(UsageError)?;
            let width: u32 = w.parse().map_err(|_| UsageError)?;
            let height: u32 = h.parse().map_err(|_| UsageError)?;
            if width == 0 || height == 0 {
                return Err(UsageError);
            }
            Ok(serde_json::json!({ "action": "resize", "width": width, "height": height }))
        }
        ["--reset"] => Ok(serde_json::json!({ "action": "reset" })),
        ["--full"] => Ok(serde_json::json!({ "action": "full" })),
        ["--exit"] => Ok(serde_json::json!({ "action": "exit" })),
        _ => Err(UsageError),
    }
}

pub fn run(command: Command) -> Result<(), String> {
    let value = match command {
        Command::New {
            title,
            link,
            extras,
            focus,
        } => {
            let link = link.as_deref().map(absolute).transpose()?;
            let mut body = extras_json(extras)?;
            body.insert("title".into(), title.into());
            body.insert("link".into(), link.into());
            let value = client::artifact_call("POST", "/api/artifacts", Some(&body.into()))?;
            with_focus(value, focus)
        }
        Command::Relink { id, link } => client::artifact_call(
            "POST",
            &format!("/api/artifacts/{}/relink", percent_encode(&id)),
            Some(&serde_json::json!({ "link": absolute(&link)? })),
        )?,
        Command::Put {
            id,
            source,
            extras,
            focus,
        } => {
            let source = absolute(&source)?;
            let mut body = extras_json(extras)?;
            body.insert("source".into(), source.into());
            let value = client::artifact_call(
                "POST",
                &format!("/api/artifacts/{}/put", percent_encode(&id)),
                Some(&body.into()),
            )?;
            with_focus(value, focus)
        }
        Command::List => client::artifact_call("GET", "/api/artifacts", None)?,
        Command::Show { id } => client::artifact_call(
            "GET",
            &format!("/api/artifacts/{}", percent_encode(&id)),
            None,
        )?,
        Command::Delete { id } => client::artifact_call(
            "DELETE",
            &format!("/api/artifacts/{}", percent_encode(&id)),
            None,
        )?,
        Command::Log { id } => client::artifact_call(
            "GET",
            &format!("/api/artifacts/{}/log", percent_encode(&id)),
            None,
        )?,
        Command::Pane { id, action } => {
            let value = client::artifact_call(
                "POST",
                &format!("/api/artifacts/{}/pane", percent_encode(&id)),
                Some(&action),
            )?;
            if value["viewers"].as_u64() == Some(0) {
                return Err(NO_VIEWER.to_string());
            }
            value
        }
        Command::State { id, what } => {
            let base = format!("/api/artifacts/{}/state", percent_encode(&id));
            match what {
                StateWhat::All => client::artifact_call("GET", &base, None)?,
                StateWhat::Key(key) => {
                    client::artifact_call("GET", &format!("{base}/{}", percent_encode(&key)), None)?
                }
                StateWhat::Clear => client::artifact_call("DELETE", &base, None)?,
            }
        }
        Command::PaneShown => {
            let value = client::artifact_call("GET", "/api/artifact-pane", None)?;
            if value["pane"].is_null() {
                return Err(NOT_REPORTED.to_string());
            }
            value
        }
    };
    println!("{value}");
    Ok(())
}

/// `--focus` on `new` or `put`: once the artifact is written, every open
/// viewer switches to it, as `canvas focus` does, and the output gains
/// `viewers`. The artifact exists either way, so a focus that reached nobody
/// is a stderr warning, not a failure an agent would answer by running the
/// command again.
fn with_focus(mut value: serde_json::Value, focus: bool) -> serde_json::Value {
    if !focus {
        return value;
    }
    let Some(id) = value["id"].as_str().map(str::to_string) else {
        eprintln!("written, but canvasd's answer carried no id to focus");
        return value;
    };
    match client::focus_artifact(&id) {
        Ok(viewers) => {
            if viewers == 0 {
                eprintln!("{}", crate::focus::NO_VIEWER_ARTIFACT);
            }
            value["viewers"] = viewers.into();
        }
        Err(e) => eprintln!("written, but could not focus the artifact: {e}"),
    }
    value
}

/// canvasd reads the path itself, so it must not depend on this shell's
/// working directory. Symlinks stay as written, so a link follows them.
fn absolute(path: &str) -> Result<String, String> {
    std::path::absolute(path)
        .map(|p| p.to_string_lossy().into_owned())
        .map_err(|e| format!("cannot resolve {path}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| w.to_string()).collect()
    }

    #[test]
    fn pane_parses_each_action() {
        let Ok(Command::Pane { id, action }) =
            parse_args(&args(&["pane", "art-1", "--size", "960x600"]))
        else {
            panic!("--size did not parse");
        };
        assert_eq!(id, "art-1");
        assert_eq!(
            action,
            serde_json::json!({ "action": "resize", "width": 960, "height": 600 })
        );
        for (flag, name) in [("--reset", "reset"), ("--full", "full"), ("--exit", "exit")] {
            let Ok(Command::Pane { action, .. }) = parse_args(&args(&["pane", "art-1", flag]))
            else {
                panic!("{flag} did not parse");
            };
            assert_eq!(action["action"], name);
        }
        assert!(matches!(
            parse_args(&args(&["pane"])),
            Ok(Command::PaneShown)
        ));
    }

    #[test]
    fn focus_goes_anywhere_after_new_or_put_and_nowhere_else() {
        let Ok(Command::New { title, focus, .. }) =
            parse_args(&args(&["new", "--focus", "--title", "Plan"]))
        else {
            panic!("new --focus did not parse");
        };
        assert_eq!((title.as_deref(), focus), (Some("Plan"), true));
        let Ok(Command::Put {
            id, source, focus, ..
        }) = parse_args(&args(&["put", "art-1", "site", "--focus"]))
        else {
            panic!("put --focus did not parse");
        };
        assert_eq!(
            (id.as_str(), source.as_str(), focus),
            ("art-1", "site", true)
        );
        assert!(matches!(
            parse_args(&args(&["put", "art-1", "site"])),
            Ok(Command::Put { focus: false, .. })
        ));
        assert!(parse_args(&args(&["new", "--focus", "--focus"])).is_err());
        assert!(parse_args(&args(&["show", "art-1", "--focus"])).is_err());
    }

    #[test]
    fn widget_and_refresh_go_anywhere_after_new_or_put() {
        let Ok(Command::Put {
            id,
            source,
            extras,
            focus,
        }) = parse_args(&args(&[
            "put",
            "--refresh",
            "df -h",
            "art-1",
            "--every",
            "10",
            "site",
            "--widget",
            "w.html",
            "--focus",
        ]))
        else {
            panic!("put with extras did not parse");
        };
        assert_eq!(
            (id.as_str(), source.as_str(), focus),
            ("art-1", "site", true)
        );
        assert_eq!(
            extras,
            Extras {
                widget: Some("w.html".to_string()),
                refresh: Some("df -h".to_string()),
                every: Some("10".to_string()),
            }
        );
        assert!(parse_args(&args(&["new", "--every", "10"])).is_err());
        assert!(parse_args(&args(&["new", "--widget", "a", "--widget", "b"])).is_err());
        assert!(parse_args(&args(&["new", "--refresh"])).is_err());
        assert!(parse_args(&args(&["show", "art-1", "--widget", "w.html"])).is_err());
    }

    #[test]
    fn an_interval_under_the_minimum_is_an_error() {
        let extras = Extras {
            refresh: Some("date".to_string()),
            every: Some("2".to_string()),
            ..Default::default()
        };
        assert_eq!(
            extras_json(extras).unwrap_err(),
            "--every must be at least 5 seconds"
        );
    }

    #[test]
    fn state_parses_all_one_key_and_clear() {
        let what = |words: &[&str]| match parse_args(&args(words)) {
            Ok(Command::State { id, what }) => {
                assert_eq!(id, "art-1");
                what
            }
            _ => panic!("{words:?} did not parse as state"),
        };
        assert_eq!(what(&["state", "art-1"]), StateWhat::All);
        assert_eq!(
            what(&["state", "art-1", "score"]),
            StateWhat::Key("score".into())
        );
        assert_eq!(what(&["state", "art-1", "--clear"]), StateWhat::Clear);
        assert!(parse_args(&args(&["state"])).is_err());
        assert!(parse_args(&args(&["state", "art-1", "--bogus"])).is_err());
        assert!(parse_args(&args(&["state", "art-1", "a", "b"])).is_err());
    }

    #[test]
    fn pane_refuses_a_bad_size() {
        for bad in ["960", "0x600", "960x", "wide x tall"] {
            assert!(
                parse_args(&args(&["pane", "art-1", "--size", bad])).is_err(),
                "{bad}"
            );
        }
        assert!(parse_args(&args(&["pane", "art-1"])).is_err());
    }
}
