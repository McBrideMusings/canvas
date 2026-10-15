//! The tailnet listener and forwarding (ADR-0006), driven over real TCP on
//! 127.0.0.1 with a stand-in for Tailscale's WhoIs.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use axum::body::Body;
use axum::http::Request;
use canvasd::build_router;
use canvasd::listener::{self, Caller, Guard, SelfInfo, Tailnet};
use canvasd::sharing::{ListenerConfig, Rules, Sharing};
use canvasd::state::AppState;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tower::ServiceExt;

/// Knows 127.0.0.1 and 100.64.0.7 as people; `unknown_loopback` makes
/// 127.0.0.1 a stranger.
struct Stand {
    unknown_loopback: bool,
}

impl Tailnet for Stand {
    fn self_info(&self) -> Result<SelfInfo, String> {
        Ok(SelfInfo {
            addrs: vec!["127.0.0.1".parse().unwrap()],
            names: vec!["box".into(), "box.tailnet.test".into()],
        })
    }

    fn whois(&self, ip: IpAddr) -> Result<Caller, String> {
        match ip.to_string().as_str() {
            "127.0.0.1" if !self.unknown_loopback => Ok(Caller {
                login: "a@example.test".into(),
                node: "laptop".into(),
            }),
            "100.64.0.7" => Ok(Caller {
                login: "b@example.test".into(),
                node: "phone".into(),
            }),
            _ => Err(format!("{ip} is not on the tailnet")),
        }
    }
}

async fn start(
    state: AppState,
    hosts: &[&str],
    proxy: Option<&str>,
    unknown_loopback: bool,
) -> SocketAddr {
    let config = ListenerConfig {
        port: 0,
        proxy: proxy.map(|p| p.parse().unwrap()),
        hosts: hosts.iter().map(|h| h.to_string()).collect(),
    };
    let tailnet: Arc<dyn Tailnet> = Arc::new(Stand { unknown_loopback });
    let (socket, me) = listener::bind(&config, tailnet.as_ref()).unwrap();
    let addr = socket.local_addr().unwrap();
    let app = listener::router(state, Guard::new(&config, tailnet, &me));
    tokio::spawn(listener::serve_tcp(socket, app));
    addr
}

/// Sends one request with `headers` and returns the status and body.
async fn http(
    addr: SocketAddr,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
) -> (u16, String) {
    let mut request =
        format!("{method} {path} HTTP/1.1\r\nConnection: close\r\nContent-Length: 0\r\n");
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str("\r\n");
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut raw = String::new();
    stream.read_to_string(&mut raw).await.unwrap();
    let status = raw.split(' ').nth(1).unwrap().parse().unwrap();
    let body = raw.split("\r\n\r\n").nth(1).unwrap_or_default().to_string();
    (status, body)
}

#[tokio::test]
async fn a_direct_request_to_the_tailnet_address_answers() {
    let addr = start(AppState::new(), &[], None, false).await;
    let host = addr.to_string();
    let (status, body) = http(addr, "GET", "/api/state", &[("Host", &host)]).await;
    assert_eq!(status, 200, "{body}");
    let (status, _) = http(addr, "GET", "/api/state", &[("Host", "box.tailnet.test")]).await;
    assert_eq!(status, 200);
}

#[tokio::test]
async fn a_request_through_the_proxy_with_an_allowed_host_answers() {
    let addr = start(AppState::new(), &["canvas.test"], Some("127.0.0.1"), false).await;
    let ok = [
        ("Host", "canvas.test"),
        ("X-Forwarded-For", "9.9.9.9, 100.64.0.7"),
    ];
    let (status, body) = http(addr, "GET", "/api/state", &ok).await;
    assert_eq!(status, 200, "{body}");

    // The last forwarded address is the client, and WhoIs must know it.
    let stranger = [
        ("Host", "canvas.test"),
        ("X-Forwarded-For", "100.64.0.7, 100.64.0.99"),
    ];
    assert_eq!(http(addr, "GET", "/api/state", &stranger).await.0, 403);
    // A proxy request with no forwarded client is refused.
    assert_eq!(
        http(addr, "GET", "/api/state", &[("Host", "canvas.test")])
            .await
            .0,
        403
    );
}

#[tokio::test]
async fn a_foreign_host_or_origin_is_refused() {
    let addr = start(AppState::new(), &["canvas.test"], None, false).await;
    let (status, body) = http(addr, "GET", "/api/state", &[("Host", "evil.test")]).await;
    assert_eq!(status, 403);
    assert!(body.contains("evil.test"), "{body}");

    let host = addr.to_string();
    let foreign = [("Host", host.as_str()), ("Origin", "https://evil.test")];
    assert_eq!(http(addr, "GET", "/api/state", &foreign).await.0, 403);
    let own = [("Host", host.as_str()), ("Origin", "https://canvas.test")];
    assert_eq!(http(addr, "GET", "/api/state", &own).await.0, 200);
}

#[tokio::test]
async fn a_caller_whois_cannot_identify_is_refused() {
    let addr = start(AppState::new(), &[], None, true).await;
    let host = addr.to_string();
    let (status, body) = http(addr, "GET", "/api/state", &[("Host", &host)]).await;
    assert_eq!(status, 403);
    assert!(body.contains("not identified"), "{body}");
}

#[tokio::test]
async fn routes_that_run_things_on_this_machine_are_not_served() {
    let addr = start(AppState::new(), &[], None, false).await;
    let host = addr.to_string();
    let h = [("Host", host.as_str())];
    assert_eq!(http(addr, "POST", "/api/open", &h).await.0, 403);
    assert_eq!(http(addr, "POST", "/api/artifacts", &h).await.0, 403);
    assert_eq!(http(addr, "GET", "/api/projects", &h).await.0, 403);
}

#[tokio::test]
async fn sharing_toml_names_the_port_proxy_and_hosts() {
    let dir = std::env::temp_dir().join(format!("canvasd-sharing-{}", uuid()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("sharing.toml"),
        "[listener]\nport = 4100\nproxy = \"100.64.0.9\"\nhosts = [\"canvas.test\"]\n",
    )
    .unwrap();
    let state = AppState::open(&dir).await;
    let l = state.sharing.listener.clone().unwrap();
    assert_eq!((l.port, l.hosts), (4100, vec!["canvas.test".to_string()]));
    assert_eq!(l.proxy, Some("100.64.0.9".parse().unwrap()));
    let _ = std::fs::remove_dir_all(&dir);
}

fn uuid() -> String {
    format!("{}-{:?}", std::process::id(), std::time::Instant::now())
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect()
}

#[tokio::test]
async fn a_shared_post_reaches_upstream_with_its_image_bytes() {
    let upstream = AppState::new();
    let upstream_addr = start(upstream.clone(), &[], None, false).await;

    let local = AppState::new().with_sharing(Sharing {
        upstream: Some(format!("http://{upstream_addr}")),
        timeline: Rules {
            include: vec!["/work/*".into()],
            exclude: vec!["/work/private".into()],
        },
        ..Sharing::default()
    });
    let app = build_router(local);

    let png = b"\x89PNG stand-in bytes";
    let upload = Request::builder()
        .method("POST")
        .uri("/api/media?ext=png")
        .body(Body::from(png.to_vec()))
        .unwrap();
    let response = app.clone().oneshot(upload).await.unwrap();
    let bytes = http_body_util::BodyExt::collect(response.into_body())
        .await
        .unwrap()
        .to_bytes();
    let name = serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()["name"]
        .as_str()
        .unwrap()
        .to_string();

    let post = |cwd: &str, images: Vec<String>| {
        let body = serde_json::json!({
            "session_id": "s1", "cwd": cwd, "agent": "claude-code",
            "html": "<p>hi <img src=\"canvas-image:0\"></p>", "images": images,
        });
        Request::builder()
            .method("POST")
            .uri("/api/posts")
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    };
    // Excluded and unmatched folders send nothing; the shared one sends its image.
    assert_eq!(
        app.clone()
            .oneshot(post("/work/private", vec![]))
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        app.clone()
            .oneshot(post("/elsewhere/x", vec![]))
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        app.clone()
            .oneshot(post("/work/app", vec![name]))
            .await
            .unwrap()
            .status(),
        200
    );

    let mut cards = Vec::new();
    for _ in 0..100 {
        cards = upstream.inner.read().await.cards.iter().cloned().collect();
        if !cards.is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_eq!(cards.len(), 1, "only the shared post arrives");
    assert_eq!(cards[0].images.len(), 1);
    assert_eq!(std::fs::read(&cards[0].images[0]).unwrap(), png);
    assert_eq!(upstream.inner.read().await.sessions["s1"].cwd, "/work/app");
}

#[tokio::test]
async fn an_artifact_pane_in_a_browser_reaches_its_files_errors_and_state_only() {
    let addr = start(AppState::new(), &[], None, false).await;
    let host = addr.to_string();
    let origin = format!("http://{host}");
    let pane = [("Host", host.as_str()), ("Origin", "null")];
    let viewer = [("Host", host.as_str()), ("Origin", origin.as_str())];
    // Past the guard: canvasd answers, whatever the empty body earns.
    for (method, path, headers) in [
        ("GET", "/artifacts/art-x/app.js", &pane),
        ("GET", "/api/artifacts/art-x/state", &viewer),
        ("PUT", "/api/artifacts/art-x/state/k", &viewer),
        ("POST", "/api/artifacts/art-x/errors", &viewer),
    ] {
        assert_ne!(
            http(addr, method, path, headers).await.0,
            403,
            "{method} {path}"
        );
    }
    // A null origin reads nothing else.
    assert_eq!(http(addr, "GET", "/api/state", &pane).await.0, 403);
    assert_eq!(
        http(addr, "POST", "/artifacts/art-x/app.js", &pane).await.0,
        403
    );
    // Nothing here opens a link or a path, or changes an artifact itself.
    for (method, path) in [
        ("POST", "/api/artifacts/art-x/open"),
        ("POST", "/api/artifacts/art-x/reset"),
        ("DELETE", "/api/artifacts/art-x/state"),
        ("PUT", "/api/artifacts/art-x/data"),
        ("PUT", "/api/artifacts/art-x/state/a/b"),
    ] {
        assert_eq!(
            http(addr, method, path, &viewer).await.0,
            403,
            "{method} {path}"
        );
    }
}
