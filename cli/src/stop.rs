//! The `stop` hook: when a turn produced something Canvas exists to show and
//! no `canvas post` ran during it, block the stop once with a reminder. The
//! session-start guidance is read once, many tool calls before a turn ends;
//! this puts the check at the moment the agent would otherwise finish.
//!
//! The turn is the stretch of the transcript after the last real user prompt
//! (not a tool result, not a hook's `isMeta` feedback). A reply to the block
//! sets `stop_hook_active`, which the caller checks first, so the hook never
//! loops.

use canvasd::stop_triggers::StopTriggers;
use serde_json::Value;

const IMAGE_EXTENSIONS: [&str; 5] = [".png", ".jpg", ".jpeg", ".gif", ".webp"];

/// Returns the block reason when the last turn in `transcript` (Claude Code's
/// JSONL) has a trigger `cfg` enables and no `canvas post`; `None` otherwise.
pub fn reason(transcript: &str, cfg: &StopTriggers) -> Option<String> {
    if !cfg.enabled {
        return None;
    }
    let lines: Vec<Value> = transcript
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    let start = lines.iter().rposition(is_prompt).map_or(0, |i| i + 1);
    let turn = &lines[start..];

    let mut triggers: Vec<&str> = Vec::new();
    let mut posted = false;
    let mut last_text = String::new();

    for line in turn.iter().filter(|l| l["type"] == "assistant") {
        let Some(blocks) = line["message"]["content"].as_array() else {
            continue;
        };
        for block in blocks {
            match block["type"].as_str() {
                Some("text") => last_text = block["text"].as_str().unwrap_or("").to_string(),
                Some("tool_use") => {
                    let input = &block["input"];
                    match block["name"].as_str() {
                        Some("Bash") => {
                            posted |= input["command"].as_str().is_some_and(runs_canvas_post);
                        }
                        Some("Read") => {
                            let path = input["file_path"].as_str().unwrap_or("").to_lowercase();
                            if cfg.image && IMAGE_EXTENSIONS.iter().any(|e| path.ends_with(e)) {
                                triggers.push("looked at a screenshot or image");
                            }
                        }
                        Some("Write" | "Edit" | "NotebookEdit") => {
                            let path = input["file_path"]
                                .as_str()
                                .or_else(|| input["notebook_path"].as_str())
                                .unwrap_or("");
                            if cfg.file && !cfg.scratch.iter().any(|p| path.starts_with(p.as_str()))
                            {
                                triggers.push("created or changed a file");
                            }
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
    }
    if posted {
        return None;
    }

    if cfg.report && last_text.contains("Look for:") {
        triggers.push("ended with a closing report");
    }
    if cfg.verify && asks_for_verification(&last_text, &cfg.phrases) {
        triggers.push("asked the user to verify or try something");
    }
    if cfg.links.is_some_and(|n| link_count(&last_text) >= n) {
        triggers.push("listed several links or paths");
    }
    let long_block = cfg.long_block.filter(|&n| has_long_block(&last_text, n));
    if let Some(n) = long_block {
        triggers.push(match n {
            15 => "wrote a code block or table over 15 lines in chat",
            _ => "wrote a long code block or table in chat",
        });
    }
    triggers.sort_unstable();
    triggers.dedup();
    if triggers.is_empty() {
        return None;
    }

    Some(format!(
        "This turn {}, and no `canvas post` ran. Post it to Canvas now \
         (`canvas post`; images as `![](/abs/path.png)`, files as `[name](/abs/path)`), \
         then give your reply. If the post would only repeat the chat text, skip it.",
        triggers.join("; ")
    ))
}

/// True when a shell segment of `cmd` runs `canvas post` (bare or by path, after
/// optional `VAR=x` words). Segments end at `;`, `&`, `|` or a newline outside
/// quotes and `$(...)`, so the text in a commit message or a grep pattern does
/// not count. An unterminated quote leaves the rest as one segment.
fn runs_canvas_post(cmd: &str) -> bool {
    // Open contexts, innermost last: `'`, `"` or `(`.
    let mut open: Vec<u8> = Vec::new();
    let bytes = cmd.as_bytes();
    let mut start = 0;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        match open.last().copied() {
            Some(b'\'') => {
                if b == b'\'' {
                    open.pop();
                }
            }
            top => {
                let in_double = top == Some(b'"');
                if b == b'\\' {
                    i += 1;
                } else if in_double && b == b'"' {
                    open.pop();
                } else if b == b'$' && bytes.get(i + 1) == Some(&b'(') {
                    open.push(b'(');
                    i += 1;
                } else if in_double {
                } else if b == b'\'' || b == b'"' {
                    open.push(b);
                } else if b == b'(' {
                    open.push(b'(');
                } else if b == b')' {
                    open.pop();
                } else if open.is_empty() && matches!(b, b';' | b'&' | b'|' | b'\n') {
                    if segment_is_canvas_post(&cmd[start..i]) {
                        return true;
                    }
                    start = i + 1;
                }
            }
        }
        i += 1;
    }
    segment_is_canvas_post(&cmd[start.min(cmd.len())..])
}

fn segment_is_canvas_post(segment: &str) -> bool {
    let mut words = segment
        .split_whitespace()
        .skip_while(|w| w.split_once('=').is_some_and(|(k, _)| is_var_name(k)));
    let program = words.next().unwrap_or("");
    (program == "canvas" || program.ends_with("/canvas")) && words.next() == Some("post")
}

fn is_var_name(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
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

/// True when `text` contains one of `phrases` (lower-cased) — wording that
/// hands the user a check to do.
fn asks_for_verification(text: &str, phrases: &[String]) -> bool {
    let lower = text.to_lowercase();
    phrases.iter().any(|p| lower.contains(p.as_str()))
}

/// URLs plus Markdown links to local files in `text`.
fn link_count(text: &str) -> usize {
    text.matches("http://").count() + text.matches("https://").count() + text.matches("](/").count()
}

/// True when `text` holds a fenced block, or a run of table rows, longer than
/// `limit` lines.
fn has_long_block(text: &str, limit: usize) -> bool {
    let mut in_fence = false;
    let mut fence_lines = 0;
    let mut table_lines = 0;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            if in_fence && fence_lines > limit {
                return true;
            }
            in_fence = !in_fence;
            fence_lines = 0;
            table_lines = 0;
            continue;
        }
        if in_fence {
            fence_lines += 1;
        } else if line.trim_start().starts_with('|') {
            table_lines += 1;
            if table_lines > limit {
                return true;
            }
        } else {
            table_lines = 0;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> StopTriggers {
        canvasd::stop_triggers::parse(canvasd::profiles::builtin_default("stop-triggers").unwrap())
            .unwrap()
    }

    fn user(text: &str) -> String {
        serde_json::json!({"type":"user","message":{"content":text}}).to_string()
    }

    fn tool(name: &str, input: Value) -> String {
        serde_json::json!({"type":"assistant","message":{"content":[
            {"type":"tool_use","name":name,"input":input}]}})
        .to_string()
    }

    fn say(text: &str) -> String {
        serde_json::json!({"type":"assistant","message":{"content":[
            {"type":"text","text":text}]}})
        .to_string()
    }

    fn transcript(lines: &[String]) -> String {
        lines.join("\n")
    }

    #[test]
    fn edit_without_a_post_blocks() {
        let t = transcript(&[
            user("fix it"),
            tool("Edit", serde_json::json!({"file_path":"/repo/a.js"})),
            say("done"),
        ]);
        assert!(reason(&t, &cfg())
            .unwrap()
            .contains("created or changed a file"));
    }

    #[test]
    fn image_read_blocks() {
        let t = transcript(&[
            user("look"),
            tool(
                "Read",
                serde_json::json!({"file_path":"/private/tmp/x/Shot.PNG"}),
            ),
            say("it looks fine"),
        ]);
        assert!(reason(&t, &cfg()).unwrap().contains("screenshot"));
    }

    #[test]
    fn a_post_in_the_turn_suppresses_the_block() {
        let t = transcript(&[
            user("fix it"),
            tool("Edit", serde_json::json!({"file_path":"/repo/a.js"})),
            tool(
                "Bash",
                serde_json::json!({"command":"canvas post - <<'EOF'\nhi\nEOF"}),
            ),
            say("done"),
        ]);
        assert_eq!(reason(&t, &cfg()), None);
    }

    #[test]
    fn only_a_command_named_canvas_post_counts() {
        for yes in [
            "canvas post -",
            "cd x && canvas post -",
            "~/.local/bin/canvas post --update abc f.html",
            "FOO=1 canvas post -",
            "cat f | canvas post - --format html",
            "canvas post - <<'EOF'\nhi\nEOF",
        ] {
            assert!(runs_canvas_post(yes), "{yes}");
        }
        for no in [
            "git commit -m 'docs: canvas post'",
            "git commit -m \"$(cat <<'EOF'\ndocs\n\ncanvas post\nEOF\n)\"",
            "grep -r \"canvas post\" .",
            "echo \"a; canvas post\"",
            "echo 'x' && grep canvas post.txt",
            "canvas postmortem",
            "canvas wait abc",
        ] {
            assert!(!runs_canvas_post(no), "{no}");
        }
    }

    #[test]
    fn a_trigger_in_an_earlier_turn_does_not_count() {
        let t = transcript(&[
            user("fix it"),
            tool("Edit", serde_json::json!({"file_path":"/repo/a.js"})),
            say("done"),
            user("thanks, what does it do?"),
            say("it does x"),
        ]);
        assert_eq!(reason(&t, &cfg()), None);
    }

    #[test]
    fn hook_feedback_does_not_start_a_new_turn() {
        let feedback =
            serde_json::json!({"type":"user","isMeta":true,"message":{"content":"Stop hook feedback: x"}})
                .to_string();
        let t = transcript(&[
            user("fix it"),
            tool("Edit", serde_json::json!({"file_path":"/repo/a.js"})),
            say("done"),
            feedback,
            say("done again"),
        ]);
        assert!(reason(&t, &cfg()).is_some());
    }

    #[test]
    fn scratch_files_and_plain_answers_do_not_block() {
        let t = transcript(&[
            user("q"),
            tool(
                "Write",
                serde_json::json!({"file_path":"/private/tmp/claude/x/plan.md"}),
            ),
            say("short answer"),
        ]);
        assert_eq!(reason(&t, &cfg()), None);
    }

    #[test]
    fn closing_report_and_long_blocks_block() {
        let t = transcript(&[user("q"), say("**Run:**\n```\nx\n```\n**Look for:** y")]);
        assert!(reason(&t, &cfg()).unwrap().contains("closing report"));

        let long = format!("```\n{}```", "line\n".repeat(16));
        let t = transcript(&[user("q"), say(&long)]);
        assert!(reason(&t, &cfg()).unwrap().contains("over 15 lines"));

        let short = format!("```\n{}```", "line\n".repeat(15));
        assert_eq!(reason(&transcript(&[user("q"), say(&short)]), &cfg()), None);
    }

    #[test]
    fn verification_requests_and_several_links_block() {
        let t = transcript(&[user("q"), say("Please verify the header looks right.")]);
        assert!(reason(&t, &cfg()).unwrap().contains("verify"));

        let links = "See https://a.example, https://b.example and https://c.example";
        let t = transcript(&[user("q"), say(links)]);
        assert!(reason(&t, &cfg()).unwrap().contains("links"));

        let two = "See https://a.example and https://b.example";
        assert_eq!(reason(&transcript(&[user("q"), say(two)]), &cfg()), None);
    }

    #[test]
    fn the_profile_text_decides_what_triggers() {
        let t = transcript(&[
            user("q"),
            tool("Edit", serde_json::json!({"file_path":"/repo/a.js"})),
            say("done"),
        ]);
        let parse = |text| canvasd::stop_triggers::parse(text).unwrap();
        assert!(reason(&t, &parse("file")).is_some());
        assert_eq!(reason(&t, &parse("image")), None);
        assert_eq!(reason(&t, &parse("file\noff")), None);
        assert_eq!(reason(&t, &parse("file\nscratch /repo/")), None);
    }
}
