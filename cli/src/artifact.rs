//! `canvas artifact new|put|relink|list|show|delete|log|pane`: artifacts are
//! folders of files canvasd keeps until someone deletes them, or a folder or
//! HTML file of the person's that an artifact links to, shown on the Artifacts
//! page as real web pages. Every verb prints canvasd's JSON on one line and,
//! like `canvas post`, fails loudly: one stderr line, non-zero exit.
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
    },
    Put {
        id: String,
        source: String,
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
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
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
            Ok(Command::New { title, link })
        }
        ["put", id, source] => Ok(Command::Put {
            id: id.to_string(),
            source: source.to_string(),
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
        Command::New { title, link } => {
            let link = link.as_deref().map(absolute).transpose()?;
            client::artifact_call(
                "POST",
                "/api/artifacts",
                Some(&serde_json::json!({ "title": title, "link": link })),
            )?
        }
        Command::Relink { id, link } => client::artifact_call(
            "POST",
            &format!("/api/artifacts/{}/relink", percent_encode(&id)),
            Some(&serde_json::json!({ "link": absolute(&link)? })),
        )?,
        Command::Put { id, source } => {
            let source = absolute(&source)?;
            client::artifact_call(
                "POST",
                &format!("/api/artifacts/{}/put", percent_encode(&id)),
                Some(&serde_json::json!({ "source": source })),
            )?
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
