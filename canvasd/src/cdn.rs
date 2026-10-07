//! Downloads the CDN assets an export inlines. Only the hosts a card's CSP
//! lets it load from are fetched, over HTTPS, one request at a time with a
//! 10s limit on the whole download and at most `MAX_ASSET_BYTES + 1` bytes
//! read, so the export can tell an oversize body from one that fits. This is
//! outbound only; canvasd still listens on nothing but its Unix socket.

use std::io::Read;
use std::net::{SocketAddr, ToSocketAddrs};
use std::sync::{mpsc, Arc, OnceLock};
use std::time::{Duration, Instant};

use canvas_core::MAX_ASSET_BYTES;
use ureq::rustls;

/// The hosts the viewer's card CSP allows scripts, styles and fonts from.
pub const HOSTS: &[&str] = &[
    "cdnjs.cloudflare.com",
    "cdn.jsdelivr.net",
    "unpkg.com",
    "fonts.googleapis.com",
    "fonts.gstatic.com",
];

pub const TIMEOUT: Duration = Duration::from_secs(10);

/// Names an `http://host:port` origin that stands in for every CDN host:
/// `https://unpkg.com/a.js` is fetched as `<origin>/unpkg.com/a.js`. Tests
/// point it at a local server.
pub const OVERRIDE_ENV: &str = "CANVAS_CDN_ORIGIN";

/// Only a debug build (tests, `admin dev`) reads `OVERRIDE_ENV`; a release
/// build always fetches the real host.
const HONOR_OVERRIDE: bool = cfg!(debug_assertions);

/// True for an `https://` URL on one of `HOSTS`.
pub fn allowed(url: &str) -> bool {
    url.strip_prefix("https://")
        .map(|rest| {
            let host = rest.split(['/', '?', '#']).next().unwrap_or("");
            HOSTS.iter().any(|h| host.eq_ignore_ascii_case(h))
        })
        .unwrap_or(false)
}

/// `reference` resolved against `base`, as an absolute URL with its scheme
/// lowercased. A reference that is already absolute, or protocol-relative
/// (taken as https), needs no base; a relative one with no base, or one with
/// a scheme other than http(s), resolves to None.
pub fn join(base: Option<&str>, reference: &str) -> Option<String> {
    let scheme_len = reference
        .find(':')
        .filter(|&i| {
            i > 0
                && reference[..i]
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
        })
        .map(|i| i + 1);
    if let Some(n) = scheme_len {
        let scheme = reference[..n].to_ascii_lowercase();
        return (matches!(scheme.as_str(), "http:" | "https:") && reference[n..].starts_with("//"))
            .then(|| format!("{scheme}{}", &reference[n..]));
    }
    if let Some(rest) = reference.strip_prefix("//") {
        return Some(format!("https://{rest}"));
    }
    let base = base?;
    if reference.starts_with(['?', '#']) {
        let cut = if reference.starts_with('?') {
            ['?', '#'].as_slice()
        } else {
            ['#'].as_slice()
        };
        let end = base.find(cut).unwrap_or(base.len());
        return Some(format!("{}{reference}", &base[..end]));
    }
    let scheme_end = base.find("://")? + 3;
    let path_start = base[scheme_end..]
        .find(['/', '?', '#'])
        .map(|i| scheme_end + i)
        .unwrap_or(base.len());
    let origin = &base[..path_start];
    if reference.starts_with('/') {
        return Some(format!("{origin}{}", normalize(reference)));
    }
    let base_path = base[path_start..].split(['?', '#']).next().unwrap_or("");
    let dir = &base_path[..base_path.rfind('/').map(|i| i + 1).unwrap_or(0)];
    let dir = if dir.is_empty() { "/" } else { dir };
    Some(format!(
        "{origin}{}",
        normalize(&format!("{dir}{reference}"))
    ))
}

/// Removes `.` and `..` segments from an absolute path, leaving its query.
fn normalize(path: &str) -> String {
    let (path, query) = match path.find(['?', '#']) {
        Some(i) => path.split_at(i),
        None => (path, ""),
    };
    let mut out: Vec<&str> = Vec::new();
    let segments: Vec<&str> = path.split('/').skip(1).collect();
    for (i, seg) in segments.iter().enumerate() {
        let last = i + 1 == segments.len();
        match *seg {
            "." => {
                if last {
                    out.push("");
                }
            }
            ".." => {
                out.pop();
                if last {
                    out.push("");
                }
            }
            s => out.push(s),
        }
    }
    format!("/{}{query}", out.join("/"))
}

/// The URL actually requested for `url`, given the override origin a build
/// honours (`honor`) and the one set (`origin`).
fn request_url(url: &str, origin: Option<&str>, honor: bool) -> String {
    match (honor, origin, url.strip_prefix("https://")) {
        (true, Some(origin), Some(rest)) => format!("{}/{rest}", origin.trim_end_matches('/')),
        _ => url.to_string(),
    }
}

/// Most redirects one download follows, each to an allowed host.
const MAX_REDIRECTS: usize = 5;

/// Downloads `url`, which `allowed` has passed, within `timeout`. Errors are
/// one line naming why, for an export warning's `reason`.
pub fn fetch(url: &str, timeout: Duration) -> Result<Vec<u8>, String> {
    let origin = std::env::var(OVERRIDE_ENV).ok();
    let started = Instant::now();
    let result = download(
        url,
        timeout,
        |u| request_url(u, origin.as_deref(), HONOR_OVERRIDE),
        &tls_config(),
        &system_lookup(),
    );
    let ms = started.elapsed().as_millis();
    match &result {
        Ok(bytes) => canvas_core::log::info(
            "export fetch",
            &[("url", &url), ("bytes", &bytes.len()), ("ms", &ms)],
        ),
        Err(reason) => canvas_core::log::warn(
            "export fetch failed",
            &[("url", &url), ("error", reason), ("ms", &ms)],
        ),
    }
    result
}

/// Most connections one request opens. Each but the last gets half the time
/// left to finish its TCP connect and TLS handshake, the last all of it; one
/// that stalls there is dropped for a fresh one, since a CDN edge sometimes
/// sits on a new connection for seconds while the next answers at once.
const SETUP_ATTEMPTS: u32 = 3;

/// The TLS settings ureq's own default uses: the *ring* provider and the
/// webpki roots.
fn tls_config() -> Arc<rustls::ClientConfig> {
    static CONFIG: OnceLock<Arc<rustls::ClientConfig>> = OnceLock::new();
    CONFIG
        .get_or_init(|| {
            let roots = rustls::RootCertStore {
                roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
            };
            Arc::new(
                rustls::ClientConfig::builder_with_provider(
                    rustls::crypto::ring::default_provider().into(),
                )
                .with_safe_default_protocol_versions()
                .expect("the ring provider supports the default TLS versions")
                .with_root_certificates(roots)
                .with_no_client_auth(),
            )
        })
        .clone()
}

/// Runs the TLS handshake under the attempt's setup limit. ureq leaves the
/// socket's read and write timeouts at the whole request's deadline while it
/// shakes hands, so without this one stalled handshake spends all of it.
struct SetupLimit {
    tls: Arc<rustls::ClientConfig>,
    until: Instant,
}

impl ureq::TlsConnector for SetupLimit {
    fn connect(
        &self,
        dns_name: &str,
        io: Box<dyn ureq::ReadWrite>,
    ) -> Result<Box<dyn ureq::ReadWrite>, ureq::Error> {
        // A timeout can only shorten here: `until` is never past the deadline.
        let left = self
            .until
            .saturating_duration_since(Instant::now())
            .max(Duration::from_millis(1));
        let restore = match io.socket() {
            Some(socket) => {
                let saved = (socket.read_timeout()?, socket.write_timeout()?);
                socket.set_read_timeout(Some(left))?;
                socket.set_write_timeout(Some(left))?;
                Some(saved)
            }
            None => None,
        };
        let stream = self.tls.connect(dns_name, io)?;
        if let (Some((read, write)), Some(socket)) = (restore, stream.socket()) {
            socket.set_read_timeout(read)?;
            socket.set_write_timeout(write)?;
        }
        Ok(stream)
    }
}

/// Whether `e` is a socket timeout. A socket read or write timeout on macOS
/// fails with EAGAIN (`WouldBlock`) rather than `TimedOut`; ureq turns that
/// into `TimedOut` for a response read, but not for the TLS handshake.
fn is_timeout(e: &(dyn std::error::Error + 'static)) -> bool {
    e.downcast_ref::<std::io::Error>().is_some_and(|io| {
        matches!(
            io.kind(),
            std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
        )
    })
}

/// Looks up a `host:port`'s addresses: the system resolver, or a test's
/// stand-in.
type Lookup = Arc<dyn Fn(&str) -> std::io::Result<Vec<SocketAddr>> + Send + Sync>;

fn system_lookup() -> Lookup {
    Arc::new(|netloc| netloc.to_socket_addrs().map(Iterator::collect))
}

/// Looks up `url`'s host by `deadline`. ureq's own lookup has no deadline and
/// runs after its connect timeout has started, so a slow resolver would spend
/// both the request's time and an attempt's setup window. The lookup runs on
/// a thread of its own, left to finish there when it outlasts the deadline.
/// Only a timeout or a URL with no host is an `Err`; the lookup's own result,
/// failure included, is handed to ureq to report as it would its own.
fn resolve(
    url: &str,
    deadline: Instant,
    lookup: &Lookup,
) -> Result<std::io::Result<Vec<SocketAddr>>, Box<ureq::Error>> {
    let fail = |kind, message: String| Box::new(std::io::Error::new(kind, message).into());
    let parsed = url::Url::parse(url)
        .map_err(|e| fail(std::io::ErrorKind::InvalidInput, format!("{url}: {e}")))?;
    let (Some(host), Some(port)) = (parsed.host_str(), parsed.port_or_known_default()) else {
        return Err(fail(
            std::io::ErrorKind::InvalidInput,
            format!("{url}: no host"),
        ));
    };
    let netloc = format!("{host}:{port}");
    let (send, receive) = mpsc::channel();
    let lookup = lookup.clone();
    std::thread::spawn(move || {
        let _ = send.send(lookup(&netloc));
    });
    receive
        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .map_err(|e| match e {
            mpsc::RecvTimeoutError::Timeout => fail(
                std::io::ErrorKind::TimedOut,
                format!("looking up {host} timed out"),
            ),
            mpsc::RecvTimeoutError::Disconnected => fail(
                std::io::ErrorKind::Other,
                format!("looking up {host} failed"),
            ),
        })
}

fn first_line(message: String) -> String {
    message.lines().next().unwrap_or(&message).to_string()
}

/// GETs `url` by `deadline`, opening a fresh connection when one stalls in
/// its TCP connect or TLS handshake (see `SETUP_ATTEMPTS`). The host is
/// looked up once per request, before the first attempt's setup window starts.
fn get(
    url: &str,
    deadline: Instant,
    tls: &Arc<rustls::ClientConfig>,
    lookup: &Lookup,
) -> Result<ureq::Response, Box<ureq::Error>> {
    let found = Arc::new(resolve(url, deadline, lookup)?);
    let mut attempt = 1;
    loop {
        let started = Instant::now();
        let left = deadline.saturating_duration_since(started);
        if left.is_zero() {
            return Err(Box::new(
                std::io::Error::from(std::io::ErrorKind::TimedOut).into(),
            ));
        }
        let setup = if attempt == SETUP_ATTEMPTS {
            left
        } else {
            left / 2
        };
        let found = found.clone();
        let agent = ureq::AgentBuilder::new()
            .redirects(0)
            // With no redirects and no proxy, ureq asks only for `url`'s host.
            .resolver(move |_: &str| match &*found {
                Ok(addrs) => Ok(addrs.clone()),
                Err(e) => Err(std::io::Error::new(e.kind(), e.to_string())),
            })
            .timeout_connect(setup)
            .tls_connector(Arc::new(SetupLimit {
                tls: tls.clone(),
                until: started + setup,
            }))
            // Google Fonts picks the font format by user agent; this one gets
            // woff2.
            .user_agent("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Safari/605.1.15")
            .build();
        match agent.get(url).timeout(left).call() {
            Err(ureq::Error::Transport(t))
                if attempt < SETUP_ATTEMPTS
                    && t.kind() == ureq::ErrorKind::ConnectionFailed
                    && std::error::Error::source(&t).is_some_and(is_timeout) =>
            {
                canvas_core::log::warn(
                    "export fetch setup stalled",
                    &[
                        ("url", &url),
                        ("attempt", &attempt),
                        ("ms", &started.elapsed().as_millis()),
                    ],
                );
                attempt += 1;
            }
            other => return other.map_err(Box::new),
        }
    }
}

/// Follows redirects itself so every hop stays on an allowed host; `route`
/// maps a URL to the one actually requested.
fn download(
    url: &str,
    timeout: Duration,
    route: impl Fn(&str) -> String,
    tls: &Arc<rustls::ClientConfig>,
    lookup: &Lookup,
) -> Result<Vec<u8>, String> {
    let deadline = Instant::now() + timeout;
    let timed_out = || format!("timed out after {}s", timeout.as_secs_f32().ceil());
    let mut current = url.to_string();
    for _ in 0..=MAX_REDIRECTS {
        let response = get(&route(&current), deadline, tls, lookup).map_err(|e| match *e {
            ureq::Error::Status(code, _) => format!("HTTP {code}"),
            ureq::Error::Transport(t) if std::error::Error::source(&t).is_some_and(is_timeout) => {
                timed_out()
            }
            ureq::Error::Transport(t) => first_line(t.to_string()),
        })?;
        if (300..400).contains(&response.status()) {
            let next = response
                .header("location")
                .and_then(|l| join(Some(&current), l))
                .ok_or_else(|| format!("HTTP {} with no usable Location", response.status()))?;
            if !allowed(&next) {
                return Err(format!("redirected off the allowed hosts to {next}"));
            }
            current = next;
            continue;
        }
        let mut bytes = Vec::new();
        response
            .into_reader()
            .take(MAX_ASSET_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| {
                if is_timeout(&e) {
                    timed_out()
                } else {
                    first_line(e.to_string())
                }
            })?;
        return Ok(bytes);
    }
    Err(format!("more than {MAX_REDIRECTS} redirects"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn only_https_on_the_allowed_hosts() {
        assert!(allowed("https://cdn.jsdelivr.net/npm/a.js"));
        assert!(allowed("https://fonts.googleapis.com?x"));
        assert!(!allowed("http://unpkg.com/a.js"));
        assert!(!allowed("https://unpkg.com.evil.test/a.js"));
        assert!(!allowed("https://example.com/a.js"));
    }

    #[test]
    fn joins_relative_references() {
        let base = Some("https://cdnjs.cloudflare.com/x/6.0/css/all.min.css?v=1");
        assert_eq!(
            join(base, "../webfonts/fa.woff2").as_deref(),
            Some("https://cdnjs.cloudflare.com/x/6.0/webfonts/fa.woff2")
        );
        assert_eq!(
            join(base, "/y/b.css").as_deref(),
            Some("https://cdnjs.cloudflare.com/y/b.css")
        );
        assert_eq!(
            join(base, "./a.woff2?v=2").as_deref(),
            Some("https://cdnjs.cloudflare.com/x/6.0/css/a.woff2?v=2")
        );
        assert_eq!(
            join(None, "//unpkg.com/a.css").as_deref(),
            Some("https://unpkg.com/a.css")
        );
        assert_eq!(join(None, "a.css"), None);
        assert_eq!(
            join(None, "HTTPS://unpkg.com/a.css").as_deref(),
            Some("https://unpkg.com/a.css")
        );
        assert_eq!(join(base, "ftp://x/a.css"), None);
        assert_eq!(
            join(base, "?v=2").as_deref(),
            Some("https://cdnjs.cloudflare.com/x/6.0/css/all.min.css?v=2")
        );
    }

    #[test]
    fn the_override_is_read_only_by_a_debug_build() {
        let url = "https://unpkg.com/a.js";
        let origin = Some("http://127.0.0.1:9/");
        assert_eq!(request_url(url, origin, false), url);
        assert_eq!(
            request_url(url, origin, true),
            "http://127.0.0.1:9/unpkg.com/a.js"
        );
        assert_eq!(HONOR_OVERRIDE, cfg!(debug_assertions));
    }

    /// A local HTTPS server for `localhost` under a test CA; `answer(n)`
    /// says whether its `n`th connection (from 1) finishes the handshake and
    /// answers `ok`, or is held open without a byte. Returns the URL, the
    /// client settings that trust the CA, and the count of connections.
    fn tls_server(
        answer: impl Fn(usize) -> bool + Send + 'static,
    ) -> (String, Arc<rustls::ClientConfig>, Arc<AtomicUsize>) {
        use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
        let provider = || Arc::new(rustls::crypto::ring::default_provider());
        let server = Arc::new(
            rustls::ServerConfig::builder_with_provider(provider())
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_no_client_auth()
                .with_single_cert(
                    vec![CertificateDer::from(
                        include_bytes!("../tests/fixtures/tls/localhost.der").to_vec(),
                    )],
                    PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
                        include_bytes!("../tests/fixtures/tls/localhost.key.der").to_vec(),
                    )),
                )
                .unwrap(),
        );
        let mut roots = rustls::RootCertStore::empty();
        roots
            .add(CertificateDer::from(
                include_bytes!("../tests/fixtures/tls/ca.der").to_vec(),
            ))
            .unwrap();
        let client = Arc::new(
            rustls::ClientConfig::builder_with_provider(provider())
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_root_certificates(roots)
                .with_no_client_auth(),
        );
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!(
            "https://localhost:{}/",
            listener.local_addr().unwrap().port()
        );
        let seen = Arc::new(AtomicUsize::new(0));
        let counter = seen.clone();
        std::thread::spawn(move || {
            let mut held = Vec::new();
            for tcp in listener.incoming().flatten() {
                let n = counter.fetch_add(1, Ordering::SeqCst) + 1;
                if !answer(n) {
                    held.push(tcp);
                    continue;
                }
                let conn = rustls::ServerConnection::new(server.clone()).unwrap();
                let mut tls = rustls::StreamOwned::new(conn, tcp);
                let mut request = Vec::new();
                let mut byte = [0u8; 1];
                while !request.ends_with(b"\r\n\r\n") && tls.read(&mut byte).unwrap_or(0) == 1 {
                    request.push(byte[0]);
                }
                let _ = tls.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok");
                let _ = tls.flush();
            }
        });
        (url, client, seen)
    }

    #[test]
    fn a_stalled_handshake_is_dropped_for_a_fresh_connection() {
        let (url, tls, seen) = tls_server(|n| n > 1);
        let started = Instant::now();
        let body = download(
            &url,
            Duration::from_secs(4),
            |u| u.to_string(),
            &tls,
            &system_lookup(),
        );
        assert_eq!(body.as_deref(), Ok(&b"ok"[..]));
        // The first connection gets half the 4s to shake hands.
        let ms = started.elapsed().as_millis();
        assert!((1900..3000).contains(&ms), "took {ms}ms");
        assert_eq!(seen.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn a_handshake_that_never_finishes_is_a_timeout() {
        let (url, tls, seen) = tls_server(|_| false);
        let started = Instant::now();
        let body = download(
            &url,
            Duration::from_millis(1500),
            |u| u.to_string(),
            &tls,
            &system_lookup(),
        );
        assert_eq!(body, Err("timed out after 2s".to_string()));
        let ms = started.elapsed().as_millis();
        assert!((1400..2000).contains(&ms), "took {ms}ms");
        assert_eq!(seen.load(Ordering::SeqCst), SETUP_ATTEMPTS as usize);
    }

    /// The system lookup after `delay`, counting its calls.
    fn slow_lookup(delay: Duration) -> (Lookup, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        let lookup: Lookup = Arc::new(move |netloc| {
            counter.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(delay);
            system_lookup()(netloc)
        });
        (lookup, calls)
    }

    #[test]
    fn a_lookup_that_outlasts_the_deadline_is_a_timeout() {
        let (url, tls, seen) = tls_server(|_| true);
        let (lookup, _) = slow_lookup(Duration::from_secs(5));
        let started = Instant::now();
        let body = download(
            &url,
            Duration::from_secs(1),
            |u| u.to_string(),
            &tls,
            &lookup,
        );
        assert_eq!(body, Err("timed out after 1s".to_string()));
        let ms = started.elapsed().as_millis();
        assert!((950..1500).contains(&ms), "took {ms}ms");
        assert_eq!(seen.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_slow_lookup_leaves_the_handshake_its_own_window() {
        let (url, tls, seen) = tls_server(|_| true);
        // Longer than the first attempt's half of the 2s, were it counted.
        let (lookup, calls) = slow_lookup(Duration::from_millis(1200));
        let body = download(
            &url,
            Duration::from_secs(2),
            |u| u.to_string(),
            &tls,
            &lookup,
        );
        assert_eq!(body.as_deref(), Ok(&b"ok"[..]));
        assert_eq!(seen.load(Ordering::SeqCst), 1);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_failed_lookup_reads_as_ureqs_own() {
        let (url, tls, seen) = tls_server(|_| true);
        let lookup: Lookup = Arc::new(|_| Err(std::io::Error::other("no such host")));
        let body = download(
            &url,
            Duration::from_secs(2),
            |u| u.to_string(),
            &tls,
            &lookup,
        );
        let port = url
            .trim_start_matches("https://localhost:")
            .trim_end_matches('/');
        assert_eq!(
            body,
            Err(format!(
                "{url}: Dns Failed: resolve dns name 'localhost:{port}': no such host"
            ))
        );
        assert_eq!(seen.load(Ordering::SeqCst), 0);
    }
}
