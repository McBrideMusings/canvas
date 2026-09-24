//! Reads a Claude Code transcript (JSONL at `transcript_path`) and turns the
//! entries since the last real user prompt into the `extract` module's input
//! shape.

use crate::extract::TranscriptEntry;

/// Read the JSONL file at `path` and return the `TranscriptEntry` list for
/// everything after the last real user prompt (a `type: "user"` line whose
/// content is plain text or a pasted attachment, not a tool-result carrier).
/// Any parse failure on an individual line is skipped rather than fatal —
/// a transcript can carry lines this CLI doesn't need to understand.
pub fn entries_since_last_prompt(path: &str) -> std::io::Result<Vec<TranscriptEntry>> {
    let raw = std::fs::read_to_string(path)?;
    let lines: Vec<serde_json::Value> = raw
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();

    let last_prompt_idx = lines.iter().rposition(is_real_user_prompt);
    let start = match last_prompt_idx {
        Some(i) => i + 1,
        None => 0,
    };

    Ok(lines[start..].iter().filter_map(line_to_entry).collect())
}

fn is_real_user_prompt(line: &serde_json::Value) -> bool {
    if line.get("type").and_then(|t| t.as_str()) != Some("user") {
        return false;
    }
    let content = line.pointer("/message/content");
    match content {
        Some(serde_json::Value::String(_)) => true,
        Some(serde_json::Value::Array(items)) => {
            // A content array that is entirely tool_result blocks is a
            // tool-result carrier, not something the user typed.
            !items.is_empty()
                && !items
                    .iter()
                    .all(|item| item.get("type").and_then(|t| t.as_str()) == Some("tool_result"))
        }
        _ => false,
    }
}

fn line_to_entry(line: &serde_json::Value) -> Option<TranscriptEntry> {
    let content = line.pointer("/message/content")?.as_array()?;
    let mut entry = TranscriptEntry::default();

    for item in content {
        match item.get("type").and_then(|t| t.as_str()) {
            Some("text") => {
                if let Some(t) = item.get("text").and_then(|v| v.as_str()) {
                    entry.assistant_text.push(t.to_string());
                }
            }
            Some("tool_use") => {
                if let Some(input) = item.get("input") {
                    entry.tool_inputs.push(input.clone());
                }
            }
            Some("tool_result") => {
                if let Some(c) = item.get("content") {
                    entry.tool_outputs.push(c.clone());
                }
            }
            _ => {}
        }
    }

    if entry.assistant_text.is_empty() && entry.tool_inputs.is_empty() && entry.tool_outputs.is_empty() {
        None
    } else {
        Some(entry)
    }
}
