//! `canvas artifact new|put|list|show|delete`: artifacts are folders of files
//! canvasd keeps until someone deletes them, shown on the Artifacts page as
//! real web pages. Every verb prints canvasd's JSON on one line and, like
//! `canvas post`, fails loudly: one stderr line, non-zero exit.

use crate::client::{self, percent_encode};

pub struct UsageError;

pub enum Command {
    New { title: Option<String> },
    Put { id: String, source: String },
    List,
    Show { id: String },
    Delete { id: String },
}

pub fn parse_args(args: &[String]) -> Result<Command, UsageError> {
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    match words.as_slice() {
        ["new"] => Ok(Command::New { title: None }),
        ["new", "--title", title] => Ok(Command::New {
            title: Some(title.to_string()),
        }),
        ["put", id, source] => Ok(Command::Put {
            id: id.to_string(),
            source: source.to_string(),
        }),
        ["list"] => Ok(Command::List),
        ["show", id] => Ok(Command::Show { id: id.to_string() }),
        ["delete", id] => Ok(Command::Delete { id: id.to_string() }),
        _ => Err(UsageError),
    }
}

pub fn run(command: Command) -> Result<(), String> {
    let value = match command {
        Command::New { title } => client::artifact_call(
            "POST",
            "/api/artifacts",
            Some(&serde_json::json!({ "title": title })),
        )?,
        Command::Put { id, source } => {
            // canvasd copies from the path, so it must not depend on this
            // shell's working directory.
            let source = std::path::absolute(&source)
                .map_err(|e| format!("cannot resolve {source}: {e}"))?;
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
    };
    println!("{value}");
    Ok(())
}
