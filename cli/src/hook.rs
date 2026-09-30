//! Dispatches one Claude Code hook invocation: reads the hook JSON Claude
//! Code writes to stdin, and calls canvasd. `session-start` only prints the guidance
//! block and registers nothing; every other
//! error — bad stdin, no canvasd listening, an unrecognised event — is
//! returned as `Err` so `main` can swallow it and exit 0 silently. A hook
//! must never slow or break the Claude session it's attached to.

use std::io::Read;

use crate::agent::{AgentAdapter, HookInput};
use crate::client;

fn read_input(adapter: &dyn AgentAdapter) -> Result<HookInput, Box<dyn std::error::Error>> {
    let mut raw = String::new();
    std::io::stdin().read_to_string(&mut raw)?;
    Ok(adapter.parse_hook(&raw)?)
}

/// Returns the guidance block on every `session-start` (Claude Code adds
/// SessionStart stdout to the session's context, including the times it
/// re-fires after compaction) — `None` for every other event. It registers
/// nothing: the daemon creates a session when its first `canvas post`
/// arrives, so a session that never posts never shows up in Canvas. The text
/// is whichever `posting-guidance` profile (repo, else global) the settings
/// page has assigned for this `cwd`, falling back to the compiled-in default
/// on any fetch failure or when nothing is assigned.
pub fn run(
    event: &str,
    adapter: &dyn AgentAdapter,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    match event {
        "session-start" => {
            let text = match read_input(adapter) {
                Ok(input) => {
                    client::fetch_profile_text(canvasd::profiles::KIND_POSTING_GUIDANCE, &input.cwd)
                        .unwrap_or_else(|| crate::guidance::TEXT.to_string())
                }
                Err(_) => crate::guidance::TEXT.to_string(),
            };
            Ok(Some(text))
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
            // The profile in effect for this directory; the compiled-in
            // default when nothing is assigned, canvasd is unreachable, or
            // the text no longer parses.
            let builtin = || {
                canvasd::stop_triggers::parse(
                    canvasd::profiles::builtin_default(canvasd::profiles::KIND_STOP_TRIGGERS)
                        .unwrap_or_default(),
                )
            };
            let triggers =
                client::fetch_profile_text(canvasd::profiles::KIND_STOP_TRIGGERS, &input.cwd)
                    .and_then(|text| canvasd::stop_triggers::parse(&text).ok())
                    .map_or_else(builtin, Ok)?;
            Ok(crate::stop::reason(adapter, &transcript, &triggers))
        }
        other => Err(format!("unknown hook event: {other}").into()),
    }
}
