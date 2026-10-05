//! Downloads the CDN assets an export inlines. Only the hosts a card's CSP
//! lets it load from are fetched, over HTTPS, one request at a time with a
//! 10s limit on the whole download and at most `MAX_ASSET_BYTES + 1` bytes
//! read, so the export can tell an oversize body from one that fits. This is
//! outbound only; canvasd still listens on nothing but its Unix socket.

use std::io::Read;
use std::time::{Duration, Instant};

use canvas_core::MAX_ASSET_BYTES;

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
    let result = download(url, timeout, |u| {
        request_url(u, origin.as_deref(), HONOR_OVERRIDE)
    });
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

/// Follows redirects itself so every hop stays on an allowed host; `route`
/// maps a URL to the one actually requested.
fn download(
    url: &str,
    timeout: Duration,
    route: impl Fn(&str) -> String,
) -> Result<Vec<u8>, String> {
    let deadline = Instant::now() + timeout;
    let agent = ureq::AgentBuilder::new()
        .redirects(0)
        // Google Fonts picks the font format by user agent; this one gets
        // woff2.
        .user_agent("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Safari/605.1.15")
        .build();
    let timed_out = || format!("timed out after {}s", timeout.as_secs_f32().ceil());
    let reason = |message: String| {
        let lower = message.to_ascii_lowercase();
        if lower.contains("timed out") || lower.contains("timeout") {
            timed_out()
        } else {
            message.lines().next().unwrap_or(&message).to_string()
        }
    };
    let mut current = url.to_string();
    for _ in 0..=MAX_REDIRECTS {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(timed_out());
        }
        let response = agent
            .get(&route(&current))
            .timeout(left)
            .call()
            .map_err(|e| match e {
                ureq::Error::Status(code, _) => format!("HTTP {code}"),
                ureq::Error::Transport(t) => reason(t.to_string()),
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
            .map_err(|e| reason(e.to_string()))?;
        return Ok(bytes);
    }
    Err(format!("more than {MAX_REDIRECTS} redirects"))
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
