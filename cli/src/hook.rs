//! Dispatches one Claude Code hook invocation: reads the hook JSON Claude
//! Code writes to stdin, and calls canvasd. `session-start` registration is
//! best-effort and never blocks the guidance block it returns; every other
//! error — bad stdin, no canvasd listening, an unrecognised event — is
//! returned as `Err` so `main` can swallow it and exit 0 silently. A hook
//! must never slow or break the Claude session it's attached to.

use std::io::Read;

use crate::client;

#[derive(Debug, serde::Deserialize)]
struct HookInput {
    session_id: String,
    #[serde(default)]
    cwd: String,
}

fn read_input() -> Result<HookInput, Box<dyn std::error::Error>> {
    let mut raw = String::new();
    std::io::stdin().read_to_string(&mut raw)?;
    Ok(serde_json::from_str(&raw)?)
}

/// Returns the guidance block on every `session-start` (Claude Code adds
/// SessionStart stdout to the session's context, including the times it
/// re-fires after compaction) — `None` for every other event. Registering
/// the session with canvasd is best-effort: posting only needs a running
/// canvasd, not a registered session (`canvas post` creates one server-side
/// if it's missing), so a session started while canvasd is unreachable —
/// e.g. mid-restart during `admin deploy canvas` — still gets its guidance
/// instead of losing it to a swallowed registration error. The text itself
/// is whichever `posting-guidance` profile (repo, else global) the settings
/// page has assigned for this `cwd`, falling back to the compiled-in default
/// on any fetch failure or when nothing is assigned.
pub fn run(event: &str) -> Result<Option<String>, Box<dyn std::error::Error>> {
    match event {
        "session-start" => {
            let text = match read_input() {
                Ok(input) => {
                    let _ = client::upsert_session(&input.session_id, &input.cwd);
                    client::fetch_profile(canvasd::profiles::KIND_POSTING_GUIDANCE, &input.cwd)
                        .and_then(|p| p.effective_text().map(str::to_string))
                        .unwrap_or_else(|| crate::guidance::TEXT.to_string())
                }
                Err(_) => crate::guidance::TEXT.to_string(),
            };
            Ok(Some(text))
        }
        "session-end" => {
            let input = read_input()?;
            client::end_session(&input.session_id)?;
            Ok(None)
        }
        other => Err(format!("unknown hook event: {other}").into()),
    }
}
