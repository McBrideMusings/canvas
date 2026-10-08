//! Settings' routes for the instruction and reminder layers
//! (`canvas_core::instructions`): read and preview them through the same
//! `compose` the hooks call, write the person's file and a project's
//! `.canvas/` file, set the Include flag, and list the projects that have
//! posted. A project write is the one place canvasd writes into a folder the
//! person owns (ADR-0005): only the kind's fixed name under `.canvas/`, only
//! at a git root a posting session recorded, never through a symlink.

use std::path::{Path, PathBuf};

use axum::extract::{Path as UrlPath, Query, State};
use axum::http::StatusCode;
use axum::Json;
use canvas_core::instructions::{self, Kind, Overrides};
use canvas_core::log;
use serde::{Deserialize, Serialize};

use crate::state::AppState;

/// A status and the one line of text a refused request answers with.
type Refusal = (StatusCode, String);

fn kind_of(name: &str) -> Result<Kind, Refusal> {
    Kind::from_name(name).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            format!("no instruction kind named {name:?}"),
        )
    })
}

fn data_dir(state: &AppState) -> Result<&Path, Refusal> {
    state.data_dir().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "canvasd has no data dir".to_string(),
        )
    })
}

/// Logs why a write was refused and answers with it.
fn refuse(status: StatusCode, kind: Kind, layer: &str, reason: String) -> Refusal {
    log::warn(
        "instruction layer refused",
        &[
            ("kind", &kind.name()),
            ("layer", &layer),
            ("reason", &reason),
        ],
    );
    (status, reason)
}

/// A reminders text the hook can't parse would silently fall back to the
/// built-in reminders, so it is refused with the offending line.
fn check_text(kind: Kind, layer: &str, text: &str) -> Result<(), Refusal> {
    if kind == Kind::Reminders {
        if let Err(e) = canvas_core::reminders::parse(text) {
            return Err(refuse(StatusCode::BAD_REQUEST, kind, layer, e));
        }
    }
    Ok(())
}

#[derive(Deserialize)]
pub struct RootQuery {
    root: Option<PathBuf>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LayersView {
    kind: Kind,
    include: bool,
    #[serde(flatten)]
    composed: instructions::Composed,
}

/// `GET /api/instructions/:kind?root=<git root>`: the layers a session at
/// `root` reads (no project layer without one), plus the Include flag.
pub async fn get_layers(
    State(state): State<AppState>,
    UrlPath(kind): UrlPath<String>,
    Query(q): Query<RootQuery>,
) -> Result<Json<LayersView>, Refusal> {
    let kind = kind_of(&kind)?;
    let dir = state.data_dir();
    let composed = instructions::compose(kind, dir, q.root.as_deref(), &Overrides::default());
    let include = dir.map(instructions::include).unwrap_or_default().get(kind);
    Ok(Json(LayersView {
        kind,
        include,
        composed,
    }))
}

#[derive(Deserialize)]
pub struct ComposeBody {
    root: Option<PathBuf>,
    person: Option<String>,
    project: Option<String>,
}

/// `POST /api/instructions/:kind/compose`: the text a session at `root`
/// would read with `person` and `project` standing in for those files,
/// unsaved. Settings' live preview.
pub async fn compose(
    State(state): State<AppState>,
    UrlPath(kind): UrlPath<String>,
    Json(body): Json<ComposeBody>,
) -> Result<Json<instructions::Composed>, Refusal> {
    let kind = kind_of(&kind)?;
    let overrides = Overrides {
        person: body.person,
        project: body.project,
    };
    Ok(Json(instructions::compose(
        kind,
        state.data_dir(),
        body.root.as_deref(),
        &overrides,
    )))
}

#[derive(Deserialize)]
pub struct TextBody {
    text: String,
}

#[derive(Serialize)]
pub struct Written {
    path: PathBuf,
    bytes: usize,
}

/// Logs a layer write's outcome and answers it: `text` blank means the file
/// was deleted.
fn report(
    kind: Kind,
    layer: &str,
    path: &Path,
    text: &str,
    result: std::io::Result<()>,
) -> Result<Json<Written>, Refusal> {
    let blank = text.trim().is_empty();
    let bytes = if blank { 0 } else { text.len() };
    match result {
        Ok(()) => {
            log::info(
                if blank {
                    "instruction layer deleted"
                } else {
                    "instruction layer written"
                },
                &[
                    ("kind", &kind.name()),
                    ("layer", &layer),
                    ("path", &path.display()),
                    ("bytes", &bytes),
                ],
            );
            Ok(Json(Written {
                path: path.to_path_buf(),
                bytes,
            }))
        }
        Err(e) => {
            log::error(
                "instruction layer write failed",
                &[
                    ("kind", &kind.name()),
                    ("layer", &layer),
                    ("path", &path.display()),
                    ("error", &e),
                ],
            );
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("{}: {e}", path.display()),
            ))
        }
    }
}

/// `PUT /api/instructions/:kind/person` `{text}`: the person's layer in the
/// data dir; blank text deletes the file.
pub async fn put_person(
    State(state): State<AppState>,
    UrlPath(kind): UrlPath<String>,
    Json(body): Json<TextBody>,
) -> Result<Json<Written>, Refusal> {
    let kind = kind_of(&kind)?;
    check_text(kind, "person", &body.text)?;
    let dir = data_dir(&state)?;
    let path = dir.join(kind.file_name());
    let _held = state.layer_writes.lock().await;
    let result = if body.text.trim().is_empty() {
        match std::fs::remove_file(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            other => other,
        }
    } else {
        instructions::write_atomic(&path, body.text.as_bytes())
    };
    report(kind, "person", &path, &body.text, result)
}

#[derive(Deserialize)]
pub struct ProjectBody {
    root: PathBuf,
    text: String,
}

fn is_symlink(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink())
}

/// Why canvasd won't write `kind`'s file at `root`, if it won't: a root no
/// session posted from, one that is no longer a git root, or a `.canvas`
/// folder or target file that is a symlink or not what its name says. The
/// write itself never follows a link either (`crate::project_files`); these
/// checks name the reason.
fn project_refusal(state: &AppState, root: &Path, kind: Kind) -> Option<String> {
    if !state.seen_projects().iter().any(|p| p.root == root) {
        return Some(format!(
            "{} is not a project that has posted to Canvas",
            root.display()
        ));
    }
    if instructions::git_root(root).as_deref() != Some(root) {
        return Some(format!("{} is no longer a git root", root.display()));
    }
    let folder = root.join(instructions::PROJECT_DIR);
    if is_symlink(&folder) {
        return Some(format!("{} is a symlink", folder.display()));
    }
    if folder.exists() && !folder.is_dir() {
        return Some(format!("{} is not a folder", folder.display()));
    }
    let target = instructions::project_path(root, kind);
    if is_symlink(&target) {
        return Some(format!("{} is a symlink", target.display()));
    }
    if target.exists() && !target.is_file() {
        return Some(format!("{} is not a file", target.display()));
    }
    None
}

/// `PUT /api/instructions/:kind/project` `{root, text}`: the project's
/// `.canvas/<file>` at a recorded git root, creating `.canvas/` when absent;
/// blank text deletes the file.
pub async fn put_project(
    State(state): State<AppState>,
    UrlPath(kind): UrlPath<String>,
    Json(body): Json<ProjectBody>,
) -> Result<Json<Written>, Refusal> {
    let kind = kind_of(&kind)?;
    check_text(kind, "project", &body.text)?;
    let _held = state.layer_writes.lock().await;
    if let Some(reason) = project_refusal(&state, &body.root, kind) {
        return Err(refuse(StatusCode::FORBIDDEN, kind, "project", reason));
    }
    let path = instructions::project_path(&body.root, kind);
    let result = if body.text.trim().is_empty() {
        crate::project_files::remove(&body.root, kind.file_name())
    } else {
        crate::project_files::write(&body.root, kind.file_name(), body.text.as_bytes()).map(
            |created| {
                if created {
                    log::info(
                        "project instructions folder created",
                        &[
                            ("kind", &kind.name()),
                            ("path", &path.with_file_name("").display()),
                        ],
                    );
                }
            },
        )
    };
    report(kind, "project", &path, &body.text, result)
}

#[derive(Deserialize)]
pub struct IncludeBody {
    include: bool,
}

/// `PUT /api/instructions/:kind/include` `{include}`: switches the built-in
/// layer on or off; answers both kinds' flags.
pub async fn put_include(
    State(state): State<AppState>,
    UrlPath(kind): UrlPath<String>,
    Json(body): Json<IncludeBody>,
) -> Result<Json<instructions::Include>, Refusal> {
    let kind = kind_of(&kind)?;
    let dir = data_dir(&state)?;
    let _held = state.layer_writes.lock().await;
    let flags = instructions::set_include(dir, kind, body.include)
        .map_err(|e| refuse(StatusCode::INTERNAL_SERVER_ERROR, kind, "built-in", e))?;
    log::info(
        "instructions include set",
        &[
            ("kind", &kind.name()),
            ("layer", &"built-in"),
            ("include", &body.include),
            ("path", &dir.join(kind.include_file()).display()),
        ],
    );
    Ok(Json(flags))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectView {
    #[serde(flatten)]
    project: instructions::SeenProject,
    /// Whether `.canvas/instructions.md` exists at the root.
    instructions: bool,
    /// Whether `.canvas/reminders.txt` exists at the root.
    reminders: bool,
}

/// `GET /api/projects`: every recorded git root, newest first, with whether
/// it has each layer file.
pub async fn list_projects(State(state): State<AppState>) -> Json<Vec<ProjectView>> {
    let views: Vec<ProjectView> = state
        .seen_projects()
        .into_iter()
        .map(|project| ProjectView {
            instructions: instructions::project_path(&project.root, Kind::Instructions).is_file(),
            reminders: instructions::project_path(&project.root, Kind::Reminders).is_file(),
            project,
        })
        .collect();
    Json(views)
}
