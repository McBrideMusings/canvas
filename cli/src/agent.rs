//! Everything that differs between coding agents, behind one trait: which
//! environment variable names the session, what a hook writes to stdin, and
//! how the agent's transcript reads as a finished turn. `canvas post` and the
//! hooks go through an `AgentAdapter` rather than reading one agent's formats
//! directly. `canvas-core`'s `Agent` stays the plain value stored on a
//! session; it carries no behavior.

use canvas_core::Agent;
use serde_json::Value;

/// What a hook reads from stdin.
#[derive(Debug, serde::Deserialize)]
pub struct HookInput {
    pub session_id: String,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub transcript_path: String,
}

/// The facts about one finished turn that the stop triggers look at. A turn
/// is the stretch of a transcript after a real user prompt.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Turn {
    /// A `canvas post` ran during the turn.
    pub posted: bool,
    /// Paths the agent read.
    pub read_paths: Vec<String>,
    /// Paths the agent created or changed.
    pub written_paths: Vec<String>,
    /// The last text the agent said.
    pub last_text: String,
}

pub trait AgentAdapter: Sync {
    /// The value stored on each session this agent starts.
    fn agent(&self) -> Agent;
    /// Name an `--agent` flag selects this adapter by.
    fn name(&self) -> &'static str;
    /// The environment variable the agent sets in its tool shells to the
    /// session id.
    fn session_env(&self) -> &'static str;
    /// Parses the JSON a hook of this agent receives on stdin.
    fn parse_hook(&self, stdin: &str) -> Result<HookInput, serde_json::Error>;
    /// The last finished turn in this agent's transcript text; an empty
    /// `Turn` when there is none.
    fn last_turn(&self, transcript: &str) -> Turn;
}

pub struct ClaudeCode;

impl AgentAdapter for ClaudeCode {
    fn agent(&self) -> Agent {
        Agent::ClaudeCode
    }

    fn name(&self) -> &'static str {
        "claude-code"
    }

    /// Claude Code sets this in every Bash tool shell, equal to the
    /// `session_id` its hooks get on stdin. There is no other reliable way for
    /// a plain shell command to learn which session it's running in.
    fn session_env(&self) -> &'static str {
        "CLAUDE_CODE_SESSION_ID"
    }

    fn parse_hook(&self, stdin: &str) -> Result<HookInput, serde_json::Error> {
        serde_json::from_str(stdin)
    }

    /// Reads Claude Code's JSONL. A turn is the stretch after a real user
    /// prompt (not a tool result, not a hook's `isMeta` feedback). Only the
    /// turn just before the new prompt counts: Claude Code may or may not have
    /// written the new prompt to the transcript when the hook runs, and a
    /// trailing prompt with no turn after it is skipped.
    fn last_turn(&self, transcript: &str) -> Turn {
        let lines: Vec<Value> = transcript
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect();
        let end = match lines.iter().rposition(is_prompt) {
            Some(i) if i + 1 == lines.len() => i,
            _ => lines.len(),
        };
        let start = lines[..end]
            .iter()
            .rposition(is_prompt)
            .map_or(0, |i| i + 1);

        let mut turn = Turn::default();
        for line in lines[start..end]
            .iter()
            .filter(|l| l["type"] == "assistant")
        {
            let Some(blocks) = line["message"]["content"].as_array() else {
                continue;
            };
            for block in blocks {
                match block["type"].as_str() {
                    Some("text") => {
                        turn.last_text = block["text"].as_str().unwrap_or("").to_string()
                    }
                    Some("tool_use") => {
                        let input = &block["input"];
                        match block["name"].as_str() {
                            Some("Bash") => {
                                turn.posted |= input["command"]
                                    .as_str()
                                    .is_some_and(crate::stop::runs_canvas_post);
                            }
                            Some("Read") => {
                                turn.read_paths
                                    .push(input["file_path"].as_str().unwrap_or("").to_string());
                            }
                            Some("Write" | "Edit" | "NotebookEdit") => {
                                let path = input["file_path"]
                                    .as_str()
                                    .or_else(|| input["notebook_path"].as_str())
                                    .unwrap_or("");
                                turn.written_paths.push(path.to_string());
                            }
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }
        }
        turn
    }
}

/// A real prompt: a user line, not `isMeta`, with text or an image in it
/// (a line holding only tool results is the agent's own tool loop).
fn is_prompt(line: &Value) -> bool {
    if line["type"] != "user" || line["isMeta"] == true {
        return false;
    }
    match &line["message"]["content"] {
        Value::String(_) => true,
        Value::Array(blocks) => blocks
            .iter()
            .any(|b| matches!(b["type"].as_str(), Some("text" | "image"))),
        _ => false,
    }
}

const ADAPTERS: [&dyn AgentAdapter; 1] = [&ClaudeCode];

/// The adapter an `--agent <name>` flag names.
pub fn by_name(name: &str) -> Option<&'static dyn AgentAdapter> {
    ADAPTERS.iter().copied().find(|a| a.name() == name)
}

/// The adapter whose session env var is set in this process, with the session
/// id it holds.
pub fn from_env() -> Option<(&'static dyn AgentAdapter, String)> {
    ADAPTERS
        .iter()
        .copied()
        .find_map(|a| std::env::var(a.session_env()).ok().map(|id| (a, id)))
}

/// Every session env var `canvas post` looks for.
pub fn session_envs() -> Vec<&'static str> {
    ADAPTERS.iter().map(|a| a.session_env()).collect()
}

/// What a hook runs as when no `--agent` flag is given.
pub fn default_adapter() -> &'static dyn AgentAdapter {
    &ClaudeCode
}
