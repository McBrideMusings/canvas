//! Dispatches one Claude Code hook invocation: reads the hook JSON Claude
//! Code writes to stdin, and calls canvasd. Every error — bad stdin, no
//! canvasd listening, a malformed transcript — is returned as `Err` so
//! `main` can swallow it and exit 0 silently; a hook must never slow or
//! break the Claude session it's attached to.

use std::io::Read;

use crate::{client, transcript};

#[derive(Debug, serde::Deserialize)]
struct HookInput {
    session_id: String,
    #[serde(default)]
    cwd: String,
    #[serde(default)]
    transcript_path: String,
    /// The turn's final reply. Claude Code is often still writing the
    /// transcript when Stop fires, so this is the only reliable copy of it.
    #[serde(default)]
    last_assistant_message: String,
}

pub fn run(event: &str) -> Result<(), Box<dyn std::error::Error>> {
    let mut raw = String::new();
    std::io::stdin().read_to_string(&mut raw)?;
    let input: HookInput = serde_json::from_str(&raw)?;

    match event {
        "session-start" => {
            client::upsert_session(&input.session_id, &input.cwd)?;
        }
        "session-end" => {
            client::end_session(&input.session_id)?;
        }
        "stop" => {
            // An unreadable transcript still leaves the final reply to scan.
            let mut entries =
                transcript::entries_since_last_prompt(&input.transcript_path).unwrap_or_default();
            entries.push(crate::extract::TranscriptEntry {
                assistant_text: vec![input.last_assistant_message],
                ..Default::default()
            });
            let extracted = crate::extract::extract(&entries);
            client::post_turn(
                &input.session_id,
                &input.cwd,
                extracted.links,
                extracted.paths,
                extracted.images,
            )?;
        }
        other => return Err(format!("unknown hook event: {other}").into()),
    }

    Ok(())
}
