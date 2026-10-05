//! Artifacts: folders of files canvasd owns under `artifacts/<id>/` in its
//! data directory, or a folder or HTML file the person owns that an artifact
//! links to, shown on the viewer's Artifacts page as real web pages
//! (ADR-0002). The records persist in `artifacts.json` beside
//! `profiles.json`, outside the 24h stream, and are never evicted or pruned:
//! an artifact stays until someone deletes it.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use canvas_core::{Artifact, ArtifactSize, ArtifactSource, ArtifactView};
use serde::{Deserialize, Serialize};

pub const ARTIFACTS_FILE: &str = "artifacts.json";
pub const ARTIFACTS_DIR: &str = "artifacts";
pub const ID_PREFIX: &str = "art-";

/// How much of the entry page is read looking for `canvas-size`.
const SIZE_SCAN_BYTES: usize = 64 * 1024;

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
            data_dir: Some(dir.to_path_buf()),
        }
    }

    pub fn has_data_dir(&self) -> bool {
        self.data_dir.is_some()
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
        }
    }

    /// Every artifact, most recently changed first.
    pub fn views(&self) -> Vec<ArtifactView> {
        let mut list: Vec<&Artifact> = self.records.values().collect();
        list.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        list.into_iter().map(|a| self.view(a)).collect()
    }
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

/// The Content-Security-Policy every artifact file is served with. Scripts,
/// styles and fonts come from the artifact's own folder, inline, or the
/// three CDNs a card may use; `connect-src 'none'` keeps the page from
/// fetching anything, `form-action 'none'` from posting a form, and the
/// `sandbox` directive keeps it in an opaque origin even when it is opened
/// outside the viewer's sandboxed iframe.
pub fn content_security_policy(id: &str) -> String {
    let own = format!("canvas://localhost/{ARTIFACTS_DIR}/{id}/");
    let cdns = "https://cdnjs.cloudflare.com https://cdn.jsdelivr.net https://unpkg.com";
    format!(
        "default-src 'none'; \
         script-src {own} 'unsafe-inline' 'unsafe-eval' blob: {cdns}; \
         style-src {own} 'unsafe-inline' {cdns} https://fonts.googleapis.com; \
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
/// removed). Returns how many files it wrote. A symlinked file is copied as
/// its content; a symlinked folder or a link to nothing is skipped, so a link
/// cycle can't recurse forever. A destination file that is itself a symlink
/// is removed first, so the copy writes a file rather than through the link.
pub fn copy_into(source: &Path, folder: &Path) -> std::io::Result<usize> {
    let meta = std::fs::metadata(source)?;
    if meta.is_dir() {
        copy_dir(source, folder)
    } else {
        let name = source.file_name().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "source has no file name")
        })?;
        copy_file(source, &folder.join(name))?;
        Ok(1)
    }
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<usize> {
    std::fs::create_dir_all(to)?;
    let mut written = 0;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let src = entry.path();
        let dest = to.join(entry.file_name());
        let link = entry.file_type()?.is_symlink();
        let meta = match std::fs::metadata(&src) {
            Ok(meta) => meta,
            // A link to nothing has nothing to copy.
            Err(e) if link && e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        if meta.is_dir() {
            if !link {
                written += copy_dir(&src, &dest)?;
            }
        } else {
            copy_file(&src, &dest)?;
            written += 1;
        }
    }
    Ok(written)
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
}
