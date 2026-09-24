//! Pure extraction of links, paths and images from the transcript entries
//! produced since the last real user prompt. No I/O happens in this module —
//! every filesystem check the caller wants (e.g. "does this path exist") is
//! passed in, which is what makes it the test seam.

use std::collections::BTreeSet;
use std::path::Path;

pub const IMAGE_EXTS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "svg"];

/// One assistant turn's worth of material to scan: the text the assistant
/// wrote, plus the raw JSON inputs and outputs of every tool call it made.
#[derive(Debug, Clone, Default)]
pub struct TranscriptEntry {
    pub assistant_text: Vec<String>,
    pub tool_inputs: Vec<serde_json::Value>,
    pub tool_outputs: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct Extracted {
    pub links: Vec<String>,
    pub paths: Vec<String>,
    pub images: Vec<String>,
}

/// Extract links, existing absolute paths, and image files from a turn's
/// entries. Links and non-image paths only come from assistant text; images
/// are also pulled from tool-call inputs and outputs (a screenshot a tool
/// saved but never mentioned in prose still needs to show up).
pub fn extract(entries: &[TranscriptEntry]) -> Extracted {
    let mut links: BTreeSet<String> = BTreeSet::new();
    let mut paths: BTreeSet<String> = BTreeSet::new();
    let mut images: BTreeSet<String> = BTreeSet::new();

    for entry in entries {
        for text in &entry.assistant_text {
            for url in find_urls(text) {
                links.insert(url);
            }
            for candidate in find_path_tokens(text) {
                if !is_absolute_and_exists(&candidate) {
                    continue;
                }
                if is_image_path(&candidate) {
                    images.insert(candidate);
                } else {
                    paths.insert(candidate);
                }
            }
        }

        for value in entry.tool_inputs.iter().chain(entry.tool_outputs.iter()) {
            for s in strings_in(value) {
                for candidate in find_path_tokens(&s) {
                    if is_image_path(&candidate) && is_absolute_and_exists(&candidate) {
                        images.insert(candidate);
                    }
                }
            }
        }
    }

    Extracted {
        links: links.into_iter().collect(),
        paths: paths.into_iter().collect(),
        images: images.into_iter().collect(),
    }
}

fn is_absolute_and_exists(candidate: &str) -> bool {
    let p = Path::new(candidate);
    p.is_absolute() && p.exists()
}

fn is_image_path(candidate: &str) -> bool {
    Path::new(candidate)
        .extension()
        .map(|e| IMAGE_EXTS.contains(&e.to_string_lossy().to_lowercase().as_str()))
        .unwrap_or(false)
}

/// Split text into candidate tokens on whitespace and the punctuation that
/// commonly wraps a URL or path in prose or markdown (parens, brackets,
/// quotes, backticks, angle brackets), so `` `/tmp/x.png` `` or
/// `(https://example.com)` yield a clean token.
fn tokenize(text: &str) -> Vec<&str> {
    text.split(|c: char| c.is_whitespace() || "()[]{}<>\"'`,;".contains(c))
        .filter(|t| !t.is_empty())
        .collect()
}

fn find_urls(text: &str) -> Vec<String> {
    tokenize(text)
        .into_iter()
        .filter(|t| t.starts_with("http://") || t.starts_with("https://"))
        .map(|t| t.trim_end_matches(['.', ':']).to_string())
        .collect()
}

fn find_path_tokens(text: &str) -> Vec<String> {
    tokenize(text)
        .into_iter()
        .filter(|t| t.starts_with('/'))
        .map(|t| t.trim_end_matches(['.', ':']).to_string())
        .filter(|t| t.len() > 1)
        .collect()
}

/// Walk a JSON value and collect every string leaf, depth-first.
fn strings_in(value: &serde_json::Value) -> Vec<String> {
    let mut out = Vec::new();
    collect_strings(value, &mut out);
    out
}

fn collect_strings(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::String(s) => out.push(s.clone()),
        serde_json::Value::Array(items) => {
            for item in items {
                collect_strings(item, out);
            }
        }
        serde_json::Value::Object(map) => {
            for v in map.values() {
                collect_strings(v, out);
            }
        }
        _ => {}
    }
}
