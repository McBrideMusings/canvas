//! `canvas export <id>` writes the card as one standalone page: images as
//! `data:` URIs, links resolved from the card's targets, its `canvas data`
//! value baked in. Spawns the real built binary as a throwaway daemon on its
//! own socket.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

fn canvas_bin() -> &'static str {
    env!("CARGO_BIN_EXE_canvas")
}

struct Daemon {
    child: std::process::Child,
    socket: PathBuf,
}

impl Daemon {
    fn dir(&self) -> &Path {
        self.socket.parent().unwrap()
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

static NEXT: AtomicUsize = AtomicUsize::new(0);

fn spawn_daemon() -> Daemon {
    let data_dir = std::env::temp_dir().join(format!(
        "canvas-export-test-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&data_dir);
    std::fs::create_dir_all(&data_dir).unwrap();
    let socket = data_dir.join("canvasd.sock");
    let child = Command::new(canvas_bin())
        .arg("daemon")
        .env("CANVAS_DATA_DIR", &data_dir)
        .env("CANVAS_SOCKET", &socket)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn canvas daemon");
    let daemon = Daemon { child, socket };
    let deadline = Instant::now() + Duration::from_secs(5);
    while std::os::unix::net::UnixStream::connect(&daemon.socket).is_err() {
        if Instant::now() > deadline {
            panic!("daemon on {} never came up", daemon.socket.display());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    daemon
}

fn run(daemon: &Daemon, args: &[&str], stdin: Option<&[u8]>) -> std::process::Output {
    run_as(daemon, "export-test", args, stdin)
}

fn run_as(
    daemon: &Daemon,
    session: &str,
    args: &[&str],
    stdin: Option<&[u8]>,
) -> std::process::Output {
    let mut child = Command::new(canvas_bin())
        .args(args)
        .current_dir(daemon.dir())
        .env("CANVAS_SOCKET", &daemon.socket)
        .env("CANVAS_DATA_DIR", daemon.dir())
        .env("CLAUDE_CODE_SESSION_ID", session)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to run canvas");
    {
        use std::io::Write;
        let mut pipe = child.stdin.take().unwrap();
        if let Some(bytes) = stdin {
            pipe.write_all(bytes).unwrap();
        }
    }
    child.wait_with_output().unwrap()
}

fn post(daemon: &Daemon, html: &str) -> String {
    post_as(daemon, "export-test", html)
}

fn post_as(daemon: &Daemon, session: &str, html: &str) -> String {
    let posted = run_as(
        daemon,
        session,
        &["post", "-", "--format", "html"],
        Some(html.as_bytes()),
    );
    assert!(posted.status.success(), "{:?}", posted);
    let posted: serde_json::Value = serde_json::from_slice(&posted.stdout).unwrap();
    posted["card_id"].as_str().unwrap().to_string()
}

/// Runs `canvas export <id> -o out.html`; returns the printed JSON and the page.
fn export(daemon: &Daemon, card_id: &str) -> (serde_json::Value, String) {
    let out = daemon.dir().join("out.html");
    let output = run(
        daemon,
        &["export", card_id, "-o", out.to_str().unwrap()],
        None,
    );
    assert!(output.status.success(), "{:?}", output);
    let printed: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(printed["path"], out.to_str().unwrap());
    assert_eq!(printed["cards"], 1);
    (printed, std::fs::read_to_string(&out).unwrap())
}

#[test]
fn export_inlines_the_image() {
    let daemon = spawn_daemon();
    let image = daemon.dir().join("dot.png");
    std::fs::write(&image, b"abc").unwrap();
    let card_id = post(
        &daemon,
        &format!("<p><img src=\"{}\"></p>", image.display()),
    );

    let (printed, page) = export(&daemon, &card_id);
    assert_eq!(printed["warnings"], serde_json::json!([]));
    assert!(
        page.contains("<img src=\"data:image/png;base64,YWJj\">"),
        "{page}"
    );
    assert!(!page.contains("/api/"));
    assert!(!page.contains(&format!("src=\"{}", daemon.dir().display())));
}

#[test]
fn export_warns_for_a_deleted_image_and_shows_a_placeholder() {
    let daemon = spawn_daemon();
    let image = daemon.dir().join("gone.png");
    std::fs::write(&image, b"abc").unwrap();
    let card_id = post(&daemon, &format!("<img src=\"{}\">", image.display()));
    std::fs::remove_file(&image).unwrap();

    let (printed, page) = export(&daemon, &card_id);
    let warnings = printed["warnings"].as_array().unwrap();
    assert_eq!(warnings.len(), 1, "{printed}");
    assert_eq!(warnings[0]["kind"], "missing-image");
    assert_eq!(warnings[0]["target"], image.to_str().unwrap());
    assert!(page.contains("image missing: gone.png"), "{page}");
}

#[test]
fn export_delivers_the_latest_data_value() {
    let daemon = spawn_daemon();
    let card_id = post(
        &daemon,
        "<p id=n></p><script>addEventListener('message',e=>{if(e.data.type==='canvas-data')n.textContent=e.data.value.n})</script>",
    );
    let pushed = run(&daemon, &["data", &card_id, "-"], Some(br#"{"n":42}"#));
    assert!(pushed.status.success(), "{:?}", pushed);

    let (_, page) = export(&daemon, &card_id);
    assert!(page.contains(r#"var v={"n":42}"#), "{page}");
    assert!(page.contains("type:'canvas-data',value:v"));
}

#[test]
fn export_resolves_web_and_local_links() {
    let daemon = spawn_daemon();
    let file = daemon.dir().join("notes.md");
    std::fs::write(&file, b"x").unwrap();
    let card_id = post(
        &daemon,
        &format!(
            "<a href=\"https://example.com/a\">web</a> <a href=\"{}\">notes</a>",
            file.display()
        ),
    );

    let (_, page) = export(&daemon, &card_id);
    assert!(
        page.contains(
            r#"<a href="https://example.com/a" target="_blank" rel="noopener">web</a> notes"#
        ),
        "{page}"
    );
    assert!(!page.contains("#canvas-open-"));
}

#[test]
fn export_fails_with_one_line_for_an_unknown_id() {
    let daemon = spawn_daemon();
    let output = run(&daemon, &["export", "no-such-card"], None);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(stderr.lines().count(), 1, "{stderr}");
    assert!(stderr.contains("404"), "{stderr}");
    assert!(!daemon.dir().join("no-such-card.html").exists());
}

#[test]
fn export_fails_with_one_line_when_the_file_cannot_be_written() {
    let daemon = spawn_daemon();
    let card_id = post(&daemon, "<p>x</p>");
    let out = daemon.dir().join("no-such-dir").join("out.html");
    let output = run(
        &daemon,
        &["export", &card_id, "-o", out.to_str().unwrap()],
        None,
    );
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(stderr.lines().count(), 1, "{stderr}");
    assert!(stderr.contains("cannot write"), "{stderr}");
}

#[test]
fn export_writes_card_id_html_by_default() {
    let daemon = spawn_daemon();
    let card_id = post(&daemon, "<h1>Title</h1>");
    let output = run(&daemon, &["export", &card_id], None);
    assert!(output.status.success(), "{:?}", output);
    let page = std::fs::read_to_string(daemon.dir().join(format!("{card_id}.html"))).unwrap();
    assert!(page.contains("<title>Title</title>"));
}

/// Runs `canvas export --all [extra] -o all.zip`; returns the printed JSON
/// and the zip's entries by name.
fn export_all(
    daemon: &Daemon,
    extra: &[&str],
) -> (
    serde_json::Value,
    std::collections::BTreeMap<String, String>,
) {
    let out = daemon.dir().join("all.zip");
    let mut args = vec!["export", "--all"];
    args.extend_from_slice(extra);
    args.extend_from_slice(&["-o", out.to_str().unwrap()]);
    let output = run(daemon, &args, None);
    assert!(output.status.success(), "{:?}", output);
    let printed: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(printed["path"], out.to_str().unwrap());
    let mut archive = zip::ZipArchive::new(std::fs::File::open(&out).unwrap()).unwrap();
    let mut entries = std::collections::BTreeMap::new();
    for i in 0..archive.len() {
        use std::io::Read;
        let mut entry = archive.by_index(i).unwrap();
        let mut text = String::new();
        entry.read_to_string(&mut text).unwrap();
        entries.insert(entry.name().to_string(), text);
    }
    (printed, entries)
}

/// The `href`s of the index's rows, in order.
fn index_links(index: &str) -> Vec<String> {
    index
        .split("<li><a href=\"")
        .skip(1)
        .map(|row| row[..row.find('"').unwrap()].to_string())
        .collect()
}

#[test]
fn export_all_zips_every_card_newest_first() {
    let daemon = spawn_daemon();
    let first = post_as(&daemon, "session-a", "<h1>First</h1>");
    let second = post_as(&daemon, "session-b", "<p>second line</p><p>more</p>");
    let third = post_as(&daemon, "session-a", "<h2>Third</h2>");

    let (printed, entries) = export_all(&daemon, &[]);
    assert_eq!(printed["cards"], 3, "{printed}");
    assert_eq!(printed["warnings"], serde_json::json!([]));
    let mut names: Vec<&str> = entries.keys().map(String::as_str).collect();
    names.sort();
    let mut expected = vec![
        "index.html".to_string(),
        format!("{first}.html"),
        format!("{second}.html"),
        format!("{third}.html"),
    ];
    expected.sort();
    assert_eq!(names, expected);

    let index = &entries["index.html"];
    let links = index_links(index);
    assert_eq!(
        links,
        vec![
            format!("{third}.html"),
            format!("{second}.html"),
            format!("{first}.html")
        ]
    );
    for link in &links {
        assert!(entries.contains_key(link), "{link} not in the zip");
    }
    assert!(index.contains(">Third</a>"), "{index}");
    assert!(index.contains(">second line</a>"), "{index}");
    // A session's name is its cwd's basename.
    let name = daemon.dir().file_name().unwrap().to_str().unwrap();
    assert!(index.contains(&format!("<span>{name}</span>")), "{index}");
    assert!(entries[&format!("{first}.html")].contains("<title>First</title>"));
}

#[test]
fn export_all_narrows_to_one_session() {
    let daemon = spawn_daemon();
    let a = post_as(&daemon, "session-a", "<h1>A</h1>");
    post_as(&daemon, "session-b", "<h1>B</h1>");

    let (printed, entries) = export_all(&daemon, &["--session", "session-a"]);
    assert_eq!(printed["cards"], 1, "{printed}");
    let names: Vec<&str> = entries.keys().map(String::as_str).collect();
    let page = format!("{a}.html");
    assert_eq!(names, vec![page.as_str(), "index.html"]);
    assert_eq!(index_links(&entries["index.html"]), vec![page.clone()]);
}

#[test]
fn export_all_tags_a_missing_image_with_its_card() {
    let daemon = spawn_daemon();
    let image = daemon.dir().join("gone.png");
    std::fs::write(&image, b"abc").unwrap();
    post(&daemon, "<h1>Fine</h1>");
    let broken = post(&daemon, &format!("<img src=\"{}\">", image.display()));
    std::fs::remove_file(&image).unwrap();

    let (printed, entries) = export_all(&daemon, &[]);
    assert_eq!(printed["cards"], 2, "{printed}");
    let warnings = printed["warnings"].as_array().unwrap();
    assert_eq!(warnings.len(), 1, "{printed}");
    assert_eq!(warnings[0]["card_id"], broken.as_str());
    assert_eq!(warnings[0]["kind"], "missing-image");
    assert_eq!(warnings[0]["target"], image.to_str().unwrap());
    assert!(entries[&format!("{broken}.html")].contains("image missing: gone.png"));
}

#[test]
fn export_all_fails_with_one_line_for_an_unknown_session() {
    let daemon = spawn_daemon();
    post(&daemon, "<p>x</p>");
    let out = daemon.dir().join("all.zip");
    let output = run(
        &daemon,
        &[
            "export",
            "--all",
            "--session",
            "nope",
            "-o",
            out.to_str().unwrap(),
        ],
        None,
    );
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(stderr.lines().count(), 1, "{stderr}");
    assert!(stderr.contains("no session with id nope"), "{stderr}");
    assert!(!out.exists());
}

#[test]
fn export_all_fails_with_one_line_when_the_zip_cannot_be_written() {
    let daemon = spawn_daemon();
    post(&daemon, "<p>x</p>");
    let out = daemon.dir().join("no-such-dir").join("all.zip");
    let output = run(
        &daemon,
        &["export", "--all", "-o", out.to_str().unwrap()],
        None,
    );
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(stderr.lines().count(), 1, "{stderr}");
    assert!(stderr.contains("cannot write"), "{stderr}");
}

#[test]
fn export_all_of_an_empty_daemon_writes_just_the_index() {
    let daemon = spawn_daemon();
    let (printed, entries) = export_all(&daemon, &[]);
    assert_eq!(printed["cards"], 0, "{printed}");
    let names: Vec<&str> = entries.keys().map(String::as_str).collect();
    assert_eq!(names, vec!["index.html"]);
    assert!(entries["index.html"].contains("No posts to export"));
}

#[test]
fn export_all_fails_with_one_line_when_canvasd_is_unreachable() {
    let daemon = spawn_daemon();
    let out = daemon.dir().join("all.zip");
    std::fs::write(&out, b"earlier export").unwrap();
    let output = Command::new(canvas_bin())
        .args(["export", "--all", "-o", out.to_str().unwrap()])
        .env("CANVAS_SOCKET", daemon.dir().join("nothing.sock"))
        .env("CANVAS_DATA_DIR", daemon.dir())
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(stderr.lines().count(), 1, "{stderr}");
    assert!(stderr.contains("could not reach canvasd"), "{stderr}");
    assert_eq!(std::fs::read(&out).unwrap(), b"earlier export");
}
