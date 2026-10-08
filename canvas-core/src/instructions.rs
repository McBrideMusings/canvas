//! Instructions (what an agent reads at session start) and post reminders
//! (which finished turns prompt a reminder to post) each come from up to three
//! layers, read in this order:
//!
//! 1. Canvas's built-in text, compiled in from `plugin/`, unless the person
//!    has switched its Include flag off;
//! 2. the person's own file in the data dir (`instructions.md`,
//!    `reminders.txt`);
//! 3. the project's file under `.canvas/` at the git root of the working
//!    directory (none outside git).
//!
//! `compose` joins them with one blank line, skipping a missing or empty
//! layer. The hooks, `canvas instructions` and canvasd all call it, so what
//! Settings previews is what a session reads.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const BUILTIN_INSTRUCTIONS: &str = include_str!("../../plugin/instructions.md");
pub const BUILTIN_REMINDERS: &str = include_str!("../../plugin/reminders.txt");

/// The file in the data dir that holds the Include flag per kind.
pub const INCLUDE_FILE: &str = "instructions-include.json";

/// The file in the data dir listing the projects that have posted.
pub const PROJECTS_FILE: &str = "projects.json";

/// The folder at a git root that holds a project's layers.
pub const PROJECT_DIR: &str = ".canvas";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Instructions,
    Reminders,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Instructions => "instructions",
            Kind::Reminders => "reminders",
        }
    }

    pub fn from_name(name: &str) -> Option<Kind> {
        match name {
            "instructions" => Some(Kind::Instructions),
            "reminders" => Some(Kind::Reminders),
            _ => None,
        }
    }

    /// The layer's file name, the same in the data dir and under `.canvas/`.
    pub fn file_name(self) -> &'static str {
        match self {
            Kind::Instructions => "instructions.md",
            Kind::Reminders => "reminders.txt",
        }
    }

    pub fn builtin(self) -> &'static str {
        match self {
            Kind::Instructions => BUILTIN_INSTRUCTIONS,
            Kind::Reminders => BUILTIN_REMINDERS,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Source {
    #[serde(rename = "built-in")]
    BuiltIn,
    #[serde(rename = "person")]
    Person,
    #[serde(rename = "project")]
    Project,
}

/// One layer as it went into the composed text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Layer {
    /// "Built-in", "Yours", or the git root's folder name.
    pub name: String,
    pub source: Source,
    /// The file it was read from; `None` for the built-in text.
    pub path: Option<PathBuf>,
    /// The text as joined, trailing whitespace trimmed.
    pub text: String,
    /// The composed text's line this layer starts on, counting from 1.
    pub start_line: usize,
    pub lines: usize,
    pub chars: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Composed {
    pub text: String,
    pub layers: Vec<Layer>,
}

/// Unsaved text standing in for a layer's file (Settings' preview). `None`
/// reads the file.
#[derive(Debug, Clone, Default)]
pub struct Overrides {
    pub person: Option<String>,
    pub project: Option<String>,
}

/// The person's Include-built-in switch per kind; a missing file, or a
/// missing field, means on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Include {
    #[serde(default = "on")]
    pub instructions: bool,
    #[serde(default = "on")]
    pub reminders: bool,
}

fn on() -> bool {
    true
}

impl Default for Include {
    fn default() -> Self {
        Include {
            instructions: true,
            reminders: true,
        }
    }
}

impl Include {
    pub fn get(self, kind: Kind) -> bool {
        match kind {
            Kind::Instructions => self.instructions,
            Kind::Reminders => self.reminders,
        }
    }
}

/// One checkout that has posted to Canvas, as canvasd records it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SeenProject {
    pub root: PathBuf,
    pub name: String,
    pub last_seen: String,
}

/// The Include flags in `data_dir`; on for both when the file is missing. A
/// file that can't be read or parsed also reads as on, and logs why.
pub fn include(data_dir: &Path) -> Include {
    read_include(data_dir).unwrap_or_else(|why| {
        crate::log::warn("include flags unreadable, using on", &[("error", &why)]);
        Include::default()
    })
}

fn read_include(data_dir: &Path) -> Result<Include, String> {
    let path = data_dir.join(INCLUDE_FILE);
    match std::fs::read_to_string(&path) {
        Ok(raw) => serde_json::from_str(&raw).map_err(|e| format!("{}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Include::default()),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// Sets one kind's Include flag, writing through a temp file and a rename.
/// Refuses to overwrite a file it can't read, which would reset the other
/// kind's flag.
pub fn set_include(data_dir: &Path, kind: Kind, value: bool) -> Result<Include, String> {
    let mut flags = read_include(data_dir)?;
    match kind {
        Kind::Instructions => flags.instructions = value,
        Kind::Reminders => flags.reminders = value,
    }
    let path = data_dir.join(INCLUDE_FILE);
    let json = serde_json::to_string_pretty(&flags).map_err(|e| e.to_string())?;
    write_atomic(&path, json.as_bytes()).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(flags)
}

/// The projects canvasd has recorded, newest first as stored; empty when
/// none have been.
pub fn seen_projects(data_dir: &Path) -> Vec<SeenProject> {
    std::fs::read_to_string(data_dir.join(PROJECTS_FILE))
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

/// Writes `bytes` to a temp file beside `path`, then renames it into place.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let tmp = path.with_file_name(format!(".{name}.{}.tmp", std::process::id()));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// The nearest folder at or above `dir` holding a `.git` entry (a folder, or
/// a worktree's file); `None` outside git, and for a relative or empty path,
/// which would resolve against the caller's own directory.
pub fn git_root(dir: &Path) -> Option<PathBuf> {
    if !dir.is_absolute() {
        return None;
    }
    dir.ancestors()
        .find(|d| d.join(".git").exists())
        .map(Path::to_path_buf)
}

/// The project layer's file for `kind` at `root`.
pub fn project_path(root: &Path, kind: Kind) -> PathBuf {
    root.join(PROJECT_DIR).join(kind.file_name())
}

/// Joins the layers in effect for `kind` at `cwd`. `data_dir` `None` (no
/// HOME) means no person layer and the built-in included; `cwd` `None`, or a
/// directory outside git, means no project layer. An override applies only
/// to a layer that exists here: none for the person without a data dir, none
/// for the project outside git.
pub fn compose(
    kind: Kind,
    data_dir: Option<&Path>,
    cwd: Option<&Path>,
    overrides: &Overrides,
) -> Composed {
    let mut pieces: Vec<(String, Source, Option<PathBuf>, String)> = Vec::new();
    let flags = data_dir.map(include).unwrap_or_default();
    if flags.get(kind) {
        pieces.push((
            "Built-in".into(),
            Source::BuiltIn,
            None,
            kind.builtin().into(),
        ));
    }
    if let Some(dir) = data_dir {
        let path = dir.join(kind.file_name());
        let text = overrides.person.clone().unwrap_or_else(|| read(&path));
        pieces.push(("Yours".into(), Source::Person, Some(path), text));
    }
    if let Some(root) = cwd.and_then(git_root) {
        let path = project_path(&root, kind);
        let text = overrides.project.clone().unwrap_or_else(|| read(&path));
        let name = root
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| root.display().to_string());
        pieces.push((name, Source::Project, Some(path), text));
    }

    let mut composed = Composed::default();
    let mut next_line = 1;
    for (name, source, path, text) in pieces {
        let text = text.trim_end().to_string();
        if text.trim().is_empty() {
            continue;
        }
        if !composed.text.is_empty() {
            composed.text.push_str("\n\n");
            next_line += 1;
        }
        let lines = text.lines().count();
        composed.text.push_str(&text);
        composed.layers.push(Layer {
            name,
            source,
            path,
            chars: text.chars().count(),
            text,
            start_line: next_line,
            lines,
        });
        next_line += lines;
    }
    if !composed.text.is_empty() {
        composed.text.push('\n');
    }
    composed
}

/// A missing or unreadable file reads as an empty layer.
fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

/// The reminders in effect at `cwd`, parsed from the composed layers top to
/// bottom. When the person's or the project's layer doesn't parse, the
/// built-in reminders apply instead and the second value names the layer,
/// its file and the offending line.
pub fn reminders(
    data_dir: Option<&Path>,
    cwd: Option<&Path>,
) -> (crate::reminders::Reminders, Option<String>) {
    let composed = compose(Kind::Reminders, data_dir, cwd, &Overrides::default());
    match check_reminders(&composed) {
        Ok(()) => (
            crate::reminders::parse(&composed.text).unwrap_or_default(),
            None,
        ),
        Err(why) => (
            crate::reminders::parse(BUILTIN_REMINDERS).unwrap_or_default(),
            Some(why),
        ),
    }
}

/// Checks each layer on its own, so an error's line number is the file's. A
/// directive parses the same alone as joined, so this is the joined text's
/// verdict too.
pub fn check_reminders(composed: &Composed) -> Result<(), String> {
    for layer in &composed.layers {
        if let Err(e) = crate::reminders::parse(&layer.text) {
            let place = layer
                .path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| layer.name.clone());
            return Err(format!("{place}: {e}"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Dirs {
        base: PathBuf,
        data: PathBuf,
        repo: PathBuf,
        outside: PathBuf,
    }

    impl Drop for Dirs {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.base);
        }
    }

    fn dirs(name: &str) -> Dirs {
        let base =
            std::env::temp_dir().join(format!("canvas-compose-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let data = base.join("data");
        let repo = base.join("repo");
        let outside = base.join("outside");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::create_dir_all(repo.join("src/deep")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        Dirs {
            base,
            data,
            repo,
            outside,
        }
    }

    fn sources(c: &Composed) -> Vec<Source> {
        c.layers.iter().map(|l| l.source).collect()
    }

    #[test]
    fn built_in_then_person_then_project_with_one_blank_line_between() {
        let d = dirs("order");
        std::fs::write(d.data.join("instructions.md"), "mine\n\n\n").unwrap();
        std::fs::create_dir_all(d.repo.join(".canvas")).unwrap();
        std::fs::write(d.repo.join(".canvas/instructions.md"), "theirs\nline 2\n").unwrap();

        let c = compose(
            Kind::Instructions,
            Some(&d.data),
            Some(&d.repo.join("src/deep")),
            &Overrides::default(),
        );
        let builtin = BUILTIN_INSTRUCTIONS.trim_end();
        assert_eq!(c.text, format!("{builtin}\n\nmine\n\ntheirs\nline 2\n"));
        assert_eq!(
            sources(&c),
            [Source::BuiltIn, Source::Person, Source::Project]
        );
        let n = builtin.lines().count();
        assert_eq!(c.layers[1].start_line, n + 2);
        assert_eq!(c.layers[2].start_line, n + 4);
        assert_eq!((c.layers[2].lines, c.layers[2].chars), (2, 13));
        assert_eq!(c.layers[2].name, "repo");
        assert_eq!(
            c.layers[2].path,
            Some(d.repo.join(".canvas/instructions.md"))
        );
    }

    #[test]
    fn include_off_drops_the_built_in_and_the_person_starts_at_line_1() {
        let d = dirs("off");
        std::fs::write(d.data.join("instructions.md"), "mine\n").unwrap();
        set_include(&d.data, Kind::Instructions, false).unwrap();

        let c = compose(
            Kind::Instructions,
            Some(&d.data),
            Some(&d.outside),
            &Overrides::default(),
        );
        assert_eq!(c.text, "mine\n");
        assert_eq!(c.layers[0].start_line, 1);
        assert!(include(&d.data).reminders, "the other kind stays on");
    }

    #[test]
    fn an_empty_or_missing_layer_is_skipped_and_outside_git_has_no_project() {
        let d = dirs("empty");
        std::fs::write(d.data.join("instructions.md"), "  \n\n").unwrap();
        let c = compose(
            Kind::Instructions,
            Some(&d.data),
            Some(&d.outside),
            &Overrides::default(),
        );
        assert_eq!(sources(&c), [Source::BuiltIn]);
        assert_eq!(c.text, BUILTIN_INSTRUCTIONS);

        let c = compose(
            Kind::Instructions,
            Some(&d.data),
            Some(&d.repo),
            &Overrides::default(),
        );
        assert_eq!(
            sources(&c),
            [Source::BuiltIn],
            "a repo with no .canvas file"
        );
    }

    #[test]
    fn a_relative_or_empty_cwd_has_no_project_layer() {
        assert_eq!(git_root(Path::new("")), None);
        assert_eq!(git_root(Path::new("src")), None);
    }

    #[test]
    fn a_corrupt_include_file_reads_as_on_and_is_never_overwritten() {
        let d = dirs("corrupt");
        std::fs::write(d.data.join(INCLUDE_FILE), "{not json").unwrap();
        assert_eq!(include(&d.data), Include::default());
        assert!(set_include(&d.data, Kind::Reminders, false).is_err());
        assert_eq!(
            std::fs::read_to_string(d.data.join(INCLUDE_FILE)).unwrap(),
            "{not json"
        );
    }

    #[test]
    fn overrides_stand_in_for_the_files() {
        let d = dirs("over");
        std::fs::write(d.data.join("instructions.md"), "saved\n").unwrap();
        let over = Overrides {
            person: Some("unsaved".into()),
            project: Some("draft".into()),
        };
        let c = compose(Kind::Instructions, Some(&d.data), Some(&d.repo), &over);
        assert!(c.text.ends_with("\n\nunsaved\n\ndraft\n"), "{}", c.text);
    }

    #[test]
    fn a_project_no_image_cancels_the_built_in_image_in_that_repo_only() {
        let d = dirs("cancel");
        std::fs::create_dir_all(d.repo.join(".canvas")).unwrap();
        std::fs::write(d.repo.join(".canvas/reminders.txt"), "no image\n").unwrap();

        let (here, why) = reminders(Some(&d.data), Some(&d.repo));
        assert!(!here.image && why.is_none());
        let (elsewhere, _) = reminders(Some(&d.data), Some(&d.outside));
        assert!(elsewhere.image);
    }

    #[test]
    fn a_layer_that_does_not_parse_falls_back_to_the_built_in_and_names_its_line() {
        let d = dirs("bad");
        std::fs::write(d.data.join("reminders.txt"), "no image\nfile\nlinks many\n").unwrap();
        let (r, why) = reminders(Some(&d.data), Some(&d.outside));
        assert!(r.image && !r.file, "built-in reminders apply");
        let why = why.unwrap();
        assert!(why.contains("reminders.txt: line 3:"), "{why}");
    }
}
