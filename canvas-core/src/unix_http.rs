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
    stream.write_all(head.as_bytes())?;
    stream.write_all(body)?;
    Ok(stream)
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
    let head = read_head(&mut reader)?;
    let mut body = Vec::new();
    reader.read_to_end(&mut body)?;
    Ok(Response {
        status: head.status,
        content_type: head.content_type,
        headers: head.headers,
        body,
    })
}

/// A GET whose body stays open; the caller reads lines from `body`.
/// `timeout` bounds each read, so a stream that goes quiet for longer than
/// that ends with an error instead of blocking forever.
pub fn open_stream(socket: &Path, path: &str, timeout: Duration) -> io::Result<Stream> {
    let stream = send(socket, "GET", path, &[], &[], Some(timeout))?;
    let mut reader = BufReader::new(stream);
    let status = read_head(&mut reader)?.status;
    Ok(Stream {
        status,
        body: reader,
    })
}
