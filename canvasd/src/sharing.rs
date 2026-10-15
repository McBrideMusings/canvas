//! `sharing.toml` in `CANVAS_DATA_DIR` (ADR-0006): which items go upstream and
//! how this daemon answers other machines. Read once when canvasd starts.

use std::net::IpAddr;
use std::path::Path;
use std::time::Duration;

use canvas_core::PostRequest;
use serde::Deserialize;

pub const FILE: &str = "sharing.toml";

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sharing {
    /// Base URL of the upstream canvasd, such as `https://canvas.example`.
    pub upstream: Option<String>,
    #[serde(default)]
    pub timeline: Rules,
    #[serde(default)]
    pub artifacts: Rules,
    pub listener: Option<ListenerConfig>,
}

/// Glob lists for one kind of item, matched against the git root of the
/// session's working directory, else the working directory itself. Exclude
/// wins; with no include nothing matches.
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rules {
    #[serde(default)]
    pub include: Vec<String>,
    #[serde(default)]
    pub exclude: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListenerConfig {
    /// TCP port to listen on.
    pub port: u16,
    /// The one address to listen on. Without it the listener does not start.
    pub bind: Option<IpAddr>,
    /// Addresses or CIDR ranges (`100.64.0.0/10`, `100.70.1.2`) admitted as
    /// callers. Without the key the listener does not start; an empty list
    /// admits nothing.
    pub allow: Option<Vec<Cidr>>,
    /// Address of a reverse proxy whose requests carry the real client in
    /// `X-Forwarded-For`.
    pub proxy: Option<IpAddr>,
    /// Host names besides the bind address that the listener answers, such as
    /// the proxy's name for this machine.
    #[serde(default)]
    pub hosts: Vec<String>,
}

/// An IP address range: `addr/prefix`, or a single address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cidr {
    addr: IpAddr,
    prefix: u8,
}

impl Cidr {
    pub fn contains(&self, ip: IpAddr) -> bool {
        // An IPv4-mapped IPv6 address is the IPv4 address it wraps.
        let ip = match ip {
            IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, IpAddr::V4),
            v4 => v4,
        };
        match (self.addr, ip) {
            (IpAddr::V4(net), IpAddr::V4(ip)) => prefix_matches(
                u128::from(u32::from(net)),
                u128::from(u32::from(ip)),
                self.prefix,
                32,
            ),
            (IpAddr::V6(net), IpAddr::V6(ip)) => {
                prefix_matches(u128::from(net), u128::from(ip), self.prefix, 128)
            }
            _ => false,
        }
    }
}

fn prefix_matches(net: u128, ip: u128, prefix: u8, bits: u8) -> bool {
    let shift = u32::from(bits - prefix);
    shift >= 128 || (net >> shift) == (ip >> shift)
}

impl std::str::FromStr for Cidr {
    type Err = String;

    fn from_str(text: &str) -> Result<Cidr, String> {
        let (addr, prefix) = match text.split_once('/') {
            Some((addr, prefix)) => (addr, Some(prefix)),
            None => (text, None),
        };
        let addr: IpAddr = addr
            .trim()
            .parse()
            .map_err(|_| format!("{text:?} is not an IP address or CIDR range"))?;
        let bits = if addr.is_ipv4() { 32 } else { 128 };
        let prefix = match prefix {
            Some(p) => p
                .trim()
                .parse::<u8>()
                .ok()
                .filter(|p| *p <= bits)
                .ok_or_else(|| format!("{text:?} has a bad prefix length"))?,
            None => bits,
        };
        Ok(Cidr { addr, prefix })
    }
}

impl<'de> Deserialize<'de> for Cidr {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Cidr, D::Error> {
        let text = String::deserialize(d)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

impl Sharing {
    /// Reads `sharing.toml` from `dir`. A missing file is the default (nothing
    /// shared, no listener); one that can't be read or parsed is an error the
    /// caller logs before using the default.
    pub fn load(dir: &Path) -> Result<Sharing, String> {
        let path = dir.join(FILE);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Sharing::default()),
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// Whether a post from a session at `cwd` (inside `git_root`, when known)
    /// goes upstream.
    pub fn shares_post(&self, req: &PostRequest) -> bool {
        self.upstream.is_some() && self.timeline.allows(&share_key(req))
    }
}

/// This machine's name as other machines see it: the host name without a
/// trailing `.local`.
pub fn machine_name() -> String {
    let mut buf = [0u8; 256];
    // SAFETY: the buffer is 256 bytes and gethostname writes at most that many.
    let ok = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) } == 0;
    let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
    let name = String::from_utf8_lossy(&buf[..end]).into_owned();
    let name = name.strip_suffix(".local").unwrap_or(&name);
    if ok && !name.is_empty() {
        name.to_string()
    } else {
        "this machine".to_string()
    }
}

/// What the rules match: the git root when it holds the working directory,
/// else the working directory.
pub fn share_key(req: &PostRequest) -> String {
    match &req.git_root {
        Some(root) if Path::new(&req.cwd).starts_with(root) => root.clone(),
        _ => req.cwd.clone(),
    }
}

impl Rules {
    pub fn allows(&self, path: &str) -> bool {
        self.include.iter().any(|g| glob_match(g, path))
            && !self.exclude.iter().any(|g| glob_match(g, path))
    }
}

/// Glob match where `*` stops at `/`, `**` crosses it, `?` is one non-`/`
/// character and a leading `~/` is the home folder.
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let pattern = match (pattern.strip_prefix("~/"), std::env::var("HOME")) {
        (Some(rest), Ok(home)) => format!("{}/{rest}", home.trim_end_matches('/')),
        _ => pattern.to_string(),
    };
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    glob(&p, &t)
}

fn glob(p: &[char], t: &[char]) -> bool {
    match p.first() {
        None => t.is_empty(),
        Some('*') => {
            let deep = p.get(1) == Some(&'*');
            let rest = &p[if deep { 2 } else { 1 }..];
            (0..=t.len()).any(|n| (deep || !t[..n].contains(&'/')) && glob(rest, &t[n..]))
        }
        Some('?') => matches!(t.first(), Some(c) if *c != '/') && glob(&p[1..], &t[1..]),
        Some(c) => t.first() == Some(c) && glob(&p[1..], &t[1..]),
    }
}

/// Sends one post upstream as bytes: each image or video the post names is
/// uploaded to the upstream's `POST /api/media`, then the post itself goes to
/// `POST /api/posts` carrying the names that answered. Blocking.
pub fn forward_post(upstream: &str, media_dir: &Path, mut req: PostRequest) -> Result<(), String> {
    let base = upstream.trim_end_matches('/');
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(60))
        .build();
    let mut names = Vec::new();
    for name in &req.images {
        let path = media_dir.join(name);
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .ok_or_else(|| format!("{name} has no extension"))?;
        let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let answer: serde_json::Value = agent
            .post(&format!("{base}/api/media?ext={ext}"))
            .send_bytes(&bytes)
            .map_err(|e| format!("media upload: {e}"))?
            .into_string()
            .map_err(|e| format!("media upload answer: {e}"))
            .and_then(|t| {
                serde_json::from_str(&t).map_err(|e| format!("media upload answer: {e}"))
            })?;
        let stored = answer
            .get("name")
            .and_then(|n| n.as_str())
            .ok_or("media upload answered no name")?;
        names.push(stored.to_string());
    }
    req.images = names;
    // The pid belongs to this machine; upstream's sweep must not check it.
    req.pid = None;
    req.machine = Some(machine_name());
    agent
        .post(&format!("{base}/api/posts"))
        .set("content-type", "application/json")
        .send_string(&serde_json::to_string(&req).map_err(|e| e.to_string())?)
        .map_err(|e| format!("post: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_star_stops_at_slash_and_double_star_crosses_it() {
        assert!(glob_match("/work/*", "/work/app"));
        assert!(!glob_match("/work/*", "/work/app/sub"));
        assert!(glob_match("/work/**", "/work/app/sub"));
        assert!(glob_match("/work/a?p", "/work/app"));
        assert!(!glob_match("/work/a?p", "/work/a/p"));
    }

    #[test]
    fn exclude_wins_and_no_include_shares_nothing() {
        let rules = Rules {
            include: vec!["/work/*".into()],
            exclude: vec!["/work/secret".into()],
        };
        assert!(rules.allows("/work/app"));
        assert!(!rules.allows("/work/secret"));
        assert!(!rules.allows("/elsewhere/app"));
        assert!(!Rules::default().allows("/work/app"));
    }

    #[test]
    fn parses_the_documented_shape() {
        let s: Sharing = toml::from_str(
            r#"
            upstream = "http://100.64.0.1:7000"
            [timeline]
            include = ["/work/*"]
            [listener]
            port = 7000
            bind = "100.64.0.1"
            allow = ["100.64.0.0/10", "192.168.1.5"]
            proxy = "100.64.0.9"
            hosts = ["canvas.test"]
            "#,
        )
        .unwrap();
        let l = s.listener.unwrap();
        assert_eq!(l.bind, Some("100.64.0.1".parse().unwrap()));
        let allow = l.allow.clone().unwrap();
        assert!(allow[0].contains("100.127.255.255".parse().unwrap()));
        assert!(!allow[0].contains("100.128.0.0".parse().unwrap()));
        assert!(allow[1].contains("192.168.1.5".parse().unwrap()));
        assert!(!allow[1].contains("192.168.1.6".parse().unwrap()));
        assert_eq!((l.port, l.hosts), (7000, vec!["canvas.test".to_string()]));
        assert_eq!(l.proxy, Some("100.64.0.9".parse().unwrap()));
        assert!(toml::from_str::<Sharing>("bogus = 1").is_err());
        assert!(
            toml::from_str::<Sharing>("[listener]\nport = 1\nallow = [\"100.64.0.0/33\"]").is_err()
        );
    }
}
