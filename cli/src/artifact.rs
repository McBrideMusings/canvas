//! `canvas artifact new|put|relink|list|show|delete|log|pane`: artifacts are
//! folders of files canvasd keeps until someone deletes them, or a folder or
//! HTML file of the person's that an artifact links to, shown on the Artifacts
//! page as real web pages. Every verb prints canvasd's JSON on one line and,
//! like `canvas post`, fails loudly: one stderr line, non-zero exit.
//! `--focus` on `new` or `put` then switches every open viewer to the
//! artifact and adds `viewers` to the JSON.
//!
//! `pane <id> --size WxH|--reset|--full|--exit` changes the open viewers' pane
//! for that artifact as the person would by hand, printing `{"viewers": N}`
//! and failing when N is 0; bare `pane` prints the pane a viewer last reported
//! showing, failing when no open viewer has reported one.

use crate::client::{self, percent_encode};

pub struct UsageError;

pub enum Command {
    New {
        title: Option<String>,
        link: Option<String>,
        focus: bool,
    },
    Put {
        id: String,
        source: String,
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
            Ok(Command::New { title, link, focus })
        }
        ["put", id, source] => Ok(Command::Put {
            id: id.to_string(),
            source: source.to_string(),
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
        ["pane"] => Ok(Command::PaneShown),
        ["pane", id, flag @ ..] => Ok(Command::Pane {
            id: id.to_string(),
            action: parse_pane_action(flag)?,
        }),
        _ => Err(UsageError),
    }
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
        Command::New { title, link, focus } => {
            let link = link.as_deref().map(absolute).transpose()?;
            let value = client::artifact_call(
                "POST",
                "/api/artifacts",
                Some(&serde_json::json!({ "title": title, "link": link })),
            )?;
            with_focus(value, focus)
        }
        Command::Relink { id, link } => client::artifact_call(
            "POST",
            &format!("/api/artifacts/{}/relink", percent_encode(&id)),
            Some(&serde_json::json!({ "link": absolute(&link)? })),
        )?,
        Command::Put { id, source, focus } => {
            let source = absolute(&source)?;
            let value = client::artifact_call(
                "POST",
                &format!("/api/artifacts/{}/put", percent_encode(&id)),
                Some(&serde_json::json!({ "source": source })),
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
        let Ok(Command::Put { id, source, focus }) =
            parse_args(&args(&["put", "art-1", "site", "--focus"]))
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
