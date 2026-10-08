//! Dispatches one Claude Code hook invocation: reads the hook JSON Claude
//! Code writes to stdin. `session-start` and `prompt` read the instruction and
//! reminder layers from files (`canvas_core::instructions`) and never contact
//! canvasd, so the daemon's state never changes what an agent reads;
//! `session-end` tells canvasd the session ended. Every error — bad stdin, no
//! canvasd listening, an unrecognised event — is returned as `Err` so `main`
//! can swallow it and exit 0 silently. A hook must never slow or break the
//! Claude session it's attached to.

use std::io::Read;
use std::path::PathBuf;

use canvas_core::instructions::{self, Kind, Overrides};

use crate::agent::{AgentAdapter, HookInput};
use crate::client;

fn read_input(adapter: &dyn AgentAdapter) -> Result<HookInput, Box<dyn std::error::Error>> {
    let mut raw = String::new();
    std::io::stdin().read_to_string(&mut raw)?;
    Ok(adapter.parse_hook(&raw)?)
}

/// Returns the composed instructions on every `session-start` (Claude Code
/// adds SessionStart stdout to the session's context, including the times it
/// re-fires after compaction) — `None` for every other event. It registers
/// nothing: the daemon creates a session when its first `canvas post`
/// arrives, so a session that never posts never shows up in Canvas.
pub fn run(
    event: &str,
    adapter: &dyn AgentAdapter,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    match event {
        "session-start" => {
            // Unreadable stdin still gets the built-in and the person's
            // layers, read against the hook's own directory.
            let cwd = read_input(adapter)
                .ok()
                .map(|input| PathBuf::from(input.cwd))
                .or_else(|| std::env::current_dir().ok());
            let data_dir = canvas_core::paths::data_dir();
            let composed = instructions::compose(
                Kind::Instructions,
                data_dir.as_deref(),
                cwd.as_deref(),
                &Overrides::default(),
            );
            Ok(Some(composed.text))
        }
        "session-end" => {
            let input = read_input(adapter)?;
            client::end_session(&input.session_id)?;
            Ok(None)
        }
        // Prints a one-line reminder (UserPromptSubmit stdout joins the
        // prompt's context) when the previous turn had something to show and
        // no post; prints nothing otherwise.
        "prompt" => {
            let input = read_input(adapter)?;
            let transcript = std::fs::read_to_string(&input.transcript_path)?;
            let data_dir = canvas_core::paths::data_dir();
            let cwd = PathBuf::from(&input.cwd);
            let (reminders, fallback) = instructions::reminders(data_dir.as_deref(), Some(&cwd));
            if let Some(why) = fallback {
                canvas_core::log::warn(
                    "reminders fell back to the built-in",
                    &[("cwd", &input.cwd), ("error", &why)],
                );
            }
            Ok(crate::stop::reason(adapter, &transcript, &reminders))
        }
        other => Err(format!("unknown hook event: {other}").into()),
    }
}
