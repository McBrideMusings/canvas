//! Persistent state for an artifact's page: named JSON values, one file each,
//! in a `canvas-data/` folder inside the artifact's own folder (a linked
//! single HTML file keeps it beside the file). The page asks its parent for
//! them with `canvas-state-set` and `canvas-state-get`; the viewer relays to
//! the routes in [`crate::artifact_routes`].
//!
//! A key is a name, never a path (`^[a-z0-9][a-z0-9_-]{0,63}$`), so a key
//! cannot climb out of the folder. A linked folder is often a git repository,
//! so a page must never write anywhere but `canvas-data/` itself: a symlinked
//! `canvas-data`, a symlinked key file, or a `canvas-data` that does not
//! resolve to a direct child of the artifact's folder is refused.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

/// The folder state lives in.
pub const DIR: &str = "canvas-data";
/// The most one value may take, in bytes of JSON.
pub const MAX_VALUE_BYTES: usize = 256 * 1024;
/// The most one artifact's values may take together.
pub const MAX_TOTAL_BYTES: u64 = 5 * 1024 * 1024;

/// Serializes every write, so two sets cannot both pass the quota check.
pub static WRITES: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Debug)]
pub enum Error {
    BadKey,
    NotJson(String),
    ValueTooLarge(usize),
    OverQuota(u64),
    Unsafe(String),
    NoSource,
    Io(std::io::Error),
}

impl Error {
    pub fn status(&self) -> u16 {
        match self {
            Error::BadKey | Error::NotJson(_) => 400,
            Error::ValueTooLarge(_) | Error::OverQuota(_) => 413,
            Error::Unsafe(_) => 403,
            Error::NoSource => 409,
            Error::Io(_) => 500,
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::BadKey => write!(
                f,
                "a state key is 1 to 64 characters of a-z, 0-9, _ or -, starting with a letter or digit"
            ),
            Error::NotJson(e) => write!(f, "the value is not JSON: {e}"),
            Error::ValueTooLarge(n) => write!(
                f,
                "the value is {n} bytes; one key holds at most {MAX_VALUE_BYTES}"
            ),
            Error::OverQuota(n) => write!(
                f,
                "the artifact's state would be {n} bytes; it holds at most {MAX_TOTAL_BYTES}"
            ),
            Error::Unsafe(why) => write!(f, "refused: {why}"),
            Error::NoSource => write!(f, "the artifact's folder does not exist"),
            Error::Io(e) => write!(f, "state is unavailable: {e}"),
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

/// Whether a path component is the state folder (APFS ignores case).
pub fn is_data_dir_name(name: &std::ffi::OsStr) -> bool {
    name.to_str().is_some_and(|n| n.eq_ignore_ascii_case(DIR))
}

pub fn valid_key(key: &str) -> bool {
    let bytes = key.as_bytes();
    (1..=64).contains(&bytes.len())
        && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'_' || *b == b'-')
}

/// The folder `canvas-data/` sits in for an artifact at `source`.
fn base_of(source: &Path) -> Result<PathBuf, Error> {
    if !source.exists() {
        return Err(Error::NoSource);
    }
    if source.is_file() {
        source
            .parent()
            .map(Path::to_path_buf)
            .ok_or(Error::NoSource)
    } else {
        Ok(source.to_path_buf())
    }
}

/// The checked `canvas-data/` folder, or `None` when there isn't one and
/// `create` is false. Refuses a symlink, a non-folder, and a folder whose
/// resolved path is not a direct child of the resolved artifact folder.
fn data_dir(source: &Path, create: bool) -> Result<Option<PathBuf>, Error> {
    let base = base_of(source)?;
    let dir = base.join(DIR);
    match std::fs::symlink_metadata(&dir) {
        Ok(meta) if meta.file_type().is_symlink() => {
            return Err(Error::Unsafe(format!("{DIR} is a symlink")))
        }
        Ok(meta) if !meta.is_dir() => return Err(Error::Unsafe(format!("{DIR} is not a folder"))),
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if !create {
                return Ok(None);
            }
            std::fs::create_dir(&dir)?;
        }
        Err(e) => return Err(e.into()),
    }
    let resolved = dir.canonicalize()?;
    let root = base.canonicalize()?;
    if resolved.parent() != Some(root.as_path()) {
        return Err(Error::Unsafe(format!(
            "{DIR} does not resolve inside the artifact's folder"
        )));
    }
    Ok(Some(resolved))
}

fn key_file(dir: &Path, key: &str) -> PathBuf {
    dir.join(format!("{key}.json"))
}

/// The key a file name in `canvas-data/` holds, when it is a value file.
fn key_of(name: &str) -> Option<&str> {
    name.strip_suffix(".json").filter(|k| valid_key(k))
}

/// The regular value files in `dir` as `(key, path, size)`; anything else in
/// the folder (a symlink, a folder, a stray name) is not state.
fn entries(dir: &Path) -> Result<Vec<(String, PathBuf, u64)>, Error> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let Some(key) = key_of(&name) else { continue };
        let meta = std::fs::symlink_metadata(entry.path())?;
        if meta.file_type().is_file() {
            out.push((key.to_string(), entry.path(), meta.len()));
        }
    }
    Ok(out)
}

/// Removes temp files a crash left behind; callers hold [`WRITES`].
fn sweep_temps(dir: &Path) {
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in read.filter_map(Result::ok) {
        let name = entry.file_name();
        if name
            .to_str()
            .is_some_and(|n| n.starts_with('.') && n.ends_with(".tmp"))
        {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Stores `body` (one JSON value) under `key`, atomically.
pub fn set(source: &Path, key: &str, body: &[u8]) -> Result<(), Error> {
    if !valid_key(key) {
        return Err(Error::BadKey);
    }
    if body.len() > MAX_VALUE_BYTES {
        return Err(Error::ValueTooLarge(body.len()));
    }
    serde_json::from_slice::<serde_json::Value>(body).map_err(|e| Error::NotJson(e.to_string()))?;
    let Some(dir) = data_dir(source, true)? else {
        return Err(Error::NoSource);
    };
    let target = key_file(&dir, key);
    if let Ok(meta) = std::fs::symlink_metadata(&target) {
        if !meta.file_type().is_file() {
            return Err(Error::Unsafe(format!("{key}.json is not a regular file")));
        }
    }
    sweep_temps(&dir);
    let others: u64 = entries(&dir)?
        .iter()
        .filter(|(k, _, _)| k != key)
        .map(|(_, _, size)| size)
        .sum();
    let total = others + body.len() as u64;
    if total > MAX_TOTAL_BYTES {
        return Err(Error::OverQuota(total));
    }
    let tmp = dir.join(format!(".{key}.tmp"));
    let _ = std::fs::remove_file(&tmp);
    let written = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        file.write_all(body)?;
        file.sync_all()?;
        std::fs::rename(&tmp, &target)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    Ok(written?)
}

/// Every stored value, by key; an unreadable or non-JSON file is skipped.
pub fn get_all(source: &Path) -> Result<BTreeMap<String, serde_json::Value>, Error> {
    let mut out = BTreeMap::new();
    let Some(dir) = data_dir(source, false)? else {
        return Ok(out);
    };
    for (key, path, _) in entries(&dir)? {
        if let Some(value) = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        {
            out.insert(key, value);
        }
    }
    Ok(out)
}

/// One stored value, `None` when the key holds nothing.
pub fn get(source: &Path, key: &str) -> Result<Option<serde_json::Value>, Error> {
    if !valid_key(key) {
        return Err(Error::BadKey);
    }
    let Some(dir) = data_dir(source, false)? else {
        return Ok(None);
    };
    let path = key_file(&dir, key);
    match std::fs::symlink_metadata(&path) {
        Ok(meta) if meta.file_type().is_file() => Ok(std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())),
        _ => Ok(None),
    }
}

/// Deletes every value and the folder when nothing else is in it; answers how
/// many values went.
pub fn clear(source: &Path) -> Result<usize, Error> {
    let Some(dir) = data_dir(source, false)? else {
        return Ok(0);
    };
    let found = entries(&dir)?;
    for (_, path, _) in &found {
        std::fs::remove_file(path)?;
    }
    sweep_temps(&dir);
    let _ = std::fs::remove_dir(&dir);
    Ok(found.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("canvasd-state-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn keys_are_names_not_paths() {
        for ok in ["a", "0", "score", "q-1_b", &"a".repeat(64)] {
            assert!(valid_key(ok), "{ok}");
        }
        for bad in [
            "",
            "-a",
            "_a",
            "A",
            "a.b",
            "a/b",
            "../x",
            "a b",
            "é",
            &"a".repeat(65),
        ] {
            assert!(!valid_key(bad), "{bad}");
        }
    }

    #[test]
    fn a_value_round_trips_and_clear_removes_the_folder() {
        let dir = folder();
        set(&dir, "count", b"{\"n\":3}").unwrap();
        assert_eq!(get(&dir, "count").unwrap().unwrap()["n"], 3);
        assert_eq!(get(&dir, "other").unwrap(), None);
        assert_eq!(get_all(&dir).unwrap().len(), 1);
        assert_eq!(clear(&dir).unwrap(), 1);
        assert!(!dir.join(DIR).exists());
        assert!(get_all(&dir).unwrap().is_empty());
    }

    #[test]
    fn a_linked_html_file_keeps_state_beside_it() {
        let dir = folder();
        let page = dir.join("quiz.html");
        std::fs::write(&page, "<p>q</p>").unwrap();
        set(&page, "a", b"1").unwrap();
        assert!(dir.join(DIR).join("a.json").is_file());
        assert_eq!(get(&page, "a").unwrap().unwrap(), 1);
    }

    #[test]
    fn caps_refuse_without_writing() {
        let dir = folder();
        let big = format!("\"{}\"", "x".repeat(MAX_VALUE_BYTES));
        assert!(matches!(
            set(&dir, "big", big.as_bytes()),
            Err(Error::ValueTooLarge(_))
        ));
        let chunk = format!("\"{}\"", "x".repeat(MAX_VALUE_BYTES - 2));
        for i in 0..20 {
            set(&dir, &format!("k{i}"), chunk.as_bytes()).unwrap();
        }
        assert!(matches!(
            set(&dir, "k20", chunk.as_bytes()),
            Err(Error::OverQuota(_))
        ));
        assert!(!dir.join(DIR).join("k20.json").exists());
        // Replacing a key counts only its new size.
        set(&dir, "k0", b"1").unwrap();
        let smaller = format!("\"{}\"", "x".repeat(MAX_VALUE_BYTES - 3));
        set(&dir, "k20", smaller.as_bytes()).unwrap();
    }

    #[test]
    fn a_symlinked_data_folder_or_key_is_refused() {
        let dir = folder();
        let outside = folder();
        std::os::unix::fs::symlink(&outside, dir.join(DIR)).unwrap();
        assert!(matches!(set(&dir, "a", b"1"), Err(Error::Unsafe(_))));
        assert!(matches!(get_all(&dir), Err(Error::Unsafe(_))));
        assert!(matches!(clear(&dir), Err(Error::Unsafe(_))));
        assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 0);

        let dir = folder();
        std::fs::create_dir(dir.join(DIR)).unwrap();
        let victim = outside.join("victim");
        std::fs::write(&victim, "keep").unwrap();
        std::os::unix::fs::symlink(&victim, dir.join(DIR).join("a.json")).unwrap();
        assert!(matches!(set(&dir, "a", b"1"), Err(Error::Unsafe(_))));
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep");
    }

    #[test]
    fn a_bad_key_or_body_writes_nothing() {
        let dir = folder();
        assert!(matches!(set(&dir, "../x", b"1"), Err(Error::BadKey)));
        assert!(matches!(set(&dir, "a", b"{nope"), Err(Error::NotJson(_))));
        assert!(!dir.join(DIR).exists());
    }
}
