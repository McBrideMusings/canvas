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
    card_label, decode_entities, escape_attr, escape_text, find_attr_value, replace_attr_value,
    tag_name, tags, AttrValue, Tag,
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
    // A file one reference left as a link while another put it in the page
    // isn't missing from the page.
    warnings
        .retain(|w| w.kind != ExportWarningKind::FetchFailed || !cdn.present.contains(&w.target));
    let title = escape_text(&card_label(&card.html).unwrap_or_else(|| "Canvas post".into()));
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
    // Inside an SVG or MathML `<style>`, whose text holds markup: its text
    // and CDATA sections are its CSS, up to the first other tag.
    let mut markup_style = false;

    for Tag {
        start,
        end,
        text_end,
    } in tags(html)
    {
        let text = &html[pos..start];
        if markup_style {
            out.push_str(&cdn.css(text, None, 0, true, warnings));
        } else {
            out.push_str(text);
        }
        let tag = &html[start..end];
        pos = end;

        if markup_style {
            if let Some(body) = tag.strip_prefix("<![CDATA[") {
                let (body, close) = match body.strip_suffix("]]>") {
                    Some(body) => (body, "]]>"),
                    None => (body, ""),
                };
                let css = cdn.css(body, None, 0, false, warnings);
                out.push_str("<![CDATA[");
                // A `]]>` would end the section; split it across two.
                out.push_str(&css.replace("]]>", "]]]]><![CDATA[>"));
                out.push_str(close);
                continue;
            }
            if tag.starts_with("<!--") {
                out.push_str(tag);
                continue;
            }
            markup_style = false;
        }

        if tag.starts_with("</") {
            if closing_name(tag) == "a" && anchors.pop() == Some(true) {
                continue;
            }
            out.push_str(tag);
            continue;
        }

        match tag_name(tag).as_deref() {
            Some(kind @ ("img" | "video" | "source")) => {
                let Some(src) = find_attr_value(tag, "src") else {
                    out.push_str(tag);
                    continue;
                };
                let Some(index) = tag[src.range()]
                    .strip_prefix(&image_prefix)
                    .and_then(|n| n.parse::<usize>().ok())
                else {
                    out.push_str(tag);
                    continue;
                };
                let path = card.images.get(index).map(String::as_str).unwrap_or("");
                match media_data_uri(path, read) {
                    Ok(uri) => out.push_str(&replace_attr_value(tag, src, &uri)),
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
                            out.push_str(&without_attr(tag, "src", src));
                        }
                    }
                }
            }
            Some("a") => {
                let link = find_attr_value(tag, "href").and_then(|href| {
                    let index = tag[href.range()]
                        .strip_prefix("#canvas-open-")?
                        .parse::<usize>()
                        .ok()?;
                    Some((href, card.targets.get(index).map(String::as_str)))
                });
                match link {
                    None => {
                        anchors.push(false);
                        out.push_str(tag);
                    }
                    Some((href, Some(url)))
                        if url.starts_with("http://") || url.starts_with("https://") =>
                    {
                        anchors.push(false);
                        let close = if tag.ends_with("/>") { "/>" } else { ">" };
                        let tag = replace_attr_value(tag, href, &escape_attr(url));
                        out.push_str(tag.strip_suffix(close).unwrap_or(&tag));
                        out.push_str(" target=\"_blank\" rel=\"noopener\"");
                        out.push_str(close);
                    }
                    // A local path, or an index the card has no target for:
                    // the link text stays, the anchor goes.
                    Some(_) => anchors.push(true),
                }
            }
            Some("script") => {
                let src = find_attr_value(tag, "src")
                    .and_then(|src| Some((src, cdn::join(None, &attr_url(&tag[src.range()]))?)))
                    .filter(|(_, url)| cdn::allowed(url));
                let Some((src, url)) = src else {
                    out.push_str(tag);
                    continue;
                };
                match cdn.get(&url, warnings) {
                    Some(bytes) => {
                        cdn.present.push(url.clone());
                        out.push_str(&without_attr(tag, "src", src));
                        out.push_str(&escape_raw(&String::from_utf8_lossy(&bytes), "script"));
                        // Whatever the element held is dropped with its src.
                        pos = text_end;
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
                        cdn.present.push(url.clone());
                        cdn.importing.push(url.clone());
                        let css = cdn.css(
                            &String::from_utf8_lossy(&bytes),
                            Some(&url),
                            0,
                            false,
                            warnings,
                        );
                        cdn.importing.pop();
                        match find_attr_value(tag, "media") {
                            Some(media) => out.push_str(&format!(
                                "<style media=\"{}\">",
                                tag[media.range()].replace('"', "&quot;")
                            )),
                            None => out.push_str("<style>"),
                        }
                        out.push_str(&escape_raw(&css, "style"));
                        out.push_str("</style>");
                    }
                    None => out.push_str(tag),
                }
            }
            // The scan reads an HTML `<style>`'s text as raw text and resumes
            // at its closing tag; an SVG or MathML one's it reads on.
            Some("style") if text_end == end && !tag.ends_with("/>") => {
                out.push_str(tag);
                markup_style = true;
            }
            Some("style") => {
                out.push_str(tag);
                let css = cdn.css(&html[end..text_end], None, 0, false, warnings);
                out.push_str(&escape_raw(&css, "style"));
                pos = text_end;
            }
            _ => out.push_str(tag),
        }
    }
    let text = &html[pos..];
    if markup_style {
        out.push_str(&cdn.css(text, None, 0, true, warnings));
    } else {
        out.push_str(text);
    }
    out
}

/// Downloads for one export: each URL fetched once, all of them within one
/// time budget, and at most `MAX_INLINED_BYTES` inlined in total. A URL that
/// isn't inlined warns once; `export_card` drops the warning when another
/// reference put the URL in the page (`present`).
struct Cdn<F> {
    fetch: F,
    budget: Duration,
    deadline: Instant,
    cache: HashMap<String, Result<Vec<u8>, String>>,
    warned: HashSet<String>,
    /// Every URL whose content went into the page, in order, so a
    /// stylesheet's expansion that is thrown away can drop its own.
    present: Vec<String>,
    inlined: usize,
    /// The stylesheets being expanded, outermost first, so an `@import`
    /// that closes a cycle is dropped.
    importing: Vec<String>,
    /// Every `@import` written out as a link, in order, so a stylesheet
    /// about to be wrapped in a block can tell whether it holds one.
    linked: Vec<String>,
}

impl<F: Fn(&str, Duration) -> Result<Vec<u8>, String>> Cdn<F> {
    fn new(fetch: F, budget: Duration) -> Self {
        Cdn {
            fetch,
            budget,
            deadline: Instant::now() + budget,
            cache: HashMap::new(),
            warned: HashSet::new(),
            present: Vec::new(),
            inlined: 0,
            importing: Vec::new(),
            linked: Vec::new(),
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
                self.warn(url, reason, warnings);
                None
            }
        }
    }

    /// A `fetch-failed` warning that `url` stays a link, once per URL;
    /// `export_card` drops it if `url` ends up in `present`.
    fn warn(&mut self, url: &str, reason: String, warnings: &mut Vec<ExportWarning>) {
        if self.warned.insert(url.to_string()) {
            warnings.push(ExportWarning {
                kind: ExportWarningKind::FetchFailed,
                target: url.to_string(),
                reason,
            });
        }
    }

    /// `css` with each CDN `@import` replaced by the stylesheet it names and
    /// each CDN `url()` by a `data:` URI. A reference relative to `base` that
    /// stays a link is made absolute, since the page no longer sits beside it.
    /// The text is read as the browser tokenizes it: strings, comments and
    /// escapes are copied untouched, and only a whole `url(` or `@import`
    /// token is a reference. With `in_markup`, `css` is text in SVG or MathML,
    /// where the browser decodes entities: each reference is decoded before
    /// it is read, and each replacement is written with entities.
    fn css(
        &mut self,
        css: &str,
        base: Option<&str>,
        depth: usize,
        in_markup: bool,
        warnings: &mut Vec<ExportWarning>,
    ) -> String {
        let markup = |text: String| if in_markup { escape_text(&text) } else { text };
        let b = css.as_bytes();
        let mut out = String::with_capacity(css.len());
        let mut pos = 0usize;
        let mut i = 0usize;
        while i < b.len() {
            match b[i] {
                b'/' if b.get(i + 1) == Some(&b'*') => i = comment_end(b, i),
                b'"' | b'\'' => i = string_end(b, i).0,
                b'\\' => i += escape_len(b, i),
                b'@' if starts_with_word(b, i, b"@import") => {
                    let Some(mut import) = parse_import(css, i) else {
                        i += "@import".len();
                        continue;
                    };
                    if in_markup {
                        import.reference = decode_entities(&import.reference);
                    }
                    out.push_str(&css[pos..i]);
                    match self.import(&import, base, depth, warnings) {
                        Some(text) => out.push_str(&markup(text)),
                        None => out.push_str(&css[i..import.end]),
                    }
                    pos = import.end;
                    i = import.end;
                }
                b'u' | b'U'
                    if (i == 0 || !ident_byte(b[i - 1])) && starts_with_ci(b, i, b"url(") =>
                {
                    let (reference, end) = url_token(css, i);
                    out.push_str(&css[pos..i]);
                    let reference = reference.map(|r| {
                        if in_markup {
                            decode_entities(r)
                        } else {
                            r.to_string()
                        }
                    });
                    match reference.and_then(|r| self.url(&r, base, warnings)) {
                        Some(text) => out.push_str(&markup(text)),
                        None => out.push_str(&css[i..end]),
                    }
                    pos = end;
                    i = end;
                }
                _ => i += 1,
            }
        }
        out.push_str(&css[pos..]);
        out
    }

    /// What replaces one `@import`: the stylesheet it names under its
    /// conditions, or the import itself, made absolute. None keeps it as
    /// written. An import of a CDN stylesheet that stays a link warns. One
    /// that closes a cycle is dropped, as the browser ignores it. A
    /// stylesheet that holds an import staying a link is not inlined under
    /// conditions, since the browser ignores an `@import` inside a block.
    fn import(
        &mut self,
        import: &Import,
        base: Option<&str>,
        depth: usize,
        warnings: &mut Vec<ExportWarning>,
    ) -> Option<String> {
        let absolute = cdn::join(base, &import.reference);
        let inlined = match &absolute {
            Some(u) if self.importing.contains(u) => return Some(String::new()),
            Some(u) if cdn::allowed(u) => {
                if depth >= MAX_IMPORT_DEPTH {
                    let reason =
                        format!("skipped: nested more than {MAX_IMPORT_DEPTH} @imports deep");
                    self.warn(u, reason, warnings);
                    None
                } else {
                    let (bytes_before, links_before, present_before) =
                        (self.inlined, self.linked.len(), self.present.len());
                    self.get(u, warnings).and_then(|bytes| {
                        self.importing.push(u.clone());
                        let text = self.css(
                            &String::from_utf8_lossy(&bytes),
                            Some(u),
                            depth + 1,
                            false,
                            warnings,
                        );
                        self.importing.pop();
                        match (import.block(), self.linked.get(links_before)) {
                            (Some(block), Some(nested)) => {
                                let reason = format!(
                                    "kept as a link: its @import of {nested} would be ignored inside {block}"
                                );
                                self.inlined = bytes_before;
                                self.present.truncate(present_before);
                                self.warn(u, reason, warnings);
                                None
                            }
                            _ => {
                                self.present.push(u.clone());
                                Some(text)
                            }
                        }
                    })
                }
            }
            _ => None,
        };
        if let Some(text) = inlined {
            return Some(import.wrap(text));
        }
        self.linked
            .push(absolute.clone().unwrap_or_else(|| import.reference.clone()));
        match absolute {
            Some(u) if u != import.reference => {
                let sep = if import.conditions.is_empty() {
                    ""
                } else {
                    " "
                };
                Some(format!(
                    "@import url(\"{}\"){sep}{};",
                    u.replace('"', "%22"),
                    import.conditions
                ))
            }
            _ => None,
        }
    }

    /// What replaces one `url()` naming `reference`: a CDN file as a `data:`
    /// URI, or a relative reference made absolute. None keeps it as written.
    fn url(
        &mut self,
        reference: &str,
        base: Option<&str>,
        warnings: &mut Vec<ExportWarning>,
    ) -> Option<String> {
        if reference.is_empty()
            || reference
                .get(..5)
                .is_some_and(|p| p.eq_ignore_ascii_case("data:"))
            || reference.starts_with('#')
        {
            return None;
        }
        let absolute = cdn::join(base, reference)?;
        let (target, fragment) = match absolute.find('#') {
            Some(i) => absolute.split_at(i),
            None => (absolute.as_str(), ""),
        };
        if cdn::allowed(target) {
            if let Some(bytes) = self.get(target, warnings) {
                self.present.push(target.to_string());
                let path = target.split('?').next().unwrap_or(target);
                let mime = mime_guess::from_path(path).first_or_octet_stream();
                return Some(format!(
                    "url(\"data:{};base64,{}{fragment}\")",
                    mime.essence_str(),
                    base64(&bytes)
                ));
            }
        }
        (absolute != reference).then(|| format!("url(\"{}\")", absolute.replace('"', "%22")))
    }
}

/// One `@import` statement: the reference it names, the conditions after it
/// as written and split into its `layer`, `supports()` and media list, and
/// the index just past its `;`.
struct Import {
    reference: String,
    conditions: String,
    /// `Some("")` for an anonymous layer.
    layer: Option<String>,
    supports: Option<String>,
    media: String,
    end: usize,
}

impl Import {
    /// The outermost block `wrap` puts the stylesheet in, if any.
    fn block(&self) -> Option<&'static str> {
        if self.layer.is_some() {
            Some("@layer")
        } else if self.supports.is_some() {
            Some("@supports")
        } else if !self.media.is_empty() {
            Some("@media")
        } else {
            None
        }
    }

    /// `text` under this import's conditions, as the blocks that apply them
    /// in place: `@layer` outermost, then `@supports`, then `@media`.
    fn wrap(&self, mut text: String) -> String {
        if !self.media.is_empty() {
            text = format!("@media {}{{{text}}}", self.media);
        }
        if let Some(condition) = &self.supports {
            text = format!("@supports ({condition}){{{text}}}");
        }
        match self.layer.as_deref() {
            Some("") => format!("@layer{{{text}}}"),
            Some(name) => format!("@layer {name}{{{text}}}"),
            None => text,
        }
    }
}

/// The `@import` at `at`, or None when it doesn't parse: no string or
/// `url()` after it, a block before its `;`, or an unclosed condition.
fn parse_import(css: &str, at: usize) -> Option<Import> {
    let b = css.as_bytes();
    let start = skip_space(b, at + "@import".len());
    let (reference, after) = match b.get(start)? {
        b'"' | b'\'' => match string_end(b, start) {
            (end, true) => (&css[start + 1..end - 1], end),
            (_, false) => return None,
        },
        _ if starts_with_ci(b, start, b"url(") => match url_token(css, start) {
            (Some(reference), end) => (reference, end),
            (None, _) => return None,
        },
        _ => return None,
    };
    let mut i = after;
    while let Some(&c) = b.get(i) {
        match c {
            b';' => break,
            b'{' | b'}' => return None,
            b'"' | b'\'' => i = string_end(b, i).0,
            b'/' if b.get(i + 1) == Some(&b'*') => i = comment_end(b, i),
            b'(' => i = paren_end(b, i, true)?,
            b'\\' => i += escape_len(b, i),
            _ => i += 1,
        }
    }
    let semi = i.min(b.len());
    let conditions = css[after..semi].trim();
    let mut rest = conditions;
    let mut layer = None;
    if starts_with_ci(rest.as_bytes(), 0, b"layer(") {
        let close = paren_end(rest.as_bytes(), 5, true)?;
        layer = Some(rest[6..close - 1].trim().to_string());
        rest = rest[close..].trim_start();
    } else if starts_with_word(rest.as_bytes(), 0, b"layer") {
        layer = Some(String::new());
        rest = rest[5..].trim_start();
    }
    let mut supports = None;
    if starts_with_ci(rest.as_bytes(), 0, b"supports(") {
        let close = paren_end(rest.as_bytes(), 8, true)?;
        supports = Some(rest[9..close - 1].trim().to_string());
        rest = rest[close..].trim_start();
    }
    Some(Import {
        reference: reference.to_string(),
        conditions: conditions.to_string(),
        layer,
        supports,
        media: rest.to_string(),
        end: (semi + 1).min(b.len()),
    })
}

/// The `url(` token at `at` as the reference it names and the index just
/// past its `)`. The reference is None where the browser reads none: a
/// string followed by more than space, or an unquoted URL holding a quote,
/// `(`, space or escape, which is a bad URL. The token still runs to its own
/// matching `)`, so nothing inside it is read as a reference; an unquoted one
/// whose parentheses never balance ends at its first `)`, as the browser
/// ends a bad URL.
fn url_token(css: &str, at: usize) -> (Option<&str>, usize) {
    let b = css.as_bytes();
    let open = at + "url".len();
    let start = skip_space(b, open + 1);
    let quoted = matches!(b.get(start), Some(b'"' | b'\''));
    let end =
        paren_end(b, open, quoted).or_else(|| (!quoted).then(|| first_close(b, start)).flatten());
    let Some(end) = end else {
        return (None, b.len());
    };
    let inner = &css[start..end - 1];
    let reference = if quoted {
        match string_end(b, start) {
            (close, true) if skip_space(b, close) == end - 1 => {
                Some(&inner[1..close - start - 1]).filter(|url| !url.contains('\\'))
            }
            _ => None,
        }
    } else {
        let url = inner.trim_end_matches(is_space);
        (!url.contains(['"', '\'', '(', '\\']) && !url.contains(is_space)).then_some(url)
    };
    (reference, end)
}

/// The index just past the `)` matching the `(` at `open`, skipping
/// escapes, and strings and comments too when `strings` is set. None when
/// the text ends first.
fn paren_end(b: &[u8], open: usize, strings: bool) -> Option<usize> {
    let mut depth = 0usize;
    let mut i = open;
    while let Some(&c) = b.get(i) {
        match c {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i + 1);
                }
            }
            b'\\' => i += escape_len(b, i) - 1,
            b'"' | b'\'' if strings => {
                i = string_end(b, i).0;
                continue;
            }
            b'/' if strings && b.get(i + 1) == Some(&b'*') => {
                i = comment_end(b, i);
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// The index just past the first unescaped `)` from `from`.
fn first_close(b: &[u8], from: usize) -> Option<usize> {
    let mut i = from;
    while let Some(&c) = b.get(i) {
        match c {
            b')' => return Some(i + 1),
            b'\\' => i += escape_len(b, i),
            _ => i += 1,
        }
    }
    None
}

/// How many bytes the escape at `at` spans: the backslash and the character
/// after it, or both bytes of an escaped CRLF.
fn escape_len(b: &[u8], at: usize) -> usize {
    if b.get(at + 1..at + 3) == Some(b"\r\n") {
        3
    } else {
        2
    }
}

/// The index just past the string opening at `at`, and whether its closing
/// quote was found. An unescaped newline ends it unclosed, as in the browser.
fn string_end(b: &[u8], at: usize) -> (usize, bool) {
    let quote = b[at];
    let mut i = at + 1;
    while let Some(&c) = b.get(i) {
        match c {
            b'\\' => i += escape_len(b, i),
            b'\n' | b'\r' | b'\x0c' => return (i, false),
            _ if c == quote => return (i + 1, true),
            _ => i += 1,
        }
    }
    (b.len(), false)
}

/// The index just past the comment opening at `at`, or the end of the text.
fn comment_end(b: &[u8], at: usize) -> usize {
    b[at + 2..]
        .windows(2)
        .position(|w| w == b"*/")
        .map_or(b.len(), |i| at + 2 + i + 2)
}

fn skip_space(b: &[u8], mut i: usize) -> usize {
    while b.get(i).is_some_and(|&c| is_space(c as char)) {
        i += 1;
    }
    i
}

fn is_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0c')
}

fn ident_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'-' || c == b'_' || c >= 0x80
}

fn starts_with_ci(b: &[u8], i: usize, word: &[u8]) -> bool {
    b.get(i..i + word.len())
        .is_some_and(|s| s.eq_ignore_ascii_case(word))
}

/// `word` at `i`, not followed by more of an identifier.
fn starts_with_word(b: &[u8], i: usize, word: &[u8]) -> bool {
    starts_with_ci(b, i, word) && !b.get(i + word.len()).is_some_and(|&c| ident_byte(c))
}

/// The absolute URL a `<link rel="stylesheet">` loads. An alternate
/// stylesheet is off until chosen, so it is never inlined as a live one.
fn stylesheet_href(tag: &str) -> Option<String> {
    let rel: Vec<String> = tag[find_attr_value(tag, "rel")?.range()]
        .split_whitespace()
        .map(str::to_ascii_lowercase)
        .collect();
    if !rel.iter().any(|w| w == "stylesheet") || rel.iter().any(|w| w == "alternate") {
        return None;
    }
    let href = find_attr_value(tag, "href")?;
    cdn::join(None, &attr_url(&tag[href.range()]))
}

/// A URL attribute's value as the browser reads it: surrounding whitespace
/// trimmed and `&amp;` decoded (Google Fonts links join families with it).
fn attr_url(value: &str) -> String {
    value.trim().replace("&amp;", "&")
}

/// `tag` with its `name="…"` (or unquoted `name=…`) attribute, whose value
/// is `value`, removed.
fn without_attr(tag: &str, name: &str, value: AttrValue) -> String {
    let attr_start = tag[..value.outer().start]
        .trim_end()
        .trim_end_matches('=')
        .trim_end()
        .len()
        - name.len();
    format!(
        "{}{}",
        tag[..attr_start].trim_end(),
        &tag[value.outer().end..]
    )
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
    fn script_text_that_looks_like_a_tag_does_not_stop_the_rewrite() {
        let c = card(
            r##"<script>if(i<n)s='it\'s';</script><img src="/api/cards/c1/images/0"><a href="#canvas-open-0">x</a>"##,
            &["/x/a.png"],
            &["https://example.com/"],
        );
        let r = export_card(&c, None, files, offline);
        assert!(r.html.contains("<script>if(i<n)s='it\\'s';</script>"));
        assert!(r.html.contains(r#"<img src="data:image/png;base64,YWJj">"#));
        assert!(r
            .html
            .contains(r#"href="https://example.com/" target="_blank""#));
        assert!(!r.html.contains("/api/"));
    }

    #[test]
    fn a_self_closed_style_resumes_where_the_scan_does() {
        let html = r#"<style/><b>x</b></style><img src="/api/cards/c1/images/0">"#;
        let r = export_card(&card(html, &["/x/a.png"], &[]), None, files, offline);
        assert!(r.html.contains(r#"<style/><b>x</b></style>"#));
        assert!(r.html.contains(r#"<img src="data:image/png;base64,YWJj">"#));
    }

    #[test]
    fn a_quote_in_text_does_not_stop_the_rewrite() {
        for html in [
            r#"a < b's <img src="/api/cards/c1/images/0">"#,
            r#"a <'s<img src="/api/cards/c1/images/0">"#,
            r#"<p title=x'y>t</p><img src="/api/cards/c1/images/0">"#,
        ] {
            let r = export_card(&card(html, &["/x/a.png"], &[]), None, files, offline);
            assert!(
                r.html.contains(r#"<img src="data:image/png;base64,YWJj">"#),
                "{html}"
            );
        }
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
        assert!(r.html.contains("<title>Canvas post</title>"), "{}", r.html);
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
    fn unquoted_values_are_rewritten_in_double_quotes() {
        let c = card(
            "<img src=/api/cards/c1/images/0 alt=a><video src=/api/cards/c1/images/1 muted></video><a href=#canvas-open-0>site</a>",
            &["/x/a.png", "/x/gone.webm"],
            &["https://example.com/?a=1&b=2"],
        );
        let r = export_card(&c, None, files, offline);
        assert!(
            r.html
                .contains(r#"<img src="data:image/png;base64,YWJj" alt=a>"#),
            "{}",
            r.html
        );
        assert!(r.html.contains("<video muted></video>"), "{}", r.html);
        assert!(r.html.contains(
            r#"<a href="https://example.com/?a=1&amp;b=2" target="_blank" rel="noopener">site</a>"#
        ));
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
            "https://cdnjs.cloudflare.com/x/css/end.css" => {
                Ok(b".e::after{content:\"]]>\"}".to_vec())
            }
            "https://cdnjs.cloudflare.com/x/css/m.css?a=1&b=2" => {
                Ok(b".m>b{content:\"<b>&amp;\"}".to_vec())
            }
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
            ("'<!---><script>'", "'<!---><script>'"),
            (
                "'<!--<script/<script\t<script\n'",
                "'<!--\\x3Cscript/\\x3Cscript\t\\x3Cscript\n'",
            ),
            ("<!--\nn-->0;'<script>'", "<!--\nn-->0;'<script>'"),
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
    fn svg_style_imports_from_a_cdn_written_as_markup() {
        // Each pair: the card, and what export writes. WebKit reads each
        // written `<style>` as the CSS the card's did, stylesheets inlined.
        for (html, want) in [
            (
                r#"<svg><style>@import "https://cdnjs.cloudflare.com/x/css/m.css?a=1&amp;b=2";.c{content:"&#169;";src:url(https://cdnjs.cloudflare.com/x/font/f.woff2?v=1)}</style></svg>"#,
                r#"<svg><style>.m&gt;b{content:"&lt;b&gt;&amp;amp;"}.c{content:"&#169;";src:url("data:font/woff2;base64,YWI=")}</style></svg>"#,
            ),
            (
                r#"<svg><style><![CDATA[@import "https://cdnjs.cloudflare.com/x/css/m.css?a=1&b=2";.c{content:"&#169;"}]]></style></svg>"#,
                r#"<svg><style><![CDATA[.m>b{content:"<b>&amp;"}.c{content:"&#169;"}]]></style></svg>"#,
            ),
            (
                r#"<svg><style>/*x*/<!--c-->@import "https://cdnjs.cloudflare.com/x/css/b.css";</style></svg>"#,
                r#"<svg><style>/*x*/<!--c-->.b{}</style></svg>"#,
            ),
            (
                r#"<svg><style><![CDATA[@import "https://cdnjs.cloudflare.com/x/css/end.css";]]></style></svg>"#,
                r#"<svg><style><![CDATA[.e::after{content:"]]]]><![CDATA[>"}]]></style></svg>"#,
            ),
        ] {
            let r = export_card(&card(html, &[], &[]), None, files, cdn_files);
            assert!(r.html.contains(want), "{html}\n{}", r.html);
            assert!(r.warnings.is_empty(), "{html}: {:?}", r.warnings);
        }
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
    fn an_import_closing_a_cycle_is_dropped() {
        let c = card(
            r#"<link rel="stylesheet" href="https://unpkg.com/a.css">"#,
            &[],
            &[],
        );
        let fetch = |url: &str, _: Duration| match url {
            "https://unpkg.com/a.css" => {
                Ok(b"@import \"b.css\";@import \"a.css\";/* @import \"x.css\"; */.a{}".to_vec())
            }
            "https://unpkg.com/b.css" => Ok(b"@import url(a.css) screen;.b{}".to_vec()),
            _ => Err("HTTP 404".to_string()),
        };
        let r = export_card(&c, None, files, fetch);
        assert!(
            r.html
                .contains(r#"<style>.b{}/* @import "x.css"; */.a{}</style>"#),
            "{}",
            r.html
        );
        assert!(r.warnings.is_empty());
    }

    #[test]
    fn references_inside_css_strings_stay_text() {
        let css = r#"@import "c;d.css";.a{content:"url(i.png) @import 'x.css';\"url(j.png)"}.b{content:'a\'url(k.png)'}"#;
        let c = card(
            r#"<link rel="stylesheet" href="https://unpkg.com/p/s.css">"#,
            &[],
            &[],
        );
        let fetch = move |url: &str, _: Duration| match url {
            "https://unpkg.com/p/s.css" => Ok(css.as_bytes().to_vec()),
            "https://unpkg.com/p/c;d.css" => Ok(b".cd{}".to_vec()),
            _ => Err("HTTP 404".to_string()),
        };
        let r = export_card(&c, None, files, fetch);
        let want = css.replace(r#"@import "c;d.css";"#, ".cd{}");
        assert!(
            r.html.contains(&format!("<style>{want}</style>")),
            "{}",
            r.html
        );
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    }

    #[test]
    fn a_url_ends_at_its_own_closing_parenthesis() {
        let svg = "data:image/svg+xml,<svg><rect fill='url(%23a)' stroke='url(%23b)'/></svg>";
        let css = format!(".a{{background:url({svg})}}.b{{background:url( \"{svg}\" )}}.c{{background:URL(i.png)}}.d{{background:url(x(y)}}.e{{background:url(j.png)}}.g{{content:\"x\\\r\ny\"}}.h{{background:url(k.png)}}.f{{background:url(\"a\\20b.png\")}}");
        let c = card(
            r#"<link rel="stylesheet" href="https://unpkg.com/p/s.css">"#,
            &[],
            &[],
        );
        let body = css.clone();
        let fetch = move |url: &str, _: Duration| match url {
            "https://unpkg.com/p/s.css" => Ok(body.clone().into_bytes()),
            _ => Err("HTTP 404".to_string()),
        };
        let r = export_card(&c, None, files, fetch);
        let want = css
            .replace("URL(i.png)", r#"url("https://unpkg.com/p/i.png")"#)
            .replace("url(j.png)", r#"url("https://unpkg.com/p/j.png")"#)
            .replace("url(k.png)", r#"url("https://unpkg.com/p/k.png")"#);
        assert!(
            r.html.contains(&format!("<style>{want}</style>")),
            "{}",
            r.html
        );
        let targets: Vec<_> = r.warnings.iter().map(|w| w.target.as_str()).collect();
        assert_eq!(
            targets,
            [
                "https://unpkg.com/p/i.png",
                "https://unpkg.com/p/j.png",
                "https://unpkg.com/p/k.png"
            ]
        );
    }

    #[test]
    fn layer_and_supports_imports_are_inlined() {
        let c = card(
            r#"<link rel="stylesheet" href="https://unpkg.com/a.css">"#,
            &[],
            &[],
        );
        let fetch = |url: &str, _: Duration| {
            match url {
            "https://unpkg.com/a.css" => Ok(
                b"@import url(b.css) layer(base) supports(display:grid) screen;@import \"b.css\" LAYER;@import 'b.css' supports(selector(a>b));"
                    .to_vec(),
            ),
            "https://unpkg.com/b.css" => Ok(b".b{}".to_vec()),
            _ => Err("HTTP 404".to_string()),
        }
        };
        let r = export_card(&c, None, files, fetch);
        assert!(
            r.html.contains(
                "<style>@layer base{@supports (display:grid){@media screen{.b{}}}}@layer{.b{}}@supports (selector(a>b)){.b{}}</style>"
            ),
            "{}",
            r.html
        );
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    }

    #[test]
    fn a_conditional_import_holding_a_link_stays_a_link() {
        let c = card(
            r#"<link rel="stylesheet" href="https://unpkg.com/a.css">"#,
            &[],
            &[],
        );
        let fetch = |url: &str, _: Duration| {
            match url {
            "https://unpkg.com/a.css" => Ok(
                b"@import \"b.css\" layer(x);@import \"d.css\" print;@import \"e.css\";@import \"f.css\" screen;".to_vec(),
            ),
            "https://unpkg.com/f.css" => Ok(b"@import \"a.css\";.f{}".to_vec()),
            "https://unpkg.com/b.css" => Ok(b"@import \"https://example.com/c.css\";.b{}".to_vec()),
            "https://unpkg.com/d.css" => Ok(b"@import \"gone.css\";.d{}".to_vec()),
            "https://unpkg.com/e.css" => Ok(b"@import \"https://example.com/c.css\";.e{}".to_vec()),
            _ => Err("HTTP 404".to_string()),
        }
        };
        let r = export_card(&c, None, files, fetch);
        assert!(
            r.html.contains(
                r#"<style>@import url("https://unpkg.com/b.css") layer(x);@import url("https://unpkg.com/d.css") print;@import "https://example.com/c.css";.e{}@media screen{.f{}}</style>"#
            ),
            "{}",
            r.html
        );
        let got: Vec<_> = r
            .warnings
            .iter()
            .map(|w| (w.kind, w.target.as_str(), w.reason.as_str()))
            .collect();
        assert_eq!(
            got,
            [
                (
                    ExportWarningKind::FetchFailed,
                    "https://unpkg.com/b.css",
                    "kept as a link: its @import of https://example.com/c.css would be ignored inside @layer"
                ),
                (ExportWarningKind::FetchFailed, "https://unpkg.com/gone.css", "HTTP 404"),
                (
                    ExportWarningKind::FetchFailed,
                    "https://unpkg.com/d.css",
                    "kept as a link: its @import of https://unpkg.com/gone.css would be ignored inside @media"
                ),
            ]
        );
    }

    #[test]
    fn an_import_past_the_depth_limit_warns() {
        let c = card(
            r#"<link rel="stylesheet" href="https://unpkg.com/a.css">"#,
            &[],
            &[],
        );
        let fetch = |url: &str, _: Duration| {
            let name = url
                .strip_prefix("https://unpkg.com/")
                .and_then(|n| n.strip_suffix(".css"))
                .filter(|n| matches!(*n, "a" | "b" | "c" | "d" | "e"))
                .ok_or_else(|| "HTTP 404".to_string())?;
            let next = (name.as_bytes()[0] + 1) as char;
            Ok(format!("@import \"{next}.css\";.{name}{{}}").into_bytes())
        };
        let r = export_card(&c, None, files, fetch);
        assert!(
            r.html.contains(
                r#"<style>@import url("https://unpkg.com/f.css");.e{}.d{}.c{}.b{}.a{}</style>"#
            ),
            "{}",
            r.html
        );
        let got: Vec<_> = r
            .warnings
            .iter()
            .map(|w| (w.kind, w.target.as_str(), w.reason.as_str()))
            .collect();
        assert_eq!(
            got,
            [(
                ExportWarningKind::FetchFailed,
                "https://unpkg.com/f.css",
                "skipped: nested more than 4 @imports deep"
            )]
        );
    }

    #[test]
    fn a_sheet_too_deep_to_inline_but_inlined_elsewhere_does_not_warn() {
        let c = card(
            r#"<link rel="stylesheet" href="https://unpkg.com/a.css">"#,
            &[],
            &[],
        );
        let fetch = |url: &str, _: Duration| {
            let body = match url.strip_prefix("https://unpkg.com/") {
                Some("a.css") => "@import \"y.css\";@import \"b.css\";@import \"x.css\";.a{}",
                Some("b.css") => "@import \"c.css\";.b{}",
                Some("c.css") => "@import \"d.css\";.c{}",
                Some("d.css") => "@import \"e.css\";.d{}",
                Some("e.css") => "@import \"x.css\";@import \"y.css\";.e{}",
                Some("x.css") => ".x{}",
                Some("y.css") => ".y{}",
                _ => return Err("HTTP 404".to_string()),
            };
            Ok(body.as_bytes().to_vec())
        };
        let r = export_card(&c, None, files, fetch);
        assert!(
            r.html.contains(
                r#"<style>.y{}@import url("https://unpkg.com/x.css");@import url("https://unpkg.com/y.css");.e{}.d{}.c{}.b{}.x{}.a{}</style>"#
            ),
            "{}",
            r.html
        );
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    }

    #[test]
    fn a_sheet_kept_as_a_link_but_inlined_elsewhere_does_not_warn() {
        let c = card(
            r#"<link rel="stylesheet" href="https://unpkg.com/a.css">"#,
            &[],
            &[],
        );
        let fetch = |url: &str, _: Duration| {
            let body = match url.strip_prefix("https://unpkg.com/") {
                Some("a.css") => {
                    "@import \"e.css\" print;@import \"b.css\" layer(l);@import \"f.css\" layer(l);@import \"f.css\";"
                }
                Some("b.css") => "@import \"https://example.com/c.css\";@import \"e.css\";.b{}",
                Some("e.css") => "@import \"gone.css\";.e{}",
                Some("f.css") => "@import \"https://example.com/c.css\";.f{}",
                _ => return Err("HTTP 404".to_string()),
            };
            Ok(body.as_bytes().to_vec())
        };
        let r = export_card(&c, None, files, fetch);
        assert!(
            r.html.contains(
                r#"<style>@import url("https://unpkg.com/e.css") print;@import url("https://unpkg.com/b.css") layer(l);@import url("https://unpkg.com/f.css") layer(l);@import "https://example.com/c.css";.f{}</style>"#
            ),
            "{}",
            r.html
        );
        // e.css went into b.css's expansion, which b.css staying a link threw
        // away, so e.css is still missing; f.css is in the page.
        let got: Vec<_> = r.warnings.iter().map(|w| w.target.as_str()).collect();
        assert_eq!(
            got,
            [
                "https://unpkg.com/gone.css",
                "https://unpkg.com/e.css",
                "https://unpkg.com/b.css"
            ]
        );
    }

    #[test]
    fn a_sheet_repeated_past_the_inlining_total_does_not_warn() {
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
        assert!(r
            .html
            .contains(r#"@import url("https://unpkg.com/b.css");"#));
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    }

    #[test]
    fn repeated_imports_stop_at_the_inlining_total() {
        let c = card(
            r#"<link rel="stylesheet" href="https://unpkg.com/a.css">"#,
            &[],
            &[],
        );
        let a: String = (0..40).map(|n| format!("@import \"b{n}.css\";")).collect();
        // a.css counts toward the total too.
        let fit = (MAX_INLINED_BYTES - a.len()) / MAX_ASSET_BYTES;
        let b = vec![b'x'; MAX_ASSET_BYTES];
        let fetch = move |url: &str, _: Duration| match url {
            "https://unpkg.com/a.css" => Ok(a.clone().into_bytes()),
            u if u.starts_with("https://unpkg.com/b") => Ok(b.clone()),
            _ => Err("HTTP 404".to_string()),
        };
        let r = export_card(&c, None, files, fetch);
        assert!(
            r.html.len() < MAX_INLINED_BYTES + 64 * 1024,
            "{}",
            r.html.len()
        );
        assert_eq!(r.warnings.len(), 40 - fit);
        assert_eq!(
            r.warnings[0].target,
            format!("https://unpkg.com/b{fit}.css")
        );
        assert!(r
            .warnings
            .iter()
            .all(|w| w.reason.contains("already inlined 8 MB")));
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
        assert!(r.html.contains("<title>hi</title>"), "{}", r.html);
    }

    #[test]
    fn a_card_with_no_text_is_titled_canvas_post() {
        let r = export_card(
            &card("<hr><script>x</script>", &[], &[]),
            None,
            files,
            offline,
        );
        assert!(r.html.contains("<title>Canvas post</title>"), "{}", r.html);
    }
}
