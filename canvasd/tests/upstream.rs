//! A daemon whose `sharing.toml` names an upstream merges the upstream's
//! stream into its own state and events (ADR-0006).

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use canvas_core::{StateResponse, UpstreamStatus};
use canvasd::sharing::Sharing;
use canvasd::state::{AppState, CanvasEvent};
use canvasd::{build_router, upstream};
use http_body_util::BodyExt;
use serde_json::json;
use tower::ServiceExt;

/// An upstream canvasd on its own runtime, so dropping it closes every
/// connection the way a machine going away does.
struct Hub {
    state: AppState,
    addr: SocketAddr,
    runtime: Option<tokio::runtime::Runtime>,
}

impl Drop for Hub {
    fn drop(&mut self) {
        self.stop();
    }
}

impl Hub {
    fn start(state: AppState, addr: SocketAddr) -> Hub {
        let listener = std::net::TcpListener::bind(addr).unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let app = build_router(state.clone());
        runtime.spawn(async move {
            let listener = tokio::net::TcpListener::from_std(listener).unwrap();
            axum::serve(listener, app).await.unwrap();
        });
        Hub {
            state,
            addr,
            runtime: Some(runtime),
        }
    }

    fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    fn stop(&mut self) {
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }

    async fn call(&self, request: Request<Body>) -> axum::response::Response {
        build_router(self.state.clone())
            .oneshot(request)
            .await
            .unwrap()
    }

    async fn upload(&self, bytes: &[u8]) -> String {
        let response = self
            .call(
                Request::builder()
                    .method("POST")
                    .uri("/api/media?ext=png")
                    .body(Body::from(bytes.to_vec()))
                    .unwrap(),
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        json_body::<serde_json::Value>(response).await["name"]
            .as_str()
            .unwrap()
            .to_string()
    }

    async fn post(&self, session: &str, html: &str, images: Vec<String>) {
        post(&build_router(self.state.clone()), session, html, images).await;
    }
}

async fn post(app: &axum::Router, session: &str, html: &str, images: Vec<String>) {
    let request = Request::builder()
        .method("POST")
        .uri("/api/posts")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({
                "session_id": session,
                "cwd": "/work/app",
                "agent": "claude-code",
                "html": html,
                "images": images,
            })
            .to_string(),
        ))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

async fn json_body<T: serde::de::DeserializeOwned>(response: axum::response::Response) -> T {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

fn spoke(upstream_url: &str) -> AppState {
    AppState::new().with_sharing(Sharing {
        upstream: Some(upstream_url.to_string()),
        ..Sharing::default()
    })
}

async fn state_of(state: &AppState) -> StateResponse {
    let response = build_router(state.clone())
        .oneshot(
            Request::builder()
                .uri("/api/state")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    json_body(response).await
}

/// Polls the daemon's `/api/state` until `ready` holds.
async fn wait_for(
    state: &AppState,
    what: &str,
    ready: impl Fn(&StateResponse) -> bool,
) -> StateResponse {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let snapshot = state_of(state).await;
        if ready(&snapshot) {
            return snapshot;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {what}: {snapshot:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn online(s: &StateResponse) -> bool {
    s.upstream.as_ref().is_some_and(|u| u.online)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn merges_the_upstream_stream_and_drops_it_when_the_link_is_lost() {
    let png = std::fs::read(format!(
        "{}/../app/src-tauri/icons/tray.png",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap();
    let mut hub = Hub::start(AppState::new(), "127.0.0.1:0".parse().unwrap());
    let image = hub.upload(&png).await;
    hub.post(
        "hub-1",
        "<p>before the link</p><img src=\"canvas-image:0\">",
        vec![image],
    )
    .await;

    let local = spoke(&hub.url());
    upstream::spawn(local.clone());
    let mut viewer = local.remote_events.subscribe();
    post(
        &build_router(local.clone()),
        "spoke-1",
        "<p>mine</p>",
        vec![],
    )
    .await;

    // The snapshot brings the upstream's earlier post in beside our own.
    let merged = wait_for(&local, "the upstream's first post", |s| {
        online(s) && s.cards.len() == 2
    })
    .await;
    let remote_card = merged
        .cards
        .iter()
        .find(|c| c.session_id == "hub-1")
        .expect("the upstream's card");
    assert!(remote_card.html.contains("before the link"));
    assert_eq!(remote_card.images.len(), 1);
    assert!(
        remote_card.images[0].starts_with("upstream:") && remote_card.images[0].ends_with(".png"),
        "a remote card names no path on this machine: {:?}",
        remote_card.images
    );
    let remote_session = merged.sessions.iter().find(|s| s.id == "hub-1").unwrap();
    assert_eq!(
        remote_session.machine.as_deref(),
        Some(canvasd::sharing::machine_name().as_str()),
        "the session names the machine it came from"
    );
    let own_session = merged.sessions.iter().find(|s| s.id == "spoke-1").unwrap();
    assert_eq!(own_session.machine, remote_session.machine);

    // Its image bytes come through this daemon.
    let image_response = build_router(local.clone())
        .oneshot(
            Request::builder()
                .uri(format!("/api/cards/{}/images/0", remote_card.id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(image_response.status(), StatusCode::OK);
    let bytes = image_response
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes();
    assert_eq!(bytes.as_ref(), png.as_slice());

    // A peer on the tailnet listener is offered only this machine's own
    // items, never the upstream's.
    let mut as_peer = Request::builder()
        .uri("/api/state")
        .body(Body::empty())
        .unwrap();
    as_peer.extensions_mut().insert(canvasd::listener::Caller {
        login: "peer@example".into(),
        node: "peer".into(),
    });
    let peer_view: StateResponse =
        json_body(build_router(local.clone()).oneshot(as_peer).await.unwrap()).await;
    assert_eq!(peer_view.cards.len(), 1);
    assert_eq!(peer_view.cards[0].session_id, "spoke-1");
    assert!(peer_view.upstream.is_none());

    // A post made after the link is up arrives as an event.
    hub.post("hub-1", "<p>after the link</p>", vec![]).await;
    let live = wait_for(&local, "the live post", |s| s.cards.len() == 3).await;
    assert_eq!(live.cards[0].session_id, "hub-1", "newest first");
    let mut saw_card = false;
    while let Ok(event) = viewer.try_recv() {
        if let CanvasEvent::CardUpserted(card) = event {
            saw_card |= card.html.contains("after the link");
        }
    }
    assert!(saw_card, "viewers hear the upstream's card-upserted");

    // Our own forwarded session is not shown twice: the hub holding a session
    // with the same id as ours adds nothing.
    hub.post("spoke-1", "<p>forwarded copy</p>", vec![]).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(state_of(&local).await.cards.len(), 3);

    // The hub goes away: its items drop, ours stay, and the state says why.
    let addr = hub.addr;
    let hub_state = hub.state.clone();
    hub.stop();
    let down = wait_for(&local, "the hub-offline status", |s| {
        s.upstream
            .as_ref()
            .is_some_and(|u| !u.online && u.error.is_some())
    })
    .await;
    assert_eq!(down.cards.len(), 1);
    assert_eq!(down.cards[0].session_id, "spoke-1");
    assert!(down.sessions.iter().all(|s| s.id == "spoke-1"));
    let mut told = false;
    while let Ok(event) = viewer.try_recv() {
        told |= matches!(
            event,
            CanvasEvent::UpstreamChanged(UpstreamStatus { online: false, .. })
        );
    }
    assert!(told, "viewers are told the upstream is offline");

    // It comes back: the snapshot refills.
    let hub = Hub::start(hub_state, addr);
    let back = wait_for(&local, "the upstream to return", |s| {
        online(s) && s.cards.iter().any(|c| c.session_id == "hub-1")
    })
    .await;
    assert!(back.upstream.unwrap().error.is_none());
    drop(hub);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unreachable_upstream_shows_offline_and_keeps_local_items() {
    // Nothing listens here: bind and drop to learn a free port.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let local = spoke(&format!("http://127.0.0.1:{port}"));
    upstream::spawn(local.clone());
    post(
        &build_router(local.clone()),
        "spoke-1",
        "<p>mine</p>",
        vec![],
    )
    .await;
    let s = wait_for(&local, "the offline status", |s| {
        s.upstream
            .as_ref()
            .is_some_and(|u| !u.online && u.error.is_some())
    })
    .await;
    assert_eq!(s.cards.len(), 1);
}
