//! The artifact routes: `/api/artifacts` for the records, `/artifacts/:id/…`
//! for the files a pane loads. See [`crate::artifacts`].

use std::path::PathBuf;

use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use canvas_core::{ArtifactSource, NewArtifactRequest, PutArtifactRequest, RelinkArtifactRequest};

use crate::artifacts::{self, Artifacts};
use crate::state::{AppState, CanvasEvent};

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

fn failed(what: &str, id: &str, e: &dyn std::fmt::Display) -> Response {
    canvas_core::log::error(what, &[("id", &id), ("error", e)]);
    (StatusCode::INTERNAL_SERVER_ERROR, format!("{what}: {e}")).into_response()
}

/// Writes the records and tells every viewer about the artifact. A failed
/// save is an error: an artifact that would vanish on restart must not look
/// created.
pub(crate) async fn save_and_publish(
    state: &AppState,
    artifacts: &Artifacts,
    id: &str,
) -> Result<canvas_core::ArtifactView, Response> {
    artifacts
        .save()
        .await
        .map_err(|e| failed("saving artifacts.json failed", id, &e))?;
    let record = artifacts.records.get(id).ok_or_else(not_found)?;
    let view = artifacts.view(record);
    state.publish(CanvasEvent::ArtifactUpserted(view.clone()));
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
    body: Option<Json<NewArtifactRequest>>,
) -> Response {
    let NewArtifactRequest { title, link } = body.map(|Json(b)| b).unwrap_or_default();
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
    let record = artifacts.new_record(title, source);
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
    match save_and_publish(&state, &artifacts, &id).await {
        Ok(view) => {
            artifacts
                .fingerprints
                .insert(id.clone(), artifacts::fingerprint(&root));
            state.watcher.watch(&id, &root);
            canvas_core::log::info(
                "artifact created",
                &[
                    ("id", &id),
                    ("kind", &if owned { "owned" } else { "linked" }),
                    ("path", &view.path),
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
    match save_and_publish(&state, &artifacts, &id).await {
        Ok(view) => {
            state.watcher.watch(&id, &root);
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

/// `canvas artifact show`: the record plus its folder, entry page and
/// declared size.
pub async fn get_artifact(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let artifacts = state.artifacts.read().await;
    match artifacts.records.get(&id) {
        Some(record) => Json(artifacts.view(record)).into_response(),
        None => not_found(),
    }
}

/// `canvas artifact put`: copies a file or a folder's contents into the
/// artifact and stamps `updatedAt`; every viewer showing it reloads.
pub async fn put_artifact(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<PutArtifactRequest>,
) -> Response {
    let source = PathBuf::from(&req.source);
    if !source.is_absolute() {
        return (StatusCode::BAD_REQUEST, "source must be an absolute path").into_response();
    }
    let folder = {
        let artifacts = state.artifacts.read().await;
        let Some(record) = artifacts.records.get(&id) else {
            return not_found();
        };
        if let ArtifactSource::Linked { link } = &record.source {
            return bad_request(format!(
                "{id} is linked to {link}; save its files there instead"
            ));
        }
        match artifacts.folder(&id) {
            Some(folder) => folder,
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
        artifacts::copy_into(&resolved, &folder).map(|n| (n, artifacts::fingerprint(&folder)))
    })
    .await;
    let (files, print) = match copied {
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
    artifacts.fingerprints.insert(id.clone(), print);
    match save_and_publish(&state, &artifacts, &id).await {
        Ok(view) => {
            canvas_core::log::info(
                "artifact put",
                &[
                    ("id", &id),
                    ("source", &source.display()),
                    ("files", &files),
                ],
            );
            Json(view).into_response()
        }
        Err(response) => response,
    }
}

/// `canvas artifact delete`: removes the record, and an owned artifact's
/// folder. A linked artifact's files belong to the person and stay.
pub async fn delete_artifact(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let mut artifacts = state.artifacts.write().await;
    let Some(record) = artifacts.records.remove(&id) else {
        return not_found();
    };
    if let Err(e) = artifacts.save().await {
        artifacts.records.insert(id.clone(), record);
        return failed("saving artifacts.json failed", &id, &e);
    }
    state.watcher.unwatch(&id);
    artifacts.fingerprints.remove(&id);
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
    state.publish(CanvasEvent::ArtifactRemoved(id.clone()));
    canvas_core::log::info("artifact deleted", &[("id", &id)]);
    Json(serde_json::json!({ "deleted": id })).into_response()
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
