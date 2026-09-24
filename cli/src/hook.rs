//! Dispatches one Claude Code hook invocation: reads the hook JSON Claude
//! Code writes to stdin, and calls canvasd. Every error — bad stdin, no
//! canvasd listening, a malformed transcript — is returned as `Err` so
//! `main` can swallow it and exit 0 silently; a hook must never slow or
//! break the Claude session it's attached to.

use std::io::Read;

use crate::{client, pid, transcript};

#[derive(Debug, serde::Deserialize)]
struct HookInput {
    session_id: String,
    #[serde(default)]
    cwd: String,
    #[serde(default)]
    transcript_path: String,
}

pub fn run(event: &str) -> Result<(), Box<dyn std::error::Error>> {
    let mut raw = String::new();
    std::io::stdin().read_to_string(&mut raw)?;
    let input: HookInput = serde_json::from_str(&raw)?;

    match event {
        "session-start" => {
            let claude_pid = pid::claude_pid().ok_or("could not resolve claude_pid")?;
            client::upsert_session(&input.session_id, &input.cwd, claude_pid)?;
        }
        "session-end" => {
            client::end_session(&input.session_id)?;
        }
        "stop" => {
            let entries = transcript::entries_since_last_prompt(&input.transcript_path)?;
            let extracted = crate::extract::extract(&entries);
            client::post_turn(
                &input.session_id,
                extracted.links,
                extracted.paths,
                extracted.images,
            )?;
        }
        other => return Err(format!("unknown hook event: {other}").into()),
    }

    Ok(())
}
