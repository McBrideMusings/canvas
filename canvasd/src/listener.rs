//! The network listener (ADR-0006): plain HTTP on the `sharing.toml` `bind`
//! address and port, serving the same router as the Unix socket behind a guard
//! that admits only callers in `allow` and checks Host and Origin. A reverse
//! proxy terminates TLS.

use std::collections::HashSet;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{header, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Router;

use crate::sharing::{Cidr, ListenerConfig};
use crate::state::AppState;

/// Put on a request that came in over the listener, so routes can tell it
/// from one on the Unix socket. Holds the caller's address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Caller {
    pub ip: IpAddr,
}

/// The TCP peer of a connection, put on each request by [`serve_tcp`].
#[derive(Debug, Clone, Copy)]
pub struct PeerAddr(pub SocketAddr);

pub struct Guard {
    hosts: HashSet<String>,
    allow: Vec<Cidr>,
    proxy: Option<IpAddr>,
}

impl Guard {
    /// Fails when `config` has no `bind` or no `allow`.
    pub fn new(config: &ListenerConfig) -> Result<Guard, String> {
        let bind = config.bind.ok_or("sharing.toml [listener] has no bind")?;
        let allow = config
            .allow
            .clone()
            .ok_or("sharing.toml [listener] has no allow")?;
        let hosts = std::iter::once(bind.to_string())
            .chain(config.hosts.iter().cloned())
            .map(|h| normalize_host(&h))
            .collect();
        Ok(Guard {
            hosts,
            allow,
            proxy: config.proxy,
        })
    }
}

fn normalize_host(host: &str) -> String {
    host.trim()
        .trim_start_matches('[')
        .trim_end_matches('.')
        .to_ascii_lowercase()
}

/// The host of a `Host` header value: without its port, v6 brackets or a
/// trailing dot.
fn header_host(value: &str) -> String {
    let host = match value.strip_prefix('[') {
        Some(rest) => rest.split(']').next().unwrap_or(rest),
        None => value.rsplit_once(':').map_or(value, |(h, _)| h),
    };
    normalize_host(host)
}

fn origin_host(value: &str) -> Option<String> {
    let url = url::Url::parse(value).ok()?;
    let host = url.host_str()?;
    Some(normalize_host(host.trim_end_matches(']')))
}

/// Whether the listener serves this request at all: reads, except the
/// daemon's own folders-and-machine routes under `/api`, plus the two routes
/// a machine posts through and the three an artifact pane in a browser needs:
/// its script errors, and its saved state, which `canvasd` confines to one
/// JSON file per key in the artifact's `canvas-data/`. Opening a link or a
/// path stays with the machine's own person.
fn route_served(method: &Method, path: &str) -> bool {
    let api = path.strip_prefix("/api/");
    let artifact_part = api
        .and_then(|api| api.strip_prefix("artifacts/"))
        .and_then(|rest| rest.split_once('/'))
        .map(|(_, part)| part);
    if method == Method::POST {
        return path == "/api/posts" || path == "/api/media" || artifact_part == Some("errors");
    }
    if method == Method::PUT {
        return artifact_part
            .is_some_and(|p| p.strip_prefix("state/").is_some_and(|k| !k.contains('/')));
    }
    if method != Method::GET && method != Method::HEAD {
        return false;
    }
    match api {
        None => true,
        Some(api) => {
            api == "state"
                || api == "events"
                || api.starts_with("cards/")
                || artifact_part == Some("state")
        }
    }
}

fn refuse(reason: &str, peer: &SocketAddr, request: &Request) -> Response {
    refuse_for(reason, peer, None, request)
}

fn refuse_for(
    reason: &str,
    peer: &SocketAddr,
    caller: Option<IpAddr>,
    request: &Request,
) -> Response {
    let header = |name| {
        request
            .headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
    };
    canvas_core::log::warn(
        "listener refused",
        &[
            ("reason", &reason),
            ("peer", &peer),
            ("caller", &caller.map_or(String::new(), |ip| ip.to_string())),
            ("host", &header(header::HOST)),
            ("origin", &header(header::ORIGIN)),
            ("method", &request.method()),
            ("path", &request.uri().path()),
        ],
    );
    (StatusCode::FORBIDDEN, format!("refused: {reason}")).into_response()
}

async fn guard(State(guard): State<Arc<Guard>>, mut request: Request, next: Next) -> Response {
    let Some(PeerAddr(peer)) = request.extensions().get::<PeerAddr>().copied() else {
        return (StatusCode::FORBIDDEN, "refused: no peer address").into_response();
    };
    let host = request
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(header_host);
    match host {
        Some(host) if guard.hosts.contains(&host) => {}
        Some(host) => return refuse(&format!("host {host:?} is not served"), &peer, &request),
        None => return refuse("no Host header", &peer, &request),
    }
    if let Some(origin) = request.headers().get(header::ORIGIN) {
        // An artifact pane is a sandboxed frame in an opaque origin, so the
        // module scripts and sub-resources it loads from its own folder carry
        // `Origin: null`, and WebKit and Chrome send no Referer with them.
        // Only reads of `/artifacts/` get that pass; the artifact's id (40
        // random bits, never listed outside the viewer's own state) is what
        // names a page the caller may read.
        let pane_read = origin.as_bytes() == b"null"
            && matches!(*request.method(), Method::GET | Method::HEAD)
            && request.uri().path().starts_with("/artifacts/");
        let origin = origin.to_str().ok().and_then(origin_host);
        match origin {
            Some(o) if guard.hosts.contains(&o) => {}
            _ if pane_read => {}
            _ => return refuse("origin is not served", &peer, &request),
        }
    }
    if !route_served(request.method(), request.uri().path()) {
        return refuse("route is not served here", &peer, &request);
    }
    let client = if Some(peer.ip()) == guard.proxy {
        let forwarded = request
            .headers()
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.rsplit(',').next())
            .and_then(|a| a.trim().parse::<IpAddr>().ok());
        match forwarded {
            Some(ip) => ip,
            None => {
                return refuse(
                    "proxy request without a usable X-Forwarded-For",
                    &peer,
                    &request,
                )
            }
        }
    } else {
        peer.ip()
    };
    if !guard.allow.iter().any(|net| net.contains(client)) {
        return refuse_for(
            &format!("caller {client} is not in allow"),
            &peer,
            Some(client),
            &request,
        );
    }
    request.extensions_mut().insert(Caller { ip: client });
    next.run(request).await
}

/// The router the listener serves: the daemon's router behind the guard.
pub fn router(state: AppState, guard_state: Guard) -> Router {
    crate::build_router(state).layer(axum::middleware::from_fn_with_state(
        Arc::new(guard_state),
        guard,
    ))
}

/// Binds exactly the `bind` address and `port` of `config`.
pub fn bind(config: &ListenerConfig) -> Result<std::net::TcpListener, String> {
    let ip = config.bind.ok_or("sharing.toml [listener] has no bind")?;
    let listener = std::net::TcpListener::bind((ip, config.port))
        .map_err(|e| format!("bind {ip}:{}: {e}", config.port))?;
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    Ok(listener)
}

/// Serves `app` on `listener` until the process exits, putting each
/// connection's peer on its requests as [`PeerAddr`].
pub async fn serve_tcp(listener: std::net::TcpListener, app: Router) {
    use hyper::body::Incoming;
    use hyper_util::rt::TokioIo;
    use tower::ServiceExt;

    let listener = match tokio::net::TcpListener::from_std(listener) {
        Ok(l) => l,
        Err(e) => {
            canvas_core::log::error("listener failed", &[("error", &e)]);
            return;
        }
    };
    loop {
        let (stream, peer) = match listener.accept().await {
            Ok(conn) => conn,
            Err(e) => {
                canvas_core::log::error("listener accept failed", &[("error", &e)]);
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                continue;
            }
        };
        let app = app.clone();
        tokio::spawn(async move {
            let service =
                hyper::service::service_fn(move |mut request: hyper::Request<Incoming>| {
                    request.extensions_mut().insert(PeerAddr(peer));
                    let request = request.map(Body::new);
                    app.clone().oneshot(request)
                });
            let _ = hyper::server::conn::http1::Builder::new()
                .serve_connection(TokioIo::new(stream), service)
                .await;
        });
    }
}

/// Starts the listener when `sharing.toml` has a `[listener]` table. A failed
/// bind (the address not up yet) is retried every 30s; a config without `bind`
/// or `allow` is logged once and never retried.
pub fn spawn(state: AppState) {
    let Some(config) = state.sharing.listener.clone() else {
        return;
    };
    let guard = match Guard::new(&config) {
        Ok(guard) => guard,
        Err(e) => {
            canvas_core::log::warn("listener not started", &[("error", &e)]);
            return;
        }
    };
    tokio::spawn(async move {
        loop {
            let attempt = {
                let config = config.clone();
                tokio::task::spawn_blocking(move || bind(&config)).await
            };
            match attempt {
                Ok(Ok(listener)) => {
                    let addr = listener.local_addr().ok();
                    canvas_core::log::info(
                        "listener started",
                        &[
                            (
                                "bind",
                                &config.bind.map_or(String::new(), |b| b.to_string()),
                            ),
                            ("port", &addr.map_or(config.port, |a| a.port())),
                        ],
                    );
                    serve_tcp(listener, router(state.clone(), guard)).await;
                    return;
                }
                Ok(Err(e)) => canvas_core::log::warn("listener not started", &[("error", &e)]),
                Err(e) => canvas_core::log::warn("listener not started", &[("error", &e)]),
            }
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        }
    });
}
