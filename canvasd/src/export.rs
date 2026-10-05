//! One card as a standalone HTML page: images and videos become `data:` URIs
//! read from the card's stored paths, `#canvas-open-<n>` anchors become real
//! links or plain text from the card's targets, and the card's latest
//! `canvas data` value is delivered by a shim. None of the viewer's iframe
//! wrapping (theme rewrite, forced colours, contrast pass, resize script,
//! sandbox) is carried; `prefers-color-scheme` rules stay as written, so the
//! reader's system picks the palette. Scripts, stylesheets (with their
//! `@import`s) and `url()` assets on the CDN hosts a card may load from
//! (`crate::cdn::HOSTS`) are downloaded and inlined, so the page needs no
//! network; one that can't be fetched stays a link and warns. Pure apart from
//! the injected file reader and downloader, which are the test seams.

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

use canvas_core::html::{
    escape_attr, escape_text, find_attr_value_range, first_heading, next_tag, tag_name,
};
use canvas_core::{
    base64, Card, ExportResult, ExportWarning, ExportWarningKind, EXPORT_DOWNLOAD_SECS,
    MAX_ASSET_BYTES, MEDIA_EXTS,
};

use crate::cdn;

/// How many `@import`s deep a CDN stylesheet is followed.
const MAX_IMPORT_DEPTH: usize = 4;

/// Total bytes of CDN content one export inlines, counting a stylesheet once
/// per place it lands, so a stylesheet that imports another many times can't
/// grow the page without bound.
const MAX_INLINED_BYTES: usize = 16 * MAX_ASSET_BYTES;

/// `fetch(url, timeout)` downloads one URL within `timeout`.
pub fn export_card(
    card: &Card,
    data: Option<&serde_json::Value>,
    read: impl Fn(&str) -> io::Result<Vec<u8>>,
    fetch: impl Fn(&str, Duration) -> Result<Vec<u8>, String>,
) -> ExportResult {
    let mut warnings = Vec::new();
    let mut cdn = Cdn::new(fetch, Duration::from_secs(EXPORT_DOWNLOAD_SECS));
    let body = rewrite(card, &read, &mut cdn, &mut warnings);
    let title = escape_text(&first_heading(&body).unwrap_or_else(|| "Canvas post".into()));
    let mut html = String::with_capacity(body.len() + 1024);
    html.push_str("<!doctype html>\n<html><head><meta charset=\"utf-8\">");
    html.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">");
    html.push_str(&format!("<title>{title}</title>"));
    // A post replies to the viewer with parent.postMessage; at the top level
    // parent is this window, so the reply is stopped here before the page's
    // own listeners see it.
    html.push_str(
        "<script>window.addEventListener('message',function(e){\
         if(e.source===window&&e.data&&e.data.type==='canvas-reply')e.stopImmediatePropagation();\
         },true);</script>",
    );
    html.push_str(
        "<style>:root{color-scheme:light dark;}\
         body{margin:0;font-family:-apple-system,BlinkMacSystemFont,sans-serif;}\
         .canvas-export{box-sizing:border-box;max-width:878px;margin:0 auto;padding:16px;overflow-wrap:anywhere;}\
         :where(.canvas-export) *{max-width:100%;}\
         :where(.canvas-export) :is(img,video){height:auto;}\
         .canvas-missing{display:inline-block;padding:12px 16px;border:1px dashed currentColor;border-radius:6px;opacity:.7;font-size:13px;}\
         </style></head><body><div class=\"canvas-export\">",
    );
    html.push_str(&body);
    html.push_str("</div>");
    if let Some(value) = data {
        html.push_str(&data_shim(value));
    }
    html.push_str("</body></html>\n");
    ExportResult { html, warnings }
}

/// Delivers `value` to the page's own message listener the way the viewer
/// does, once the page has loaded. `<` is escaped so the value can't close
/// the script element.
fn data_shim(value: &serde_json::Value) -> String {
    let json = serde_json::to_string(value)
        .unwrap_or_else(|_| "null".into())
        .replace('<', "\\u003c")
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029");
    format!(
        "<script>(function(){{var v={json};function send(){{window.postMessage({{type:'canvas-data',value:v}},'*');}}\
         if(document.readyState==='complete')send();else window.addEventListener('load',send);}})();</script>"
    )
}

fn rewrite<F: Fn(&str, Duration) -> Result<Vec<u8>, String>>(
    card: &Card,
    read: &impl Fn(&str) -> io::Result<Vec<u8>>,
    cdn: &mut Cdn<F>,
    warnings: &mut Vec<ExportWarning>,
) -> String {
    let html = card.html.as_str();
    let image_prefix = format!("/api/cards/{}/images/", card.id);
    let mut out = String::with_capacity(html.len());
    // One entry per open <a>: true when its tags are dropped (a local path).
    let mut anchors: Vec<bool> = Vec::new();
    let mut pos = 0usize;

    while let Some((start, end)) = next_tag(html, pos) {
        out.push_str(&html[pos..start]);
        let tag = &html[start..end];
        pos = end;

        if tag.starts_with("</") {
            if closing_name(tag) == "a" && anchors.pop() == Some(true) {
                continue;
            }
            out.push_str(tag);
            continue;
        }

        match tag_name(tag).as_deref() {
            Some(kind @ ("img" | "video" | "source")) => {
                let Some((vs, ve)) = find_attr_value_range(tag, "src") else {
                    out.push_str(tag);
                    continue;
                };
                let Some(index) = tag[vs..ve]
                    .strip_prefix(&image_prefix)
                    .and_then(|n| n.parse::<usize>().ok())
                else {
                    out.push_str(tag);
                    continue;
                };
                let path = card.images.get(index).map(String::as_str).unwrap_or("");
                match media_data_uri(path, read) {
                    Ok(uri) => {
                        out.push_str(&tag[..vs]);
                        out.push_str(&uri);
                        out.push_str(&tag[ve..]);
                    }
                    Err(reason) => {
                        warnings.push(ExportWarning {
                            kind: ExportWarningKind::MissingImage,
                            target: path.to_string(),
                            reason,
                        });
                        let name = Path::new(path)
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        if kind == "img" {
                            out.push_str(&format!(
                                "<span class=\"canvas-missing\" role=\"img\">image missing: {}</span>",
                                escape_text(&name)
                            ));
                        } else {
                            // Drop ` src="…"` whole: an empty src would make
                            // the browser request the page's own URL.
                            out.push_str(&without_attr(tag, "src", (vs, ve)));
                        }
                    }
                }
            }
            Some("a") => {
                let link = find_attr_value_range(tag, "href").and_then(|(vs, ve)| {
                    let index = tag[vs..ve]
                        .strip_prefix("#canvas-open-")?
                        .parse::<usize>()
                        .ok()?;
                    Some((vs, ve, card.targets.get(index).map(String::as_str)))
                });
                match link {
                    None => {
                        anchors.push(false);
                        out.push_str(tag);
                    }
                    Some((vs, ve, Some(url)))
                        if url.starts_with("http://") || url.starts_with("https://") =>
                    {
                        anchors.push(false);
                        let close = if tag.ends_with("/>") { "/>" } else { ">" };
                        out.push_str(&tag[..vs]);
                        out.push_str(&escape_attr(url));
                        out.push_str(tag[ve..].strip_suffix(close).unwrap_or(&tag[ve..]));
                        out.push_str(" target=\"_blank\" rel=\"noopener\"");
                        out.push_str(close);
                    }
                    // A local path, or an index the card has no target for:
                    // the link text stays, the anchor goes.
                    Some(_) => anchors.push(true),
                }
            }
            Some("script") => {
                let src = find_attr_value_range(tag, "src")
                    .and_then(|(vs, ve)| Some((vs, ve, cdn::join(None, &attr_url(&tag[vs..ve]))?)))
                    .filter(|(_, _, url)| cdn::allowed(url));
                let Some((vs, ve, url)) = src else {
                    out.push_str(tag);
                    continue;
                };
                match cdn.get(&url, warnings) {
                    Some(bytes) => {
                        out.push_str(&without_attr(tag, "src", (vs, ve)));
                        out.push_str(&escape_raw(&String::from_utf8_lossy(&bytes), "script"));
                        // Whatever the element held is dropped with its src.
                        pos = raw_text_end(html, end, "script");
                    }
                    None => out.push_str(tag),
                }
            }
            Some("link") => {
                let Some(url) = stylesheet_href(tag).filter(|url| cdn::allowed(url)) else {
                    out.push_str(tag);
                    continue;
                };
                match cdn.get(&url, warnings) {
                    Some(bytes) => {
                        cdn.importing.push(url.clone());
                        let css =
                            cdn.css(&String::from_utf8_lossy(&bytes), Some(&url), 0, warnings);
                        cdn.importing.pop();
                        match find_attr_value_range(tag, "media") {
                            Some((ms, me)) => out.push_str(&format!(
                                "<style media=\"{}\">",
                                tag[ms..me].replace('"', "&quot;")
                            )),
                            None => out.push_str("<style>"),
                        }
                        out.push_str(&escape_raw(&css, "style"));
                        out.push_str("</style>");
                    }
                    None => out.push_str(tag),
                }
            }
            Some("style") => {
                let close = raw_text_end(html, end, "style");
                out.push_str(tag);
                let css = cdn.css(&html[end..close], None, 0, warnings);
                out.push_str(&escape_raw(&css, "style"));
                pos = close;
            }
            _ => out.push_str(tag),
        }
    }
    out.push_str(&html[pos..]);
    out
}

/// Downloads for one export: each URL fetched once, all of them within one
/// time budget, and at most `MAX_INLINED_BYTES` inlined in total. A URL that
/// isn't inlined warns once.
struct Cdn<F> {
    fetch: F,
    budget: Duration,
    deadline: Instant,
    cache: HashMap<String, Result<Vec<u8>, String>>,
    warned: HashSet<String>,
    inlined: usize,
    /// The stylesheets being expanded, outermost first, so an `@import`
    /// cycle stays an import.
    importing: Vec<String>,
}

impl<F: Fn(&str, Duration) -> Result<Vec<u8>, String>> Cdn<F> {
    fn new(fetch: F, budget: Duration) -> Self {
        Cdn {
            fetch,
            budget,
            deadline: Instant::now() + budget,
            cache: HashMap::new(),
            warned: HashSet::new(),
            inlined: 0,
            importing: Vec::new(),
        }
    }

    /// The body of `url`, or None after a `fetch-failed` warning naming it.
    fn get(&mut self, url: &str, warnings: &mut Vec<ExportWarning>) -> Option<Vec<u8>> {
        if !self.cache.contains_key(url) {
            let left = self.deadline.saturating_duration_since(Instant::now());
            let result = if left.is_zero() {
                Err(format!(
                    "skipped: the export's {}s download time ran out",
                    self.budget.as_secs()
                ))
            } else {
                (self.fetch)(url, left.min(cdn::TIMEOUT)).and_then(|bytes| {
                    if bytes.len() > MAX_ASSET_BYTES {
                        Err(format!("over {} KB", MAX_ASSET_BYTES / 1024))
                    } else {
                        Ok(bytes)
                    }
                })
            };
            self.cache.insert(url.to_string(), result);
        }
        let result = match self.cache.get(url) {
            Some(Ok(bytes)) if self.inlined + bytes.len() > MAX_INLINED_BYTES => Err(format!(
                "skipped: the export already inlined {} MB",
                MAX_INLINED_BYTES / (1024 * 1024)
            )),
            Some(Ok(bytes)) => Ok(bytes.clone()),
            Some(Err(reason)) => Err(reason.clone()),
            None => return None,
        };
        match result {
            Ok(bytes) => {
                self.inlined += bytes.len();
                Some(bytes)
            }
            Err(reason) => {
                if self.warned.insert(url.to_string()) {
                    warnings.push(ExportWarning {
                        kind: ExportWarningKind::FetchFailed,
                        target: url.to_string(),
                        reason,
                    });
                }
                None
            }
        }
    }

    /// `css` with each CDN `@import` replaced by the stylesheet it names and
    /// each CDN `url()` by a `data:` URI. A reference relative to `base` that
    /// stays a link is made absolute, since the page no longer sits beside it.
    fn css(
        &mut self,
        css: &str,
        base: Option<&str>,
        depth: usize,
        warnings: &mut Vec<ExportWarning>,
    ) -> String {
        let lower = css.to_ascii_lowercase();
        let mut out = String::with_capacity(css.len());
        let mut pos = 0usize;
        loop {
            let import = lower[pos..].find("@import").map(|i| pos + i);
            let url = lower[pos..].find("url(").map(|i| pos + i);
            let comment = lower[pos..].find("/*").map(|i| pos + i);
            let Some(at) = import.into_iter().chain(url).chain(comment).min() else {
                break;
            };
            out.push_str(&css[pos..at]);
            if Some(at) == comment {
                let end = css[at + 2..]
                    .find("*/")
                    .map(|i| at + 2 + i + 2)
                    .unwrap_or(css.len());
                out.push_str(&css[at..end]);
                pos = end;
            } else if Some(at) == import {
                let Some((reference, conditions, end)) = parse_import(css, at) else {
                    out.push_str("@import");
                    pos = at + "@import".len();
                    continue;
                };
                pos = end;
                let absolute = cdn::join(base, &reference);
                let inlined = match &absolute {
                    Some(u)
                        if cdn::allowed(u)
                            && depth < MAX_IMPORT_DEPTH
                            && !layered(&conditions)
                            && !self.importing.contains(u) =>
                    {
                        self.get(u, warnings).map(|bytes| {
                            self.importing.push(u.clone());
                            let text = self.css(
                                &String::from_utf8_lossy(&bytes),
                                Some(u),
                                depth + 1,
                                warnings,
                            );
                            self.importing.pop();
                            text
                        })
                    }
                    _ => None,
                };
                match (inlined, absolute) {
                    (Some(text), _) if conditions.is_empty() => out.push_str(&text),
                    (Some(text), _) => out.push_str(&format!("@media {conditions}{{{text}}}")),
                    (None, Some(u)) if u != reference => {
                        let sep = if conditions.is_empty() { "" } else { " " };
                        out.push_str(&format!(
                            "@import url(\"{}\"){sep}{conditions};",
                            u.replace('"', "%22")
                        ));
                    }
                    (None, _) => out.push_str(&css[at..end]),
                }
            } else {
                let after = &css[at + 4..];
                let Some(close) = after.find(')') else {
                    pos = at;
                    break;
                };
                let end = at + 4 + close + 1;
                pos = end;
                let reference = after[..close].trim().trim_matches(['"', '\'']);
                if reference.is_empty()
                    || reference.len() >= 5 && reference[..5].eq_ignore_ascii_case("data:")
                    || reference.starts_with('#')
                {
                    out.push_str(&css[at..end]);
                    continue;
                }
                let Some(absolute) = cdn::join(base, reference) else {
                    out.push_str(&css[at..end]);
                    continue;
                };
                let (target, fragment) = match absolute.find('#') {
                    Some(i) => absolute.split_at(i),
                    None => (absolute.as_str(), ""),
                };
                if cdn::allowed(target) {
                    if let Some(bytes) = self.get(target, warnings) {
                        let path = target.split('?').next().unwrap_or(target);
                        let mime = mime_guess::from_path(path).first_or_octet_stream();
                        out.push_str(&format!(
                            "url(\"data:{};base64,{}{fragment}\")",
                            mime.essence_str(),
                            base64(&bytes)
                        ));
                        continue;
                    }
                }
                if absolute == reference {
                    out.push_str(&css[at..end]);
                } else {
                    out.push_str(&format!("url(\"{}\")", absolute.replace('"', "%22")));
                }
            }
        }
        out.push_str(&css[pos..]);
        out
    }
}

/// The `@import` at `at` as (reference, the media or other conditions after
/// it, the index just past its `;`), or None when it doesn't parse.
fn parse_import(css: &str, at: usize) -> Option<(String, String, usize)> {
    let start = at + "@import".len();
    let semi = css[start..].find(';')?;
    let statement = css[start..start + semi].trim();
    let bytes = statement.as_bytes();
    let (reference, rest) = if bytes.len() >= 4 && bytes[..4].eq_ignore_ascii_case(b"url(") {
        let close = statement.find(')')?;
        (
            statement[4..close].trim().trim_matches(['"', '\'']),
            &statement[close + 1..],
        )
    } else if let Some(q @ ('"' | '\'')) = statement.chars().next() {
        let close = statement[1..].find(q)? + 1;
        (&statement[1..close], &statement[close + 1..])
    } else {
        return None;
    };
    Some((
        reference.to_string(),
        rest.trim().to_string(),
        start + semi + 1,
    ))
}

/// An `@import` into a cascade layer or behind `supports()` can't be
/// rewritten as an `@media` block, so it stays an import.
fn layered(conditions: &str) -> bool {
    let c = conditions.to_ascii_lowercase();
    c.starts_with("layer") || c.starts_with("supports(")
}

/// The absolute URL a `<link rel="stylesheet">` loads. An alternate
/// stylesheet is off until chosen, so it is never inlined as a live one.
fn stylesheet_href(tag: &str) -> Option<String> {
    let (rs, re) = find_attr_value_range(tag, "rel")?;
    let rel: Vec<String> = tag[rs..re]
        .split_whitespace()
        .map(str::to_ascii_lowercase)
        .collect();
    if !rel.iter().any(|w| w == "stylesheet") || rel.iter().any(|w| w == "alternate") {
        return None;
    }
    let (hs, he) = find_attr_value_range(tag, "href")?;
    cdn::join(None, &attr_url(&tag[hs..he]))
}

/// A URL attribute's value as the browser reads it: surrounding whitespace
/// trimmed and `&amp;` decoded (Google Fonts links join families with it).
fn attr_url(value: &str) -> String {
    value.trim().replace("&amp;", "&")
}

/// `tag` with its `name="…"` attribute, whose value spans `value`, removed.
fn without_attr(tag: &str, name: &str, (vs, ve): (usize, usize)) -> String {
    let attr_start = tag[..vs]
        .trim_end_matches(['"', '\''])
        .trim_end()
        .trim_end_matches('=')
        .trim_end()
        .len()
        - name.len();
    format!("{}{}", tag[..attr_start].trim_end(), &tag[ve + 1..])
}

/// Where the raw text of a `<script>` or `<style>` element opened before
/// `from` ends: its closing tag, or the end of the HTML.
fn raw_text_end(html: &str, from: usize, name: &str) -> usize {
    html[from..]
        .to_ascii_lowercase()
        .find(&format!("</{name}"))
        .map(|i| from + i)
        .unwrap_or(html.len())
}

/// `text` with every `</name` written `<\/name`, so it can't close the
/// element it is inlined into. In a script, a `<script` between `<!--` and
/// `-->` is also written `\x3Cscript`: it would put the parser in the
/// double-escaped state, where the closing `</script>` no longer closes the
/// element. Both forms read the same inside a JS string, regex or comment,
/// and nothing else in the text changes.
fn escape_raw(text: &str, name: &str) -> String {
    let close = format!("</{name}");
    let script = name == "script";
    let lower = text.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    // The parser's script-data-escaped state: after `<!--`, until `-->`.
    let mut escaped = false;
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    let mut i = 0;
    while i < bytes.len() {
        let rest = &bytes[i..];
        let (with, skip) = if rest.starts_with(close.as_bytes()) {
            ("<\\/", 2)
        } else if script && !escaped && rest.starts_with(b"<!--") {
            escaped = true;
            // From the dashes, so `<!-->` ends the state at once.
            i += 2;
            continue;
        } else if script && escaped && rest.starts_with(b"-->") {
            escaped = false;
            i += 3;
            continue;
        } else if script
            && escaped
            && rest.starts_with(b"<script")
            && matches!(
                rest.get(7),
                None | Some(b'\t' | b'\n' | b'\x0c' | b'\r' | b' ' | b'/' | b'>')
            )
        {
            ("\\x3C", 1)
        } else {
            i += 1;
            continue;
        };
        out.push_str(&text[copied..i]);
        out.push_str(with);
        i += skip;
        copied = i;
    }
    out.push_str(&text[copied..]);
    out
}

fn media_data_uri(
    path: &str,
    read: &impl Fn(&str) -> io::Result<Vec<u8>>,
) -> Result<String, String> {
    if path.is_empty() {
        return Err("the card has no file for this image".into());
    }
    let ext = Path::new(path)
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    if !MEDIA_EXTS.contains(&ext.as_str()) {
        return Err(format!("not an image or video file: .{ext}"));
    }
    let bytes = read(path).map_err(|e| e.to_string())?;
    let mime = mime_guess::from_path(path).first_or_octet_stream();
    Ok(format!(
        "data:{};base64,{}",
        mime.essence_str(),
        base64(&bytes)
    ))
}

fn closing_name(tag: &str) -> String {
    tag.trim_start_matches("</")
        .trim_end_matches('>')
        .trim()
        .to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(html: &str, images: &[&str], targets: &[&str]) -> Card {
        Card {
            id: "c1".into(),
            session_id: "s".into(),
            at: "2026-10-05T00:00:00Z".into(),
            updated_at: None,
            html: html.into(),
            images: images.iter().map(|s| s.to_string()).collect(),
            targets: targets.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn files(path: &str) -> io::Result<Vec<u8>> {
        match path {
            "/x/a.png" => Ok(b"abc".to_vec()),
            _ => Err(io::Error::new(io::ErrorKind::NotFound, "No such file")),
        }
    }

    fn offline(url: &str, _: Duration) -> Result<Vec<u8>, String> {
        Err(format!("offline: {url}"))
    }

    #[test]
    fn image_becomes_data_uri() {
        let c = card(
            r#"<p><img src="/api/cards/c1/images/0" alt="a"></p>"#,
            &["/x/a.png"],
            &[],
        );
        let r = export_card(&c, None, files, offline);
        assert!(r
            .html
            .contains(r#"<img src="data:image/png;base64,YWJj" alt="a">"#));
        assert!(!r.html.contains("/api/"));
        assert!(r.warnings.is_empty());
    }

    #[test]
    fn missing_image_warns_and_shows_placeholder() {
        let c = card(
            r#"<img src="/api/cards/c1/images/0">"#,
            &["/x/gone.png"],
            &[],
        );
        let r = export_card(&c, None, files, offline);
        assert_eq!(r.warnings.len(), 1);
        assert_eq!(r.warnings[0].kind, ExportWarningKind::MissingImage);
        assert_eq!(r.warnings[0].target, "/x/gone.png");
        assert!(r.html.contains("image missing: gone.png"));
        assert!(!r.html.contains("<img"));
    }

    #[test]
    fn missing_video_loses_its_src() {
        let c = card(
            r#"<video controls src = "/api/cards/c1/images/0" muted></video>"#,
            &["/x/gone.webm"],
            &[],
        );
        let r = export_card(&c, None, files, offline);
        assert_eq!(r.warnings.len(), 1);
        assert!(
            r.html.contains("<video controls muted></video>"),
            "{}",
            r.html
        );
    }

    #[test]
    fn title_decodes_entities_once() {
        let r = export_card(
            &card("<h1>a &amp;lt; b</h1>", &[], &[]),
            None,
            files,
            offline,
        );
        assert!(r.html.contains("<title>a &amp;lt; b</title>"), "{}", r.html);
    }

    #[test]
    fn links_become_real_or_plain() {
        let c = card(
            "<a href=\"#canvas-open-0\">site</a> and <a href='#canvas-open-1'><code>/x/f.rs</code></a>",
            &[],
            &["https://example.com/?a=1&b=2", "/x/f.rs"],
        );
        let r = export_card(&c, None, files, offline);
        assert!(r.html.contains(
            r#"<a href="https://example.com/?a=1&amp;b=2" target="_blank" rel="noopener">site</a>"#
        ));
        assert!(r.html.contains(" and <code>/x/f.rs</code><"));
        assert!(!r.html.contains("canvas-open"));
    }

    #[test]
    fn data_value_is_delivered_and_escaped() {
        let c = card("<h2>Load <em>now</em></h2>", &[], &[]);
        let v = serde_json::json!({"n": 3, "s": "</script>"});
        let r = export_card(&c, Some(&v), files, offline);
        assert!(r.html.contains(r#"var v={"n":3,"s":"\u003c/script>"}"#));
        assert!(!r.html.contains("\"</script>"));
        assert!(r.html.contains("type:'canvas-data',value:v"));
        assert!(r.html.contains("<title>Load now</title>"));
    }

    fn cdn_files(url: &str, _: Duration) -> Result<Vec<u8>, String> {
        match url {
            "https://unpkg.com/lib.js" => Ok(b"var s='</script>',t='<!--<SCRIPT>';".to_vec()),
            "https://cdnjs.cloudflare.com/x/css/a.css" => {
                Ok(b"@import url('b.css') print;.a{src:url(../font/f.woff2?v=1)}".to_vec())
            }
            "https://cdnjs.cloudflare.com/x/css/b.css" => Ok(b".b{}".to_vec()),
            "https://cdnjs.cloudflare.com/x/font/f.woff2?v=1" => Ok(b"ab".to_vec()),
            "https://unpkg.com/big.js" => Ok(vec![b'x'; MAX_ASSET_BYTES + 1]),
            _ => Err("HTTP 500".into()),
        }
    }

    #[test]
    fn cdn_script_is_inlined_and_escaped() {
        let c = card(
            r#"<script src="https://unpkg.com/lib.js" defer></script><p>x</p>"#,
            &[],
            &[],
        );
        let r = export_card(&c, None, files, cdn_files);
        assert!(
            r.html.contains(
                r#"<script defer>var s='<\/script>',t='<!--\x3CSCRIPT>';</script><p>x</p>"#
            ),
            "{}",
            r.html
        );
        assert!(r.warnings.is_empty());
    }

    #[test]
    fn only_a_script_start_inside_an_html_comment_is_escaped() {
        for (text, want) in [
            (
                "<!-- a\nx='<script src=y>'",
                "<!-- a\nx='\\x3Cscript src=y>'",
            ),
            ("'<!--</script>'", "'<!--<\\/script>'"),
            ("'<!--<scripts>'", "'<!--<scripts>'"),
            ("'<!-- --><script>'", "'<!-- --><script>'"),
            ("'<!--><script>'", "'<!--><script>'"),
            ("<!--<script", "<!--\\x3Cscript"),
        ] {
            assert_eq!(escape_raw(text, "script"), want);
        }
        assert_eq!(escape_raw("/*<!--<script>*/", "style"), "/*<!--<script>*/");
    }

    #[test]
    fn cdn_stylesheet_imports_and_fonts_are_inlined() {
        let c = card(
            r#"<link rel="stylesheet" href="//cdnjs.cloudflare.com/x/css/a.css" media="screen">"#,
            &[],
            &[],
        );
        let r = export_card(&c, None, files, cdn_files);
        assert!(
            r.html.contains(
                r#"<style media="screen">@media print{.b{}}.a{src:url("data:font/woff2;base64,YWI=")}</style>"#
            ),
            "{}",
            r.html
        );
        assert!(r.warnings.is_empty());
    }

    #[test]
    fn style_block_imports_from_a_cdn() {
        let c = card(
            "<style>@import \"https://cdnjs.cloudflare.com/x/css/b.css\";.c{background:url(https://example.com/i.png)}</style>",
            &[],
            &[],
        );
        let r = export_card(&c, None, files, cdn_files);
        assert!(
            r.html
                .contains("<style>.b{}.c{background:url(https://example.com/i.png)}</style>"),
            "{}",
            r.html
        );
    }

    #[test]
    fn failed_and_oversize_downloads_warn_once_and_stay_links() {
        let c = card(
            r#"<script src="https://unpkg.com/big.js"></script><link rel="stylesheet" href="https://unpkg.com/gone.css"><link rel="stylesheet" href="https://unpkg.com/gone.css"><script src="https://example.com/other.js"></script>"#,
            &[],
            &[],
        );
        let r = export_card(&c, None, files, cdn_files);
        assert!(r
            .html
            .contains(r#"<script src="https://unpkg.com/big.js"></script>"#));
        assert_eq!(r.html.matches("https://unpkg.com/gone.css").count(), 2);
        assert!(r.html.contains("https://example.com/other.js"));
        let got: Vec<_> = r
            .warnings
            .iter()
            .map(|w| (w.kind, w.target.as_str(), w.reason.as_str()))
            .collect();
        assert_eq!(
            got,
            vec![
                (
                    ExportWarningKind::FetchFailed,
                    "https://unpkg.com/big.js",
                    "over 512 KB"
                ),
                (
                    ExportWarningKind::FetchFailed,
                    "https://unpkg.com/gone.css",
                    "HTTP 500"
                ),
            ]
        );
    }

    #[test]
    fn relative_reference_that_stays_a_link_becomes_absolute() {
        let c = card(
            r#"<link rel="stylesheet" href="https://unpkg.com/p/s.css">"#,
            &[],
            &[],
        );
        let fetch = |url: &str, _: Duration| match url {
            "https://unpkg.com/p/s.css" => Ok(b".a{background:url(i.png)}".to_vec()),
            _ => Err("HTTP 404".to_string()),
        };
        let r = export_card(&c, None, files, fetch);
        assert!(r
            .html
            .contains(r#"<style>.a{background:url("https://unpkg.com/p/i.png")}</style>"#));
        assert_eq!(r.warnings.len(), 1);
    }

    #[test]
    fn an_import_cycle_stays_an_import() {
        let c = card(
            r#"<link rel="stylesheet" href="https://unpkg.com/a.css">"#,
            &[],
            &[],
        );
        let fetch = |url: &str, _: Duration| match url {
            "https://unpkg.com/a.css" => {
                Ok(b"@import \"a.css\";/* @import \"x.css\"; */.a{}".to_vec())
            }
            _ => Err("HTTP 404".to_string()),
        };
        let r = export_card(&c, None, files, fetch);
        assert!(
            r.html.contains(r#"<style>@import url("https://unpkg.com/a.css");/* @import "x.css"; */.a{}</style>"#),
            "{}",
            r.html
        );
        assert!(r.warnings.is_empty());
    }

    #[test]
    fn repeated_imports_stop_at_the_inlining_total() {
        let c = card(
            r#"<link rel="stylesheet" href="https://unpkg.com/a.css">"#,
            &[],
            &[],
        );
        let a = "@import \"b.css\";".repeat(40);
        let b = vec![b'x'; MAX_ASSET_BYTES];
        let fetch = move |url: &str, _: Duration| match url {
            "https://unpkg.com/a.css" => Ok(a.clone().into_bytes()),
            "https://unpkg.com/b.css" => Ok(b.clone()),
            _ => Err("HTTP 404".to_string()),
        };
        let r = export_card(&c, None, files, fetch);
        assert!(
            r.html.len() < MAX_INLINED_BYTES + 64 * 1024,
            "{}",
            r.html.len()
        );
        assert_eq!(r.warnings.len(), 1);
        assert_eq!(r.warnings[0].target, "https://unpkg.com/b.css");
        assert!(r.warnings[0].reason.contains("already inlined 8 MB"));
    }

    #[test]
    fn downloads_past_the_time_budget_are_skipped() {
        let mut cdn = Cdn::new(|_: &str, _: Duration| Ok(b"x".to_vec()), Duration::ZERO);
        let mut warnings = Vec::new();
        assert_eq!(cdn.get("https://unpkg.com/a.js", &mut warnings), None);
        assert_eq!(
            warnings[0].reason,
            "skipped: the export's 0s download time ran out"
        );
    }

    #[test]
    fn link_href_is_read_as_the_browser_reads_it() {
        let c = card(
            r#"<link rel="stylesheet" href=" https://fonts.googleapis.com/css2?family=A&amp;family=B "><link rel="alternate stylesheet" href="https://unpkg.com/dark.css">"#,
            &[],
            &[],
        );
        let fetch = |url: &str, _: Duration| match url {
            "https://fonts.googleapis.com/css2?family=A&family=B" => Ok(b".f{}".to_vec()),
            _ => Err("HTTP 404".to_string()),
        };
        let r = export_card(&c, None, files, fetch);
        assert!(r.html.contains("<style>.f{}</style>"), "{}", r.html);
        assert!(r.html.contains(r#"href="https://unpkg.com/dark.css""#));
        assert!(r.warnings.is_empty());
    }

    #[test]
    fn no_data_no_shim() {
        let r = export_card(&card("<p>hi</p>", &[], &[]), None, files, offline);
        assert!(!r.html.contains("canvas-data"));
        assert!(r.html.contains("<title>Canvas post</title>"));
    }
}
