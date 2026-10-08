//! The Codex integration: Canvas's hook groups inside `~/.codex/hooks.json`
//! (`$CODEX_HOME` when set), beside whatever other tools keep there.
//!
//! Install touches only the groups whose command is Canvas's own and leaves
//! every other entry, and the file's key order, as it found them. A marker
//! file, `canvas-hooks-version`, records which `canvas` wrote the groups.
//!
//! Codex refuses a hook it has not reviewed: each trusted hook has a
//! `[hooks.state."<hooks.json>:<event>:<group>:<handler>"]` table with a
//! `trusted_hash` in `config.toml`. Only Codex computes that hash (its TUI
//! shows "Hooks need review" at the next launch), so install cannot grant
//! trust; `status` reports `NeedsReview` until those tables exist.
//!
//! Codex's sandbox (`read-only` and `workspace-write` alike) refuses the
//! connect to canvasd's Unix socket, so install also writes
//! `rules/canvas.rules`, a file Canvas owns whole: a `prefix_rule` that lets
//! the `canvas` subcommands which only talk to canvasd run outside the
//! sandbox. Codex matches a rule only against a plain command, so a post
//! through a pipe or heredoc, or by the binary's full path, stays sandboxed.
//! `export` and `snapshot` are left out because they write to any path.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use super::Status;

/// Codex event name, the snake_case name in trust keys, `canvas hook` event.
const EVENTS: [(&str, &str, &str); 3] = [
    ("SessionStart", "session_start", "session-start"),
    ("UserPromptSubmit", "user_prompt_submit", "prompt"),
    ("SessionEnd", "session_end", "session-end"),
];

const MARKER: &str = "canvas-hooks-version";
const RULES: &str = "rules/canvas.rules";
const RULES_TEXT: &str = r#"# Written by `canvas integrations install codex`; it replaces this file.
# Codex's sandbox blocks canvasd's Unix socket and writes to Canvas's data folder, so
# these subcommands, which touch only those, run outside it.
prefix_rule(
    pattern = ["canvas", ["post", "data", "focus", "wait", "replies", "card", "theme", "artifact", "instructions"]],
    decision = "allow",
    justification = "Canvas reaches canvasd over a Unix socket the sandbox blocks",
    match = ["canvas post plan.html", "canvas artifact put art-0123456789 dir"],
    not_match = ["canvas export --all", "canvas snapshot c1 out.png", "canvas integrations install codex"],
)
"#;
const VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn status() -> Result<Status, String> {
    status_in(&home()?)
}

pub fn install() -> Result<(), String> {
    install_in(&home()?)
}

fn home() -> Result<PathBuf, String> {
    if let Some(dir) = std::env::var_os("CODEX_HOME") {
        return Ok(PathBuf::from(dir));
    }
    let home = std::env::var_os("HOME").ok_or("HOME is not set")?;
    Ok(PathBuf::from(home).join(".codex"))
}

fn command(sub: &str) -> String {
    format!("\"$HOME/.local/bin/canvas\" hook {sub} --agent codex")
}

fn group(sub: &str) -> Value {
    json!({ "hooks": [{ "type": "command", "command": command(sub), "timeout": 5 }] })
}

fn is_canvas_group(group: &Value) -> bool {
    group["hooks"].as_array().is_some_and(|hooks| {
        hooks.iter().any(|h| {
            h["command"]
                .as_str()
                .is_some_and(|c| c.contains("canvas\" hook") && c.contains("--agent codex"))
        })
    })
}

fn read_hooks(path: &Path) -> Result<Option<Value>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("could not read {}: {e}", path.display())),
    };
    serde_json::from_str(&text)
        .map(Some)
        .map_err(|e| format!("could not parse {}: {e}", path.display()))
}

/// Puts Canvas's group for each event in place: where Canvas's group already
/// is, else at the end of the event's list, so no other tool's group changes
/// position (trust keys carry the position).
fn merge(doc: &mut Value) -> Result<(), String> {
    let root = doc
        .as_object_mut()
        .ok_or("hooks.json is not a JSON object")?;
    let hooks = root
        .entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or("hooks.json \"hooks\" is not an object")?;
    for (event, _, sub) in EVENTS {
        let groups = hooks
            .entry(event)
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .ok_or_else(|| format!("hooks.json \"{event}\" is not an array"))?;
        let at = groups.iter().position(is_canvas_group);
        groups.retain(|g| !is_canvas_group(g));
        let at = at.unwrap_or(groups.len()).min(groups.len());
        groups.insert(at, group(sub));
    }
    Ok(())
}

fn install_in(home: &Path) -> Result<(), String> {
    std::fs::create_dir_all(home)
        .map_err(|e| format!("could not create {}: {e}", home.display()))?;
    let path = home.join("hooks.json");
    let current = read_hooks(&path)?;
    let mut doc = current.clone().unwrap_or_else(|| json!({}));
    merge(&mut doc)?;
    if current.as_ref() != Some(&doc) {
        let mut text = serde_json::to_string_pretty(&doc).map_err(|e| e.to_string())?;
        text.push('\n');
        write_atomic(&path, &text)?;
    }
    let rules = home.join(RULES);
    if std::fs::read_to_string(&rules).ok().as_deref() != Some(RULES_TEXT) {
        std::fs::create_dir_all(home.join("rules"))
            .map_err(|e| format!("could not create {}: {e}", home.join("rules").display()))?;
        write_atomic(&rules, RULES_TEXT)?;
    }
    let marker = home.join(MARKER);
    if std::fs::read_to_string(&marker).ok().as_deref() != Some(VERSION) {
        write_atomic(&marker, VERSION)?;
    }
    Ok(())
}

fn write_atomic(path: &Path, text: &str) -> Result<(), String> {
    // A symlinked file (a dotfiles repo) keeps its link: replace the target.
    let path = &path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let tmp = path.with_extension("canvas-tmp");
    std::fs::write(&tmp, text).map_err(|e| format!("could not write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("could not replace {}: {e}", path.display()))
}

fn status_in(home: &Path) -> Result<Status, String> {
    let path = home.join("hooks.json");
    let Some(doc) = read_hooks(&path)? else {
        return Ok(Status::NotInstalled);
    };
    let mut present = 0;
    let mut slots = Vec::new();
    let mut exact = true;
    for (event, snake, sub) in EVENTS {
        let groups = doc["hooks"][event]
            .as_array()
            .map_or(&[][..], Vec::as_slice);
        match groups.iter().position(is_canvas_group) {
            Some(i) => {
                present += 1;
                exact &= groups[i] == group(sub);
                slots.push(format!("{}:{snake}:{i}:0", path.display()));
            }
            None => exact = false,
        }
    }
    if present == 0 {
        return Ok(Status::NotInstalled);
    }
    let marker = std::fs::read_to_string(home.join(MARKER)).ok();
    let rules = std::fs::read_to_string(home.join(RULES)).ok();
    if !exact || marker.as_deref() != Some(VERSION) || rules.as_deref() != Some(RULES_TEXT) {
        return Ok(Status::OutOfDate);
    }
    let config = std::fs::read_to_string(home.join("config.toml")).unwrap_or_default();
    let trusted = slots
        .iter()
        .all(|slot| config.contains(&format!("[hooks.state.\"{slot}\"]")));
    Ok(if trusted {
        Status::Current
    } else {
        Status::NeedsReview
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const OTHERS: &str = r#"{
  "hooks": {
    "Stop": [{ "hooks": [{ "type": "command", "command": "other stop" }] }],
    "SessionStart": [
      { "hooks": [{ "type": "command", "command": "other start" }] },
      { "matcher": "*", "hooks": [{ "type": "command", "command": "another" }] }
    ]
  }
}
"#;

    fn temp_home(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("canvas-codex-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn install_twice_is_byte_identical_and_keeps_other_hooks() {
        let home = temp_home("idem");
        std::fs::write(home.join("hooks.json"), OTHERS).unwrap();
        install_in(&home).unwrap();
        let first = std::fs::read(home.join("hooks.json")).unwrap();
        install_in(&home).unwrap();
        assert_eq!(first, std::fs::read(home.join("hooks.json")).unwrap());

        let doc: Value = serde_json::from_slice(&first).unwrap();
        let start = doc["hooks"]["SessionStart"].as_array().unwrap();
        assert_eq!(start[0]["hooks"][0]["command"], "other start");
        assert_eq!(start[1]["hooks"][0]["command"], "another");
        assert!(is_canvas_group(&start[2]));
        assert_eq!(doc["hooks"]["Stop"].as_array().unwrap().len(), 1);
        assert_eq!(doc["hooks"]["SessionEnd"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn status_follows_install_marker_and_trust() {
        let home = temp_home("status");
        assert_eq!(status_in(&home).unwrap(), Status::NotInstalled);
        std::fs::write(home.join("hooks.json"), OTHERS).unwrap();
        assert_eq!(status_in(&home).unwrap(), Status::NotInstalled);

        install_in(&home).unwrap();
        assert_eq!(status_in(&home).unwrap(), Status::NeedsReview);

        let path = home.join("hooks.json");
        let config: String = [
            ("session_start", 2),
            ("user_prompt_submit", 0),
            ("session_end", 0),
        ]
        .iter()
        .map(|(e, i)| {
            format!(
                "[hooks.state.\"{}:{e}:{i}:0\"]\ntrusted_hash = \"sha256:x\"\n",
                path.display()
            )
        })
        .collect();
        std::fs::write(home.join("config.toml"), config).unwrap();
        assert_eq!(status_in(&home).unwrap(), Status::Current);

        std::fs::write(home.join(MARKER), "0.0.0").unwrap();
        assert_eq!(status_in(&home).unwrap(), Status::OutOfDate);
        install_in(&home).unwrap();
        assert_eq!(status_in(&home).unwrap(), Status::Current);

        std::fs::remove_file(home.join(RULES)).unwrap();
        assert_eq!(status_in(&home).unwrap(), Status::OutOfDate);
        install_in(&home).unwrap();
        assert_eq!(
            std::fs::read_to_string(home.join(RULES)).unwrap(),
            RULES_TEXT
        );
        assert_eq!(status_in(&home).unwrap(), Status::Current);
    }
}
