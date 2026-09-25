//! Dispatches one Claude Code hook invocation: reads the hook JSON Claude
//! Code writes to stdin, and calls canvasd. Every error — bad stdin, no
//! canvasd listening, an unrecognised event — is returned as `Err` so
//! `main` can swallow it and exit 0 silently; a hook must never slow or
//! break the Claude session it's attached to.

use std::io::Read;

use crate::client;

#[derive(Debug, serde::Deserialize)]
struct HookInput {
    session_id: String,
    #[serde(default)]
    cwd: String,
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
        other => return Err(format!("unknown hook event: {other}").into()),
    }

    Ok(())
}
