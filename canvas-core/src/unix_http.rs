//! Minimal blocking HTTP/1.0 client over a Unix domain socket.
//!
//! canvasd listens on a socket, not a TCP port, and the CLI, hooks and app
//! are its only clients. HTTP/1.0 with `Connection: close` means the server
//! frames the body by closing the connection, so there is no chunked
//! decoding to write — a plain read to EOF is the whole body, and a streaming
//! response (`/api/events`) is just a body that stays open.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

pub struct Response {
    pub status: u16,
    pub content_type: Option<String>,
    /// Every header line, names lowercased, in the order they came.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Response {
    /// The first header named `name` (lowercase).
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }
}

struct Head {
    status: u16,
    content_type: Option<String>,
    headers: Vec<(String, String)>,
}

/// An open response whose body is read incrementally.
pub struct Stream {
    pub status: u16,
    pub body: BufReader<UnixStream>,
}

fn send(
    socket: &Path,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &[u8],
    timeout: Option<Duration>,
) -> io::Result<UnixStream> {
    let mut stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(timeout)?;
    stream.set_write_timeout(timeout)?;
    let mut head = format!("{method} {path} HTTP/1.0\r\nHost: localhost\r\n");
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    if !body.is_empty() {
        head.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    head.push_str("\r\n");
    stream
        .write_all(head.as_bytes())
        .and_then(|()| stream.write_all(body))
        .map_err(|e| no_answer(e, timeout))?;
    Ok(stream)
}

/// A read or write past `SO_RCVTIMEO`/`SO_SNDTIMEO` fails with EAGAIN
/// (`WouldBlock`, "Resource temporarily unavailable") on macOS, which reads as
/// a connect failure. The connection was made; canvasd stalled for the whole
/// timeout (taking the request, before the head, or partway through the
/// body), so say that, as `TimedOut`.
fn no_answer(e: io::Error, timeout: Option<Duration>) -> io::Error {
    match (e.kind(), timeout) {
        (io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut, Some(timeout)) => io::Error::new(
            io::ErrorKind::TimedOut,
            format!("canvasd stalled for {timeout:?} mid-request"),
        ),
        _ => e,
    }
}

fn read_head(reader: &mut BufReader<UnixStream>) -> io::Result<Head> {
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let status = line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "malformed status line"))?;
    let mut content_type = None;
    let mut headers = Vec::new();
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "response ended inside the headers",
            ));
        }
        let line = line.trim_end();
        if line.is_empty() {
            return Ok(Head {
                status,
                content_type,
                headers,
            });
        }
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("content-type") {
                content_type = Some(value.trim().to_string());
            }
            headers.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
        }
    }
}

/// One request, read to EOF. `timeout` bounds each socket read and write.
pub fn request(
    socket: &Path,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &[u8],
    timeout: Option<Duration>,
) -> io::Result<Response> {
    let stream = send(socket, method, path, headers, body, timeout)?;
    let mut reader = BufReader::new(stream);
    let head = read_head(&mut reader).map_err(|e| no_answer(e, timeout))?;
    let mut body = Vec::new();
    reader
        .read_to_end(&mut body)
        .map_err(|e| no_answer(e, timeout))?;
    Ok(Response {
        status: head.status,
        content_type: head.content_type,
        headers: head.headers,
        body,
    })
}

/// One request to canvasd's socket ([`crate::paths::socket_path`]), read to
/// EOF and logged: its status and duration, the daemon's error text for a 4xx
/// or 5xx, that it didn't answer in time, or why canvasd was unreachable.
pub fn call(
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &[u8],
    timeout: Duration,
) -> io::Result<Response> {
    let socket = crate::paths::socket_path()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no HOME to place the socket"))?;
    let started = std::time::Instant::now();
    let result = request(&socket, method, path, headers, body, Some(timeout));
    log_call(method, path, started.elapsed().as_millis(), &result);
    result
}

/// One line for a failed [`call`]: canvasd's timeout message as it stands,
/// anything else as a connection that was never made.
pub fn failure_text(e: &io::Error) -> String {
    if e.kind() == io::ErrorKind::TimedOut {
        e.to_string()
    } else {
        format!("could not reach canvasd: {e}")
    }
}

fn log_call(method: &str, path: &str, ms: u128, result: &io::Result<Response>) {
    use crate::log;
    let request = format!("{method} {path}");
    match result {
        Ok(response) if response.status < 400 => {
            log::info(&request, &[("status", &response.status), ("ms", &ms)])
        }
        Ok(response) => {
            let text = log::error_text(&response.body);
            let mut fields: Vec<(&str, &dyn std::fmt::Display)> =
                vec![("status", &response.status), ("ms", &ms)];
            if let Some(text) = &text {
                fields.push(("error", text));
            }
            log::warn(&request, &fields)
        }
        Err(e) if e.kind() == io::ErrorKind::TimedOut => log::warn(
            "canvasd timed out",
            &[("request", &request), ("ms", &ms), ("error", e)],
        ),
        Err(e) => log::warn(
            "canvasd unreachable",
            &[("request", &request), ("ms", &ms), ("error", e)],
        ),
    }
}

/// Percent-encodes everything but unreserved characters, for a path segment
/// or a query value.
pub fn percent_encode(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// A GET whose body stays open; the caller reads lines from `body`.
/// `timeout` bounds each read, so a stream that goes quiet for longer than
/// that ends with an error instead of blocking forever.
pub fn open_stream(socket: &Path, path: &str, timeout: Duration) -> io::Result<Stream> {
    let stream = send(socket, "GET", path, &[], &[], Some(timeout))?;
    let mut reader = BufReader::new(stream);
    let status = read_head(&mut reader)
        .map_err(|e| no_answer(e, Some(timeout)))?
        .status;
    Ok(Stream {
        status,
        body: reader,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    fn socket_path(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("cv-uh-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("s.sock")
    }

    #[test]
    fn a_server_that_accepts_and_never_answers_is_a_timeout_not_unreachable() {
        let path = socket_path("silent");
        let listener = UnixListener::bind(&path).unwrap();
        let held = std::thread::spawn(move || listener.accept().map(|(stream, _)| stream));

        let err = request(
            &path,
            "GET",
            "/",
            &[],
            &[],
            Some(Duration::from_millis(200)),
        )
        .err()
        .expect("a server that never answers can't produce a response");
        let _ = held.join();

        assert_eq!(err.kind(), io::ErrorKind::TimedOut, "{err}");
        assert_eq!(failure_text(&err), "canvasd stalled for 200ms mid-request");
    }

    #[test]
    fn no_listener_is_unreachable() {
        let path = socket_path("absent");
        let err = request(
            &path,
            "GET",
            "/",
            &[],
            &[],
            Some(Duration::from_millis(200)),
        )
        .err()
        .expect("there is nothing to answer");
        assert!(
            failure_text(&err).starts_with("could not reach canvasd: "),
            "{err}"
        );
    }
}
