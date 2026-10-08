//! `canvas instructions` and the hooks that read the same layers: the
//! built-in text, the person's file in `CANVAS_DATA_DIR`, and the project's
//! file under `.canvas/` at the git root. Runs the real built binary with no
//! canvasd listening, since the hooks read files, not the daemon.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const BUILTIN: &str = include_str!("../../plugin/instructions.md");

fn canvas_bin() -> &'static str {
    env!("CARGO_BIN_EXE_canvas")
}

struct Dirs {
    base: PathBuf,
    data: PathBuf,
    repo: PathBuf,
    other_repo: PathBuf,
    outside: PathBuf,
}

impl Drop for Dirs {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

fn dirs(name: &str) -> Dirs {
    let base =
        std::env::temp_dir().join(format!("canvas-instructions-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let d = Dirs {
        data: base.join("data"),
        repo: base.join("repo"),
        other_repo: base.join("other"),
        outside: base.join("outside"),
        base,
    };
    std::fs::create_dir_all(&d.data).unwrap();
    std::fs::create_dir_all(d.repo.join(".git")).unwrap();
    std::fs::create_dir_all(d.repo.join(".canvas")).unwrap();
    std::fs::create_dir_all(d.other_repo.join(".git")).unwrap();
    std::fs::create_dir_all(&d.outside).unwrap();
    d
}

fn canvas(d: &Dirs, args: &[&str], stdin: &[u8]) -> Output {
    let mut child = Command::new(canvas_bin())
        .args(args)
        .current_dir(&d.outside)
        .env("CANVAS_DATA_DIR", &d.data)
        // Nothing listens here: the hooks must not need canvasd.
        .env("CANVAS_SOCKET", d.base.join("no-canvasd.sock"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to run canvas");
    child.stdin.take().unwrap().write_all(stdin).unwrap();
    child.wait_with_output().unwrap()
}

fn session_start(d: &Dirs, cwd: &Path) -> String {
    let input = serde_json::json!({"session_id": "s", "cwd": cwd});
    let out = canvas(d, &["hook", "session-start"], input.to_string().as_bytes());
    assert!(out.status.success(), "{out:?}");
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn session_start_prints_built_in_then_person_then_project_with_canvasd_stopped() {
    let d = dirs("order");
    std::fs::write(d.data.join("instructions.md"), "Record motion as video.\n").unwrap();
    std::fs::write(
        d.repo.join(".canvas/instructions.md"),
        "Use the house palette.\n",
    )
    .unwrap();

    assert_eq!(
        session_start(&d, &d.repo),
        format!("{BUILTIN}\nRecord motion as video.\n\nUse the house palette.\n")
    );
}

#[test]
fn include_off_leaves_the_person_and_project_text_only() {
    let d = dirs("include");
    std::fs::write(d.data.join("instructions.md"), "mine\n").unwrap();
    std::fs::write(d.repo.join(".canvas/instructions.md"), "theirs\n").unwrap();

    let out = canvas(&d, &["instructions", "include", "off"], b"");
    assert!(out.status.success(), "{out:?}");
    assert_eq!(session_start(&d, &d.repo), "mine\n\ntheirs\n");

    let log = std::fs::read_to_string(d.data.join("logs/cli.log")).unwrap();
    assert!(
        log.contains("instructions include set kind=instructions include=false"),
        "{log}"
    );
}

#[test]
fn a_project_no_image_stops_the_image_reminder_in_that_repo_only() {
    let d = dirs("reminders");
    std::fs::write(d.repo.join(".canvas/reminders.txt"), "no image\n").unwrap();
    let transcript = d.base.join("transcript.jsonl");
    let lines = [
        serde_json::json!({"type":"user","message":{"content":"look"}}),
        serde_json::json!({"type":"assistant","message":{"content":[
            {"type":"tool_use","name":"Read","input":{"file_path":"/x/shot.png"}}]}}),
    ];
    let text: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
    std::fs::write(&transcript, text.join("\n")).unwrap();

    let prompt = |cwd: &Path| {
        let input = serde_json::json!({"session_id":"s","cwd":cwd,"transcript_path":transcript});
        let out = canvas(&d, &["hook", "prompt"], input.to_string().as_bytes());
        assert!(out.status.success(), "{out:?}");
        String::from_utf8(out.stdout).unwrap()
    };
    assert_eq!(prompt(&d.repo), "");
    assert!(
        !prompt(&d.other_repo).is_empty(),
        "a repo without the file still gets it"
    );
}

#[test]
fn outside_git_there_is_no_project_layer_and_no_error() {
    let d = dirs("outside");
    let out = canvas(&d, &["instructions", "--json"], b"");
    assert!(out.status.success() && out.stderr.is_empty(), "{out:?}");
    let layers: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(layers.as_array().unwrap().len(), 1);
    assert_eq!(session_start(&d, &d.outside), BUILTIN);
}

#[test]
fn json_lists_each_layer_with_its_source_path_and_text() {
    let d = dirs("json");
    std::fs::write(d.data.join("instructions.md"), "mine\n").unwrap();
    std::fs::write(d.repo.join(".canvas/instructions.md"), "theirs\n").unwrap();
    let root = d.repo.to_str().unwrap();

    let out = canvas(&d, &["instructions", "--json", "--cwd", root], b"");
    assert!(out.status.success(), "{out:?}");
    let layers: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let pick = |i: usize, key: &str| layers[i][key].clone();
    assert_eq!(pick(0, "source"), "built-in");
    assert_eq!(pick(0, "path"), serde_json::Value::Null);
    assert_eq!(pick(1, "source"), "person");
    assert_eq!(
        pick(1, "path"),
        d.data.join("instructions.md").to_str().unwrap()
    );
    assert_eq!(pick(1, "text"), "mine");
    assert_eq!(pick(2, "source"), "project");
    assert_eq!(pick(2, "name"), "repo");
    assert_eq!(
        pick(2, "path"),
        d.repo.join(".canvas/instructions.md").to_str().unwrap()
    );
    assert_eq!(pick(2, "text"), "theirs");

    let text = canvas(&d, &["instructions", "--cwd", root], b"");
    assert_eq!(
        String::from_utf8(text.stdout).unwrap(),
        session_start(&d, &d.repo)
    );
}

#[test]
fn projects_is_an_empty_list_until_canvasd_records_one() {
    let d = dirs("projects");
    let out = canvas(&d, &["instructions", "projects"], b"");
    assert!(out.status.success(), "{out:?}");
    assert_eq!(String::from_utf8(out.stdout).unwrap().trim(), "[]");
}

#[test]
fn projects_fails_naming_a_projects_file_that_does_not_parse() {
    let d = dirs("projects-corrupt");
    std::fs::write(d.data.join("projects.json"), "[{\"root\":").unwrap();
    let out = canvas(&d, &["instructions", "projects"], b"");
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert!(out.stdout.is_empty());
    assert!(String::from_utf8(out.stderr)
        .unwrap()
        .contains("projects.json: "));
}

#[test]
fn profile_and_guidance_are_gone() {
    let d = dirs("gone");
    for args in [&["profile", "list"][..], &["guidance"][..]] {
        let out = canvas(&d, args, b"");
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert!(String::from_utf8_lossy(&out.stderr).starts_with("usage: canvas"));
    }
}

#[test]
fn the_built_in_text_ends_with_the_precedence_rule() {
    let last = BUILTIN.trim_end().lines().last().unwrap();
    assert!(
        last.starts_with("Instructions after this line are the person's own"),
        "{last}"
    );
    assert!(last.ends_with("they win."), "{last}");
}
