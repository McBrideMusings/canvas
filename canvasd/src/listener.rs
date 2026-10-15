//! The tailnet listener (ADR-0006): plain HTTP on this machine's Tailscale
//! address and the `sharing.toml` port, serving the same router as the Unix
//! socket behind a guard that names every caller by Tailscale WhoIs and checks
//! Host and Origin. A reverse proxy on the tailnet terminates TLS.

use std::collections::HashSet;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{header, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Router;

use crate::sharing::ListenerConfig;
use crate::state::AppState;

/// The caller Tailscale named for a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Caller {
    pub login: String,
    pub node: String,
}

/// This machine as the tailnet knows it.
#[derive(Debug, Clone)]
pub struct SelfInfo {
    pub addrs: Vec<IpAddr>,
    pub names: Vec<String>,
}

/// What the listener asks Tailscale. [`TailscaleCli`] answers from the local
/// tailscaled; tests stand in.
pub trait Tailnet: Send + Sync + 'static {
    fn self_info(&self) -> Result<SelfInfo, String>;
    fn whois(&self, ip: IpAddr) -> Result<Caller, String>;
}

/// Asks the local tailscaled through the `tailscale` CLI, itself a client of
/// the daemon's LocalAPI.
pub struct TailscaleCli;

impl TailscaleCli {
    fn run(args: &[&str]) -> Result<serde_json::Value, String> {
        // The lowercase name is the CLI; `MacOS/Tailscale` starts the GUI app
        // and prints an error instead of JSON.
        let app = "/Applications/Tailscale.app/Contents/MacOS/tailscale";
        let bin = if std::path::Path::new(app).exists() {
            app
        } else {
            "tailscale"
        };
        let mut child = std::process::Command::new(bin)
            .args(args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| format!("{bin}: {e}"))?;
        // Read both pipes on threads so a large answer never blocks the child
        // while this waits on it.
        let read = |pipe: Option<Box<dyn std::io::Read + Send>>| {
            std::thread::spawn(move || {
                let mut buf = Vec::new();
                if let Some(mut pipe) = pipe {
                    let _ = pipe.read_to_end(&mut buf);
                }
                buf
            })
        };
        let stdout = read(child.stdout.take().map(|p| Box::new(p) as _));
        let stderr = read(child.stderr.take().map(|p| Box::new(p) as _));
        let deadline = std::time::Instant::now() + TAILSCALE_TIMEOUT;
        let status = loop {
            match child.try_wait().map_err(|e| e.to_string())? {
                Some(status) => break status,
                None if std::time::Instant::now() >= deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("tailscale {} timed out", args[0]));
                }
                None => std::thread::sleep(std::time::Duration::from_millis(20)),
            }
        };
        let out_bytes = stdout.join().unwrap_or_default();
        let err_bytes = stderr.join().unwrap_or_default();
        if !status.success() {
            let err = String::from_utf8_lossy(&err_bytes);
            return Err(format!("tailscale {}: {}", args[0], err.trim()));
        }
        let out = out_bytes;
        serde_json::from_slice(&out).map_err(|e| format!("tailscale {}: {e}", args[0]))
    }
}

impl Tailnet for TailscaleCli {
    fn self_info(&self) -> Result<SelfInfo, String> {
        let status = Self::run(&["status", "--json"])?;
        let me = status.get("Self").ok_or("tailscale status has no Self")?;
        let addrs: Vec<IpAddr> = me
            .get("TailscaleIPs")
            .and_then(|a| a.as_array())
            .into_iter()
            .flatten()
            .filter_map(|a| a.as_str()?.parse().ok())
            .collect();
        let mut names = Vec::new();
        if let Some(dns) = me.get("DNSName").and_then(|n| n.as_str()) {
            let dns = dns.trim_end_matches('.').to_ascii_lowercase();
            if let Some((short, _)) = dns.split_once('.') {
                names.push(short.to_string());
            }
            names.push(dns);
        }
        Ok(SelfInfo { addrs, names })
    }

    fn whois(&self, ip: IpAddr) -> Result<Caller, String> {
        let who = Self::run(&["whois", "--json", &ip.to_string()])?;
        let login = who
            .pointer("/UserProfile/LoginName")
            .and_then(|l| l.as_str())
            .unwrap_or_default();
        let node = who
            .pointer("/Node/Name")
            .and_then(|n| n.as_str())
            .unwrap_or_default();
        if login.is_empty() && node.is_empty() {
            return Err(format!("tailscale whois {ip} named no user or node"));
        }
        Ok(Caller {
            login: login.to_string(),
            node: node.trim_end_matches('.').to_string(),
        })
    }
}

const TAILSCALE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// How long a WhoIs answer is reused for the same address.
const WHOIS_TTL: std::time::Duration = std::time::Duration::from_secs(30);

/// The TCP peer of a connection, put on each request by [`serve_tcp`].
#[derive(Debug, Clone, Copy)]
pub struct PeerAddr(pub SocketAddr);

pub struct Guard {
    tailnet: Arc<dyn Tailnet>,
    hosts: HashSet<String>,
    proxy: Option<IpAddr>,
    /// Recent WhoIs answers, so a page's many requests ask Tailscale once.
    known: std::sync::Mutex<std::collections::HashMap<IpAddr, (std::time::Instant, Caller)>>,
}

impl Guard {
    pub fn new(config: &ListenerConfig, tailnet: Arc<dyn Tailnet>, me: &SelfInfo) -> Guard {
        let hosts = me
            .addrs
            .iter()
            .map(|a| a.to_string())
            .chain(me.names.iter().cloned())
            .chain(config.hosts.iter().cloned())
            .map(|h| normalize_host(&h))
            .collect();
        Guard {
            tailnet,
            hosts,
            proxy: config.proxy,
            known: Default::default(),
        }
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
    let cached = guard
        .known
        .lock()
        .ok()
        .and_then(|known| known.get(&client).cloned())
        .filter(|(at, _)| at.elapsed() < WHOIS_TTL);
    let tailnet = guard.tailnet.clone();
    let looked_up = match cached {
        Some((_, caller)) => Ok(Ok(caller)),
        None => tokio::task::spawn_blocking(move || tailnet.whois(client)).await,
    };
    let caller = match looked_up {
        Ok(Ok(caller)) => {
            if let Ok(mut known) = guard.known.lock() {
                known.insert(client, (std::time::Instant::now(), caller.clone()));
            }
            caller
        }
        Ok(Err(e)) => {
            return refuse(
                &format!("caller {client} not identified: {e}"),
                &peer,
                &request,
            )
        }
        Err(e) => return refuse(&format!("whois failed: {e}"), &peer, &request),
    };
    request.extensions_mut().insert(caller);
    next.run(request).await
}

/// The router the listener serves: the daemon's router behind the guard.
pub fn router(state: AppState, guard_state: Guard) -> Router {
    crate::build_router(state).layer(axum::middleware::from_fn_with_state(
        Arc::new(guard_state),
        guard,
    ))
}

/// Binds the port named by `config` on this machine's first IPv4 Tailscale
/// address (else its first address).
pub fn bind(
    config: &ListenerConfig,
    tailnet: &dyn Tailnet,
) -> Result<(std::net::TcpListener, SelfInfo), String> {
    let me = tailnet.self_info()?;
    let ip = me
        .addrs
        .iter()
        .find(|a| a.is_ipv4())
        .or(me.addrs.first())
        .copied()
        .ok_or("tailscale reports no address for this machine")?;
    let listener = std::net::TcpListener::bind((ip, config.port))
        .map_err(|e| format!("bind {ip}:{}: {e}", config.port))?;
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    Ok((listener, me))
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

/// Starts the listener when `sharing.toml` has a `[listener]` table. Tailscale
/// may not be up when the daemon starts, so a failed bind is retried every 30s.
pub fn spawn(state: AppState, tailnet: Arc<dyn Tailnet>) {
    let Some(config) = state.sharing.listener.clone() else {
        return;
    };
    tokio::spawn(async move {
        loop {
            let attempt = {
                let (config, tailnet) = (config.clone(), tailnet.clone());
                tokio::task::spawn_blocking(move || bind(&config, tailnet.as_ref())).await
            };
            match attempt {
                Ok(Ok((listener, me))) => {
                    let addr = listener.local_addr().ok();
                    canvas_core::log::info(
                        "listener started",
                        &[("addr", &addr.map_or(String::new(), |a| a.to_string()))],
                    );
                    let app = router(state.clone(), Guard::new(&config, tailnet, &me));
                    serve_tcp(listener, app).await;
                    return;
                }
                Ok(Err(e)) => canvas_core::log::warn("listener not started", &[("error", &e)]),
                Err(e) => canvas_core::log::warn("listener not started", &[("error", &e)]),
            }
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        }
    });
}
