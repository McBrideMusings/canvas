//! Artifacts: folders of files canvasd owns under `artifacts/<id>/` in its
//! data directory, or a folder or HTML file the person owns that an artifact
//! links to, shown on the viewer's Artifacts page as real web pages
//! (ADR-0002). The records persist in `artifacts.json` beside
//! `profiles.json`, outside the 24h stream, and are never evicted or pruned:
//! an artifact stays until someone deletes it.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::{Path, PathBuf};

use canvas_core::{
    Artifact, ArtifactExtras, ArtifactOpenedLink, ArtifactRefresh, ArtifactScriptError,
    ArtifactSize, ArtifactSource, ArtifactView, RefreshError, ScriptErrorReport,
};
use serde::{Deserialize, Serialize};

use crate::provenance::{self, Action, Actor};

pub const ARTIFACTS_FILE: &str = "artifacts.json";
pub const ARTIFACTS_DIR: &str = "artifacts";
pub const ID_PREFIX: &str = "art-";

/// How much of the entry page is read looking for `canvas-size`.
const SIZE_SCAN_BYTES: usize = 64 * 1024;

/// The most HTML a widget may carry; it rides on every artifact event.
const MAX_WIDGET_BYTES: usize = 64 * 1024;

/// How many script errors canvasd keeps per artifact; older ones drop off.
const SCRIPT_ERRORS_KEPT: usize = 50;
/// The longest message or source a script error keeps, in bytes.
const SCRIPT_ERROR_TEXT_MAX: usize = 2048;
/// How many opened links canvasd keeps per artifact; older ones drop off.
const OPENED_LINKS_KEPT: usize = 50;
/// The least time between two links one artifact's page opens.
const OPEN_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

#[derive(Default, Serialize, Deserialize)]
struct ArtifactsFile {
    #[serde(default)]
    artifacts: BTreeMap<String, Artifact>,
}

/// Every artifact record, and the data directory their folders live under.
/// Without a data directory (`AppState::new()`) there is nowhere to keep a
/// folder, so every artifact route answers 503.
#[derive(Default)]
pub struct Artifacts {
    pub records: BTreeMap<String, Artifact>,
    /// The [`fingerprint`] of each artifact's files when `updatedAt` was last
    /// stamped, in memory only. The watcher stamps again only when the folder
    /// no longer matches, so the writes of a `put` (which stamps itself)
    /// don't reload the pane a second time.
    pub fingerprints: HashMap<String, u64>,
    /// The newest [`SCRIPT_ERRORS_KEPT`] errors each artifact's page threw
    /// in a viewer's pane, oldest first, in memory only.
    script_errors: HashMap<String, VecDeque<ArtifactScriptError>>,
    /// The newest [`OPENED_LINKS_KEPT`] links each artifact's page had opened
    /// in the default browser, oldest first, in memory only.
    opened_links: HashMap<String, VecDeque<ArtifactOpenedLink>>,
    opened_at: HashMap<String, std::time::Instant>,
    /// Each artifact's latest failed refresh run, until the next success; in
    /// memory only.
    pub refresh_errors: HashMap<String, RefreshError>,
    /// The latest value pushed into each artifact (its refresh or `canvas
    /// data`), last write wins, in memory only.
    pub data: HashMap<String, serde_json::Value>,
    data_dir: Option<PathBuf>,
}

impl Artifacts {
    pub async fn load(dir: &Path) -> Self {
        let records = match tokio::fs::read(dir.join(ARTIFACTS_FILE)).await {
            Ok(bytes) => match serde_json::from_slice::<ArtifactsFile>(&bytes) {
                Ok(file) => file.artifacts,
                Err(e) => {
                    // Kept aside, so the next save can't overwrite the only
                    // copy of every record.
                    let aside = dir.join(format!(
                        "{ARTIFACTS_FILE}.unreadable-{}",
                        chrono::Utc::now().format("%Y%m%dT%H%M%SZ")
                    ));
                    let _ = tokio::fs::rename(dir.join(ARTIFACTS_FILE), &aside).await;
                    canvas_core::log::error(
                        "artifacts.json unreadable; moved aside",
                        &[("error", &e), ("moved_to", &aside.display())],
                    );
                    BTreeMap::new()
                }
            },
            Err(_) => BTreeMap::new(),
        };
        Artifacts {
            records,
            fingerprints: HashMap::new(),
            script_errors: HashMap::new(),
            opened_links: HashMap::new(),
            opened_at: HashMap::new(),
            refresh_errors: HashMap::new(),
            data: HashMap::new(),
            data_dir: Some(dir.to_path_buf()),
        }
    }

    pub fn has_data_dir(&self) -> bool {
        self.data_dir.is_some()
    }

    /// Appends `action` on `id` by `actor` to the provenance log.
    pub async fn log_action(&self, id: &str, action: Action, actor: &Actor) {
        if let Some(dir) = &self.data_dir {
            provenance::append(dir, id, action, actor).await;
        }
    }

    /// `id`'s provenance lines, oldest first; `None` without a data directory.
    pub async fn log_of(&self, id: &str) -> Option<std::io::Result<Vec<provenance::Entry>>> {
        match &self.data_dir {
            Some(dir) => Some(provenance::read(dir, id).await),
            None => None,
        }
    }

    /// Keeps one error `id`'s page threw, dropping the oldest past
    /// [`SCRIPT_ERRORS_KEPT`]. Long text is cut, so a page can't fill memory.
    pub fn record_script_error(&mut self, id: &str, mut report: ScriptErrorReport) {
        report.message = clip(report.message);
        report.source = report.source.map(clip);
        let kept = self.script_errors.entry(id.to_string()).or_default();
        if kept.len() == SCRIPT_ERRORS_KEPT {
            kept.pop_front();
        }
        kept.push_back(ArtifactScriptError {
            at: chrono::Utc::now().to_rfc3339(),
            report,
        });
    }

    /// Keeps one link `id`'s page had opened, dropping the oldest past
    /// [`OPENED_LINKS_KEPT`]. False, keeping nothing, when `id` opened one
    /// less than [`OPEN_INTERVAL`] ago: a page script can post the open
    /// message with no click, so this bounds how fast it can launch tabs.
    pub fn record_opened_link(&mut self, id: &str, url: String) -> bool {
        let now = std::time::Instant::now();
        if let Some(last) = self.opened_at.get(id) {
            if now.duration_since(*last) < OPEN_INTERVAL {
                return false;
            }
        }
        self.opened_at.insert(id.to_string(), now);
        let kept = self.opened_links.entry(id.to_string()).or_default();
        if kept.len() == OPENED_LINKS_KEPT {
            kept.pop_front();
        }
        kept.push_back(ArtifactOpenedLink {
            at: chrono::Utc::now().to_rfc3339(),
            url,
        });
        true
    }

    /// [`Self::view`] plus the errors its page threw and the links it had
    /// opened, for `artifact show`.
    pub fn view_with_errors(&self, artifact: &Artifact) -> ArtifactView {
        let mut view = self.view(artifact);
        if let Some(kept) = self.script_errors.get(&artifact.id) {
            view.script_errors = kept.iter().cloned().collect();
        }
        if let Some(kept) = self.opened_links.get(&artifact.id) {
            view.opened_links = kept.iter().cloned().collect();
        }
        view
    }

    /// Forgets everything kept in memory for `id`, once its record is gone.
    pub fn forget(&mut self, id: &str) {
        self.fingerprints.remove(id);
        self.script_errors.remove(id);
        self.opened_links.remove(id);
        self.opened_at.remove(id);
        self.refresh_errors.remove(id);
        self.data.remove(id);
    }

    /// The folder an owned artifact `id` keeps its files in. Only ids
    /// canvasd minted reach here, so the id is a safe path segment.
    pub fn folder(&self, id: &str) -> Option<PathBuf> {
        self.data_dir
            .as_ref()
            .map(|d| d.join(ARTIFACTS_DIR).join(id))
    }

    /// Where `artifact`'s files are: its owned folder, or the path it links.
    pub fn source_path(&self, artifact: &Artifact) -> Option<PathBuf> {
        match &artifact.source {
            ArtifactSource::Owned => self.folder(&artifact.id),
            ArtifactSource::Linked { link } => Some(PathBuf::from(link)),
        }
    }

    /// [`Self::source_path`] for the record `id`, when there is one.
    pub fn source_path_of(&self, id: &str) -> Option<PathBuf> {
        self.records.get(id).and_then(|a| self.source_path(a))
    }

    /// Writes every record to `artifacts.json` through a temporary file, so
    /// a crash mid-write leaves the old file whole.
    pub async fn save(&self) -> std::io::Result<()> {
        let Some(dir) = &self.data_dir else {
            return Ok(());
        };
        let file = ArtifactsFile {
            artifacts: self.records.clone(),
        };
        let bytes = serde_json::to_vec_pretty(&file).map_err(std::io::Error::other)?;
        let path = dir.join(ARTIFACTS_FILE);
        let tmp = path.with_extension("json.tmp");
        tokio::fs::write(&tmp, bytes).await?;
        tokio::fs::rename(&tmp, &path).await
    }

    /// A fresh `art-` id no record holds.
    pub fn new_id(&self) -> String {
        loop {
            let hex = uuid::Uuid::new_v4().simple().to_string();
            let id = format!("{ID_PREFIX}{}", &hex[..10]);
            if !self.records.contains_key(&id) {
                return id;
            }
        }
    }

    pub fn new_record(&self, title: Option<String>, source: ArtifactSource) -> Artifact {
        let now = chrono::Utc::now().to_rfc3339();
        Artifact {
            id: self.new_id(),
            title: title.filter(|t| !t.trim().is_empty()),
            source,
            created_at: now.clone(),
            updated_at: now,
            widget_html: None,
            refresh: None,
        }
    }

    /// The record plus what its files hold right now.
    pub fn view(&self, artifact: &Artifact) -> ArtifactView {
        let root = self.source_path(artifact).unwrap_or_default();
        let source_missing =
            matches!(artifact.source, ArtifactSource::Linked { .. }) && !root.exists();
        let entry = entry_name(&root);
        let size = entry_page(&root).and_then(|page| declared_size(&page));
        ArtifactView {
            artifact: artifact.clone(),
            path: root.to_string_lossy().into_owned(),
            source_missing,
            entry,
            size,
            script_errors: Vec::new(),
            opened_links: Vec::new(),
            refresh_error: self.refresh_errors.get(&artifact.id).cloned(),
            data: self.data.get(&artifact.id).cloned(),
            changed: None,
        }
    }

    /// Every artifact, most recently changed first.
    pub fn views(&self) -> Vec<ArtifactView> {
        let mut list: Vec<&Artifact> = self.records.values().collect();
        list.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        list.into_iter().map(|a| self.view(a)).collect()
    }
}

/// Sets on `record` the widget and refresh `extras` carry, each one given
/// replacing the record's own. A refresh needs a command, an interval of at
/// least [`canvas_core::MIN_REFRESH_SECS`] and the absolute directory it runs
/// in; `pid` is the agent process asking, whose exit stops it. The error is
/// canvasd's answer, and leaves `record` as it was.
pub fn apply_extras(
    record: &mut Artifact,
    extras: ArtifactExtras,
    pid: Option<u32>,
) -> Result<(), String> {
    let refresh = match extras.refresh {
        Some(refresh) => {
            if refresh.command.trim().is_empty() {
                return Err("a refresh needs a command".to_string());
            }
            if refresh.every_secs < canvas_core::MIN_REFRESH_SECS {
                return Err(format!(
                    "a refresh must run at least every {} seconds",
                    canvas_core::MIN_REFRESH_SECS
                ));
            }
            let cwd = extras
                .cwd
                .filter(|c| Path::new(c).is_absolute())
                .ok_or("a refresh needs the absolute directory it runs in")?;
            Some(ArtifactRefresh {
                command: refresh.command,
                every_secs: refresh.every_secs,
                cwd,
                pid,
            })
        }
        None => None,
    };
    if let Some(widget) = extras.widget_html {
        if widget.trim().is_empty() {
            return Err("a widget needs some HTML".to_string());
        }
        if widget.len() > MAX_WIDGET_BYTES {
            return Err(format!(
                "a widget is at most {} KB of HTML",
                MAX_WIDGET_BYTES / 1024
            ));
        }
        record.widget_html = Some(widget);
    }
    if refresh.is_some() {
        record.refresh = refresh;
    }
    Ok(())
}

fn clip(mut text: String) -> String {
    if text.len() > SCRIPT_ERROR_TEXT_MAX {
        let mut end = SCRIPT_ERROR_TEXT_MAX;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    text
}

fn is_html(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".html") || lower.ends_with(".htm")
}

/// Checks a path offered to `new --link` or `relink`: absolute, and an
/// existing folder or `.html`/`.htm` file. The error is canvasd's answer.
pub fn check_link(link: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(link);
    if !path.is_absolute() {
        return Err(format!("{link} is not an absolute path"));
    }
    let meta = std::fs::metadata(&path).map_err(|_| format!("no folder or HTML file at {link}"))?;
    let html = path
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(is_html);
    if meta.is_dir() || (meta.is_file() && html) {
        Ok(path)
    } else {
        Err(format!("{link} is not a folder or an .html file"))
    }
}

/// The page an artifact rooted at `root` opens: a linked HTML file itself,
/// else [`entry_name`] inside the folder.
fn entry_page(root: &Path) -> Option<PathBuf> {
    if root.is_file() {
        return Some(root.to_path_buf());
    }
    entry_name(root).map(|name| root.join(name))
}

/// The entry's file name: a linked HTML file's own name, else `index.html`,
/// else the folder's only top-level `.html`/`.htm` file.
pub fn entry_name(folder: &Path) -> Option<String> {
    if folder.is_file() {
        return folder
            .file_name()
            .and_then(|n| n.to_str())
            .map(str::to_string);
    }
    if folder.join("index.html").is_file() {
        return Some("index.html".to_string());
    }
    let pages: Vec<String> = std::fs::read_dir(folder)
        .ok()?
        .filter_map(Result::ok)
        .filter(|e| e.path().is_file())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|name| is_html(name))
        .collect();
    match pages.as_slice() {
        [only] => Some(only.clone()),
        _ => None,
    }
}

/// The `WxH` of the page's `<meta name="canvas-size" content="WxH">`.
pub fn declared_size(page: &Path) -> Option<ArtifactSize> {
    use std::io::Read;
    let mut head = Vec::new();
    std::fs::File::open(page)
        .ok()?
        .take(SIZE_SCAN_BYTES as u64)
        .read_to_end(&mut head)
        .ok()?;
    parse_canvas_size(&String::from_utf8_lossy(&head))
}

pub fn parse_canvas_size(html: &str) -> Option<ArtifactSize> {
    let lower = html.to_ascii_lowercase();
    let mut from = 0;
    while let Some(start) = lower[from..].find("<meta").map(|i| i + from) {
        let end = lower[start..].find('>').map_or(lower.len(), |i| i + start);
        let tag = &html[start..end];
        from = end;
        if attr(tag, "name").is_some_and(|n| n.eq_ignore_ascii_case("canvas-size")) {
            let content = attr(tag, "content")?;
            let (w, h) = content
                .split_once(['x', 'X', '×'])
                .map(|(w, h)| (w.trim(), h.trim()))?;
            let size = ArtifactSize {
                width: w.parse().ok()?,
                height: h.parse().ok()?,
            };
            return (size.width > 0 && size.height > 0).then_some(size);
        }
    }
    None
}

/// The value of `name="…"` (or `'…'`, or unquoted) inside one tag.
fn attr<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let lower = tag.to_ascii_lowercase();
    let mut from = 0;
    while let Some(i) = lower[from..].find(name).map(|i| i + from) {
        from = i + name.len();
        let before_ok = i > 0 && lower.as_bytes()[i - 1].is_ascii_whitespace();
        let rest = tag[from..].trim_start();
        let Some(rest) = rest.strip_prefix('=').filter(|_| before_ok) else {
            continue;
        };
        let rest = rest.trim_start();
        return match rest.chars().next()? {
            q @ ('"' | '\'') => rest[1..].split(q).next(),
            _ => rest.split(|c: char| c.is_whitespace() || c == '/').next(),
        };
    }
    None
}

/// Why a file request was refused; logged, and answered as a plain 404.
#[derive(Debug, PartialEq, Eq)]
pub enum Refusal {
    /// A `..` or `.` segment, or a backslash.
    BadSegment,
    /// Nothing at that path.
    Missing,
    /// The path resolves (through a symlink) outside the artifact's folder.
    Escapes,
    /// A folder with no `index.html`.
    NoIndex,
    /// The linked folder or file no longer exists.
    SourceMissing,
}

impl Refusal {
    pub fn as_str(&self) -> &'static str {
        match self {
            Refusal::BadSegment => "bad path segment",
            Refusal::Missing => "no such file",
            Refusal::Escapes => "resolves outside the artifact folder",
            Refusal::NoIndex => "folder has no index.html",
            Refusal::SourceMissing => "the linked source is missing",
        }
    }
}

/// The file `rel` names inside `folder`, confined to it: no `..` or `.`
/// segment, and the fully resolved path (symlinks followed) must still sit
/// under the resolved folder. An empty `rel` is the artifact's entry page; a
/// folder is its `index.html`. When `folder` is a linked HTML file, that file
/// is the whole artifact: only an empty `rel` reaches it.
pub fn resolve_file(folder: &Path, rel: &str) -> Result<PathBuf, Refusal> {
    if !folder.exists() {
        return Err(Refusal::SourceMissing);
    }
    if folder.is_file() {
        if !rel.split('/').all(str::is_empty) {
            return Err(Refusal::Missing);
        }
        return folder.canonicalize().map_err(|_| Refusal::Missing);
    }
    let mut path = folder.to_path_buf();
    for segment in rel.split('/').filter(|s| !s.is_empty()) {
        if segment == ".." || segment == "." || segment.contains('\\') {
            return Err(Refusal::BadSegment);
        }
        path.push(segment);
    }
    if rel.split('/').all(str::is_empty) {
        let entry = entry_name(folder).ok_or(Refusal::NoIndex)?;
        path.push(entry);
    }
    let root = folder.canonicalize().map_err(|_| Refusal::Missing)?;
    let mut resolved = path.canonicalize().map_err(|_| Refusal::Missing)?;
    if !resolved.starts_with(&root) {
        return Err(Refusal::Escapes);
    }
    if resolved.is_dir() {
        resolved = resolved
            .join("index.html")
            .canonicalize()
            .map_err(|_| Refusal::NoIndex)?;
        if !resolved.starts_with(&root) {
            return Err(Refusal::Escapes);
        }
    }
    Ok(resolved)
}

/// The `<script>` every artifact HTML page is served with, ahead of
/// the page's own: `error_relay.js` posts each uncaught error and unhandled
/// rejection to the viewer as `canvas-artifact-error`, since the pane's
/// opaque origin leaves the viewer no other way to see them. The viewer
/// accepts one only from its own pane's frame, and files it under the
/// artifact it built that frame for. Comments go and the lines join into one, so
/// the page's own line numbers stay where they were.
static ERROR_RELAY: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    let mut code = include_str!("error_relay.js").to_string();
    while let Some(start) = code.find("/*") {
        let end = code[start..]
            .find("*/")
            .map_or(code.len(), |e| start + e + 2);
        code.replace_range(start..end, "");
    }
    let line = code
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    format!("<script>{line}</script>")
});

/// An artifact's HTML page as canvasd serves it: every `<script src>`
/// without a `crossorigin` attribute gains one, so WebKit reports an error at
/// its top level in full rather than as "Script error." (the folder and the
/// three CDNs the CSP allows all answer with `Access-Control-Allow-Origin:
/// *`), and [`ERROR_RELAY`] goes where it runs
/// before any of the page's scripts but leaves the doctype first: just
/// inside `<head>`, else just inside `<html>`, else after the doctype, else
/// at the start.
pub fn served_page(html: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(html.len() + 1024);
    let lower = html.to_ascii_lowercase();
    let mut copied = 0;
    for (at, end) in tags(&lower, b"<script") {
        let name_end = at + b"<script".len();
        let attrs = &lower[name_end..end];
        if has_attr(attrs, b"src") && !has_attr(attrs, b"crossorigin") {
            out.extend_from_slice(&html[copied..name_end]);
            out.extend_from_slice(b" crossorigin");
            copied = name_end;
        }
    }
    out.extend_from_slice(&html[copied..]);

    let lower = out.to_ascii_lowercase();
    let at = [&b"<head"[..], b"<html", b"<!doctype"]
        .iter()
        .find_map(|name| tags(&lower, name).next().map(|(_, end)| end + 1))
        .unwrap_or(0);
    out.splice(at..at, ERROR_RELAY.bytes());
    out
}

const SPACE: &[u8] = b" \t\n\r\x0c";

/// Each `name` tag in lowercased `html`, as the offsets of its `<` and its
/// closing `>`. `<header>` is not a `<head` tag.
fn tags<'a>(lower: &'a [u8], name: &'a [u8]) -> impl Iterator<Item = (usize, usize)> + 'a {
    let mut from = 0;
    std::iter::from_fn(move || {
        while let Some(at) = find(&lower[from..], name).map(|i| i + from) {
            from = at + name.len();
            let next = lower.get(from).copied()?;
            if next == b'>' || next == b'/' || SPACE.contains(&next) {
                let end = find(&lower[from..], b">")? + from;
                from = end;
                return Some((at, end));
            }
        }
        None
    })
}

/// Whether a tag's lowercased attribute text names attribute `name`.
fn has_attr(attrs: &[u8], name: &[u8]) -> bool {
    let mut from = 0;
    while let Some(at) = find(&attrs[from..], name).map(|i| i + from) {
        from = at + name.len();
        let before = at.checked_sub(1).map(|i| attrs[i]);
        let after = attrs.get(from).copied();
        if before.is_some_and(|b| SPACE.contains(&b))
            && after.is_none_or(|b| b == b'=' || b == b'/' || SPACE.contains(&b))
        {
            return true;
        }
    }
    false
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// The public stylesheet canvasd serves (`viewer::stylesheet`), the one
/// stylesheet outside its folder an artifact may link.
const CANVAS_STYLESHEET: &str = "canvas://localhost/canvas.css";

/// The Content-Security-Policy every artifact file is served with. Scripts,
/// styles and fonts come from the artifact's own folder, inline, or the
/// three CDNs a card may use, and styles also from `/canvas.css`;
/// `connect-src 'none'` keeps the page from
/// fetching anything, `form-action 'none'` from posting a form, and the
/// `sandbox` directive keeps it in an opaque origin even when it is opened
/// outside the viewer's sandboxed iframe.
pub fn content_security_policy(id: &str) -> String {
    let own = format!("canvas://localhost/{ARTIFACTS_DIR}/{id}/");
    let cdns = "https://cdnjs.cloudflare.com https://cdn.jsdelivr.net https://unpkg.com";
    format!(
        "default-src 'none'; \
         script-src {own} 'unsafe-inline' 'unsafe-eval' blob: {cdns}; \
         style-src {own} {CANVAS_STYLESHEET} 'unsafe-inline' {cdns} https://fonts.googleapis.com; \
         font-src {own} data: https://fonts.gstatic.com; \
         img-src * data: blob: canvas:; \
         media-src * data: blob: canvas:; \
         connect-src 'none'; \
         form-action 'none'; \
         base-uri 'none'; \
         sandbox allow-scripts"
    )
}

/// Copies `source` into `folder`: a file under its own name, a folder's
/// contents recursively, adding to and overwriting what is there (nothing is
/// removed). Returns the files it wrote, relative to `folder`. A symlinked
/// file is copied as its content; a symlinked folder or a link to nothing is
/// skipped, so a link cycle can't recurse forever. A destination file that is
/// itself a symlink is removed first, so the copy writes a file rather than
/// through the link.
pub fn copy_into(source: &Path, folder: &Path) -> std::io::Result<Vec<String>> {
    let meta = std::fs::metadata(source)?;
    let mut written = Vec::new();
    if meta.is_dir() {
        copy_dir(source, folder, Path::new(""), &mut written)?;
    } else {
        let name = source.file_name().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "source has no file name")
        })?;
        copy_file(source, &folder.join(name))?;
        written.push(name.to_string_lossy().into_owned());
    }
    Ok(written)
}

fn copy_dir(
    from: &Path,
    to: &Path,
    relative: &Path,
    written: &mut Vec<String>,
) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let src = entry.path();
        let dest = to.join(entry.file_name());
        let rel = relative.join(entry.file_name());
        let link = entry.file_type()?.is_symlink();
        let meta = match std::fs::metadata(&src) {
            Ok(meta) => meta,
            // A link to nothing has nothing to copy.
            Err(e) if link && e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        if meta.is_dir() {
            if !link {
                copy_dir(&src, &dest, &rel, written)?;
            }
        } else {
            copy_file(&src, &dest)?;
            written.push(rel.to_string_lossy().into_owned());
        }
    }
    Ok(())
}

fn copy_file(src: &Path, dest: &Path) -> std::io::Result<()> {
    if std::fs::symlink_metadata(dest).is_ok_and(|m| m.file_type().is_symlink()) {
        std::fs::remove_file(dest)?;
    }
    std::fs::copy(src, dest).map(|_| ())
}

/// A hash of every entry under `folder`: its relative path, size, modified
/// time and inode, walked without following symlinked folders. Any write,
/// rename, add or removal changes it; reading a file does not. A linked HTML
/// file hashes as its own one entry, and a missing root hashes differently
/// from an empty folder, so a linked path disappearing counts as a change.
pub fn fingerprint(folder: &Path) -> u64 {
    use std::hash::{Hash, Hasher};
    use std::os::unix::fs::MetadataExt;
    let mut entries = Vec::new();
    let root = std::fs::metadata(folder).ok();
    if let Some(meta) = root.as_ref().filter(|m| !m.is_dir()) {
        entries.push((
            PathBuf::new(),
            meta.len(),
            meta.mtime(),
            meta.mtime_nsec(),
            meta.ino(),
        ));
    }
    let mut stack: Vec<PathBuf> = root
        .as_ref()
        .filter(|m| m.is_dir())
        .map(|_| folder.to_path_buf())
        .into_iter()
        .collect();
    while let Some(dir) = stack.pop() {
        let Ok(read) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in read.filter_map(Result::ok) {
            let path = entry.path();
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if meta.is_dir() {
                stack.push(path.clone());
            }
            let rel = path.strip_prefix(folder).unwrap_or(&path).to_path_buf();
            entries.push((rel, meta.len(), meta.mtime(), meta.mtime_nsec(), meta.ino()));
        }
    }
    entries.sort();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    root.is_some().hash(&mut hasher);
    entries.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canvas_size_reads_either_attribute_order() {
        let want = Some(ArtifactSize {
            width: 390,
            height: 844,
        });
        assert_eq!(
            parse_canvas_size(r#"<head><meta name="canvas-size" content="390x844"></head>"#),
            want
        );
        assert_eq!(
            parse_canvas_size("<META content='390 × 844' NAME=canvas-size>"),
            want
        );
        assert_eq!(
            parse_canvas_size(
                r#"<meta name="viewport" content="1x1"><meta name="canvas-size" content="390x844">"#
            ),
            want
        );
        assert_eq!(
            parse_canvas_size(r#"<meta name="canvas-size" content="wide">"#),
            None
        );
        assert_eq!(
            parse_canvas_size(r#"<meta name="canvas-size" content="0x844">"#),
            None
        );
    }

    #[test]
    fn error_relay_goes_inside_head_and_never_before_the_doctype() {
        let relay = ERROR_RELAY.as_str();
        let place = |html: &str| {
            String::from_utf8(served_page(html.as_bytes()))
                .unwrap()
                .replace(relay, "|")
        };
        assert_eq!(
            place("<!DOCTYPE html><HTML lang=en><Head><script>x()</script>"),
            "<!DOCTYPE html><HTML lang=en><Head>|<script>x()</script>"
        );
        assert_eq!(
            place("<!doctype html><html><header>h</header>"),
            "<!doctype html><html>|<header>h</header>"
        );
        assert_eq!(place("<!doctype html><p>x"), "<!doctype html>|<p>x");
        assert_eq!(place("<p>x"), "|<p>x");
    }

    #[test]
    fn error_relay_is_one_line() {
        let relay = ERROR_RELAY.as_str();
        assert!(!relay.contains('\n'));
        assert!(!relay.contains("/*"));
        assert!(relay.ends_with("})();</script>"));
    }

    #[test]
    fn script_src_tags_gain_crossorigin_once() {
        let page = |html: &str| {
            String::from_utf8(served_page(html.as_bytes()))
                .unwrap()
                .replace(ERROR_RELAY.as_str(), "")
        };
        assert_eq!(
            page(r#"<SCRIPT SRC="a.js"></SCRIPT><script>x()</script>"#),
            r#"<SCRIPT crossorigin SRC="a.js"></SCRIPT><script>x()</script>"#
        );
        assert_eq!(
            page(r#"<script type=module src=a.js crossorigin=anonymous></script>"#),
            r#"<script type=module src=a.js crossorigin=anonymous></script>"#
        );
        assert_eq!(
            page(r#"<script data-src="a"></script><scripts src=b>"#),
            r#"<script data-src="a"></script><scripts src=b>"#
        );
    }
}
