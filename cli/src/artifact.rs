//! `canvas artifact new|put|relink|list|show|delete|log`: artifacts are folders
//! of files canvasd keeps until someone deletes them, or a folder or HTML file
//! of the person's that an artifact links to, shown on the Artifacts page as
//! real web pages. Every verb prints canvasd's JSON on one line and, like
//! `canvas post`, fails loudly: one stderr line, non-zero exit.

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
}

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
