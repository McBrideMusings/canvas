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
    #[serde(default, deserialize_with = "null_as_empty")]
    pub cwd: String,
    /// Codex sends `null` when it keeps no transcript.
    #[serde(default, deserialize_with = "null_as_empty")]
    pub transcript_path: String,
}

fn null_as_empty<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    use serde::Deserialize;
    Ok(Option::<String>::deserialize(d)?.unwrap_or_default())
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

pub struct Codex;

impl AgentAdapter for Codex {
    fn agent(&self) -> Agent {
        Agent::Codex
    }

    fn name(&self) -> &'static str {
        "codex"
    }

    /// Codex sets this in every tool shell, equal to the `session_id` its
    /// hooks get on stdin and to the `id` in the rollout's `session_meta`.
    fn session_env(&self) -> &'static str {
        "CODEX_THREAD_ID"
    }

    /// Codex hooks write the same `session_id`, `cwd` and `transcript_path`
    /// keys Claude Code's do, beside `hook_event_name`, `model` and others
    /// this ignores.
    fn parse_hook(&self, stdin: &str) -> Result<HookInput, serde_json::Error> {
        serde_json::from_str(stdin)
    }

    /// Reads a Codex rollout. A turn is the stretch after a real user prompt:
    /// an `event_msg` of type `user_message` or an `item_completed` whose item
    /// is a `UserMessage`. The `response_item` user messages are injected
    /// context, not prompts. A final prompt with no agent record after it (the
    /// one Codex may already have written when the hook runs) is skipped.
    /// Shell commands come as `function_call`s or as `tools.exec_command({..})`
    /// calls inside an `exec` custom tool call; file changes are `apply_patch`
    /// calls. Codex reads text files through the shell, so `read_paths` holds
    /// `view_image` paths only.
    fn last_turn(&self, transcript: &str) -> Turn {
        let mut turns: Vec<Vec<Value>> = vec![Vec::new()];
        for line in transcript
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        {
            if is_codex_prompt(&line) {
                turns.push(Vec::new());
            } else if let Some(turn) = turns.last_mut() {
                turn.push(line);
            }
        }
        let mut turn = Turn::default();
        if turns.last().is_some_and(|t| !t.iter().any(is_agent_record)) {
            turns.pop();
        }
        let Some(lines) = turns.last() else {
            return turn;
        };
        for payload in lines
            .iter()
            .filter(|l| l["type"] == "response_item")
            .map(|l| &l["payload"])
        {
            match payload["type"].as_str() {
                Some("message") if payload["role"] == "assistant" => {
                    if let Some(text) = payload["content"]
                        .as_array()
                        .and_then(|c| c.iter().rev().find(|b| b["type"] == "output_text"))
                    {
                        turn.last_text = text["text"].as_str().unwrap_or("").to_string();
                    }
                }
                Some("function_call") => {
                    let args: Value = payload["arguments"]
                        .as_str()
                        .and_then(|a| serde_json::from_str(a).ok())
                        .unwrap_or_default();
                    match payload["name"].as_str() {
                        Some("exec_command") => turn.note_command(args["cmd"].as_str()),
                        Some("shell_command") => turn.note_command(args["command"].as_str()),
                        Some("shell") => turn.note_command(
                            args["command"]
                                .as_array()
                                .and_then(|argv| argv.last())
                                .and_then(Value::as_str),
                        ),
                        Some("view_image") => {
                            turn.read_paths
                                .push(args["path"].as_str().unwrap_or("").to_string());
                        }
                        _ => {}
                    }
                }
                Some("custom_tool_call") => {
                    let input = payload["input"].as_str().unwrap_or("");
                    match payload["name"].as_str() {
                        Some("exec") => {
                            for args in exec_command_calls(input) {
                                turn.note_command(args["cmd"].as_str());
                            }
                        }
                        Some("apply_patch") => turn.written_paths.extend(patched_paths(input)),
                        _ => {}
                    }
                }
                _ => {}
            }
        }
        turn
    }
}

impl Turn {
    fn note_command(&mut self, cmd: Option<&str>) {
        self.posted |= cmd.is_some_and(crate::stop::runs_canvas_post);
    }
}

fn is_codex_prompt(line: &Value) -> bool {
    let payload = &line["payload"];
    line["type"] == "event_msg"
        && (payload["type"] == "user_message"
            || (payload["type"] == "item_completed" && payload["item"]["type"] == "UserMessage"))
}

/// A record the agent wrote during a turn, as opposed to the bookkeeping
/// (`task_started`, `turn_context`, `token_count`) around it.
fn is_agent_record(line: &Value) -> bool {
    let payload = &line["payload"];
    line["type"] == "response_item"
        && (matches!(
            payload["type"].as_str(),
            Some("function_call" | "custom_tool_call")
        ) || (payload["type"] == "message" && payload["role"] == "assistant"))
}

/// The argument object of every `tools.exec_command({..})` call in the
/// JavaScript of an `exec` custom tool call.
fn exec_command_calls(script: &str) -> Vec<Value> {
    const CALL: &str = "tools.exec_command(";
    script
        .match_indices(CALL)
        .filter_map(|(at, _)| {
            serde_json::Deserializer::from_str(&script[at + CALL.len()..])
                .into_iter::<Value>()
                .next()?
                .ok()
        })
        .collect()
}

/// The files an `apply_patch` body adds, updates, deletes or moves to.
fn patched_paths(patch: &str) -> Vec<String> {
    const HEADERS: [&str; 4] = [
        "*** Add File: ",
        "*** Update File: ",
        "*** Delete File: ",
        "*** Move to: ",
    ];
    patch
        .lines()
        .filter_map(|l| HEADERS.iter().find_map(|h| l.strip_prefix(h)))
        .map(|p| p.trim().to_string())
        .collect()
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

const ADAPTERS: [&dyn AgentAdapter; 2] = [&ClaudeCode, &Codex];

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

#[cfg(test)]
mod tests {
    use super::*;

    const ROLLOUT: &str = include_str!("../tests/fixtures/codex-rollout.jsonl");

    fn fixed_turn() -> Turn {
        Turn {
            posted: false,
            read_paths: vec!["/tmp/shot.png".into()],
            written_paths: vec!["/work/app/src/main.rs".into()],
            last_text: "Fixed `main.rs`.\n\nLook for: a green `cargo test`.".into(),
        }
    }

    #[test]
    fn codex_fixture_yields_the_second_turn() {
        // Turn 2 has three shell commands (none a post), one image, one patched file;
        // turn 3's prompt has no agent record after it and is skipped.
        assert_eq!(Codex.last_turn(ROLLOUT), fixed_turn());
    }

    #[test]
    fn codex_turn_before_a_post_reports_the_post() {
        let first_turn: String = ROLLOUT
            .lines()
            .take_while(|l| !l.contains(r#""turn_id":"t2""#))
            .collect::<Vec<_>>()
            .join("\n");
        let turn = Codex.last_turn(&first_turn);
        assert!(turn.posted);
        assert_eq!(turn.last_text, "Posted the plan.");
        assert!(turn.written_paths.is_empty());
    }

    #[test]
    fn codex_reads_prompts_from_item_completed() {
        let rollout = [
            r#"{"type":"event_msg","payload":{"type":"item_completed","item":{"type":"UserMessage","content":[]}}}"#,
            r#"{"type":"response_item","payload":{"type":"function_call","name":"shell_command","arguments":"{\"command\":\"canvas post -\"}"}}"#,
            r#"{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"done"}]}}"#,
            r#"{"type":"event_msg","payload":{"type":"item_completed","item":{"type":"UserMessage","content":[]}}}"#,
        ]
        .join("\n");
        let turn = Codex.last_turn(&rollout);
        assert!(turn.posted);
        assert_eq!(turn.last_text, "done");
    }

    #[test]
    fn codex_with_no_turn_is_empty() {
        assert_eq!(Codex.last_turn(""), Turn::default());
    }

    #[test]
    fn codex_hook_input_matches_what_codex_writes() {
        let stdin = r#"{"session_id":"01a0f379","transcript_path":"/r.jsonl","cwd":"/work","hook_event_name":"SessionStart","model":"m","permission_mode":"bypassPermissions","source":"startup"}"#;
        let input = Codex.parse_hook(stdin).unwrap();
        assert_eq!(input.session_id, "01a0f379");
        assert_eq!(input.cwd, "/work");
        assert_eq!(input.transcript_path, "/r.jsonl");

        let no_transcript = Codex
            .parse_hook(r#"{"session_id":"s","transcript_path":null,"cwd":"/w"}"#)
            .unwrap();
        assert_eq!(no_transcript.transcript_path, "");
    }

    #[test]
    fn by_name_finds_each_adapter() {
        assert_eq!(by_name("codex").unwrap().agent(), Agent::Codex);
        assert_eq!(by_name("claude-code").unwrap().agent(), Agent::ClaudeCode);
        assert!(by_name("nope").is_none());
    }
}
