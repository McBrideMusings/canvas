//! The artifact routes: `/api/artifacts` for the records, `/artifacts/:id/…`
//! for the files a pane loads. See [`crate::artifacts`].

use std::path::PathBuf;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use canvas_core::{
    ArtifactSource, NewArtifactRequest, OpenLinkRequest, PutArtifactRequest, RelinkArtifactRequest,
    ScriptErrorReport,
};

use crate::artifact_state;
use crate::artifacts::{self, Artifacts};
use crate::provenance::{Action, Actor};
use crate::routes::MAX_DATA_BYTES;
use crate::state::{AppState, CanvasEvent, PaneAction, PaneReport};

const NO_DATA_DIR: &str = "canvasd has no data directory to keep artifacts in";
const NO_ARTIFACT: &str = "no artifact with that id";

fn no_data_dir() -> Response {
    (StatusCode::SERVICE_UNAVAILABLE, NO_DATA_DIR).into_response()
}

fn not_found() -> Response {
    (StatusCode::NOT_FOUND, NO_ARTIFACT).into_response()
}

fn bad_request(message: String) -> Response {
    (StatusCode::BAD_REQUEST, message).into_response()
}

/// The refresh a log line names: its interval, directory and command.
fn refresh_text(view: &canvas_core::ArtifactView) -> String {
    match &view.artifact.refresh {
        Some(r) => format!("every {}s in {}: {}", r.every_secs, r.cwd, r.command),
        None => "none".to_string(),
    }
}

fn failed(what: &str, id: &str, e: &dyn std::fmt::Display) -> Response {
    canvas_core::log::error(what, &[("id", &id), ("error", e)]);
    (StatusCode::INTERNAL_SERVER_ERROR, format!("{what}: {e}")).into_response()
}

/// Writes the records and tells every viewer about the artifact, with the
/// files a change wrote when it knows them. A failed save is an error: an
/// artifact that would vanish on restart must not look created.
pub(crate) async fn save_and_publish(
    state: &AppState,
    artifacts: &Artifacts,
    id: &str,
    changed: Option<Vec<String>>,
) -> Result<canvas_core::ArtifactView, Response> {
    artifacts
        .save()
        .await
        .map_err(|e| failed("saving artifacts.json failed", id, &e))?;
    let record = artifacts.records.get(id).ok_or_else(not_found)?;
    let view = artifacts.view(record);
    state.publish(CanvasEvent::ArtifactUpserted(Box::new(
        canvas_core::ArtifactView {
            changed: changed.filter(|paths| paths.len() <= canvas_core::MAX_CHANGED_PATHS),
            ..view.clone()
        },
    )));
    Ok(view)
}

/// `canvas artifact list`: every artifact, most recently changed first.
pub async fn list_artifacts(State(state): State<AppState>) -> Response {
    Json(state.artifacts.read().await.views()).into_response()
}

/// `canvas artifact new`: mints an `art-` id and answers the record with
/// its path. With `link`, the artifact points at that existing folder or
/// HTML file; otherwise canvasd makes it an empty folder of its own.
pub async fn new_artifact(
    State(state): State<AppState>,
    actor: Actor,
    body: Option<Json<NewArtifactRequest>>,
) -> Response {
    let NewArtifactRequest {
        title,
        link,
        extras,
    } = body.map(|Json(b)| b).unwrap_or_default();
    let source = match link {
        Some(link) => match artifacts::check_link(&link) {
            Ok(_) => ArtifactSource::Linked { link },
            Err(message) => return bad_request(message),
        },
        None => ArtifactSource::Owned,
    };
    let owned = source == ArtifactSource::Owned;
    let mut artifacts = state.artifacts.write().await;
    if !artifacts.has_data_dir() {
        return no_data_dir();
    }
    let mut record = artifacts.new_record(title, source);
    if let Err(message) = artifacts::apply_extras(&mut record, extras, actor.pid) {
        return bad_request(message);
    }
    let Some(root) = artifacts.source_path(&record) else {
        return no_data_dir();
    };
    if owned {
        if let Err(e) = tokio::fs::create_dir_all(&root).await {
            return failed("creating the artifact folder failed", &record.id, &e);
        }
    }
    let id = record.id.clone();
    artifacts.records.insert(id.clone(), record);
    match save_and_publish(&state, &artifacts, &id, None).await {
        Ok(view) => {
            artifacts
                .fingerprints
                .insert(id.clone(), artifacts::fingerprint(&root));
            state.watcher.watch(&id, &root);
            artifacts.log_action(&id, Action::Create, &actor).await;
            canvas_core::log::info(
                "artifact created",
                &[
                    ("id", &id),
                    ("kind", &if owned { "owned" } else { "linked" }),
                    ("path", &view.path),
                    ("widget", &view.artifact.widget_html.is_some()),
                    ("refresh", &refresh_text(&view)),
                ],
            );
            Json(view).into_response()
        }
        Err(response) => {
            artifacts.records.remove(&id);
            if owned {
                let _ = tokio::fs::remove_dir_all(&root).await;
            }
            response
        }
    }
}

/// `canvas artifact relink`: points a linked artifact at a new folder or
/// HTML file, keeping its id and record, and watches the new path.
pub async fn relink_artifact(
    State(state): State<AppState>,
    Path(id): Path<String>,
    actor: Actor,
    Json(req): Json<RelinkArtifactRequest>,
) -> Response {
    let root = match artifacts::check_link(&req.link) {
        Ok(root) => root,
        Err(message) => return bad_request(message),
    };
    let mut artifacts = state.artifacts.write().await;
    let Some(record) = artifacts.records.get_mut(&id) else {
        return not_found();
    };
    let ArtifactSource::Linked { link } = &mut record.source else {
        return bad_request(format!(
            "{id} is an owned artifact; only a linked artifact can be relinked"
        ));
    };
    let from = std::mem::replace(link, req.link.clone());
    let was_updated = std::mem::replace(&mut record.updated_at, chrono::Utc::now().to_rfc3339());
    let print = artifacts::fingerprint(&root);
    let was_print = artifacts.fingerprints.insert(id.clone(), print);
    match save_and_publish(&state, &artifacts, &id, None).await {
        Ok(view) => {
            state.watcher.watch(&id, &root);
            artifacts.log_action(&id, Action::Relink, &actor).await;
            canvas_core::log::info(
                "artifact relinked",
                &[("id", &id), ("from", &from), ("to", &view.path)],
            );
            Json(view).into_response()
        }
        Err(response) => {
            // Back to the old path, its stamp and its fingerprint, so the
            // watcher keeps comparing that path against its own baseline.
            if let Some(record) = artifacts.records.get_mut(&id) {
                record.source = ArtifactSource::Linked { link: from };
                record.updated_at = was_updated;
            }
            match was_print {
                Some(print) => artifacts.fingerprints.insert(id.clone(), print),
                None => artifacts.fingerprints.remove(&id),
            };
            response
        }
    }
}

/// `canvas artifact show`: the record plus its folder, entry page,
/// declared size and the newest errors its page threw.
pub async fn get_artifact(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let artifacts = state.artifacts.read().await;
    match artifacts.records.get(&id) {
        Some(record) => Json(artifacts.view_with_errors(record)).into_response(),
        None => not_found(),
    }
}

/// `canvas artifact put`: copies a file or a folder's contents into the
/// artifact and stamps `updatedAt`; every viewer showing it reloads.
pub async fn put_artifact(
    State(state): State<AppState>,
    Path(id): Path<String>,
    actor: Actor,
    Json(req): Json<PutArtifactRequest>,
) -> Response {
    let source = PathBuf::from(&req.source);
    if !source.is_absolute() {
        return (StatusCode::BAD_REQUEST, "source must be an absolute path").into_response();
    }
    // The widget and refresh are checked before anything is copied, and set
    // once the copy is done.
    let (folder, extras) = {
        let artifacts = state.artifacts.read().await;
        let Some(record) = artifacts.records.get(&id) else {
            return not_found();
        };
        if let ArtifactSource::Linked { link } = &record.source {
            return bad_request(format!(
                "{id} is linked to {link}; save its files there instead"
            ));
        }
        let mut checked = record.clone();
        let widget_given = req.extras.widget_html.is_some();
        let refresh_given = req.extras.refresh.is_some();
        if let Err(message) = artifacts::apply_extras(&mut checked, req.extras, actor.pid) {
            return bad_request(message);
        }
        match artifacts.folder(&id) {
            Some(folder) => (
                folder,
                (
                    checked.widget_html.filter(|_| widget_given),
                    checked.refresh.filter(|_| refresh_given),
                ),
            ),
            None => return no_data_dir(),
        }
    };
    let Ok(resolved) = source.canonicalize() else {
        return (
            StatusCode::BAD_REQUEST,
            format!("no file or folder at {}", source.display()),
        )
            .into_response();
    };
    // Copying the folder onto itself truncates every file; copying a folder
    // that holds it recurses into its own copy.
    if let Ok(own) = folder.canonicalize() {
        if resolved.starts_with(&own) || own.starts_with(&resolved) {
            return (
                StatusCode::BAD_REQUEST,
                format!(
                    "{} is the artifact's own folder or contains it; its files are already there",
                    source.display()
                ),
            )
                .into_response();
        }
    }
    // The copy runs without the lock, so a large tree never stalls the
    // viewers' state loads and page requests. The fingerprint taken after it
    // tells the watcher these writes are already stamped; the hold keeps
    // their burst open until then, however long the copy takes.
    let _hold = state.watcher.hold(&id);
    let copied = tokio::task::spawn_blocking(move || {
        artifacts::copy_into(&resolved, &folder).map(|w| (w, artifacts::fingerprint(&folder)))
    })
    .await;
    let (written, print) = match copied {
        Ok(Ok(done)) => done,
        Ok(Err(e)) => return failed("copying into the artifact failed", &id, &e),
        Err(e) => return failed("copying into the artifact failed", &id, &e),
    };
    let mut artifacts = state.artifacts.write().await;
    let Some(record) = artifacts.records.get_mut(&id) else {
        // Deleted while the copy ran.
        return not_found();
    };
    record.updated_at = chrono::Utc::now().to_rfc3339();
    // Only what this put gave replaces the record's own, so a put that ran
    // alongside this one keeps what it set.
    let (widget_html, refresh) = extras;
    if widget_html.is_some() {
        record.widget_html = widget_html;
    }
    let refresh_given = refresh.is_some();
    if refresh_given {
        record.refresh = refresh;
        // The error belonged to the command this one replaces.
        artifacts.refresh_errors.remove(&id);
    }
    artifacts.fingerprints.insert(id.clone(), print);
    let files = written.len();
    match save_and_publish(&state, &artifacts, &id, Some(written)).await {
        Ok(view) => {
            artifacts.log_action(&id, Action::Put, &actor).await;
            canvas_core::log::info(
                "artifact put",
                &[
                    ("id", &id),
                    ("source", &source.display()),
                    ("files", &files),
                    ("widget", &view.artifact.widget_html.is_some()),
                    ("refresh", &refresh_text(&view)),
                ],
            );
            Json(view).into_response()
        }
        Err(response) => response,
    }
}

/// `canvas artifact delete`: removes the record, and an owned artifact's
/// folder. A linked artifact's files belong to the person and stay.
pub async fn delete_artifact(
    State(state): State<AppState>,
    Path(id): Path<String>,
    actor: Actor,
) -> Response {
    let mut artifacts = state.artifacts.write().await;
    let Some(record) = artifacts.records.remove(&id) else {
        return not_found();
    };
    if let Err(e) = artifacts.save().await {
        artifacts.records.insert(id.clone(), record);
        return failed("saving artifacts.json failed", &id, &e);
    }
    state.watcher.unwatch(&id);
    artifacts.forget(&id);
    let owned_folder = match record.source {
        ArtifactSource::Owned => artifacts.folder(&id),
        ArtifactSource::Linked { .. } => None,
    };
    if let Some(folder) = owned_folder {
        if let Err(e) = tokio::fs::remove_dir_all(&folder).await {
            if e.kind() != std::io::ErrorKind::NotFound {
                canvas_core::log::warn(
                    "artifact folder not removed",
                    &[("id", &id), ("error", &e)],
                );
            }
        }
    }
    artifacts.log_action(&id, Action::Delete, &actor).await;
    state.publish(CanvasEvent::ArtifactRemoved(id.clone()));
    canvas_core::log::info("artifact deleted", &[("id", &id)]);
    Json(serde_json::json!({ "deleted": id })).into_response()
}

/// `canvas artifact log`: who created and changed the artifact, oldest
/// first. The lines outlive the artifact, so a deleted id still answers; an
/// id with no record and no lines is a 404.
pub async fn artifact_log(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let artifacts = state.artifacts.read().await;
    match artifacts.log_of(&id).await {
        None => no_data_dir(),
        Some(Err(e)) => failed("reading the artifact log failed", &id, &e),
        Some(Ok(lines)) if lines.is_empty() && !artifacts.records.contains_key(&id) => not_found(),
        Some(Ok(lines)) => Json(lines).into_response(),
    }
}

/// `canvas data art-…`: pushes one JSON value into the artifact's widget and
/// page, the way its refresh command's output arrives. The latest value is
/// kept in memory, so a viewer that loads later, or a frame that reloads,
/// gets it. Answers how many viewers the value reached; 404 for an unknown
/// id; 413 past [`MAX_DATA_BYTES`].
pub async fn put_artifact_data(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(value): Json<serde_json::Value>,
) -> Response {
    let size = serde_json::to_vec(&value).map(|b| b.len()).unwrap_or(0);
    if size > MAX_DATA_BYTES {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    }
    let mut artifacts = state.artifacts.write().await;
    if !artifacts.records.contains_key(&id) {
        return not_found();
    }
    artifacts.data.insert(id.clone(), value.clone());
    let viewers = state.publish(CanvasEvent::ArtifactData {
        id: id.clone(),
        value,
    });
    canvas_core::log::info(
        "artifact data",
        &[("id", &id), ("bytes", &size), ("viewers", &viewers)],
    );
    Json(serde_json::json!({ "viewers": viewers })).into_response()
}

/// The viewer relays one uncaught error or unhandled rejection from the
/// pane showing `id`; `canvas artifact show` lists the newest ones.
pub async fn report_script_error(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(report): Json<ScriptErrorReport>,
) -> Response {
    let mut artifacts = state.artifacts.write().await;
    if !artifacts.records.contains_key(&id) {
        return not_found();
    }
    canvas_core::log::warn(
        "artifact script error",
        &[
            ("id", &id),
            ("kind", &format!("{:?}", report.kind)),
            ("message", &report.message),
            ("source", &report.source.as_deref().unwrap_or("")),
            ("line", &report.line.unwrap_or(0)),
            ("column", &report.column.unwrap_or(0)),
        ],
    );
    artifacts.record_script_error(&id, report);
    StatusCode::NO_CONTENT.into_response()
}

/// The viewer relays a click on an `http(s)` link inside the pane showing
/// `id`: canvasd keeps it for `canvas artifact show` and opens it in the default
/// browser. Anything else, or a URL over 2KB, is a 400; a second open within
/// a second of the last is a 429.
pub async fn open_artifact_link(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<OpenLinkRequest>,
) -> Response {
    if !state.artifacts.read().await.records.contains_key(&id) {
        return not_found();
    }
    let url = req.url;
    if !(url.starts_with("http://") || url.starts_with("https://"))
        || url.len() > 2048
        || url.chars().any(char::is_control)
    {
        return bad_request("only an http(s) link of at most 2048 bytes opens".to_string());
    }
    if !state
        .artifacts
        .write()
        .await
        .record_opened_link(&id, url.clone())
    {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    }
    if !crate::routes::open_target(&url).await {
        canvas_core::log::warn("artifact link open failed", &[("id", &id), ("url", &url)]);
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    canvas_core::log::info("artifact link opened", &[("id", &id), ("url", &url)]);
    StatusCode::NO_CONTENT.into_response()
}

/// `canvas focus art-…`: asks every open viewer to switch to the Artifacts
/// page and open the artifact. Answers how many viewers the event reached, so
/// the CLI can fail when nobody saw it.
pub async fn focus_artifact(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    if !state.artifacts.read().await.records.contains_key(&id) {
        return not_found();
    }
    let viewers = state.publish(CanvasEvent::ArtifactFocus(id.clone()));
    canvas_core::log::info("artifact focus", &[("id", &id), ("viewers", &viewers)]);
    Json(serde_json::json!({ "viewers": viewers })).into_response()
}

/// `canvas artifact reset` and the pane menu's Reset: drops the artifact's
/// held `data` and `scriptErrors`, then asks every open viewer to rebuild its
/// pane at the entry page. Its files, pane size and full window stay. Answers
/// how many viewers the event reached.
pub async fn reset_artifact(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    {
        let mut artifacts = state.artifacts.write().await;
        if !artifacts.records.contains_key(&id) {
            return not_found();
        }
        artifacts.reset(&id);
    }
    let viewers = state.publish(CanvasEvent::ArtifactReset(id.clone()));
    canvas_core::log::info("artifact reset", &[("id", &id), ("viewers", &viewers)]);
    Json(serde_json::json!({ "viewers": viewers })).into_response()
}

/// `canvas snapshot art-…`: asks the open viewers to capture the artifact's
/// pane as rendered, answered like a card's snapshot. 404 for an unknown id.
pub async fn snapshot_artifact(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    if !state.artifacts.read().await.records.contains_key(&id) {
        return not_found();
    }
    crate::routes::snapshot(&state, crate::routes::Snapshotted::Artifact, id).await
}

/// `canvas artifact pane <id> --size|--reset|--full|--exit`: asks every open
/// viewer to open the artifact and change its pane as a drag of the grip, a
/// double-click on it, or the full-window control would. Answers how many
/// viewers the event reached.
pub async fn pane_artifact(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(action): Json<PaneAction>,
) -> Response {
    if let PaneAction::Resize { width, height } = action {
        if width == 0 || height == 0 {
            return bad_request("a pane size needs a width and height above 0".to_string());
        }
    }
    if !state.artifacts.read().await.records.contains_key(&id) {
        return not_found();
    }
    let viewers = state.publish(CanvasEvent::ArtifactPane {
        id: id.clone(),
        action,
    });
    canvas_core::log::info(
        "artifact pane",
        &[
            ("id", &id),
            ("action", &format!("{action:?}")),
            ("viewers", &viewers),
        ],
    );
    Json(serde_json::json!({ "viewers": viewers })).into_response()
}

/// The viewer reports the pane it shows after every change to it.
pub async fn report_pane(
    State(state): State<AppState>,
    Json(report): Json<PaneReport>,
) -> Response {
    canvas_core::log::info(
        "viewer pane",
        &[
            ("id", &report.id),
            ("width", &report.width),
            ("height", &report.height),
            ("full", &report.full),
            ("chosen", &format!("{:?}", report.chosen)),
        ],
    );
    *state.viewer_pane.lock().unwrap_or_else(|e| e.into_inner()) = Some(report);
    StatusCode::NO_CONTENT.into_response()
}

/// The viewer stopped showing an artifact pane (the Timeline, or no
/// artifact left to show).
pub async fn clear_pane(State(state): State<AppState>) -> Response {
    canvas_core::log::info("viewer pane cleared", &[]);
    *state.viewer_pane.lock().unwrap_or_else(|e| e.into_inner()) = None;
    StatusCode::NO_CONTENT.into_response()
}

/// Bare `canvas artifact pane`: the pane a viewer last reported, `null`
/// before any has or while no viewer is open.
pub async fn get_pane(State(state): State<AppState>) -> Response {
    let reported = state
        .viewer_pane
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let pane = reported.filter(|_| state.events.receiver_count() > 0);
    Json(serde_json::json!({ "pane": pane })).into_response()
}

/// `/artifacts/:id/` — the artifact's entry page.
pub async fn artifact_entry(state: State<AppState>, Path(id): Path<String>) -> Response {
    serve_file(state, id, String::new()).await
}

/// `/artifacts/:id/*path` — one file of the artifact, confined to its folder
/// (or, for a linked HTML file, to that file).
pub async fn artifact_file(
    state: State<AppState>,
    Path((id, path)): Path<(String, String)>,
) -> Response {
    serve_file(state, id, path).await
}

async fn serve_file(State(state): State<AppState>, id: String, rel: String) -> Response {
    let folder = {
        let artifacts = state.artifacts.read().await;
        let Some(record) = artifacts.records.get(&id) else {
            return not_found();
        };
        match artifacts.source_path(record) {
            Some(folder) => folder,
            None => return no_data_dir(),
        }
    };
    let file = match artifacts::resolve_file(&folder, &rel) {
        Ok(file) => file,
        Err(refusal) => {
            canvas_core::log::warn(
                "artifact file refused",
                &[("id", &id), ("path", &rel), ("reason", &refusal.as_str())],
            );
            return (StatusCode::NOT_FOUND, refusal.as_str()).into_response();
        }
    };
    let bytes = match tokio::fs::read(&file).await {
        Ok(bytes) => bytes,
        Err(e) => return failed("reading an artifact file failed", &id, &e),
    };
    let mime = mime_guess::from_path(&file).first_or_octet_stream();
    let bytes = if mime.essence_str() == "text/html" {
        // The freeze scan costs about 20ms per MB of page, so it runs off the
        // runtime's workers.
        let page = tokio::task::spawn_blocking(move || {
            let freeze = canvas_core::html::webkit_freeze_page_reason(&bytes);
            match freeze {
                Some(reason) => Err(reason),
                None => Ok(artifacts::served_page(&bytes)),
            }
        })
        .await;
        match page {
            Ok(Ok(page)) => page,
            Ok(Err(reason)) => return refused_page(&id, &rel, &reason),
            Err(e) => return failed("serving an artifact page failed", &id, &e),
        }
    } else {
        bytes
    };
    let content_type = match (mime.type_(), mime.subtype().as_str()) {
        (mime_guess::mime::TEXT, _) | (_, "javascript" | "json" | "xml" | "svg+xml") => {
            format!("{}; charset=utf-8", mime.essence_str())
        }
        _ => mime.essence_str().to_string(),
    };
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, "no-store".to_string()),
            (
                header::CONTENT_SECURITY_POLICY,
                artifacts::content_security_policy(&id),
            ),
            // The pane's sandboxed page has an opaque origin, so its module
            // scripts load cross-origin and need this to run.
            (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*".to_string()),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_string()),
        ],
        bytes,
    )
        .into_response()
}

/// The folder `id`'s state sits in (see [`artifact_state`]).
async fn state_source(state: &AppState, id: &str) -> Result<PathBuf, Response> {
    let artifacts = state.artifacts.read().await;
    let Some(record) = artifacts.records.get(id) else {
        return Err(not_found());
    };
    artifacts.source_path(record).ok_or_else(no_data_dir)
}

/// Answers a state error as its status and text, logging a refusal.
fn state_refused(id: &str, key: &str, e: &artifact_state::Error) -> Response {
    let status = StatusCode::from_u16(e.status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    canvas_core::log::warn(
        "artifact state refused",
        &[("id", &id), ("key", &key), ("reason", e)],
    );
    (status, e.to_string()).into_response()
}

/// Runs a blocking state operation off the async threads.
async fn state_blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, artifact_state::Error> + Send + 'static,
) -> Result<T, artifact_state::Error> {
    tokio::task::spawn_blocking(f)
        .await
        .unwrap_or_else(|e| Err(artifact_state::Error::Io(std::io::Error::other(e))))
}

/// `PUT /api/artifacts/:id/state/:key`: stores the request body, one JSON
/// value, under `key` (the viewer's relay of `canvas-state-set`). Writes
/// reload nothing: the watcher ignores `canvas-data/`.
pub async fn set_artifact_state(
    State(state): State<AppState>,
    Path((id, key)): Path<(String, String)>,
    body: Bytes,
) -> Response {
    let source = match state_source(&state, &id).await {
        Ok(source) => source,
        Err(response) => return response,
    };
    let _write = artifact_state::WRITES.lock().await;
    let bytes = body.len();
    let (key_for_set, source_for_set) = (key.clone(), source);
    match state_blocking(move || artifact_state::set(&source_for_set, &key_for_set, &body)).await {
        Ok(()) => {
            canvas_core::log::info(
                "artifact state set",
                &[("id", &id), ("key", &key), ("bytes", &bytes)],
            );
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => state_refused(&id, &key, &e),
    }
}

/// `GET /api/artifacts/:id/state`: every key as one JSON object.
pub async fn get_artifact_state(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let source = match state_source(&state, &id).await {
        Ok(source) => source,
        Err(response) => return response,
    };
    match state_blocking(move || artifact_state::get_all(&source)).await {
        Ok(values) => Json(values).into_response(),
        Err(e) => state_refused(&id, "", &e),
    }
}

/// `GET /api/artifacts/:id/state/:key`: one value; 404 when the key holds none.
pub async fn get_artifact_state_key(
    State(state): State<AppState>,
    Path((id, key)): Path<(String, String)>,
) -> Response {
    let source = match state_source(&state, &id).await {
        Ok(source) => source,
        Err(response) => return response,
    };
    let wanted = key.clone();
    match state_blocking(move || artifact_state::get(&source, &wanted)).await {
        Ok(Some(value)) => Json(value).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, "no value under that key").into_response(),
        Err(e) => state_refused(&id, &key, &e),
    }
}

/// `DELETE /api/artifacts/:id/state`: deletes every value.
pub async fn clear_artifact_state(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Response {
    let source = match state_source(&state, &id).await {
        Ok(source) => source,
        Err(response) => return response,
    };
    let _write = artifact_state::WRITES.lock().await;
    match state_blocking(move || artifact_state::clear(&source)).await {
        Ok(cleared) => {
            canvas_core::log::info("artifact state cleared", &[("id", &id), ("keys", &cleared)]);
            Json(serde_json::json!({ "cleared": cleared })).into_response()
        }
        Err(e) => state_refused(&id, "", &e),
    }
}

/// The 422 canvasd answers in place of an HTML page that would freeze
/// Canvas.app's WebKit (`canvas_core::html::webkit_freeze_page_reason`),
/// naming why in the pane; the file itself is left as it is.
fn refused_page(id: &str, rel: &str, reason: &str) -> Response {
    canvas_core::log::warn(
        "artifact page refused",
        &[("id", &id), ("path", &rel), ("reason", &reason)],
    );
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        [
            (
                header::CONTENT_TYPE,
                "text/plain; charset=utf-8".to_string(),
            ),
            (header::CACHE_CONTROL, "no-store".to_string()),
            (
                header::CONTENT_SECURITY_POLICY,
                artifacts::content_security_policy(id),
            ),
        ],
        format!("Canvas won't show this page: {reason}.\n"),
    )
        .into_response()
}
