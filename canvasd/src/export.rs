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

use std::borrow::Cow;
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::io;
use std::ops::Range;
use std::path::Path;
use std::time::{Duration, Instant};

use canvas_core::html::{
    card_title, decode_entities, escape_attr, escape_text, find_attr_value, replace_attr_value,
    tag_name, tags, AttrValue, Tag, TextKind,
};
use canvas_core::{
    base64, Card, ExportResult, ExportWarning, ExportWarningKind, EXPORT_DOWNLOAD_SECS,
    MAX_ASSET_BYTES, MAX_MEDIA_BYTES, MEDIA_EXTS,
};

use serde_json::{Map, Value};

use crate::cdn::{self, Fetched};
use crate::esm;
use crate::import_map::{self, ImportMap};

/// How many `@import`s deep a CDN stylesheet is followed.
const MAX_IMPORT_DEPTH: usize = 4;

/// Total bytes of CDN content one export writes into the page: a stylesheet
/// once per place it lands, and a file as the `data:` URI that holds it, once
/// per place that URI is written, so neither a stylesheet that imports another
/// many times nor base64 can grow the page without bound. What surrounds the
/// content (an import map's keys, a `url("…")` wrapper, a fragment) is not
/// counted.
const MAX_INLINED_BYTES: usize = 16 * MAX_ASSET_BYTES;

/// `read(path, limit)` reads one file of at most `limit` bytes (`read_media`),
/// refusing a larger one unread; `fetch(url, timeout)` downloads one
/// URL within `timeout`.
pub fn export_card(
    card: &Card,
    data: Option<&serde_json::Value>,
    read: impl Fn(&str, u64) -> io::Result<Media>,
    fetch: impl cdn::Fetch,
) -> ExportResult {
    let mut warnings = Vec::new();
    let mut cdn = Cdn::new(fetch, Duration::from_secs(EXPORT_DOWNLOAD_SECS));
    let Rewritten {
        mut body,
        card_maps,
        aliases,
        roots,
    } = rewrite(card, &read, &mut cdn, &mut warnings);
    let page_map = PageMap::new(&card_maps, &cdn.modules, &aliases);
    let mut names = std::mem::take(&mut cdn.module_names);
    // Last first, so each offset still holds when its import goes in.
    for (at, key) in roots.iter().rev() {
        let import = module_import(&page_map.specifier(Some(key), &mut names));
        body.insert_str(*at, &escape_raw(&import, "script"));
    }
    let map = page_map.finish(&mut cdn, names, &mut warnings);
    // A file one reference left as a link while another put it in the page
    // isn't missing from the page.
    warnings
        .retain(|w| w.kind != ExportWarningKind::FetchFailed || !cdn.present().contains(&w.target));
    let title = escape_text(&card_title(&card.html));
    let mut html = String::with_capacity(body.len() + 1024);
    html.push_str("<!doctype html>\n<html><head><meta charset=\"utf-8\">");
    html.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">");
    html.push_str(&format!("<title>{title}</title>"));
    if let Some(map) = map {
        html.push_str(&format!("<script type=\"importmap\">{map}</script>"));
    }
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

fn rewrite<F: cdn::Fetch>(
    card: &Card,
    read: &impl Fn(&str, u64) -> io::Result<Media>,
    cdn: &mut Cdn<F>,
    warnings: &mut Vec<ExportWarning>,
) -> Rewritten {
    let html = card.html.as_str();
    let mut refs = CardRefs::new(card, read);
    let mut out = String::with_capacity(html.len());
    let mut pos = 0usize;
    // Inside an SVG or MathML `<style>`, whose text holds markup: the
    // style's element id and everything written in it so far, kept until the
    // tag that closes it so its CSS is read whole. WebKit reads that CSS from
    // every text node inside the style, child elements' included.
    let mut markup_style: Option<(usize, Vec<StylePiece>)> = None;
    // The card's own import maps, in order, and whether the next tag is
    // the end tag of one just taken.
    let mut card_maps = Vec::new();
    let mut drop_end_tag = false;
    // Each name a card module imports a fetched module by, with its URL.
    let mut aliases = Vec::new();
    let mut roots = Vec::new();

    let mut scan = tags(html);
    while let Some(Tag {
        start,
        end,
        text: raw_text,
        opened,
        foreign,
    }) = scan.next()
    {
        let text_end = raw_text.map(|t| t.end);
        let text = &html[pos..start];
        let tag = &html[start..end];
        pos = end;

        if let Some((id, pieces)) = markup_style.as_mut() {
            if !text.is_empty() {
                pieces.push(StylePiece::Text(text));
            }
            if scan.is_open(*id) {
                match StylePiece::of_tag(tag, scan.cdata_allowed()) {
                    StylePiece::Element(_) => {
                        if let Some(tag) = style_child(tag, foreign, &mut refs, cdn, warnings) {
                            pieces.push(StylePiece::Element(tag));
                        }
                    }
                    piece => pieces.push(piece),
                }
                if let Some(raw) = raw_text {
                    let body = &html[end..raw.end];
                    pieces.push(match raw.kind {
                        TextKind::Raw => StylePiece::Raw(body, tag),
                        TextKind::Escapable => StylePiece::Escapable(body),
                    });
                    pos = raw.end;
                }
                continue;
            }
            let (_, pieces) = markup_style.take().unwrap_or_default();
            out.push_str(&cdn.markup_css(&pieces, warnings));
        } else {
            out.push_str(text);
        }

        if tag.starts_with("</") {
            // The end tag of a card import map moved to the head.
            if drop_end_tag {
                drop_end_tag = false;
                if closing_name(tag) == "script" {
                    continue;
                }
            }
            if !refs.drops_end(tag) {
                out.push_str(tag);
            }
            continue;
        }

        match tag_name(tag).as_deref() {
            Some(element @ ("img" | "video" | "source")) => match refs.media(tag, warnings) {
                None => out.push_str(tag),
                Some(Ok(tag)) => out.push_str(&tag),
                Some(Err(missing)) if element == "img" => {
                    let what = match missing.kind {
                        ExportWarningKind::MediaTooLarge => "image left out",
                        _ => "image missing",
                    };
                    out.push_str(&format!(
                        "<span class=\"canvas-missing\" role=\"img\">{what}: {}</span>",
                        escape_text(&missing.name)
                    ));
                }
                Some(Err(missing)) => out.push_str(&missing.without_src),
            },
            Some("a") => {
                if let Some(tag) = refs.anchor(tag) {
                    out.push_str(&tag);
                }
            }
            // An SVG or MathML `<script>` or `<link>` fetches nothing, so it
            // stays as written.
            Some("script" | "link") if foreign => out.push_str(tag),
            Some("script") => {
                let kind = script_type(tag);
                // The card's import map joins the page's one, in the head.
                if kind == "importmap" && find_attr_value(tag, "src").is_none() {
                    let map = text_end.and_then(|text_end| {
                        match serde_json::from_str(&html[end..text_end]) {
                            Ok(Value::Object(map)) => Some((map, text_end)),
                            _ => None,
                        }
                    });
                    match map {
                        Some((map, text_end)) => {
                            card_maps.push(map);
                            cdn.import_map = ImportMap::parse(&import_map::merge(&card_maps));
                            pos = text_end;
                            drop_end_tag = true;
                        }
                        None => out.push_str(tag),
                    }
                    continue;
                }
                // The card's own module, kept as written: the page's import
                // map serves what it imports, under the names it uses.
                if kind == "module" && find_attr_value(tag, "src").is_none() {
                    if let Some(text_end) = text_end {
                        inline_module(&html[end..text_end], cdn, &mut aliases, warnings);
                    }
                    out.push_str(tag);
                    continue;
                }
                let src = find_attr_value(tag, "src")
                    .and_then(|src| Some((src, cdn::join(None, &attr_url(&tag[src.range()]))?)))
                    .filter(|(_, url)| cdn::allowed(url));
                let Some((src, url)) = src else {
                    out.push_str(tag);
                    continue;
                };
                // A module's own imports resolve against its URL, which an
                // inlined body would lose: the import map serves it and
                // the modules it reaches, and the element imports it, once
                // the page's map is known.
                if kind == "module" {
                    match cdn.module_graph(&url, warnings) {
                        Some(key) => {
                            out.push_str(&without_attr(tag, "src", src));
                            roots.push((out.len(), key));
                            pos = text_end.unwrap_or(end);
                        }
                        None => out.push_str(tag),
                    }
                    continue;
                }
                let body = cdn.inline(&url, warnings, |cdn, bytes, warnings| {
                    cdn.text(&url, &bytes, warnings)
                });
                match body {
                    Some(body) => {
                        out.push_str(&without_attr(tag, "src", src));
                        out.push_str(&escape_raw(&body, "script"));
                        // Whatever the element held is dropped with its src.
                        pos = text_end.unwrap_or(end);
                    }
                    None => out.push_str(tag),
                }
            }
            Some("link") => {
                let Some(url) = stylesheet_href(tag).filter(|url| cdn::allowed(url)) else {
                    out.push_str(tag);
                    continue;
                };
                let css = cdn.inline(&url, warnings, |cdn, bytes, warnings| {
                    cdn.stylesheet(&url, &bytes, warnings)
                });
                match css {
                    Some(css) => {
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
            Some("style") => {
                out.push_str(tag);
                match text_end {
                    Some(text_end) => {
                        let css = cdn.css(&html[end..text_end], None, 0, warnings);
                        out.push_str(&escape_raw(&css, "style"));
                        pos = text_end;
                    }
                    // Its CSS follows unless it closed itself.
                    None => markup_style = opened.map(|id| (id, Vec::new())),
                }
            }
            _ => out.push_str(tag),
        }
    }
    let text = &html[pos..];
    match markup_style {
        Some((_, mut pieces)) => {
            if !text.is_empty() {
                pieces.push(StylePiece::Text(text));
            }
            out.push_str(&cdn.markup_css(&pieces, warnings));
        }
        None => out.push_str(text),
    }
    // A module a card map entry names whole could be imported by any script,
    // `import()` of a computed name included, so each goes in the page. A
    // prefix entry names no module until something imports through it, so
    // only the names the inline modules use reach the page.
    for url in cdn.import_map.modules() {
        if let Some(url) = module_url(url).filter(|url| cdn::allowed(url)) {
            cdn.module_graph(&url, warnings);
        }
    }
    Rewritten {
        body: out,
        card_maps,
        aliases,
        roots,
    }
}

/// The card's markup as the page's body, and what the page's import map is
/// built from.
struct Rewritten {
    body: String,
    /// The card's own import maps, in order, taken out of the body.
    card_maps: Vec<Map<String, Value>>,
    /// Each name the card's inline modules import a fetched module by, with
    /// the module's URL.
    aliases: Vec<(String, String)>,
    /// Each module script whose src the import map serves: where in the body
    /// its import goes, and the key of the module it imports.
    roots: Vec<(usize, String)>,
}

/// Fetches each module on the CDN hosts the card's inline module `source`
/// imports, through the card's import maps, recording the name it used for
/// each in `aliases`. Source that doesn't parse runs nothing, so it reaches
/// nothing.
fn inline_module<F: cdn::Fetch>(
    source: &str,
    cdn: &mut Cdn<F>,
    aliases: &mut Vec<(String, String)>,
    warnings: &mut Vec<ExportWarning>,
) {
    let Ok(module) = esm::module(source) else {
        return;
    };
    for s in module.specifiers {
        let Some(url) = cdn.import_map.resolve(&s.value, None).and_then(module_url) else {
            continue;
        };
        if !cdn::allowed(&url) {
            continue;
        }
        if let Some(key) = cdn.module_graph(&url, warnings) {
            // The name as the map's keys are written: a URL serialized.
            let name = import_map::url_like(&s.value, None)
                .map_or_else(|| s.value.clone(), |u| u.to_string());
            aliases.push((name, key));
        }
    }
}

/// The card's own references in its tags: its files, read into the page,
/// and its `#canvas-open-<n>` anchors, rewritten from its targets.
struct CardRefs<'c, R> {
    card: &'c Card,
    read: &'c R,
    image_prefix: String,
    /// One entry per open `<a>`: true when its tags are dropped (a local path).
    anchors: Vec<bool>,
    /// Bytes of image and video files inlined so far.
    media_bytes: usize,
}

/// A card file an `<img>`, `<video>` or `<source>` names that stays out of
/// the page, already warned about.
struct MissingMedia {
    kind: ExportWarningKind,
    /// The file's name.
    name: String,
    /// The tag with ` src="…"` dropped whole: an empty src would make the
    /// browser request the page's own URL.
    without_src: String,
}

impl<'c, R: Fn(&str, u64) -> io::Result<Media>> CardRefs<'c, R> {
    fn new(card: &'c Card, read: &'c R) -> Self {
        CardRefs {
            card,
            read,
            image_prefix: format!("/api/cards/{}/images/", card.id),
            anchors: Vec::new(),
            media_bytes: 0,
        }
    }

    /// For an `<img>`, `<video>` or `<source>` whose src names one of the
    /// card's files: the tag with the file as a `data:` URI, or the file
    /// left out after a warning. None for any other src.
    fn media(
        &mut self,
        tag: &str,
        warnings: &mut Vec<ExportWarning>,
    ) -> Option<Result<String, MissingMedia>> {
        let src = find_attr_value(tag, "src")?;
        let index = tag[src.range()]
            .strip_prefix(&self.image_prefix)?
            .parse::<usize>()
            .ok()?;
        let path = self
            .card
            .images
            .get(index)
            .map(String::as_str)
            .unwrap_or("");
        Some(
            match media_data_uri(path, self.read, MAX_MEDIA_BYTES - self.media_bytes) {
                Ok((uri, len)) => {
                    self.media_bytes += len;
                    Ok(replace_attr_value(tag, src, &uri))
                }
                Err((kind, reason)) => {
                    warnings.push(ExportWarning {
                        kind,
                        target: path.to_string(),
                        reason,
                    });
                    Err(MissingMedia {
                        kind,
                        name: Path::new(path)
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default(),
                        without_src: without_attr(tag, "src", src),
                    })
                }
            },
        )
    }

    /// The `<a>` start tag `tag` as the page writes it: a `#canvas-open-<n>`
    /// on a web target becomes a link opening in a new tab; one on a local
    /// path, or an index the card has no target for, is dropped (None), its
    /// link text staying.
    fn anchor<'t>(&mut self, tag: &'t str) -> Option<Cow<'t, str>> {
        let link = find_attr_value(tag, "href").and_then(|href| {
            let index = tag[href.range()]
                .strip_prefix("#canvas-open-")?
                .parse::<usize>()
                .ok()?;
            Some((href, self.card.targets.get(index).map(String::as_str)))
        });
        let written = match link {
            None => Some(Cow::Borrowed(tag)),
            Some((href, Some(url)))
                if url.starts_with("http://") || url.starts_with("https://") =>
            {
                let close = if tag.ends_with("/>") { "/>" } else { ">" };
                let tag = replace_attr_value(tag, href, &escape_attr(url));
                let open = tag.strip_suffix(close).unwrap_or(&tag);
                Some(Cow::Owned(format!(
                    "{open} target=\"_blank\" rel=\"noopener\"{close}"
                )))
            }
            Some(_) => None,
        };
        self.anchors.push(written.is_none());
        written
    }

    /// Whether the end tag `tag` closes an `<a>` whose start tag was dropped.
    fn drops_end(&mut self, tag: &str) -> bool {
        closing_name(tag) == "a" && self.anchors.pop() == Some(true)
    }
}

/// The child element tag `tag` inside an SVG or MathML `<style>` as the page
/// writes it, or None to drop it. The style reads its CSS from the text
/// inside its children too, so only attributes change: each card file and
/// CDN script or stylesheet an HTML element there loads (inside a
/// `<foreignObject>`, where it runs) becomes a `data:` URI, and anchors are
/// rewritten as anywhere else. A card import map stays where it is.
fn style_child<'t, F: cdn::Fetch, R: Fn(&str, u64) -> io::Result<Media>>(
    tag: &'t str,
    foreign: bool,
    refs: &mut CardRefs<R>,
    cdn: &mut Cdn<F>,
    warnings: &mut Vec<ExportWarning>,
) -> Option<Cow<'t, str>> {
    if tag.starts_with("</") {
        return (!refs.drops_end(tag)).then_some(Cow::Borrowed(tag));
    }
    let written = match tag_name(tag).as_deref() {
        Some("img" | "video" | "source") => match refs.media(tag, warnings) {
            None => None,
            Some(Ok(tag)) => Some(tag),
            // A placeholder's text would be read as CSS.
            Some(Err(missing)) => Some(missing.without_src),
        },
        Some("a") => return refs.anchor(tag),
        Some("script") if !foreign => {
            let src = find_attr_value(tag, "src")
                .and_then(|src| Some((src, cdn::join(None, &attr_url(&tag[src.range()]))?)))
                .filter(|(_, url)| cdn::allowed(url));
            src.and_then(|(src, url)| {
                let uri = match script_type(tag).as_str() {
                    "importmap" => return None,
                    // The page's import map serves the module and what it
                    // reaches; a `data:` src can't resolve them itself.
                    "module" => {
                        // Inside a `data:` URI it can't wait for the
                        // page's map, so it imports by name.
                        let key = cdn.module_graph(&url, warnings)?;
                        let import = module_import(&module_name(Some(&key)));
                        cdn.module_names.insert(Some(key));
                        data_uri("text/javascript", import.as_bytes())
                    }
                    _ => cdn.inline(&url, warnings, |cdn, bytes, warnings| {
                        charged_data_uri(
                            cdn,
                            &url,
                            "text/javascript",
                            &bytes,
                            bytes.len(),
                            warnings,
                        )
                    })?,
                };
                Some(replace_attr_value(tag, src, &uri))
            })
        }
        Some("link") if !foreign => {
            let href = stylesheet_href(tag)
                .filter(|url| cdn::allowed(url))
                .zip(find_attr_value(tag, "href"));
            href.and_then(|(url, href)| {
                let uri = cdn.inline(&url, warnings, |cdn, bytes, warnings| {
                    let css = cdn.stylesheet(&url, &bytes, warnings)?;
                    charged_data_uri(cdn, &url, "text/css", css.as_bytes(), css.len(), warnings)
                })?;
                Some(replace_attr_value(tag, href, &uri))
            })
        }
        _ => None,
    };
    // A hash of the file no longer matches what the data: URI holds.
    Some(match written {
        Some(tag) => match find_attr_value(&tag, "integrity") {
            Some(integrity) => Cow::Owned(without_attr(&tag, "integrity", integrity)),
            None => Cow::Owned(tag),
        },
        None => Cow::Borrowed(tag),
    })
}

/// The body of a module script that imports `specifier`.
fn module_import(specifier: &str) -> String {
    format!(
        "import {};",
        serde_json::to_string(specifier).unwrap_or_default()
    )
}

/// A module the page's import map serves, fetched from `url`: its `source`,
/// and for each of its specifiers, where it sits in the source and the URL
/// the card's maps resolve it to from `url`. None when they resolve it to
/// nothing the card could load: a blocked entry, a bare name with no entry,
/// a URL that isn't http(s), or one its CSP refuses (an address relative to
/// the page, or one off the CDN hosts).
struct Module {
    url: String,
    source: String,
    imports: Vec<(Range<usize>, Option<String>)>,
    /// The bytes counted toward the inlining total for it so far: one copy
    /// of its source as a `data:` URI, until [`PageMap::finish`] counts what
    /// the page writes.
    counted: usize,
}

const MODULE_DATA_PREFIX: &str = "data:text/javascript;base64,";

/// `text` as the `data:` URI an import map serves a module from.
fn module_data_uri(text: &str) -> String {
    format!("{MODULE_DATA_PREFIX}{}", base64(text.as_bytes()))
}

/// The length of [`module_data_uri`] of `len` bytes of text.
fn module_data_uri_len(len: usize) -> usize {
    MODULE_DATA_PREFIX.len() + len.div_ceil(3) * 4
}

/// The name the page's import map keeps what an import of `target` loads
/// under, where the page's map would send `target` itself elsewhere:
/// `target` after a reserved scheme; `canvas-export:blocked` for None. Only
/// an entry named exactly matches a URL of a scheme that isn't special, so
/// no key of the card's maps, prefix keys included, reaches one.
fn module_name(target: Option<&str>) -> String {
    match target {
        // A key ending in `/` is a prefix, which a `data:` URI can't be the
        // address of; an empty fragment ends it instead.
        Some(url) if url.ends_with('/') => format!("canvas-export:{url}#"),
        Some(url) => format!("canvas-export:{url}"),
        None => "canvas-export:blocked".to_string(),
    }
}

/// `bytes` as a UTF-8 `data:` URI of type `mime`.
fn data_uri(mime: &str, bytes: &[u8]) -> String {
    format!("data:{mime};charset=utf-8;base64,{}", base64(bytes))
}

/// [`data_uri`] of what `url`'s body became, `counted` bytes of it already
/// counted toward the inlining total and the rest counted now; None, after a
/// warning naming `url`, when the rest would pass the total.
fn charged_data_uri<F: cdn::Fetch>(
    cdn: &mut Cdn<F>,
    url: &str,
    mime: &str,
    bytes: &[u8],
    counted: usize,
    warnings: &mut Vec<ExportWarning>,
) -> Option<String> {
    let uri = data_uri(mime, bytes);
    if !cdn.charge(uri.len().saturating_sub(counted)) {
        cdn.warn(url, over_total_reason(), warnings);
        return None;
    }
    Some(uri)
}

/// One node of an SVG or MathML `<style>`, as written.
enum StylePiece<'a> {
    /// A text run, character references and all.
    Text(&'a str),
    /// A `<![CDATA[` section, through its `]]>` when it has one.
    Cdata(&'a str),
    /// A comment, bogus or not, or a doctype: nothing the CSS reads.
    Comment(&'a str),
    /// A child element's start or end tag, which the CSS reads nothing
    /// from but which keeps the tree's shape, as the page writes it.
    Element(Cow<'a, str>),
    /// The text of a child element the scan read as raw text, such as an
    /// HTML `<script>` inside a `<foreignObject>`, and that element's opening
    /// tag.
    Raw(&'a str, &'a str),
    /// The text of a child element the scan read as escapable raw text, such
    /// as an HTML `<title>` inside a `<foreignObject>`.
    Escapable(&'a str),
}

impl<'a> StylePiece<'a> {
    /// What `tag` is inside a foreign `<style>` it doesn't close, where
    /// `cdata` says whether a `<![CDATA[` there opens a section.
    fn of_tag(tag: &'a str, cdata: bool) -> Self {
        if cdata && tag.starts_with("<![CDATA[") {
            return StylePiece::Cdata(tag);
        }
        let b = tag.as_bytes();
        let bogus = match b.get(1) {
            Some(b'!' | b'?') => true,
            Some(b'/') => !b.get(2).is_some_and(u8::is_ascii_alphabetic),
            _ => false,
        };
        if bogus {
            StylePiece::Comment(tag)
        } else {
            StylePiece::Element(Cow::Borrowed(tag))
        }
    }

    fn raw(&self) -> &str {
        match self {
            StylePiece::Text(raw)
            | StylePiece::Cdata(raw)
            | StylePiece::Comment(raw)
            | StylePiece::Raw(raw, _)
            | StylePiece::Escapable(raw) => raw,
            StylePiece::Element(raw) => raw,
        }
    }

    /// What the browser reads from the piece as CSS.
    fn css(&self) -> String {
        match self {
            StylePiece::Text(text) | StylePiece::Escapable(text) => decode_entities(text),
            StylePiece::Raw(text, _) => text.to_string(),
            StylePiece::Cdata(tag) => {
                let body = &tag["<![CDATA[".len()..];
                body.strip_suffix("]]>").unwrap_or(body).to_string()
            }
            StylePiece::Comment(_) | StylePiece::Element(_) => String::new(),
        }
    }
}

use record::{Cdn, Sheet};

/// The warning's reason for a URL refused for the inlining total.
fn over_total_reason() -> String {
    format!(
        "skipped: the export already inlined {} MB",
        MAX_INLINED_BYTES / (1024 * 1024)
    )
}

/// The part of [`Cdn`] that keeps the record of the page. A body comes out
/// of it only inside [`Cdn::inline`], which takes the body's record back
/// out when the caller throws the body away, and an inlined `@import` turns
/// back into a link only through [`Cdn::import_making_room`] and
/// [`Cdn::write_import`], which take its record out with it. Each inlined
/// import's record stays paired with its edit inside a [`Sheet`], and a
/// [`Tried`] carries its record into the sheet unopened, so outside this
/// module nothing can take out a range the record didn't hand it or swap
/// one import's range for another's. The caller still owes each `Tried` a
/// `write_import` into the sheet it was tried against, at its own `@import`.
mod record {
    use super::*;

    /// Downloads for one export: each URL fetched once, all of them within one
    /// time budget, and at most `MAX_INLINED_BYTES` inlined in total. A URL that
    /// isn't inlined warns once; `export_card` drops the warning when another
    /// reference put the URL in the page ([`Cdn::present`]).
    pub(super) struct Cdn<F> {
        fetch: F,
        budget: Duration,
        deadline: Instant,
        cache: HashMap<String, Result<Fetched, String>>,
        pub(super) warned: HashSet<String>,
        /// Every URL whose content went into the page, in order, so a
        /// stylesheet's expansion that is thrown away can drop its own.
        present: Vec<String>,
        inlined: usize,
        /// Bytes `inline` has refused for the inlining total, so an `@import`
        /// can tell whether anything in it missed the total. Outside the record
        /// `undo` takes back: a try reads only whether it grew while the try ran.
        refused: usize,
        /// The stylesheets being expanded, outermost first, so an `@import`
        /// that closes a cycle is dropped.
        pub(super) importing: Vec<String>,
        /// Every `@import` written out as a link, in order, so a stylesheet
        /// about to be wrapped in a block can tell whether it holds one, and
        /// an import that wrote links can tell whether room would fix them.
        linked: Vec<Link>,
        /// Every module a module script reaches on the CDN hosts, in the order
        /// fetched, for the page's import map.
        pub(super) modules: Vec<Module>,
        /// Every module URL `module_graph` has taken up, so each is read once.
        pub(super) module_seen: HashSet<String>,
        /// What each [`module_name`] written into the body so far loads,
        /// None for one that loads nothing.
        pub(super) module_names: BTreeSet<Option<String>>,
        /// The card's import maps read so far, merged, which a module's
        /// specifiers resolve through.
        pub(super) import_map: ImportMap,
    }

    impl<F: cdn::Fetch> Cdn<F> {
        pub(super) fn new(fetch: F, budget: Duration) -> Self {
            Cdn {
                fetch,
                budget,
                deadline: Instant::now() + budget,
                cache: HashMap::new(),
                warned: HashSet::new(),
                present: Vec::new(),
                inlined: 0,
                refused: 0,
                importing: Vec::new(),
                linked: Vec::new(),
                modules: Vec::new(),
                module_seen: HashSet::new(),
                module_names: BTreeSet::new(),
                import_map: ImportMap::default(),
            }
        }

        /// Every URL whose content is in the page, in the order it went in.
        pub(super) fn present(&self) -> &[String] {
            &self.present
        }

        /// The bytes inlined into the page so far.
        pub(super) fn inlined(&self) -> usize {
            self.inlined
        }

        /// The size of `url`'s fetched body when inlining it now would pass
        /// `MAX_INLINED_BYTES`, which is when [`Cdn::inline`] refuses it.
        pub(super) fn over_total(&self, url: &str) -> Option<usize> {
            match self.cache.get(url) {
                Some(Ok(fetched)) if self.inlined + fetched.body.len() > MAX_INLINED_BYTES => {
                    Some(fetched.body.len())
                }
                _ => None,
            }
        }

        /// Counts `extra` bytes a body grows by on its way into the page
        /// toward the inlining total, inside the `put` of [`Cdn::inline`] so
        /// they come back out with the body. False, counting them as
        /// refused instead, when they would pass `MAX_INLINED_BYTES`.
        pub(super) fn charge(&mut self, extra: usize) -> bool {
            if self.inlined + extra > MAX_INLINED_BYTES {
                self.refused += extra;
                return false;
            }
            self.inlined += extra;
            true
        }

        /// Counts `written` bytes toward the inlining total for the module
        /// `url` in place of the `counted` already counted for it. False when
        /// they would pass `MAX_INLINED_BYTES`: the module stays out of the
        /// page, its `counted` bytes handed back and its URL no longer present.
        pub(super) fn settle_module(&mut self, url: &str, counted: usize, written: usize) -> bool {
            self.inlined -= counted;
            if self.inlined + written > MAX_INLINED_BYTES {
                self.refused += written;
                if let Some(i) = self.present.iter().position(|p| p == url) {
                    self.present.remove(i);
                }
                return false;
            }
            self.inlined += written;
            true
        }

        /// Every `@import` written out as a link so far, in order.
        pub(super) fn links(&self) -> &[Link] {
            &self.linked
        }

        /// The `@import`s written out as links among `added`.
        fn links_in(&self, added: &Added) -> &[Link] {
            &self.linked[added.linked.clone()]
        }

        /// The first `@import` written out as a link by `tried`'s final try.
        pub(super) fn first_link(&self, tried: &Tried) -> Option<&Link> {
            self.links_in(&tried.added).first()
        }

        /// Records an `@import` written out as a link.
        pub(super) fn link(&mut self, link: Link) {
            self.linked.push(link);
        }

        /// Where the record of the page stands now, for [`Cdn::since`].
        fn mark(&self) -> Mark {
            Mark {
                bytes: self.inlined,
                present: self.present.len(),
                linked: self.linked.len(),
            }
        }

        /// What went into the record of the page after `mark`.
        fn since(&self, mark: Mark) -> Added {
            Added {
                bytes: self.inlined - mark.bytes,
                present: mark.present..self.present.len(),
                linked: mark.linked..self.linked.len(),
            }
        }

        /// Takes `added` back out of the record of the page, for content that
        /// went in and was then thrown away. Later entries move down.
        fn undo(&mut self, added: &Added) {
            self.inlined -= added.bytes;
            self.present.drain(added.present.clone());
            self.linked.drain(added.linked.clone());
        }

        /// The URL `url`'s body came from after any redirects, which its own
        /// relative references resolve against; `url` itself until it is fetched.
        pub(super) fn source_url(&self, url: &str) -> String {
            match self.cache.get(url) {
                Some(Ok(fetched)) => fetched.url.clone(),
                _ => url.to_string(),
            }
        }

        /// Records the body of `url` as in the page and hands it to `put`,
        /// which answers what it made of it, or None when it throws it away;
        /// then everything recorded since the body went in comes back out, the
        /// records of whatever `put` inlined from it included. None, without
        /// calling `put`, after a `fetch-failed` warning naming `url`.
        pub(super) fn inline<T>(
            &mut self,
            url: &str,
            warnings: &mut Vec<ExportWarning>,
            put: impl FnOnce(&mut Self, Vec<u8>, &mut Vec<ExportWarning>) -> Option<T>,
        ) -> Option<T> {
            let mark = self.mark();
            let bytes = self.get(url, warnings)?;
            let made = put(self, bytes, warnings);
            if made.is_none() {
                self.undo(&self.since(mark));
            }
            made
        }

        /// The body of `url`, recorded as in the page, or None after a
        /// `fetch-failed` warning naming it. Only [`Cdn::inline`] calls it.
        fn get(&mut self, url: &str, warnings: &mut Vec<ExportWarning>) -> Option<Vec<u8>> {
            if !self.cache.contains_key(url) {
                let left = self.deadline.saturating_duration_since(Instant::now());
                let result = if left.is_zero() {
                    Err(format!(
                        "skipped: the export's {}s download time ran out",
                        self.budget.as_secs()
                    ))
                } else {
                    self.fetch
                        .fetch(url, left.min(cdn::TIMEOUT))
                        .and_then(|fetched| {
                            if fetched.body.len() > MAX_ASSET_BYTES {
                                Err(format!("over {} KB", MAX_ASSET_BYTES / 1024))
                            } else {
                                Ok(fetched)
                            }
                        })
                };
                self.cache.insert(url.to_string(), result);
            }
            let result = match self.cache.get(url) {
                Some(Ok(fetched)) if self.over_total(url).is_some() => {
                    self.refused += fetched.body.len();
                    Err(over_total_reason())
                }
                Some(Ok(fetched)) => Ok(fetched.body.clone()),
                Some(Err(reason)) => Err(reason.clone()),
                None => return None,
            };
            match result {
                Ok(bytes) => {
                    self.inlined += bytes.len();
                    self.present.push(url.to_string());
                    Some(bytes)
                }
                Err(reason) => {
                    self.warn(url, reason, warnings);
                    None
                }
            }
        }

        /// [`Cdn::import`] in `sheet`, where every import inlined as rules
        /// turns back into a link if this one writes a link. When every link
        /// it writes would inline given more room, just enough of the earliest
        /// of those to give that room turn back into links first, since they
        /// would anyway, and it tries again. When one of its links stays a
        /// link whatever the room, all of them would turn back, so they do
        /// before one more try, if anything in it missed the total. Each try
        /// ends when the import writes no link or none are left inlined as
        /// rules, or when freeing room would change nothing. The result goes
        /// into the sheet with [`Cdn::write_import`].
        #[allow(clippy::too_many_arguments)]
        pub(super) fn import_making_room(
            &mut self,
            import: &Import,
            base: Option<&str>,
            depth: usize,
            written: &str,
            sheet: &mut Sheet,
            warnings: &mut Vec<ExportWarning>,
        ) -> Tried {
            let mut freed = Vec::new();
            loop {
                let start = self.mark();
                let (warned, refused) = (warnings.len(), self.refused);
                let (text, url) = self.import(import, base, depth, written, warnings);
                let added = self.since(start);
                let (links, rules) = prelude(&text);
                let ruled = &sheet.ruled;
                // How many of `ruled` to free before the next try, if any.
                let n = match room_for(self.links_in(&added)) {
                    _ if !links || ruled.is_empty() => 0,
                    Some(room) => {
                        let short = (self.inlined + room).saturating_sub(MAX_INLINED_BYTES);
                        let (mut n, mut bytes) = (0, 0);
                        while n < ruled.len() && (n == 0 || bytes < short) {
                            bytes += ruled[n].added.bytes;
                            n += 1;
                        }
                        n
                    }
                    None if self.refused > refused => ruled.len(),
                    None => 0,
                };
                if n == 0 {
                    return Tried {
                        text,
                        url,
                        links,
                        rules,
                        added,
                        freed,
                    };
                }
                self.undo(&added);
                for w in warnings.drain(warned..) {
                    self.warned.remove(&w.target);
                }
                freed.extend(self.unlink(sheet, n, None));
            }
        }

        /// Puts `tried`, the `@import` written as `written` at `at`, into
        /// `sheet`. When it writes a link, every import the sheet holds
        /// inlined as rules turns back into a link first; when it is inlined
        /// as rules, it joins them. Returns the URLs turned back into links,
        /// for the caller to warn.
        pub(super) fn write_import(
            &mut self,
            sheet: &mut Sheet,
            at: Range<usize>,
            import: &Import,
            written: &str,
            mut tried: Tried,
        ) -> Vec<String> {
            let urls = if tried.links && !sheet.ruled.is_empty() {
                let n = sheet.ruled.len();
                self.unlink(sheet, n, Some(&mut tried.added))
            } else {
                Vec::new()
            };
            let edit = sheet.edits.len();
            sheet.edits.push(Edit {
                at,
                text: tried.text,
            });
            if let Some(url) = tried.url.filter(|_| tried.rules) {
                sheet.ruled.push(Inlined {
                    edit,
                    link: link_text(import, Some(&url), written),
                    url,
                    added: tried.added,
                });
            }
            urls
        }

        /// Turns the first `n` of `sheet`'s imports inlined as rules back
        /// into links, moving `later`, recorded after all of them, to where
        /// its entries sit once theirs are out of the record of the page.
        /// Returns the URLs now links.
        fn unlink(
            &mut self,
            sheet: &mut Sheet,
            n: usize,
            later: Option<&mut Added>,
        ) -> Vec<String> {
            let ruled = &mut sheet.ruled;
            let taken: Vec<Inlined> = ruled.drain(..n).collect();
            // Last first, so each one's entries are where it recorded them.
            for r in taken.iter().rev() {
                self.undo(&r.added);
            }
            let mut undone = Vec::with_capacity(taken.len());
            let mut urls = Vec::with_capacity(taken.len());
            for r in taken {
                sheet.edits[r.edit].text = r.link;
                self.linked.push(Link {
                    url: r.url.clone(),
                    room: Some(r.added.bytes),
                });
                urls.push(r.url);
                undone.push(r.added);
            }
            for r in sheet.ruled.iter_mut() {
                r.added.after_undoing(&undone);
            }
            if let Some(added) = later {
                added.after_undoing(&undone);
            }
            urls
        }
    }

    /// One stylesheet's edits, in order, and the `@import`s among them
    /// inlined as rules, which turn back into links when a later import
    /// writes a link.
    pub(super) struct Sheet {
        edits: Vec<Edit>,
        ruled: Vec<Inlined>,
    }

    impl Sheet {
        pub(super) fn new() -> Self {
            Sheet {
                edits: Vec::new(),
                ruled: Vec::new(),
            }
        }

        /// Adds an edit that is not an `@import`.
        pub(super) fn push(&mut self, edit: Edit) {
            self.edits.push(edit);
        }

        pub(super) fn into_edits(self) -> Vec<Edit> {
            self.edits
        }
    }

    /// One `@import` inlined as rules, kept so it can turn back into a link:
    /// the edit that holds its stylesheet, its URL and link form, and what its
    /// stylesheet added to the record of the page.
    struct Inlined {
        edit: usize,
        url: String,
        link: String,
        added: Added,
    }

    /// What [`Cdn::import_making_room`] wrote for one `@import`: its text, its
    /// URL when inlined, whether the text holds an `@import` that stays a link
    /// and whether it holds rules, what its final try added to the record of
    /// the page, and the URLs of the earlier imports it turned back into links.
    #[must_use]
    pub(super) struct Tried {
        text: String,
        url: Option<String>,
        links: bool,
        rules: bool,
        added: Added,
        freed: Vec<String>,
    }

    impl Tried {
        /// Whether its text holds an `@import` that stays a link.
        pub(super) fn writes_link(&self) -> bool {
            self.links
        }

        /// The stylesheet's URL when it was inlined.
        pub(super) fn url(&self) -> Option<&str> {
            self.url.as_deref()
        }

        /// The URLs of the earlier imports it turned back into links, once.
        pub(super) fn take_freed(&mut self) -> Vec<String> {
            std::mem::take(&mut self.freed)
        }
    }

    /// A point in a [`Cdn`]'s record of the page: the bytes inlined and the
    /// lengths of `present` and `linked`.
    #[derive(Clone, Copy)]
    struct Mark {
        bytes: usize,
        present: usize,
        linked: usize,
    }

    /// What went into a [`Cdn`]'s record of the page between two points.
    /// Only the record makes one, it can't be cloned, and nothing outside the
    /// record reaches one, so every range it names is one the record handed
    /// out and goes back only with the import that recorded it.
    struct Added {
        bytes: usize,
        present: Range<usize>,
        linked: Range<usize>,
    }

    impl Added {
        /// Where these entries sit once each of `undone`, all recorded before
        /// them, has been taken out.
        fn after_undoing(&mut self, undone: &[Added]) {
            let present: usize = undone.iter().map(|a| a.present.len()).sum();
            let linked: usize = undone.iter().map(|a| a.linked.len()).sum();
            self.present = self.present.start - present..self.present.end - present;
            self.linked = self.linked.start - linked..self.linked.end - linked;
        }
    }
}

impl<F: cdn::Fetch> Cdn<F> {
    /// A `fetch-failed` warning that `url` stays a link, once per URL;
    /// `export_card` drops it if `url` ends up in [`Cdn::present`].
    fn warn(&mut self, url: &str, reason: String, warnings: &mut Vec<ExportWarning>) {
        if self.warned.insert(url.to_string()) {
            warnings.push(ExportWarning {
                kind: ExportWarningKind::FetchFailed,
                target: url.to_string(),
                reason,
            });
        }
    }

    /// `url`'s body `bytes` as text, each run of bytes that isn't UTF-8 read
    /// as one U+FFFD, through [`Cdn::charged`]. None when it would pass the
    /// inlining total.
    fn text(
        &mut self,
        url: &str,
        bytes: &[u8],
        warnings: &mut Vec<ExportWarning>,
    ) -> Option<String> {
        let text = String::from_utf8_lossy(bytes).into_owned();
        self.charged(url, text, bytes.len(), warnings).ok()
    }

    /// `text`, made from `url`'s body of `read` bytes, once the bytes it
    /// grew by on its way into the page count toward the inlining total like
    /// the body's own. Err with the bytes it needs in all, after a warning
    /// naming `url`, when they would pass the total.
    fn charged(
        &mut self,
        url: &str,
        text: String,
        read: usize,
        warnings: &mut Vec<ExportWarning>,
    ) -> Result<String, usize> {
        let extra = text.len().saturating_sub(read);
        if !self.charge(extra) {
            self.warn(url, over_total_reason(), warnings);
            return Err(read + extra);
        }
        Ok(text)
    }

    /// Fetches the module at `url` and every module on the CDN hosts it
    /// reaches, each kept in `modules` with what each of its specifiers
    /// resolves to from its own URL through the card's import maps, since
    /// the import map serves it from a `data:` URI, with no path to resolve
    /// a relative one against and no URL a scope would match. Answers the key
    /// `url` is kept under, or None when it can't be fetched or read (after a
    /// `fetch-failed` warning naming it). A module on the CDN hosts it
    /// reaches that can't stays out of the map, so an import of it loads its
    /// URL from the network; an import off those hosts is blocked, as the
    /// card's CSP blocked it.
    fn module_graph(&mut self, url: &str, warnings: &mut Vec<ExportWarning>) -> Option<String> {
        let key = module_url(url::Url::parse(url).ok()?)?;
        let mut queue = VecDeque::new();
        if self.module_seen.insert(key.clone()) {
            queue.push_back(key.clone());
        }
        while let Some(next) = queue.pop_front() {
            self.inline(&next, warnings, |cdn, bytes, warnings| {
                // The page writes it as a `data:` URI, so one copy of that
                // counts now; `PageMap::finish` counts every copy it writes.
                let source = String::from_utf8_lossy(&bytes).into_owned();
                let counted = module_data_uri_len(source.len());
                if !cdn.charge(counted.saturating_sub(bytes.len())) {
                    cdn.warn(&next, over_total_reason(), warnings);
                    return None;
                }
                match esm::module(&source) {
                    Ok(module) => {
                        if let Some(reason) = computed_reason(&source, &module.computed) {
                            warnings.push(ExportWarning {
                                kind: ExportWarningKind::ComputedImport,
                                target: next.clone(),
                                reason,
                            });
                        }
                        let base = url::Url::parse(&cdn.source_url(&next)).ok();
                        let mut imports = Vec::with_capacity(module.specifiers.len());
                        for s in module.specifiers {
                            // Resolved here, scopes and all: from a `data:`
                            // URI no scope would match.
                            let target = cdn
                                .import_map
                                .resolve(&s.value, base.as_ref())
                                .and_then(module_url)
                                // The card's CSP refused one off the CDN hosts.
                                .filter(|target| cdn::allowed(target));
                            if let Some(target) = &target {
                                if cdn.module_seen.insert(target.clone()) {
                                    queue.push_back(target.clone());
                                }
                            }
                            imports.push((s.range, target));
                        }
                        cdn.modules.push(Module {
                            url: next.clone(),
                            source,
                            imports,
                            counted,
                        });
                        Some(())
                    }
                    Err(e) => {
                        cdn.warn(
                            &next,
                            format!("not a module the export can read: {e}"),
                            warnings,
                        );
                        None
                    }
                }
            });
        }
        self.modules.iter().any(|m| m.url == key).then_some(key)
    }

    /// The stylesheet `url` linked with body `bytes`, its references
    /// inlined by [`Cdn::css`] against the URL the body came from. None when
    /// its [`Cdn::text`] would pass the inlining total.
    fn stylesheet(
        &mut self,
        url: &str,
        bytes: &[u8],
        warnings: &mut Vec<ExportWarning>,
    ) -> Option<String> {
        let text = self.text(url, bytes, warnings)?;
        self.importing.push(url.to_string());
        let from = self.source_url(url);
        let css = self.css(&text, Some(&from), 0, warnings);
        self.importing.pop();
        Some(css)
    }

    /// [`Cdn::css`] of an SVG or MathML `<style>` written as `pieces`. The
    /// browser reads its CSS from every text run, character references
    /// decoded, and CDATA section joined, child elements' included, comments
    /// dropped, so the scan runs on that. Pieces before the first change and
    /// after the last stay as written; the ones between become one run of the
    /// result, in the form of the first of them (text written with entities,
    /// a CDATA section with any `]]>` split across two, or a child's raw
    /// text), followed by the tags of the child elements among them.
    fn markup_css(&mut self, pieces: &[StylePiece], warnings: &mut Vec<ExportWarning>) -> String {
        let mut css = String::new();
        let mut spans = Vec::with_capacity(pieces.len());
        for piece in pieces {
            let start = css.len();
            css.push_str(&piece.css());
            spans.push(start..css.len());
        }
        let edits = self.css_edits(&css, None, 0, warnings);
        let (Some(start), Some(end)) = (
            edits.first().map(|e| e.at.start),
            edits.last().map(|e| e.at.end),
        ) else {
            return pieces.iter().map(StylePiece::raw).collect();
        };
        // The pieces holding the edits: from the one the first edit starts in
        // to the one the last edit ends in. An edit replaces a token of at
        // least one byte, so both are pieces with CSS of their own.
        let first = (0..pieces.len())
            .find(|&i| spans[i].end > start)
            .unwrap_or_default();
        let last = (0..pieces.len())
            .rev()
            .find(|&i| spans[i].start < end)
            .unwrap_or(first);
        let out = splice(&css, &edits);
        let changed = &out[spans[first].start..out.len() - (css.len() - spans[last].end)];
        let mut result: String = pieces[..first].iter().map(StylePiece::raw).collect();
        match &pieces[first] {
            StylePiece::Cdata(_) => {
                result.push_str("<![CDATA[");
                // A `]]>` would end the section; split it across two.
                result.push_str(&changed.replace("]]>", "]]]]><![CDATA[>"));
                result.push_str("]]>");
            }
            StylePiece::Raw(_, tag) => {
                result.push_str(&escape_raw(changed, &tag_name(tag).unwrap_or_default()))
            }
            _ => result.push_str(&escape_text(changed)),
        }
        // Child elements' tags stay, emptied, so the tree keeps its shape.
        result.extend(
            pieces[first + 1..=last]
                .iter()
                .filter_map(|piece| match piece {
                    StylePiece::Element(tag) => Some(tag.as_ref()),
                    _ => None,
                }),
        );
        result.extend(pieces[last + 1..].iter().map(StylePiece::raw));
        result
    }

    /// `css` with each CDN `@import` replaced by the stylesheet it names and
    /// each CDN `url()` by a `data:` URI: [`Cdn::css_edits`] applied.
    fn css(
        &mut self,
        css: &str,
        base: Option<&str>,
        depth: usize,
        warnings: &mut Vec<ExportWarning>,
    ) -> String {
        let edits = self.css_edits(css, base, depth, warnings);
        splice(css, &edits)
    }

    /// The edits that inline `css`, in order and apart, each one changing
    /// what it replaces: each CDN `@import` replaced by the stylesheet it
    /// names and each CDN `url()` by a `data:` URI. A reference relative to
    /// `base` that stays a link is made absolute, since the page no longer
    /// sits beside it. An `@import` after the sheet's own rules, which the
    /// browser ignores, stays as written and is never fetched.
    /// The text is read as the browser tokenizes it: strings, comments and
    /// escapes are left untouched, and only a whole `url(` or `@import`
    /// token is a reference.
    fn css_edits(
        &mut self,
        css: &str,
        base: Option<&str>,
        depth: usize,
        warnings: &mut Vec<ExportWarning>,
    ) -> Vec<Edit> {
        let b = css.as_bytes();
        let mut sheet = Sheet::new();
        let mut pos = 0usize;
        let mut i = 0usize;
        // Whether a rule of the sheet's own came before the current `@import`.
        let mut after_rules = false;
        while i < b.len() {
            match b[i] {
                b'/' if b.get(i + 1) == Some(&b'*') => i = comment_end(b, i),
                b'"' | b'\'' => i = string_end(b, i).0,
                b'\\' => i += escape_len(b, i),
                b'@' if starts_with_word(b, i, b"@import") => {
                    let Some(import) = parse_import(css, i) else {
                        i += "@import".len();
                        continue;
                    };
                    // A rule of the sheet's own before this import leaves it
                    // ignored, so it stays as written and fetches nothing.
                    after_rules = after_rules || prelude(&css[pos..i]).1;
                    let written = &css[i..import.end];
                    if after_rules {
                        pos = import.end;
                        i = import.end;
                        continue;
                    }
                    let mut tried = self
                        .import_making_room(&import, base, depth, written, &mut sheet, warnings);
                    // Sheets freed to make room for an import that still
                    // writes a link went back for the link, as the rest do.
                    let reason = if tried.writes_link() {
                        let later = self
                            .first_link(&tried)
                            .map_or(&import.reference, |l| &l.url);
                        format!("kept as a link: the later @import of {later} would be ignored after its rules")
                    } else {
                        let later = tried.url().unwrap_or(&import.reference);
                        format!(
                            "kept as a link: the later @import of {later} needed its bytes under the export's {} MB total",
                            MAX_INLINED_BYTES / (1024 * 1024)
                        )
                    };
                    for url in tried.take_freed() {
                        self.warn(&url, reason.clone(), warnings);
                    }
                    for url in self.write_import(&mut sheet, i..import.end, &import, written, tried)
                    {
                        self.warn(&url, reason.clone(), warnings);
                    }
                    pos = import.end;
                    i = import.end;
                }
                b'u' | b'U'
                    if (i == 0 || !ident_byte(b[i - 1])) && starts_with_ci(b, i, b"url(") =>
                {
                    let (reference, end) = url_token(css, i);
                    if let Some(text) = reference.and_then(|r| self.url(r, base, warnings)) {
                        sheet.push(Edit { at: i..end, text });
                    }
                    pos = end;
                    i = end;
                }
                _ => i += 1,
            }
        }
        let mut edits = sheet.into_edits();
        edits.retain(|e| e.text != css[e.at.clone()]);
        edits
    }

    /// What replaces one `@import` written as `written`: the stylesheet it
    /// names under its conditions, with that stylesheet's URL, or the import
    /// itself, made absolute. An import of a CDN stylesheet that stays a link
    /// warns. One that closes a cycle is dropped, as the browser ignores it.
    /// A stylesheet that holds an import staying a link is not inlined under
    /// conditions, since the browser ignores an `@import` inside a block.
    fn import(
        &mut self,
        import: &Import,
        base: Option<&str>,
        depth: usize,
        written: &str,
        warnings: &mut Vec<ExportWarning>,
    ) -> (String, Option<String>) {
        let absolute = cdn::join(base, &import.reference);
        // The bytes that would let the link this writes inline instead.
        let mut room = None;
        let inlined = match &absolute {
            Some(u) if self.importing.contains(u) => return (String::new(), None),
            Some(u) if cdn::allowed(u) => {
                if depth >= MAX_IMPORT_DEPTH {
                    let reason =
                        format!("skipped: nested more than {MAX_IMPORT_DEPTH} @imports deep");
                    self.warn(u, reason, warnings);
                    None
                } else {
                    let (mut blocked, before) = (None, self.inlined());
                    let text = self.inline(u, warnings, |cdn, bytes, warnings| {
                        // The closers, and any U+FFFD read for bytes that
                        // aren't UTF-8, count toward the total like its bytes.
                        let closed = closed_at_end(&String::from_utf8_lossy(&bytes));
                        let closed = match cdn.charged(u, closed, bytes.len(), warnings) {
                            Ok(closed) => closed,
                            Err(needed) => {
                                blocked = Some(Some(needed));
                                return None;
                            }
                        };
                        let linked = cdn.links().len();
                        cdn.importing.push(u.clone());
                        let from = cdn.source_url(u);
                        let text = cdn.css(
                            &closed,
                            Some(&from),
                            depth + 1,
                            warnings,
                        );
                        cdn.importing.pop();
                        match (import.block(), cdn.links().get(linked)) {
                            (Some(block), Some(nested)) => {
                                let reason = format!(
                                    "kept as a link: its @import of {} would be ignored inside {block}",
                                    nested.url
                                );
                                // Its nested links' room, plus the bytes it
                                // and its inlined imports hand back now.
                                let own = cdn.inlined() - before;
                                blocked = Some(room_for(&cdn.links()[linked..]).map(|r| r + own));
                                cdn.warn(u, reason, warnings);
                                None
                            }
                            _ => Some(text),
                        }
                    });
                    if text.is_none() {
                        room = blocked.unwrap_or_else(|| self.over_total(u));
                    }
                    text
                }
            }
            _ => None,
        };
        if let Some(text) = inlined {
            return (import.wrap(text), absolute);
        }
        self.link(Link {
            url: absolute.clone().unwrap_or_else(|| import.reference.clone()),
            room,
        });
        (link_text(import, absolute.as_deref(), written), None)
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
            let uri = self.inline(target, warnings, |cdn, bytes, warnings| {
                let path = target.split('?').next().unwrap_or(target);
                let mime = mime_guess::from_path(path).first_or_octet_stream();
                let uri = format!("data:{};base64,{}", mime.essence_str(), base64(&bytes));
                // Its base64 counts toward the total like the bytes it encodes.
                let uri = cdn.charged(target, uri, bytes.len(), warnings).ok()?;
                Some(format!("url(\"{uri}{fragment}\")"))
            });
            if uri.is_some() {
                return uri;
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

/// One replacement [`Cdn::css_edits`] makes: the bytes `at` of the sheet
/// become `text`.
struct Edit {
    at: Range<usize>,
    text: String,
}

/// `css` with each of `edits`, in order and apart, in place of the bytes it
/// replaces.
fn splice(css: &str, edits: &[Edit]) -> String {
    let mut out = String::with_capacity(css.len());
    let mut pos = 0;
    for edit in edits {
        out.push_str(&css[pos..edit.at.start]);
        out.push_str(&edit.text);
        pos = edit.at.end;
    }
    out.push_str(&css[pos..]);
    out
}

/// One `@import` written out as a link: its URL, and the bytes of room under
/// the inlining total that would let it inline instead, or None when room
/// would not: a reference off the CDN hosts or that doesn't resolve, a
/// download that failed or ran out of time, an import nested too deep, or a
/// conditional import holding such a link.
struct Link {
    url: String,
    room: Option<usize>,
}

/// The room every link in `links` needs to inline, or None when one of them
/// would stay a link whatever the room.
fn room_for(links: &[Link]) -> Option<usize> {
    links.iter().map(|l| l.room).sum()
}

/// `import`, written as `written`, as a link to `absolute`: made absolute
/// when it was relative, else as written.
fn link_text(import: &Import, absolute: Option<&str>, written: &str) -> String {
    match absolute {
        Some(u) if u != import.reference => {
            let sep = if import.conditions.is_empty() {
                ""
            } else {
                " "
            };
            format!(
                "@import url(\"{}\"){sep}{};",
                u.replace('"', "%22"),
                import.conditions
            )
        }
        _ => written.to_string(),
    }
}

/// Whether `css` holds an `@import` before its first rule, and whether it
/// holds a rule, after which the browser ignores any `@import`. Space,
/// comments, `<!--`, `-->` and at-rule statements other than `@namespace`
/// are no rule: the browser keeps an `@layer` statement and drops the rest.
/// Any block is a rule.
fn prelude(css: &str) -> (bool, bool) {
    let b = css.as_bytes();
    let mut imports = false;
    let mut i = 0usize;
    loop {
        i = skip_space(b, i);
        if i >= b.len() {
            return (imports, false);
        }
        if b[i..].starts_with(b"/*") {
            i = comment_end(b, i);
        } else if b[i..].starts_with(b"<!--") {
            i += 4;
        } else if b[i..].starts_with(b"-->") {
            i += 3;
        } else if let Some(import) = starts_with_word(b, i, b"@import")
            .then(|| parse_import(css, i))
            .flatten()
        {
            imports = true;
            i = import.end;
        } else if b[i] == b'@' && !starts_with_word(b, i, b"@namespace") {
            match statement_end(b, i) {
                Some(end) => i = end,
                None => return (imports, true),
            }
        } else {
            return (imports, true);
        }
    }
}

/// The index just past the `;` ending the at-rule at `at`, or the end of
/// the text, or None when a block comes first.
fn statement_end(b: &[u8], at: usize) -> Option<usize> {
    let mut i = at;
    while let Some(&c) = b.get(i) {
        match c {
            b';' => return Some(i + 1),
            b'{' | b'}' => return None,
            b'"' | b'\'' => i = string_end(b, i).0,
            b'/' if b.get(i + 1) == Some(&b'*') => i = comment_end(b, i),
            b'\\' => i += escape_len(b, i),
            _ => i += 1,
        }
    }
    Some(b.len())
}

/// `css` as its own end of file leaves it, written so more text can follow:
/// an open string, comment or `url(` closed, an escape cut by the end read
/// as U+FFFD, every open bracket and block closed, and an at-rule statement
/// ended with `;`. A rule's selector with no block is dropped, as the browser
/// drops it, since any `;` would leave it reading on into the next rule.
/// A `}` that closes nothing at the top level is written as `]`.
fn closed_at_end(css: &str) -> String {
    /// The statement under way at the top level.
    struct Statement {
        at_rule: bool,
        start: usize,
        block: bool,
    }
    let b = css.as_bytes();
    let mut statement: Option<Statement> = None;
    let mut open: Vec<u8> = Vec::new();
    let mut stray: Vec<usize> = Vec::new();
    let mut tail = String::new();
    let mut i = 0usize;
    while i < b.len() {
        let c = b[i];
        if is_space(c as char) {
            i += 1;
            continue;
        }
        if c == b'/' && b.get(i + 1) == Some(&b'*') {
            let end = comment_end(b, i);
            if end == b.len() && (end < i + 4 || !css.ends_with("*/")) {
                tail.push_str("*/");
            }
            i = end;
            continue;
        }
        if statement.is_none() && open.is_empty() {
            if b[i..].starts_with(b"<!--") {
                i += 4;
                continue;
            }
            if b[i..].starts_with(b"-->") {
                i += 3;
                continue;
            }
            statement = Some(Statement {
                at_rule: c == b'@' && starts_ident(b, i + 1),
                start: i,
                block: false,
            });
        }
        match c {
            b'"' | b'\'' => {
                let (end, closed) = string_end(b, i);
                if !closed && end == b.len() {
                    // An escape cut by the end adds nothing to a string.
                    if escape_cut(b, i + 1) {
                        tail.push('\n');
                    }
                    tail.push(c as char);
                }
                i = end;
            }
            b'\\' => {
                if i + 1 == b.len() {
                    tail.push('\u{FFFD}');
                }
                i += escape_len(b, i);
            }
            b'u' | b'U'
                if (i == 0 || !ident_byte(b[i - 1]))
                    && starts_with_ci(b, i, b"url(")
                    && !matches!(b.get(skip_space(b, i + 4)), Some(b'"' | b'\'')) =>
            {
                match first_close(b, i + 4) {
                    Some(end) => i = end,
                    None => {
                        if escape_cut(b, i + 4) {
                            tail.push('\u{FFFD}');
                        }
                        tail.push(')');
                        i = b.len();
                    }
                }
            }
            b'(' | b'[' | b'{' => {
                if open.is_empty() && c == b'{' {
                    if let Some(s) = statement.as_mut() {
                        s.block = true;
                    }
                }
                open.push(match c {
                    b'(' => b')',
                    b'[' => b']',
                    _ => b'}',
                });
                i += 1;
            }
            b')' | b']' | b'}' => {
                if open.last() == Some(&c) {
                    open.pop();
                    if open.is_empty() && c == b'}' && statement.as_ref().is_some_and(|s| s.block) {
                        statement = None;
                    }
                } else if open.is_empty() && c == b'}' {
                    stray.push(i);
                }
                i += 1;
            }
            b';' if open.is_empty() && statement.as_ref().is_some_and(|s| s.at_rule) => {
                statement = None;
                i += 1;
            }
            _ => i += 1,
        }
    }
    // A stray `}` at the top level is one more token of its statement, but
    // inside an import's wrapper it would close the wrapper; `]` reads the
    // same at the top level and closes nothing inside a block.
    let kept = |end: usize| {
        let mut out = css[..end].to_string();
        for &at in stray.iter().take_while(|&&at| at < end) {
            out.replace_range(at..at + 1, "]");
        }
        out
    };
    match statement {
        Some(Statement {
            at_rule: false,
            start,
            block: false,
        }) => kept(start),
        statement => {
            let mut out = String::with_capacity(css.len() + tail.len() + open.len() + 1);
            out.push_str(&kept(css.len()));
            out.push_str(&tail);
            out.extend(open.iter().rev().map(|&c| c as char));
            if statement.is_some_and(|s| !s.block) {
                out.push(';');
            }
            out
        }
    }
}

/// Whether an identifier starts at `i`, as the browser reads an at-keyword's
/// name: a letter, `_` or non-ASCII character, an escape, or `-` before one
/// of those or another `-`.
fn starts_ident(b: &[u8], i: usize) -> bool {
    let start = |c: u8| c.is_ascii_alphabetic() || c == b'_' || c >= 0x80;
    let escape = |j: usize| {
        b.get(j) == Some(&b'\\') && !matches!(b.get(j + 1), Some(b'\n' | b'\r' | b'\x0c'))
    };
    match b.get(i) {
        Some(&b'-') => b.get(i + 1).is_some_and(|&c| start(c) || c == b'-') || escape(i + 1),
        Some(&c) => start(c) || escape(i),
        None => false,
    }
}

/// Whether the text from `from` to the end closes on an escape the end cut:
/// a backslash with nothing after it.
fn escape_cut(b: &[u8], from: usize) -> bool {
    let mut i = from;
    while i < b.len() {
        if b[i] == b'\\' {
            if i + 1 == b.len() {
                return true;
            }
            i += escape_len(b, i);
        } else {
            i += 1;
        }
    }
    false
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

/// `url` serialized, when it is http(s).
fn module_url(url: url::Url) -> Option<String> {
    matches!(url.scheme(), "http" | "https").then(|| url.to_string())
}

/// The `computed-import` reason for a fetched module whose `source` computes
/// imports at `computed` ([`esm::Module::computed`]): how many, and where
/// the first is, with its text cut to 60 characters. None when there are none.
fn computed_reason(source: &str, computed: &[Range<usize>]) -> Option<String> {
    let first = computed.first()?;
    let before = &source[..first.start];
    let line = before.matches('\n').count() + 1;
    let column = before[before.rfind('\n').map_or(0, |i| i + 1)..]
        .chars()
        .count()
        + 1;
    let text: String = source[first.clone()].chars().take(60).collect();
    let more = match computed.len() {
        1 => String::new(),
        n => format!(" (and {} more)", n - 1),
    };
    Some(format!(
        "computes an import from its own address, which the export makes a \
         data: URI, so it fails when the page runs: {text} at line {line}, \
         column {column}{more}"
    ))
}

/// The page's one import map. [`PageMap::new`] merges the card's own maps
/// ([`import_map::merge`]) and adds each name the card's inline modules
/// reached a fetched module by, and each fetched module under its URL,
/// wherever the card's maps don't already name it, every address still a
/// URL, so it tells where the page sends a URL ([`PageMap::specifier`]).
/// [`PageMap::finish`] writes the fetched modules out and makes each address
/// naming one its `data:` URI (an import map maps a name once, so one
/// leading to the module's URL would load it from the network).
struct PageMap {
    map: Map<String, Value>,
    resolved: ImportMap,
    /// Nothing to map: no card map and no fetched module.
    empty: bool,
}

impl PageMap {
    fn new(
        card_maps: &[Map<String, Value>],
        modules: &[Module],
        aliases: &[(String, String)],
    ) -> Self {
        let mut map = import_map::merge(card_maps);
        if !modules.is_empty() {
            let imports = map
                .entry("imports")
                .or_insert_with(|| Value::Object(Map::new()));
            if let Value::Object(imports) = imports {
                let named = aliases.iter().map(|(name, url)| (name, url));
                let urls = modules.iter().map(|m| (&m.url, &m.url));
                for (name, url) in named.chain(urls) {
                    imports
                        .entry(name.clone())
                        .or_insert_with(|| Value::String(url.clone()));
                }
            }
        }
        PageMap {
            resolved: ImportMap::parse(&map),
            map,
            empty: card_maps.is_empty() && modules.is_empty(),
        }
    }

    /// What an import of `target` (see [`Module`]) is written as: the URL
    /// itself where the page's map leaves it alone, so it loads the module
    /// the card's did; else its [`module_name`], added to `names`. The page's
    /// map resolves it from a `data:` URI, which no scope matches.
    fn specifier(&self, target: Option<&str>, names: &mut BTreeSet<Option<String>>) -> String {
        if let Some(url) = target {
            if self
                .resolved
                .resolve(url, None)
                .is_some_and(|u| u.as_str() == url)
            {
                return url.to_string();
            }
        }
        names.insert(target.map(str::to_string));
        module_name(target)
    }

    /// The map as the page writes it, each of `cdn`'s modules written with
    /// its specifiers ([`PageMap::specifier`]) as a `data:` URI, and each of
    /// `names` mapped to its module's `data:` URI, its URL when the module
    /// isn't in the page, or null when it loads nothing. Each module counts
    /// toward the inlining total once per entry the map writes it in; one
    /// that would pass the total stays out of the page, after a warning, so
    /// its entries keep its URL and it loads from there, without its
    /// `computed-import` warning. What it imports still resolves through
    /// this map, so the modules it reaches stay in. None when there is
    /// nothing to map.
    fn finish<F: cdn::Fetch>(
        mut self,
        cdn: &mut Cdn<F>,
        mut names: BTreeSet<Option<String>>,
        warnings: &mut Vec<ExportWarning>,
    ) -> Option<String> {
        if self.empty {
            return None;
        }
        let modules = std::mem::take(&mut cdn.modules);
        let mut data: HashMap<&str, String> = HashMap::with_capacity(modules.len());
        for m in &modules {
            let mut text = String::with_capacity(m.source.len());
            let mut copied = 0;
            for (range, target) in &m.imports {
                let specifier = self.specifier(target.as_deref(), &mut names);
                text.push_str(&m.source[copied..range.start]);
                text.push_str(&serde_json::to_string(&specifier).unwrap_or_default());
                copied = range.end;
            }
            text.push_str(&m.source[copied..]);
            data.insert(&m.url, module_data_uri(&text));
        }
        if !names.is_empty() {
            let imports = self
                .map
                .entry("imports")
                .or_insert_with(|| Value::Object(Map::new()));
            if let Value::Object(imports) = imports {
                for target in &names {
                    let address = match target {
                        Some(url) => Value::String(url.clone()),
                        None => Value::Null,
                    };
                    imports.insert(module_name(target.as_deref()), address);
                }
            }
        }
        // Every address map, `imports` and each scope: each entry naming a
        // module becomes its `data:` URI.
        let mut maps: Vec<&mut Map<String, Value>> = Vec::new();
        for (field, value) in self.map.iter_mut() {
            match (field.as_str(), value) {
                ("imports", Value::Object(imports)) => maps.push(imports),
                ("scopes", Value::Object(scopes)) => {
                    maps.extend(scopes.values_mut().filter_map(|scope| match scope {
                        Value::Object(scope) => Some(scope),
                        _ => None,
                    }))
                }
                _ => {}
            }
        }
        // The module an entry names, by its key and address.
        let module = |key: &str, value: &Value| match value {
            Value::String(address) if !key.ends_with('/') => {
                url::Url::parse(address).ok().map(|url| url.to_string())
            }
            _ => None,
        };
        let mut copies: HashMap<String, usize> = HashMap::new();
        for map in &maps {
            for (key, value) in map.iter() {
                if let Some(url) = module(key, value) {
                    *copies.entry(url).or_default() += 1;
                }
            }
        }
        for m in &modules {
            let written = data[m.url.as_str()].len() * copies.get(&m.url).copied().unwrap_or(0);
            if !cdn.settle_module(&m.url, m.counted, written) {
                data.remove(m.url.as_str());
                // It loads from its own address now, so an import computed
                // from that address works.
                warnings
                    .retain(|w| w.kind != ExportWarningKind::ComputedImport || w.target != m.url);
                cdn.warn(&m.url, over_total_reason(), warnings);
            }
        }
        for map in maps {
            for (key, value) in map.iter_mut() {
                if let Some(uri) = module(key, value).and_then(|url| data.get(url.as_str())) {
                    *value = Value::String(uri.clone());
                }
            }
        }
        // `<` escaped so the map can't close its script element.
        serde_json::to_string(&Value::Object(self.map))
            .ok()
            .map(|json| json.replace('<', "\\u003c"))
    }
}

/// A `<script>`'s `type`, trimmed and lowercased; empty when it has none.
fn script_type(tag: &str) -> String {
    find_attr_value(tag, "type")
        .map(|t| decode_entities(&tag[t.range()]).trim().to_ascii_lowercase())
        .unwrap_or_default()
}

/// `tag` with its `name="…"` (or unquoted `name=…`) attribute, whose value
/// is `value`, removed.
fn without_attr(tag: &str, name: &str, value: AttrValue) -> String {
    let outer = value.outer();
    let attr_start = tag[..outer.start]
        .trim_end()
        .trim_end_matches('=')
        .trim_end()
        .len()
        - name.len();
    format!("{}{}", tag[..attr_start].trim_end(), &tag[outer.end..])
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

/// One image or video file as `read_media` found it.
pub enum Media {
    Bytes(Vec<u8>),
    /// Over the limit: `size` bytes as the file system reports it, the file
    /// left unread; or, when `exact` is false, more than `size` bytes found
    /// by reading (a file that grew after its size was read, or one with no
    /// length of its own).
    TooLarge {
        size: u64,
        exact: bool,
    },
}

/// The file at `path` when it holds at most `limit` bytes. A larger file is
/// refused from its metadata before any byte is read; the read itself still
/// stops at `limit + 1`, for a file whose metadata understates it.
pub fn read_media(path: &str, limit: u64) -> io::Result<Media> {
    use std::io::Read;
    let file = std::fs::File::open(path)?;
    let size = file.metadata()?.len();
    if size > limit {
        return Ok(Media::TooLarge { size, exact: true });
    }
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Ok(Media::TooLarge {
            size: limit,
            exact: false,
        });
    }
    Ok(Media::Bytes(bytes))
}

/// `bytes` in MB to one decimal: rounded up for a file's size, so one just
/// over a cap never reads as the cap itself, and down for a bound the file is
/// only known to exceed.
fn megabytes(bytes: u64, up: bool) -> String {
    let tenths = u128::from(bytes) * 10;
    let mb = 1024 * 1024;
    let tenths = if up { tenths.div_ceil(mb) } else { tenths / mb };
    format!("{}.{}", tenths / 10, tenths % 10)
}

/// The file at `path` as a `data:` URI and its size, when it fits in `room`
/// bytes; otherwise the warning kind and reason.
fn media_data_uri(
    path: &str,
    read: &impl Fn(&str, u64) -> io::Result<Media>,
    room: usize,
) -> Result<(String, usize), (ExportWarningKind, String)> {
    let missing = |reason: String| (ExportWarningKind::MissingImage, reason);
    if path.is_empty() {
        return Err(missing("the card has no file for this image".into()));
    }
    let ext = Path::new(path)
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    if !MEDIA_EXTS.contains(&ext.as_str()) {
        return Err(missing(format!("not an image or video file: .{ext}")));
    }
    let bytes = match read(path, room as u64).map_err(|e| missing(e.to_string()))? {
        Media::Bytes(bytes) => bytes,
        Media::TooLarge { size, exact } => {
            let cap = MAX_MEDIA_BYTES / (1024 * 1024);
            let size_mb = megabytes(size, exact);
            let file = if exact {
                format!("a {size_mb} MB file")
            } else {
                format!("a file of more than {size_mb} MB")
            };
            let cap_bytes = MAX_MEDIA_BYTES as u64;
            let over_cap = if exact {
                size > cap_bytes
            } else {
                size >= cap_bytes
            };
            let reason = if over_cap {
                format!("{file} is over the {cap} MB of images and videos one export inlines")
            } else {
                format!(
                    "{file} would take the page past the {cap} MB of images and videos one export inlines"
                )
            };
            return Err((ExportWarningKind::MediaTooLarge, reason));
        }
    };
    let mime = mime_guess::from_path(path).first_or_octet_stream();
    let uri = format!("data:{};base64,{}", mime.essence_str(), base64(&bytes));
    Ok((uri, bytes.len()))
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

    fn files(path: &str, _: u64) -> io::Result<Media> {
        match path {
            "/x/a.png" => Ok(Media::Bytes(b"abc".to_vec())),
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
    fn media_past_the_page_cap_is_left_out_and_smaller_files_still_fit() {
        // `/x/tiny.png` is one byte and each `/x/<n>.png` is `n` MB; like
        // `read_media`, the reader refuses a file over `limit` unread.
        let read = |path: &str, limit: u64| {
            let size = match &path[3..path.len() - 4] {
                "tiny" => 1,
                mb => mb.parse::<u64>().unwrap_or(0) * 1024 * 1024,
            };
            Ok(if size > limit {
                Media::TooLarge { size, exact: true }
            } else {
                Media::Bytes(vec![b'x'; size as usize])
            })
        };
        let c = card(
            r#"<img src="/api/cards/c1/images/0"><img src="/api/cards/c1/images/1"><img src="/api/cards/c1/images/2"><img src="/api/cards/c1/images/3"><img src="/api/cards/c1/images/4">"#,
            &[
                "/x/tiny.png",
                "/x/40.png",
                "/x/20.png",
                "/x/20.png",
                "/x/11.png",
            ],
            &[],
        );
        let r = export_card(&c, None, read, offline);
        let reasons: Vec<_> = r
            .warnings
            .iter()
            .map(|w| (w.kind, w.target.as_str(), w.reason.as_str()))
            .collect();
        assert_eq!(
            reasons,
            [
                (
                    ExportWarningKind::MediaTooLarge,
                    "/x/40.png",
                    "a 40.0 MB file is over the 32 MB of images and videos one export inlines"
                ),
                (
                    ExportWarningKind::MediaTooLarge,
                    "/x/20.png",
                    "a 20.0 MB file would take the page past the 32 MB of images and videos one export inlines"
                ),
            ]
        );
        assert_eq!(r.html.matches("image left out: ").count(), 2);
        assert!(r.html.contains("image left out: 40.png"));
        // One byte, 20 MB, then 11 MB: under the cap.
        assert_eq!(
            r.html.matches("<img src=\"data:image/png;base64,").count(),
            3
        );
    }

    #[test]
    fn media_whose_size_was_found_by_reading_names_a_lower_bound() {
        // The reader saw more than `limit` bytes it had no size for.
        let read = |_: &str, limit: u64| {
            Ok(Media::TooLarge {
                size: limit,
                exact: false,
            })
        };
        let c = card(
            r#"<img src="/api/cards/c1/images/0">"#,
            &["/dev/a.png"],
            &[],
        );
        let r = export_card(&c, None, read, offline);
        assert_eq!(
            r.warnings[0].reason,
            "a file of more than 32.0 MB is over the 32 MB of images and videos one export inlines"
        );
        assert_eq!(megabytes(20 * 1024 * 1024 + 50 * 1024, false), "20.0");
        assert_eq!(megabytes(20 * 1024 * 1024 + 50 * 1024, true), "20.1");
        assert_eq!(megabytes(u64::MAX, true), "17592186044416.0");
    }

    #[test]
    fn read_media_refuses_a_file_over_the_limit_from_its_size() {
        let dir = std::env::temp_dir().join(format!("canvasd-read-media-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let big = dir.join("big.mov");
        // Sparse: 40 MB the file system reports and nothing written.
        std::fs::File::create(&big)
            .unwrap()
            .set_len(40 * 1024 * 1024)
            .unwrap();
        let small = dir.join("small.png");
        std::fs::write(&small, b"abc").unwrap();
        let big = read_media(&big.to_string_lossy(), 1024).unwrap();
        let small = read_media(&small.to_string_lossy(), 3).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(matches!(
            big,
            Media::TooLarge { size, exact: true } if size == 40 * 1024 * 1024
        ));
        assert!(matches!(small, Media::Bytes(b) if b == b"abc"));
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
    fn unquoted_script_src_and_stylesheet_link_are_inlined() {
        let c = card(
            "<script src=https://unpkg.com/lib.js defer></script><script async src=https://unpkg.com/lib.js></script><link rel=stylesheet href=https://cdnjs.cloudflare.com/x/css/b.css media=print><link media=screen href=//cdnjs.cloudflare.com/x/css/b.css rel=stylesheet>",
            &[],
            &[],
        );
        let r = export_card(&c, None, files, cdn_files);
        let script = r"var s='<\/script>',t='<!--\x3CSCRIPT>';</script>";
        assert!(
            r.html
                .contains(&format!("<script defer>{script}<script async>{script}")),
            "{}",
            r.html
        );
        assert!(
            r.html
                .contains(r#"<style media="print">.b{}</style><style media="screen">.b{}</style>"#),
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
                r#"<svg><style>.m&gt;b{content:"&lt;b&gt;&amp;amp;"}.c{content:"©";src:url("data:font/woff2;base64,YWI=")}</style></svg>"#,
            ),
            (
                // Quotes and parens written as references delimit the CSS.
                r#"<svg><style>@import &quot;https://cdnjs.cloudflare.com/x/css/b.css&quot;;.f{src:url&#40;&quot;https://cdnjs.cloudflare.com/x/font/f.woff2?v=1&quot;&rpar;}.s{content:"&#x5C;&quot;url(https://cdnjs.cloudflare.com/x/css/b.css)"}</style></svg>"#,
                r#"<svg><style>.b{}.f{src:url("data:font/woff2;base64,YWI=")}.s{content:"\"url(https://cdnjs.cloudflare.com/x/css/b.css)"}</style></svg>"#,
            ),
            (
                // Nothing to inline: the text stays as written.
                r#"<svg><style>.c{content:"&#169;&lt;"}</style></svg>"#,
                r#"<svg><style>.c{content:"&#169;&lt;"}</style></svg>"#,
            ),
            (
                r#"<svg><style><![CDATA[@import "https://cdnjs.cloudflare.com/x/css/m.css?a=1&b=2";.c{content:"&#169;"}]]></style></svg>"#,
                r#"<svg><style><![CDATA[.m>b{content:"<b>&amp;"}.c{content:"&#169;"}]]></style></svg>"#,
            ),
            (
                r#"<svg><style>@import "https://cdnjs.cloudflare.com/x/css/m.css?a=1&#38;b=2";@import "https://cdnjs.cloudflare.com/x/css/m.css?a=1&#x26;b=2";@import "https://cdnjs.cloudflare.com/x/css/m.css?a=1&ampb=2";.f{src:url(https://cdnjs.cloudflare.com/x/font/f.woff2&#63;v&#x3D;1)}</style></svg>"#,
                r#"<svg><style>.m&gt;b{content:"&lt;b&gt;&amp;amp;"}.m&gt;b{content:"&lt;b&gt;&amp;amp;"}.m&gt;b{content:"&lt;b&gt;&amp;amp;"}.f{src:url("data:font/woff2;base64,YWI=")}</style></svg>"#,
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
    fn svg_style_css_split_by_comments_and_cdata_is_read_whole() {
        // Each pair: the card, and what export writes. The browser joins a
        // style's text runs and CDATA sections and drops its comments, so a
        // reference split across them is still one reference; the pieces
        // before and after the change stay as written.
        for (html, want) in [
            (
                r#"<svg><style>/*k*/<!--a-->@import "https://cdnjs.cloudflare.com/x/<!--c-->css/b.css";.z{}<!--d-->.y{}</style></svg>"#,
                r#"<svg><style>/*k*/<!--a-->.b{}.z{}<!--d-->.y{}</style></svg>"#,
            ),
            (
                r#"<svg><style><![CDATA[@import "https://cdnjs.cloudflare.com/x/css/]]>m.css?a=1&amp;b=2";.c{}</style></svg>"#,
                r#"<svg><style><![CDATA[.m>b{content:"<b>&amp;"}.c{}]]></style></svg>"#,
            ),
            (
                r#"<svg><style>@import "https://cdnjs.cloudflare.com/x/css/<![CDATA[end.css";]]></style></svg>"#,
                r#"<svg><style>.e::after{content:"]]&gt;"}</style></svg>"#,
            ),
            (
                // Multi-byte text on both sides of the change.
                r#"<svg><style>/*é*/<!--a-->@import "https://cdnjs.cloudflare.com/x/<!--c-->css/b.css";.é{}</style></svg>"#,
                r#"<svg><style>/*é*/<!--a-->.b{}.é{}</style></svg>"#,
            ),
            (
                // A bogus comment and an ignored doctype split it too.
                r#"<math><style>.f{src:url(https://cdnjs.cloudflare.com/x/font/<?p?>f.woff2</ x>?v=1<!doctype x>)}</style></math>"#,
                r#"<math><style>.f{src:url("data:font/woff2;base64,YWI=")}</style></math>"#,
            ),
            (
                // Nothing to inline: every piece stays as written.
                r#"<svg><style>.c{content:"&#169;"}<!--x--><![CDATA[.d{}]]><?p></style></svg>"#,
                r#"<svg><style>.c{content:"&#169;"}<!--x--><![CDATA[.d{}]]><?p></style></svg>"#,
            ),
        ] {
            let r = export_card(&card(html, &[], &[]), None, files, cdn_files);
            assert!(r.html.contains(want), "{html}\n{}", r.html);
            assert!(r.warnings.is_empty(), "{html}: {:?}", r.warnings);
        }
    }

    #[test]
    fn an_svg_style_whose_import_stays_its_own_link_stays_as_written() {
        // The failed @import turns back into the link it was written as, an
        // edit equal to its text that css_edits drops, so markup_css finds
        // nothing to change and every piece stays as written.
        for html in [
            r#"<svg><style>@import url("https://unpkg.com/gone.css");.c{content:"&#169;&lt;"}</style></svg>"#,
            r#"<svg><style><![CDATA[@import url("https://unpkg.com/]]>gone.css");.c{}</style></svg>"#,
        ] {
            let r = export_card(&card(html, &[], &[]), None, files, cdn_files);
            assert!(r.html.contains(html), "{html}\n{}", r.html);
            let got: Vec<_> = r
                .warnings
                .iter()
                .map(|w| (w.kind, w.target.as_str()))
                .collect();
            assert_eq!(
                got,
                [(ExportWarningKind::FetchFailed, "https://unpkg.com/gone.css")],
                "{html}"
            );
        }
    }

    #[test]
    fn svg_style_css_inside_child_elements_is_read() {
        // Each pair: the card, and what export writes. WebKit reads an SVG
        // `<style>`'s CSS from all the text inside it, child elements'
        // included, until the tag that closes it; the children's tags stay.
        for (html, want) in [
            (
                r#"<svg><style><x/>@import "https://cdnjs.cloudflare.com/x/css/b.css";<y><z/></y>.c{}</style></svg>"#,
                r#"<svg><style><x/>.b{}<y><z/></y>.c{}</style></svg>"#,
            ),
            (
                r#"<svg><style>@import "https://cdnjs.cloudflare.com/x/<g>css/</g>b.css";.c{}</style></svg>"#,
                r#"<svg><style>.b{}.c{}<g></g></style></svg>"#,
            ),
            (
                // The child `</g>` closes the child, not the outer `<g>`.
                r#"<svg><g><style><g>@import "https://cdnjs.cloudflare.com/x/css/b.css";</g>.c{}</style></g></svg>"#,
                r#"<svg><g><style><g>.b{}</g>.c{}</style></g></svg>"#,
            ),
            (
                // Raw text at an integration point, decoded in a `<title>`.
                r#"<svg><style><desc><title>@import "https://cdnjs.cloudflare.com/x/css/m.css?a=1&amp;b=2";</title></desc></style></svg>"#,
                r#"<svg><style><desc><title>.m&gt;b{content:"&lt;b&gt;&amp;amp;"}</title></desc></style></svg>"#,
            ),
            (
                r#"<svg><style><foreignObject><script>@import "https://cdnjs.cloudflare.com/x/css/end.css";</script></foreignObject></style></svg>"#,
                r#"<svg><style><foreignObject><script>.e::after{content:"]]>"}</script></foreignObject></style></svg>"#,
            ),
            (
                // At an integration point `<![CDATA[` is a bogus comment.
                r#"<svg><style><desc><![CDATA[@import "https://cdnjs.cloudflare.com/x/css/b.css";]]></desc>.c{}</style></svg>"#,
                r#"<svg><style><desc><![CDATA[@import "https://cdnjs.cloudflare.com/x/css/b.css";]]></desc>.c{}</style></svg>"#,
            ),
            (
                // A breakout tag closes the style; its text is the page's.
                r#"<svg><style>.c{}<p>@import "https://cdnjs.cloudflare.com/x/css/b.css";</p></svg>"#,
                r#"<svg><style>.c{}<p>@import "https://cdnjs.cloudflare.com/x/css/b.css";</p></svg>"#,
            ),
        ] {
            let r = export_card(&card(html, &[], &[]), None, files, cdn_files);
            assert!(r.html.contains(want), "{html}\n{}", r.html);
            assert!(r.warnings.is_empty(), "{html}: {:?}", r.warnings);
        }
    }

    #[test]
    fn a_style_a_select_drops_reads_no_css() {
        // A select drops a `<style>` start tag, so what follows is the
        // select's text, never CSS (WebKit builds no style element).
        let html = r#"<select><style></style><option>.f{src:url(https://cdnjs.cloudflare.com/x/font/f.woff2?v=1)}</option></select>"#;
        let r = export_card(&card(html, &[], &[]), None, files, cdn_files);
        assert!(r.html.contains(html), "{}", r.html);
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    }

    #[test]
    fn svg_and_math_scripts_and_links_stay_as_written() {
        // The browser fetches neither an SVG `<script src>` nor an SVG or
        // MathML `<link>`, so inlining either would add code or CSS the card
        // never ran. Inside `<foreignObject>` they are HTML again.
        let kept = r#"<svg><script src="https://unpkg.com/lib.js"></script><link rel="stylesheet" href="https://cdnjs.cloudflare.com/x/css/m.css?a=1&amp;b=2"/></svg><math><link rel="stylesheet" href="https://cdnjs.cloudflare.com/x/css/b.css"></math>"#;
        let r = export_card(&card(kept, &[], &[]), None, files, cdn_files);
        assert!(r.html.contains(kept), "{}", r.html);
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);

        let html = r#"<svg><foreignObject><link rel="stylesheet" href="https://cdnjs.cloudflare.com/x/css/b.css"></foreignObject></svg>"#;
        let r = export_card(&card(html, &[], &[]), None, files, cdn_files);
        assert!(
            r.html
                .contains("<svg><foreignObject><style>.b{}</style></foreignObject></svg>"),
            "{}",
            r.html
        );
    }

    #[test]
    fn files_html_loads_inside_an_svg_style_become_data_uris_in_place() {
        // Inside an SVG `<style>`'s `<foreignObject>` an HTML script runs and
        // a stylesheet link applies, while the style reads the text inside
        // them as its own CSS: only the attributes change.
        let html = format!(
            r##"<svg><style><foreignObject><script src="https://unpkg.com/lib.js">.c{{}}</script><script type="module" src="{LIT}"></script><link rel="stylesheet" href="https://cdnjs.cloudflare.com/x/css/b.css" media="print"><img src="/api/cards/c1/images/0"><img src="/api/cards/c1/images/1"><a href="#canvas-open-0">w</a><a href="#canvas-open-1">l</a></foreignObject>.d{{}}</style></svg>"##
        );
        let c = card(
            &html,
            &["/x/a.png", "/x/gone.png"],
            &["https://e.com/", "/x/f.md"],
        );
        let fetch = |url: &str, t: Duration| match url {
            LIT => Ok(b"export const a=1;".to_vec()),
            _ => cdn_files(url, t),
        };
        let r = export_card(&c, None, files, fetch);
        let js = |s: &[u8]| format!("data:text/javascript;charset=utf-8;base64,{}", base64(s));
        let want = format!(
            r#"<svg><style><foreignObject><script src="{}">.c{{}}</script><script type="module" src="{}"></script><link rel="stylesheet" href="data:text/css;charset=utf-8;base64,{}" media="print"><img src="data:image/png;base64,{}"><img><a href="https://e.com/" target="_blank" rel="noopener">w</a>l</foreignObject>.d{{}}</style></svg>"#,
            js(b"var s='</script>',t='<!--<SCRIPT>';"),
            js(format!("import \"canvas-export:{LIT}\";").as_bytes()),
            base64(b".b{}"),
            base64(b"abc"),
        );
        assert!(r.html.contains(&want), "{}", r.html);
        let map = page_map(&r.html);
        assert_eq!(map["imports"][LIT], js_data("export const a=1;"));
        assert_eq!(
            map["imports"][format!("canvas-export:{LIT}")],
            js_data("export const a=1;")
        );
        assert_eq!(r.warnings.len(), 1, "{:?}", r.warnings);
        assert_eq!(r.warnings[0].kind, ExportWarningKind::MissingImage);
    }

    #[test]
    fn links_an_svg_style_child_cant_inline_stay_and_integrity_goes() {
        // Off the CDN hosts, not a stylesheet, or failed: as written.
        let kept = r#"<svg><style><foreignObject><link rel="stylesheet" href="https://example.com/x.css"><link rel="icon" href="/f.ico"><link rel="stylesheet" href="https://unpkg.com/gone.css"></foreignObject>.a{}</style></svg>"#;
        let r = export_card(&card(kept, &[], &[]), None, files, cdn_files);
        assert!(r.html.contains(kept), "{}", r.html);
        assert_eq!(r.warnings.len(), 1, "{:?}", r.warnings);

        // A hash of the file can't match what the data: URI holds.
        let html = r#"<svg><style><foreignObject><link rel="stylesheet" integrity="sha384-X" href="https://cdnjs.cloudflare.com/x/css/b.css"></foreignObject></style></svg>"#;
        let r = export_card(&card(html, &[], &[]), None, files, cdn_files);
        let want = format!(
            r#"<link rel="stylesheet" href="data:text/css;charset=utf-8;base64,{}">"#,
            base64(b".b{}")
        );
        assert!(r.html.contains(&want), "{}", r.html);
    }

    #[test]
    fn a_data_uri_inside_an_svg_style_counts_its_base64_toward_the_total() {
        // 14 scripts of 0.9 of the asset cap fit the 16-cap total as bytes
        // (12.6) but not as base64 (16.8): the last stays a link.
        let size = MAX_ASSET_BYTES / 10 * 9;
        let scripts: String = (0..14)
            .map(|n| format!(r#"<script src="https://unpkg.com/{n}.js"></script>"#))
            .collect();
        let html = format!("<svg><style><foreignObject>{scripts}</foreignObject></style></svg>");
        let fetch = |_: &str, _: Duration| Ok(vec![b'x'; size]);
        let r = export_card(&card(&html, &[], &[]), None, files, fetch);
        assert_eq!(r.html.matches("src=\"data:").count(), 13);
        assert_eq!(r.warnings.len(), 1, "{:?}", r.warnings);
        assert_eq!(r.warnings[0].target, "https://unpkg.com/13.js");
        assert_eq!(r.warnings[0].reason, over_total_reason());
    }

    #[test]
    fn a_stylesheet_inside_an_svg_style_counts_its_base64_toward_the_total() {
        // 14 sheets of 0.9 of the asset cap fit the 16-cap total as bytes
        // (12.6) but not as base64 (16.8): the last stays a link.
        let size = MAX_ASSET_BYTES / 10 * 9;
        let links: String = (0..14)
            .map(|n| format!(r#"<link rel="stylesheet" href="https://unpkg.com/{n}.css">"#))
            .collect();
        let html = format!("<svg><style><foreignObject>{links}</foreignObject></style></svg>");
        let fetch = |_: &str, _: Duration| Ok(vec![b'x'; size]);
        let r = export_card(&card(&html, &[], &[]), None, files, fetch);
        assert_eq!(r.html.matches("href=\"data:").count(), 13);
        assert!(r.html.contains(
            r#"<link rel="stylesheet" href="https://unpkg.com/13.css"></foreignObject>"#
        ));
        assert_eq!(r.warnings.len(), 1, "{:?}", r.warnings);
        assert_eq!(r.warnings[0].target, "https://unpkg.com/13.css");
        assert_eq!(r.warnings[0].reason, over_total_reason());
    }

    #[test]
    fn a_dropped_anchor_inside_an_svg_style_keeps_its_css_edits_in_place() {
        // The local-path anchor's start and end tags go; the @import after
        // them still becomes its sheet's rules.
        let html = r##"<svg><style><foreignObject><a href="#canvas-open-0">/**/</a></foreignObject>@import "https://cdnjs.cloudflare.com/x/css/b.css";.d{}</style></svg>"##;
        let r = export_card(&card(html, &[], &["/x/f.md"]), None, files, cdn_files);
        assert!(
            r.html
                .contains("<svg><style><foreignObject>/**/</foreignObject>.b{}.d{}</style></svg>"),
            "{}",
            r.html
        );
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    }

    #[test]
    fn a_script_inside_a_mathml_style_becomes_a_data_uri_in_place() {
        // A MathML `<style>` reads the text inside its children as CSS too,
        // and inside an `<mi>` an HTML element is HTML again.
        let html = r#"<math><style><mi><script src="https://unpkg.com/lib.js">.c{}</script></mi>.d{}</style></math>"#;
        let r = export_card(&card(html, &[], &[]), None, files, cdn_files);
        let want = format!(
            r#"<math><style><mi><script src="data:text/javascript;charset=utf-8;base64,{}">.c{{}}</script></mi>.d{{}}</style></math>"#,
            base64(b"var s='</script>',t='<!--<SCRIPT>';"),
        );
        assert!(r.html.contains(&want), "{}", r.html);
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
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

    /// The page's import map, parsed.
    fn page_map(html: &str) -> serde_json::Value {
        let start = html
            .find("<script type=\"importmap\">")
            .expect("an import map")
            + 25;
        let end = start + html[start..].find("</script>").unwrap();
        serde_json::from_str(&html[start..end]).unwrap()
    }

    fn js_data(source: &str) -> serde_json::Value {
        format!("data:text/javascript;base64,{}", base64(source.as_bytes())).into()
    }

    const LIT: &str = "https://cdn.jsdelivr.net/npm/lit@3/+esm";

    #[test]
    fn a_module_and_every_module_it_reaches_go_in_the_import_map() {
        let c = card(
            &format!(r#"<script type="module" async src="{LIT}"></script><p>x</p>"#),
            &[],
            &[],
        );
        let fetch = |url: &str, _: Duration| {
            match url {
            LIT => Ok(br#"import"/npm/a@1/+esm";export*from"./b.js";import{c}from"lit";import"https://example.com/x.js";const r=/import"\/npm\/x"/;"#.to_vec()),
            "https://cdn.jsdelivr.net/npm/a@1/+esm" => {
                Ok(br#"export default 1;import("/npm/lazy/+esm");import"/npm/lit@3/+esm";"#.to_vec())
            }
            "https://cdn.jsdelivr.net/npm/lit@3/b.js" => Ok(b"export const b=2;".to_vec()),
            "https://cdn.jsdelivr.net/npm/lazy/+esm" => Ok(b"export const z=3;".to_vec()),
            _ => Err("HTTP 404".to_string()),
        }
        };
        let r = export_card(&c, None, files, fetch);
        assert_eq!(r.warnings, []);
        assert!(
            r.html.contains(&format!(
                r#"<script type="module" async>import "{LIT}";</script><p>x</p>"#
            )),
            "{}",
            r.html
        );
        assert_eq!(
            page_map(&r.html),
            serde_json::json!({"imports": {
                LIT: js_data(r#"import"https://cdn.jsdelivr.net/npm/a@1/+esm";export*from"https://cdn.jsdelivr.net/npm/lit@3/b.js";import{c}from"canvas-export:blocked";import"canvas-export:blocked";const r=/import"\/npm\/x"/;"#),
                "canvas-export:blocked": null,
                "https://cdn.jsdelivr.net/npm/a@1/+esm": js_data(r#"export default 1;import("https://cdn.jsdelivr.net/npm/lazy/+esm");import"https://cdn.jsdelivr.net/npm/lit@3/+esm";"#),
                "https://cdn.jsdelivr.net/npm/lit@3/b.js": js_data("export const b=2;"),
                "https://cdn.jsdelivr.net/npm/lazy/+esm": js_data("export const z=3;"),
            }})
        );
        // Ahead of every script in the page.
        assert!(r.html.find("importmap").unwrap() < r.html.find("<script>").unwrap());
    }

    #[test]
    fn a_module_reached_that_cant_be_fetched_or_read_stays_on_the_network() {
        let c = card(
            &format!(r#"<script type="module" src="{LIT}"></script>"#),
            &[],
            &[],
        );
        let fetch = |url: &str, _: Duration| match url {
            LIT => Ok(br#"import"/npm/gone/+esm";import"/npm/bad/+esm";"#.to_vec()),
            "https://cdn.jsdelivr.net/npm/bad/+esm" => Ok(b"import { from".to_vec()),
            _ => Err("HTTP 404".to_string()),
        };
        let r = export_card(&c, None, files, fetch);
        let targets: Vec<_> = r.warnings.iter().map(|w| w.target.as_str()).collect();
        assert_eq!(
            targets,
            [
                "https://cdn.jsdelivr.net/npm/gone/+esm",
                "https://cdn.jsdelivr.net/npm/bad/+esm"
            ]
        );
        assert!(r.warnings[1]
            .reason
            .starts_with("not a module the export can read: "));
        assert_eq!(
            page_map(&r.html),
            serde_json::json!({"imports": {
                LIT: js_data(r#"import"https://cdn.jsdelivr.net/npm/gone/+esm";import"https://cdn.jsdelivr.net/npm/bad/+esm";"#),
            }})
        );
    }

    #[test]
    fn a_module_that_cant_be_read_stays_a_link() {
        let tag = format!(r#"<script type="module" src="{LIT}"></script>"#);
        let c = card(&tag, &[], &[]);
        let fetch = |_: &str, _: Duration| Ok(b"export {".to_vec());
        let r = export_card(&c, None, files, fetch);
        assert!(r.html.contains(&tag));
        assert!(!r.html.contains("importmap"));
        assert_eq!(r.warnings.len(), 1);
        assert_eq!(r.warnings[0].target, LIT);
    }

    #[test]
    fn a_module_that_computes_an_import_is_inlined_with_one_warning() {
        let c = card(
            &format!(r#"<script type="module" src="{LIT}"></script>"#),
            &[],
            &[],
        );
        let fetch = |url: &str, _: Duration| match url {
            LIT => Ok("import\"/npm/a/+esm\";\nexport const \u{e9}=n=>import(n);"
                .as_bytes()
                .to_vec()),
            _ => Ok(b"new URL('./w.wasm',import.meta.url);import(x);".to_vec()),
        };
        let r = export_card(&c, None, files, fetch);
        let warnings: Vec<_> = r
            .warnings
            .iter()
            .map(|w| (w.kind, w.target.as_str(), w.reason.as_str()))
            .collect();
        let reason = "computes an import from its own address, which the export makes a \
                      data: URI, so it fails when the page runs: ";
        assert_eq!(
            warnings,
            [
                (
                    ExportWarningKind::ComputedImport,
                    LIT,
                    &*format!("{reason}n at line 2, column 26")
                ),
                (
                    ExportWarningKind::ComputedImport,
                    "https://cdn.jsdelivr.net/npm/a/+esm",
                    &*format!("{reason}import.meta.url at line 1, column 20 (and 1 more)")
                ),
            ]
        );
        let map = page_map(&r.html);
        assert_eq!(map["imports"].as_object().unwrap().len(), 2);
    }

    #[test]
    fn the_cards_import_maps_merge_into_the_pages_one() {
        let c = card(
            &format!(
                r#"<script type="importmap">{{"imports":{{"lit":"{LIT}","https://cdn.jsdelivr.net/npm/b/+esm":"./mine.js"}},"scopes":{{"/s/":{{"x":"./x.js"}}}}}}</script><script type="importmap">{{"imports":{{"lit":"./other.js","y":"./y.js"}}}}</script><script type="importmap">not json</script><script type="module" src="{LIT}"></script>"#
            ),
            &[],
            &[],
        );
        let fetch = |url: &str, _: Duration| match url {
            LIT => Ok(br#"import"/npm/b/+esm";"#.to_vec()),
            "https://cdn.jsdelivr.net/npm/b/+esm" => Ok(b"export{}".to_vec()),
            _ => Err("HTTP 404".to_string()),
        };
        let r = export_card(&c, None, files, fetch);
        let lit = js_data(r#"import"canvas-export:blocked";"#);
        // The card sends b to its own file, which its CSP refuses, so b isn't
        // fetched and lit's import of it loads nothing; its "lit" names the
        // inlined module.
        assert_eq!(
            page_map(&r.html),
            serde_json::json!({
                "imports": {
                    "lit": lit,
                    "https://cdn.jsdelivr.net/npm/b/+esm": "./mine.js",
                    "y": "./y.js",
                    LIT: lit,
                    "canvas-export:blocked": null,
                },
                "scopes": {"/s/": {"x": "./x.js"}},
            })
        );
        assert_eq!(r.warnings, []);
        assert_eq!(r.html.matches("<script type=\"importmap\">").count(), 2);
        // Both taken whole, end tags included; the one that isn't JSON stays.
        assert!(
            r.html.contains(
                r#"<div class="canvas-export"><script type="importmap">not json</script><script type="module">"#
            ),
            "{}",
            r.html
        );
    }

    #[test]
    fn modules_the_card_reaches_through_its_import_map_go_in_the_page() {
        const PREFIX: &str = "https://cdn.jsdelivr.net/npm/lit@3/";
        let module = "import {html} from 'lit';import 'lit/decorators.js';import 'https://unpkg.com/direct.js';import './local.js';import 'react';";
        let c = card(
            &format!(
                r#"<script type="importmap">{{"imports":{{"lit":"{LIT}","lit/":"{PREFIX}","dep":"https://unpkg.com/dep@1/x.js"}},"scopes":{{"{PREFIX}":{{"dep":"https://unpkg.com/dep@2/x.js"}}}}}}</script><script type="module">{module}</script>"#
            ),
            &[],
            &[],
        );
        let fetch = |url: &str, _: Duration| match url {
            LIT => Ok(br#"import"dep";"#.to_vec()),
            "https://cdn.jsdelivr.net/npm/lit@3/decorators.js" => Ok(b"export const d=1;".to_vec()),
            "https://unpkg.com/direct.js" => Ok(b"export{}".to_vec()),
            "https://unpkg.com/dep@1/x.js" => Ok(b"export const one=1;".to_vec()),
            "https://unpkg.com/dep@2/x.js" => Ok(b"export const two=2;".to_vec()),
            _ => Err("HTTP 404".to_string()),
        };
        let r = export_card(&c, None, files, fetch);
        assert_eq!(r.warnings, []);
        // The card's module is kept as written.
        assert!(
            r.html
                .contains(&format!(r#"<script type="module">{module}</script>"#)),
            "{}",
            r.html
        );
        // lit sits in the scope, so its "dep" is written as dep@2's URL.
        let lit = js_data(r#"import"https://unpkg.com/dep@2/x.js";"#);
        let decorators = js_data("export const d=1;");
        let direct = js_data("export{}");
        let one = js_data("export const one=1;");
        let two = js_data("export const two=2;");
        assert_eq!(
            page_map(&r.html),
            serde_json::json!({
                "imports": {
                    "lit": lit,
                    "lit/": PREFIX,
                    "dep": one,
                    "lit/decorators.js": decorators,
                    LIT: lit,
                    "https://cdn.jsdelivr.net/npm/lit@3/decorators.js": decorators,
                    "https://unpkg.com/direct.js": direct,
                    "https://unpkg.com/dep@2/x.js": two,
                    "https://unpkg.com/dep@1/x.js": one,
                },
                "scopes": {PREFIX: {"dep": two}},
            })
        );
    }

    #[test]
    fn an_import_the_page_map_would_send_elsewhere_is_written_as_a_name() {
        // From m.js the card sends "a" to x.js in one step, though x.js is
        // itself a key; "dep" is blocked in m.js's scope though the imports
        // map it; "g" reaches gone.js, which the imports send elsewhere.
        let c = card(
            r#"<script type="importmap">{"imports":{"a":"https://unpkg.com/x.js","https://unpkg.com/x.js":"https://unpkg.com/y.js","dep":"https://unpkg.com/y.js","https://unpkg.com/gone.js":"https://unpkg.com/y.js"},"scopes":{"https://unpkg.com/m.js":{"dep":null,"g":"https://unpkg.com/gone.js"}}}</script><script type="module" src="https://unpkg.com/m.js"></script><script type="module" src="https://unpkg.com/x.js"></script>"#,
            &[],
            &[],
        );
        let fetch = |url: &str, _: Duration| match url {
            "https://unpkg.com/m.js" => {
                Ok(br#"import"a";import"dep";import"g";import"https://unpkg.com/y.js";"#.to_vec())
            }
            "https://unpkg.com/x.js" => Ok(b"export const x=1;".to_vec()),
            "https://unpkg.com/y.js" => Ok(b"export const y=1;".to_vec()),
            _ => Err("HTTP 404".to_string()),
        };
        let r = export_card(&c, None, files, fetch);
        let targets: Vec<_> = r.warnings.iter().map(|w| w.target.as_str()).collect();
        assert_eq!(targets, ["https://unpkg.com/gone.js"]);
        // The element sends x.js to the network, not through the map.
        assert!(
            r.html.contains(
                r#"<script type="module">import "https://unpkg.com/m.js";</script><script type="module">import "canvas-export:https://unpkg.com/x.js";</script>"#
            ),
            "{}",
            r.html
        );
        let x = js_data("export const x=1;");
        let y = js_data("export const y=1;");
        assert_eq!(
            page_map(&r.html),
            serde_json::json!({
                "imports": {
                    "a": x,
                    "https://unpkg.com/x.js": y,
                    "dep": y,
                    "https://unpkg.com/gone.js": y,
                    "https://unpkg.com/m.js": js_data(r#"import"canvas-export:https://unpkg.com/x.js";import"canvas-export:blocked";import"canvas-export:https://unpkg.com/gone.js";import"https://unpkg.com/y.js";"#),
                    "https://unpkg.com/y.js": y,
                    "canvas-export:https://unpkg.com/x.js": x,
                    "canvas-export:https://unpkg.com/gone.js": "https://unpkg.com/gone.js",
                    "canvas-export:blocked": null,
                },
                "scopes": {"https://unpkg.com/m.js": {"dep": null, "g": "https://unpkg.com/gone.js"}},
            })
        );
    }

    #[test]
    fn a_name_for_a_url_ending_in_a_slash_is_no_prefix() {
        // The card's prefix key blocks p/ from the page; m.js reaches it by "a".
        let c = card(
            r#"<script type="importmap">{"imports":{"a":"https://unpkg.com/p/","https://unpkg.com/p/":"https://unpkg.com/y.js"}}</script><script type="module" src="https://unpkg.com/m.js"></script>"#,
            &[],
            &[],
        );
        let fetch = |url: &str, _: Duration| match url {
            "https://unpkg.com/m.js" => Ok(br#"import"a";"#.to_vec()),
            "https://unpkg.com/p/" => Ok(b"export const p=1;".to_vec()),
            _ => Err("HTTP 404".to_string()),
        };
        let r = export_card(&c, None, files, fetch);
        let map = page_map(&r.html);
        assert_eq!(
            map["imports"]["https://unpkg.com/m.js"],
            js_data(r#"import"canvas-export:https://unpkg.com/p/#";"#)
        );
        assert_eq!(
            map["imports"]["canvas-export:https://unpkg.com/p/#"],
            js_data("export const p=1;")
        );
    }

    #[test]
    fn a_card_module_that_doesnt_parse_reaches_nothing() {
        let c = card(
            r#"<script type="importmap">{"imports":{"lit/":"https://cdn.jsdelivr.net/npm/lit@3/"}}</script><script type="module">import 'lit/a.js'; export {</script>"#,
            &[],
            &[],
        );
        let fetch =
            |_: &str, _: Duration| -> Result<Vec<u8>, String> { panic!("nothing to fetch") };
        let r = export_card(&c, None, files, fetch);
        assert_eq!(r.warnings, []);
        assert_eq!(
            page_map(&r.html),
            serde_json::json!({"imports": {"lit/": "https://cdn.jsdelivr.net/npm/lit@3/"}})
        );
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
    fn sheets_inlined_before_a_link_stay_links() {
        let c = card(
            r#"<link rel="stylesheet" href="https://unpkg.com/a.css"><link rel="stylesheet" href="https://unpkg.com/n.css"><link rel="stylesheet" href="https://unpkg.com/k.css"><link rel="stylesheet" href="https://unpkg.com/m.css"><link rel="stylesheet" href="https://unpkg.com/o.css">"#,
            &[],
            &[],
        );
        let fetch = |url: &str, _: Duration| {
            let body = match url.strip_prefix("https://unpkg.com/") {
                Some("a.css") => "@import \"e.css\";@import \"https://example.com/z.css\";",
                Some("n.css") => "@import \"y.css\";@import \"l.css\";@import \"f.css\";",
                Some("k.css") => "@import \"l.css\";@import \"https://example.com/z.css\";.k{}",
                // The sheet's own rule already leaves the link ignored.
                Some("m.css") => "@import \"g.css\";.m{}@import \"https://example.com/z.css\";",
                Some("o.css") => "@import \"s.css\";@import \"https://example.com/z.css\";",
                Some("e.css") => ".e{}",
                Some("g.css") => ".g{}",
                Some("y.css") => ".y{}",
                Some("f.css") => "@import \"https://example.com/c.css\";.f{}",
                Some("l.css") => {
                    "/*c*/<!-- @charset \"utf-8\";@layer x, y;@media screen;@import foo;-->"
                }
                Some("s.css") => "@namespace svg url(http://www.w3.org/2000/svg);",
                _ => return Err("HTTP 404".to_string()),
            };
            Ok(body.as_bytes().to_vec())
        };
        let r = export_card(&c, None, files, fetch);
        let l = r#"/*c*/<!-- @charset "utf-8";@layer x, y;@media screen;@import foo;-->"#;
        for want in [
            r#"<style>@import url("https://unpkg.com/e.css");@import "https://example.com/z.css";</style>"#.to_string(),
            format!(r#"<style>@import url("https://unpkg.com/y.css");{l}@import "https://example.com/c.css";.f{{}}</style>"#),
            format!(r#"<style>{l}@import "https://example.com/z.css";.k{{}}</style>"#),
            r#"<style>.g{}.m{}@import "https://example.com/z.css";</style>"#.to_string(),
            r#"<style>@import url("https://unpkg.com/s.css");@import "https://example.com/z.css";</style>"#.to_string(),
        ] {
            assert!(r.html.contains(&want), "{want}\n{}", r.html);
        }
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
                    "https://unpkg.com/e.css",
                    "kept as a link: the later @import of https://example.com/z.css would be ignored after its rules"
                ),
                (
                    ExportWarningKind::FetchFailed,
                    "https://unpkg.com/y.css",
                    "kept as a link: the later @import of https://example.com/c.css would be ignored after its rules"
                ),
                (
                    ExportWarningKind::FetchFailed,
                    "https://unpkg.com/s.css",
                    "kept as a link: the later @import of https://example.com/z.css would be ignored after its rules"
                ),
            ]
        );
    }

    #[test]
    fn an_import_after_the_sheets_own_rules_stays_as_written_unfetched() {
        let c = card(
            r#"<link rel="stylesheet" href="https://unpkg.com/a.css"><link rel="stylesheet" href="https://unpkg.com/m.css">"#,
            &[],
            &[],
        );
        let fetched = std::cell::RefCell::new(Vec::new());
        let fetch = |url: &str, _: Duration| {
            fetched.borrow_mut().push(url.to_string());
            let body = match url.strip_prefix("https://unpkg.com/") {
                Some("a.css") => "@import \"e.css\";.a{}@import \"b.css\"; @import \"c.css\";",
                Some("m.css") => "@import \"k.css\" screen;",
                Some("k.css") => ".k{}@import \"b.css\";",
                Some("e.css") => ".e{}",
                Some("b.css") => ".b{color:red}",
                Some("c.css") => ".c{color:red}",
                _ => return Err("HTTP 404".to_string()),
            };
            Ok(body.as_bytes().to_vec())
        };
        let r = export_card(&c, None, files, fetch);
        for want in [
            r#"<style>.e{}.a{}@import "b.css"; @import "c.css";</style>"#,
            r#"<style>@media screen{.k{}@import "b.css";}</style>"#,
        ] {
            assert!(r.html.contains(want), "{want}\n{}", r.html);
        }
        assert_eq!(
            *fetched.borrow(),
            [
                "https://unpkg.com/a.css",
                "https://unpkg.com/e.css",
                "https://unpkg.com/m.css",
                "https://unpkg.com/k.css",
            ]
        );
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    }

    #[test]
    fn an_inlined_sheet_ends_as_its_own_end_of_file_ends_it() {
        let c = card(
            r#"<link rel="stylesheet" href="https://unpkg.com/a.css"><link rel="stylesheet" href="https://unpkg.com/m.css"><link rel="stylesheet" href="https://unpkg.com/n.css"><link rel="stylesheet" href="https://unpkg.com/p.css">"#,
            &[],
            &[],
        );
        let fetch = |url: &str, _: Duration| {
            let body = match url.strip_prefix("https://unpkg.com/") {
                Some("m.css") => "@import \"l.css\";@import \"https://example.com/z.css\";",
                Some("a.css") => {
                    "@import \"l.css\";@import \"o.css\";@import \"s.css\";@import \"r.css\";.z{}"
                }
                Some("n.css") => "@import \"o.css\" screen;.n{}",
                Some("p.css") => "@import \"d.css\";@import \"https://example.com/z.css\";",
                Some("d.css") => ".d",
                Some("l.css") => "@media screen",
                Some("o.css") => ".o{color:red",
                Some("s.css") => ".s{content:\"x",
                Some("r.css") => ".r{}.dangling",
                _ => return Err("HTTP 404".to_string()),
            };
            Ok(body.as_bytes().to_vec())
        };
        let r = export_card(&c, None, files, fetch);
        for want in [
            r#"<style>@media screen;.o{color:red}.s{content:"x"}.r{}.z{}</style>"#,
            r#"<style>@media screen;@import "https://example.com/z.css";</style>"#,
            r#"<style>@media screen{.o{color:red}}.n{}</style>"#,
            // A selector with no block is no rule, so the link after it
            // leaves nothing to turn back into a link.
            r#"<style>@import "https://example.com/z.css";</style>"#,
        ] {
            assert!(r.html.contains(want), "{want}\n{}", r.html);
        }
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    }

    #[test]
    fn a_stray_brace_in_an_imported_sheet_stays_inside_its_wrapper() {
        let c = card(
            r#"<style>@import "https://unpkg.com/s.css" screen;</style>"#,
            &[],
            &[],
        );
        let fetch = |url: &str, _: Duration| match url {
            "https://unpkg.com/s.css" => Ok(b"} .a{color:red}".to_vec()),
            _ => Err("HTTP 404".to_string()),
        };
        let r = export_card(&c, None, files, fetch);
        let want = "<style>@media screen{] .a{color:red}}</style>";
        assert!(r.html.contains(want), "{}", r.html);
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    }

    #[test]
    fn closed_at_end_closes_what_the_end_of_file_closes() {
        for (css, want) in [
            ("", ""),
            (".a{}@import \"x\";", ".a{}@import \"x\";"),
            ("@media screen", "@media screen;"),
            ("@supports (display:grid", "@supports (display:grid);"),
            ("@media x{.a{color:red", "@media x{.a{color:red}}"),
            (".a{}.b", ".a{}"),
            (".a{};.b{}", ".a{};.b{}"),
            (".a{}.b:is(", ".a{}"),
            ("/* open", "/* open*/"),
            ("/*/", "/*/*/"),
            ("/**/", "/**/"),
            (".a{background:url(a.png", ".a{background:url(a.png)}"),
            (".a{background:url(a\\", ".a{background:url(a\\\u{FFFD})}"),
            (".a{content:'x", ".a{content:'x'}"),
            (".a{content:\"x\\", ".a{content:\"x\\\n\"}"),
            ("@x a\\", "@x a\\\u{FFFD};"),
            ("<!-- @import \"x\"", "<!-- @import \"x\";"),
            (".a{content:\"}\"", ".a{content:\"}\"}"),
            (".a{b:c}/* .b{ */", ".a{b:c}/* .b{ */"),
            ("@1x .a", ""),
            ("@-1 .a", ""),
            ("@-x a", "@-x a;"),
            ("@\\31 x a", "@\\31 x a;"),
            ("@x \"a\\\r\n", "@x \"a\\\r\n\";"),
            ("@x \"é", "@x \"é\";"),
            ("@x \\é", "@x \\é;"),
            ("@x URL(a", "@x URL(a);"),
            ("@x url( \"a", "@x url( \"a\");"),
            ("@media x{ ( }", "@media x{ ( })}"),
            ("@x ) ] a", "@x ) ] a;"),
            ("--> .a{}", "--> .a{}"),
            ("@media x{.a{}.b", "@media x{.a{}.b}"),
            ("} .a{color:red}", "] .a{color:red}"),
            ("@media all, } {.a{}}", "@media all, ] {.a{}}"),
            ("@x };.a{}", "@x ];.a{}"),
            (".a{ } }.b{}", ".a{ } ].b{}"),
            (".a( } ).b{}", ".a( } ).b{}"),
            (".a{ ( } ) }", ".a{ ( } ) }"),
            ("} .a{} }", "] .a{} "),
        ] {
            assert_eq!(closed_at_end(css), want, "{css:?}");
        }
    }

    #[test]
    fn a_sheet_that_turned_others_back_into_links_can_turn_back_too() {
        let c = card(
            r#"<link rel="stylesheet" href="https://unpkg.com/a.css">"#,
            &[],
            &[],
        );
        let fetch = |url: &str, _: Duration| {
            let body = match url.strip_prefix("https://unpkg.com/") {
                Some("a.css") => {
                    "@import \"y.css\";@import \"f.css\";@import \"https://example.com/z.css\";"
                }
                Some("y.css") => ".y{}",
                Some("f.css") => "@import \"https://example.com/c.css\";.f{}",
                _ => return Err("HTTP 404".to_string()),
            };
            Ok(body.as_bytes().to_vec())
        };
        let r = export_card(&c, None, files, fetch);
        let want = r#"<style>@import url("https://unpkg.com/y.css");@import url("https://unpkg.com/f.css");@import "https://example.com/z.css";</style>"#;
        assert!(r.html.contains(want), "{}", r.html);
        let got: Vec<_> = r
            .warnings
            .iter()
            .map(|w| (w.target.as_str(), w.reason.as_str()))
            .collect();
        assert_eq!(
            got,
            [
                (
                    "https://unpkg.com/y.css",
                    "kept as a link: the later @import of https://example.com/c.css would be ignored after its rules"
                ),
                (
                    "https://unpkg.com/f.css",
                    "kept as a link: the later @import of https://example.com/z.css would be ignored after its rules"
                ),
            ]
        );
    }

    #[test]
    fn a_block_kept_as_a_link_names_the_link_that_stays() {
        let c = card(
            r#"<link rel="stylesheet" href="https://unpkg.com/a.css">"#,
            &[],
            &[],
        );
        let fetch = |url: &str, _: Duration| {
            let body = match url.strip_prefix("https://unpkg.com/") {
                Some("a.css") => "@import \"g.css\" layer(x);",
                Some("g.css") => "@import \"h.css\" supports(display:grid);",
                Some("h.css") => "@import \"https://example.com/c.css\";",
                _ => return Err("HTTP 404".to_string()),
            };
            Ok(body.as_bytes().to_vec())
        };
        let r = export_card(&c, None, files, fetch);
        let want = r#"<style>@import url("https://unpkg.com/g.css") layer(x);</style>"#;
        assert!(r.html.contains(want), "{}", r.html);
        let got: Vec<_> = r
            .warnings
            .iter()
            .map(|w| (w.target.as_str(), w.reason.as_str()))
            .collect();
        // g.css holds the link to h.css, not h.css's thrown-away c.css.
        assert_eq!(
            got,
            [
                (
                    "https://unpkg.com/h.css",
                    "kept as a link: its @import of https://example.com/c.css would be ignored inside @supports"
                ),
                (
                    "https://unpkg.com/g.css",
                    "kept as a link: its @import of https://unpkg.com/h.css would be ignored inside @layer"
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
                Some("a.css") => "@import \"b.css\";@import \"x.css\";@import \"y.css\";.a{}",
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
                r#"<style>@import url("https://unpkg.com/x.css");@import url("https://unpkg.com/y.css");.e{}.d{}.c{}.b{}.x{}.y{}.a{}</style>"#
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
    fn u_fffd_growth_counts_toward_the_inlining_total() {
        // Half an asset of bytes that aren't UTF-8 reads as three halves of
        // one: past the one asset of the total fifteen sheets leave, inside
        // the two fourteen leave, where counting the decoded text on top of
        // the body would pass it. A module is written as the base64 of that
        // text, two assets and a few bytes: past the two fourteen leave,
        // inside the three thirteen leave.
        let mut bad = b"/*".to_vec();
        bad.extend(vec![0xFF; MAX_ASSET_BYTES / 2]);
        bad.extend(b"*/");
        let rows = [
            (r#"<script src="https://unpkg.com/x"></script>"#, 15),
            (r#"<link rel="stylesheet" href="https://unpkg.com/x">"#, 15),
            (
                r#"<script type="module" src="https://unpkg.com/x"></script>"#,
                14,
            ),
            (r#"<style>@import "https://unpkg.com/x";</style>"#, 15),
        ];
        for (row, misses) in rows {
            for (sheets, fits) in [(misses, false), (misses - 1, true)] {
                let fill: String = (0..sheets)
                    .map(|n| {
                        format!(r#"<link rel="stylesheet" href="https://unpkg.com/big{n}.css">"#)
                    })
                    .collect();
                let c = card(&format!("{fill}{row}"), &[], &[]);
                let bad = bad.clone();
                let fetch = move |url: &str, _: Duration| match url {
                    "https://unpkg.com/x" => Ok(bad.clone()),
                    _ => Ok(vec![b'y'; MAX_ASSET_BYTES]),
                };
                let r = export_card(&c, None, files, fetch);
                assert!(
                    r.html.len() < MAX_INLINED_BYTES + 64 * 1024,
                    "{sheets} {row}: {}",
                    r.html.len()
                );
                assert_eq!(r.html.contains(row), !fits, "{sheets} {row}");
                let got: Vec<_> = r
                    .warnings
                    .iter()
                    .map(|w| (w.target.as_str(), w.reason.as_str()))
                    .collect();
                let want = [(
                    "https://unpkg.com/x",
                    "skipped: the export already inlined 8 MB",
                )];
                assert_eq!(
                    got,
                    if fits { &want[..0] } else { &want[..] },
                    "{sheets} {row}"
                );
            }
        }
    }

    /// `n` stylesheets of one asset each, filling that much of the total.
    fn filled(n: usize) -> String {
        (0..n)
            .map(|n| format!(r#"<link rel="stylesheet" href="https://unpkg.com/big{n}.css">"#))
            .collect()
    }

    fn over_total(url: &str) -> Vec<(String, String)> {
        vec![(url.to_string(), over_total_reason())]
    }

    fn warned(r: &ExportResult) -> Vec<(String, String)> {
        r.warnings
            .iter()
            .map(|w| (w.target.clone(), w.reason.clone()))
            .collect()
    }

    #[test]
    fn a_module_data_uri_len_is_its_length() {
        for n in 0..8 {
            let text = "x".repeat(n);
            assert_eq!(module_data_uri_len(n), module_data_uri(&text).len(), "{n}");
        }
    }

    #[test]
    fn a_module_counts_once_per_map_entry_that_writes_it() {
        // One asset of module is a third more as base64: one copy fits the
        // two assets fourteen sheets leave, two copies don't.
        const M: &str = "https://unpkg.com/m.js";
        let m = format!("/*{}*/", "x".repeat(MAX_ASSET_BYTES - 4));
        let rows = [
            (
                // The name `m` and the URL itself: two copies.
                format!(
                    r#"<script type="importmap">{{"imports":{{"m":"{M}"}}}}</script><script type="module">import "m";</script>"#
                ),
                false,
            ),
            (
                format!(r#"<script type="module" src="{M}"></script>"#),
                true,
            ),
        ];
        for (row, fits) in rows {
            let c = card(&format!("{}{row}", filled(14)), &[], &[]);
            let body = m.clone();
            let fetch = move |url: &str, _: Duration| match url {
                M => Ok(body.clone().into_bytes()),
                _ => Ok(vec![b'y'; MAX_ASSET_BYTES]),
            };
            let r = export_card(&c, None, files, fetch);
            assert!(
                r.html.len() < MAX_INLINED_BYTES + 64 * 1024,
                "{row}: {}",
                r.html.len()
            );
            let map = page_map(&r.html);
            let address = &map["imports"][M];
            if fits {
                assert_eq!(warned(&r), [], "{row}");
                assert_eq!(address, &js_data(&m), "{row}");
            } else {
                assert_eq!(warned(&r), over_total(M), "{row}");
                assert_eq!(map, serde_json::json!({"imports": {"m": M, M: M}}), "{row}");
            }
        }
    }

    #[test]
    fn a_module_counts_its_specifiers_as_the_page_writes_them() {
        // Each `"./b.js"` is written as `"https://unpkg.com/b.js"`, sixteen
        // bytes longer: a.js as fetched fits the asset fifteen sheets leave
        // as base64 (two thirds of it), as written doesn't (1.38 of it).
        const A: &str = "https://unpkg.com/a.js";
        const B: &str = "https://unpkg.com/b.js";
        let a = "import\"./b.js\";".repeat(MAX_ASSET_BYTES / 30);
        let c = card(
            &format!(r#"{}<script type="module" src="{A}"></script>"#, filled(15)),
            &[],
            &[],
        );
        let fetch = move |url: &str, _: Duration| match url {
            A => Ok(a.clone().into_bytes()),
            B => Ok(b"export{}".to_vec()),
            _ => Ok(vec![b'y'; MAX_ASSET_BYTES]),
        };
        let r = export_card(&c, None, files, fetch);
        assert!(
            r.html.len() < MAX_INLINED_BYTES + 64 * 1024,
            "{}",
            r.html.len()
        );
        assert_eq!(warned(&r), over_total(A));
        assert_eq!(
            page_map(&r.html),
            serde_json::json!({"imports": {A: A, B: js_data("export{}")}})
        );
    }

    #[test]
    fn a_module_left_out_at_the_total_keeps_what_it_reaches_and_drops_its_computed_warning() {
        // As in `a_module_counts_once_per_map_entry_that_writes_it`: two
        // copies of a.js don't fit. Left out, it loads from its URL, where
        // its computed import works and its import of b.js still resolves
        // through the page's map. b.js stays in, warning still.
        const A: &str = "https://unpkg.com/a.js";
        const B: &str = "https://unpkg.com/b.js";
        let a = format!(
            "import\"./b.js\";import(x);/*{}*/",
            "x".repeat(MAX_ASSET_BYTES - 30)
        );
        let c = card(
            &format!(
                r#"{}<script type="importmap">{{"imports":{{"m":"{A}"}}}}</script><script type="module">import "m";</script>"#,
                filled(14)
            ),
            &[],
            &[],
        );
        let fetch = move |url: &str, _: Duration| match url {
            A => Ok(a.clone().into_bytes()),
            B => Ok(b"import(y);".to_vec()),
            _ => Ok(vec![b'y'; MAX_ASSET_BYTES]),
        };
        let r = export_card(&c, None, files, fetch);
        let b_computed = "computes an import from its own address, which the export makes a \
                          data: URI, so it fails when the page runs: y at line 1, column 8";
        let mut want = vec![(B.to_string(), b_computed.to_string())];
        want.extend(over_total(A));
        assert_eq!(warned(&r), want);
        assert_eq!(
            page_map(&r.html),
            serde_json::json!({"imports": {"m": A, A: A, B: js_data("import(y);")}})
        );
    }

    #[test]
    fn a_css_file_counts_its_base64_toward_the_total() {
        // A font of just under an asset fits the asset fifteen sheets leave,
        // but not as base64; one of three quarters of it does.
        const F: &str = "https://unpkg.com/f.woff2";
        let row = format!("<style>a{{src:url({F})}}</style>");
        for (size, fits) in [
            (MAX_ASSET_BYTES - 16, false),
            ((MAX_ASSET_BYTES - 100) / 4 * 3, true),
        ] {
            let c = card(&format!("{}{row}", filled(15)), &[], &[]);
            let fetch = move |url: &str, _: Duration| match url {
                F => Ok(vec![b'f'; size]),
                _ => Ok(vec![b'y'; MAX_ASSET_BYTES]),
            };
            let r = export_card(&c, None, files, fetch);
            assert!(
                r.html.len() < MAX_INLINED_BYTES + 64 * 1024,
                "{size}: {}",
                r.html.len()
            );
            assert_eq!(r.html.contains(&row), !fits, "{size}");
            assert_eq!(r.html.contains("data:font/woff2;base64,"), fits, "{size}");
            assert_eq!(
                warned(&r),
                if fits { vec![] } else { over_total(F) },
                "{size}"
            );
        }
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
        let b = format!(".b{{}}/*{}*/", "x".repeat(MAX_ASSET_BYTES - 8)).into_bytes();
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
        // Each sheet past the total turns the earliest inlined one back into
        // a link and takes its bytes, so the last `fit` are inlined.
        let links = r
            .html
            .matches(r#"@import url("https://unpkg.com/b"#)
            .count();
        assert_eq!(links, 40 - fit);
        for n in 0..40 {
            let link = format!(r#"@import url("https://unpkg.com/b{n}.css");"#);
            assert_eq!(r.html.contains(&link), n < 40 - fit, "b{n}");
        }
        let got: Vec<_> = r.warnings.iter().map(|w| w.target.clone()).collect();
        let want: Vec<_> = (0..40 - fit)
            .map(|n| format!("https://unpkg.com/b{n}.css"))
            .collect();
        assert_eq!(got, want);
        assert!(
            r.warnings[0].reason.contains(&format!(
                "@import of https://unpkg.com/b{fit}.css needed its bytes under the export's 8 MB total"
            )),
            "{}",
            r.warnings[0].reason
        );
    }

    #[test]
    fn css_edits_name_exactly_the_bytes_that_changed() {
        // s.css goes in as rules, then turns back into its link, written as
        // it was, when 404.css stays a link; only the url() is an edit.
        let a = r#"@import url("https://unpkg.com/s.css");@import url("https://unpkg.com/404.css");b{background:url(i.png)}"#;
        let fetch = |url: &str, _: Duration| match url {
            "https://unpkg.com/s.css" => Ok(b"p{}".to_vec()),
            "https://unpkg.com/i.png" => Ok(b"i".to_vec()),
            _ => Err("HTTP 404".to_string()),
        };
        let mut cdn = Cdn::new(fetch, Duration::from_secs(10));
        let mut warnings = Vec::new();
        let edits = cdn.css_edits(a, Some("https://unpkg.com/a.css"), 0, &mut warnings);
        let got: Vec<_> = edits
            .iter()
            .map(|e| (e.at.clone(), e.text.as_str()))
            .collect();
        let at = a.find("url(i.png)").unwrap();
        assert_eq!(
            got,
            [(
                at..at + "url(i.png)".len(),
                r#"url("data:image/png;base64,aQ==")"#
            )]
        );
    }

    #[test]
    fn a_sheet_past_the_total_stays_a_link_when_freeing_cannot_fit_it() {
        // The big sheets and pad.css leave 10 bytes too few for b.css; s.css
        // frees 3, so both s.css and b.css end up links.
        let a = "@import \"s.css\";@import \"b.css\";";
        let fetch = move |url: &str, _: Duration| match url {
            "https://unpkg.com/pad.css" => Ok(vec![b'z'; 10]),
            "https://unpkg.com/s.css" => Ok(b"p{}".to_vec()),
            "https://unpkg.com/b.css" => Ok(vec![b'x'; MAX_ASSET_BYTES]),
            u if u.starts_with("https://unpkg.com/big") => Ok(vec![b'y'; MAX_ASSET_BYTES]),
            _ => Err("HTTP 404".to_string()),
        };
        let mut cdn = Cdn::new(fetch, Duration::from_secs(10));
        let mut warnings = Vec::new();
        for n in 0..MAX_INLINED_BYTES / MAX_ASSET_BYTES - 1 {
            assert!(keep(
                &mut cdn,
                &format!("https://unpkg.com/big{n}.css"),
                &mut warnings
            )
            .is_some());
        }
        assert!(keep(&mut cdn, "https://unpkg.com/pad.css", &mut warnings).is_some());
        let out = cdn.css(a, Some("https://unpkg.com/a.css"), 0, &mut warnings);
        assert_eq!(
            out,
            r#"@import url("https://unpkg.com/s.css");@import url("https://unpkg.com/b.css");"#
        );
        let got: Vec<_> = warnings
            .iter()
            .map(|w| (w.target.as_str(), w.reason.as_str()))
            .collect();
        assert_eq!(
            got,
            [
                (
                    "https://unpkg.com/b.css",
                    "skipped: the export already inlined 8 MB"
                ),
                (
                    "https://unpkg.com/s.css",
                    "kept as a link: the later @import of https://unpkg.com/b.css would be ignored after its rules"
                ),
            ]
        );
        assert_eq!(cdn.inlined(), MAX_INLINED_BYTES - MAX_ASSET_BYTES + 10);
    }

    #[test]
    fn an_import_retried_after_freeing_room_counts_once() {
        // j.css fits, but its k.css misses the total by 326 bytes until
        // s.css's 607 turn back into a link; j.css then inlines whole, and
        // its first try leaves nothing in the record of the page.
        let s = format!("p{{}}/*{}*/", "x".repeat(600));
        let k = format!("q{{}}/*{}*/", "x".repeat(MAX_ASSET_BYTES - 307));
        let j = "@import \"k.css\";p{}";
        let (s2, k2) = (s.clone(), k.clone());
        let fetch = move |url: &str, _: Duration| match url {
            "https://unpkg.com/s.css" => Ok(s2.clone().into_bytes()),
            "https://unpkg.com/j.css" => Ok(j.as_bytes().to_vec()),
            "https://unpkg.com/k.css" => Ok(k2.clone().into_bytes()),
            u if u.starts_with("https://unpkg.com/big") => Ok(vec![b'y'; MAX_ASSET_BYTES]),
            _ => Err("HTTP 404".to_string()),
        };
        let mut cdn = Cdn::new(fetch, Duration::from_secs(10));
        let mut warnings = Vec::new();
        for n in 0..MAX_INLINED_BYTES / MAX_ASSET_BYTES - 1 {
            assert!(keep(
                &mut cdn,
                &format!("https://unpkg.com/big{n}.css"),
                &mut warnings
            )
            .is_some());
        }
        let out = cdn.css(
            "@import \"s.css\";@import \"j.css\";",
            Some("https://unpkg.com/a.css"),
            0,
            &mut warnings,
        );
        assert_eq!(
            out,
            format!(r#"@import url("https://unpkg.com/s.css");{k}p{{}}"#)
        );
        assert_eq!(cdn.inlined(), MAX_INLINED_BYTES - 281);
        assert_eq!(linked(&cdn), ["https://unpkg.com/s.css"]);
        let got: Vec<_> = warnings.iter().map(|w| w.target.as_str()).collect();
        assert_eq!(got, ["https://unpkg.com/s.css"]);
    }

    #[test]
    fn room_made_inside_an_import_then_around_it_counts_once() {
        // x.css's b.css misses the total by 307 bytes; x.css's own try frees
        // a.css and inlines b.css, but a.css as a link would turn s.css back
        // into one, so s.css goes first and x.css then inlines whole.
        let s = format!("p{{}}/*{}*/", "x".repeat(600));
        let a = format!("a{{}}/*{}*/", "x".repeat(600));
        let x = "@import \"a.css\";@import \"b.css\";";
        let b = format!("b{{}}/*{}*/", "x".repeat(MAX_ASSET_BYTES - 32 - 607 - 307));
        let (s2, a2, b2) = (s.clone(), a.clone(), b.clone());
        let fetch = move |url: &str, _: Duration| match url {
            "https://unpkg.com/s.css" => Ok(s2.clone().into_bytes()),
            "https://unpkg.com/x.css" => Ok(x.as_bytes().to_vec()),
            "https://unpkg.com/a.css" => Ok(a2.clone().into_bytes()),
            "https://unpkg.com/b.css" => Ok(b2.clone().into_bytes()),
            u if u.starts_with("https://unpkg.com/big") => Ok(vec![b'y'; MAX_ASSET_BYTES]),
            _ => Err("HTTP 404".to_string()),
        };
        let mut cdn = Cdn::new(fetch, Duration::from_secs(10));
        let mut warnings = Vec::new();
        for n in 0..MAX_INLINED_BYTES / MAX_ASSET_BYTES - 1 {
            assert!(keep(
                &mut cdn,
                &format!("https://unpkg.com/big{n}.css"),
                &mut warnings
            )
            .is_some());
        }
        let out = cdn.css(
            "@import \"s.css\";@import \"x.css\";",
            Some("https://unpkg.com/page.css"),
            0,
            &mut warnings,
        );
        assert_eq!(
            out,
            format!(r#"@import url("https://unpkg.com/s.css");{a}{b}"#)
        );
        assert_eq!(cdn.inlined(), MAX_INLINED_BYTES - 300);
        assert_eq!(linked(&cdn), ["https://unpkg.com/s.css"]);
        assert_eq!(
            cdn.present()[cdn.present().len() - 3..],
            [
                "https://unpkg.com/x.css",
                "https://unpkg.com/a.css",
                "https://unpkg.com/b.css"
            ]
        );
        let got: Vec<_> = warnings
            .iter()
            .map(|w| (w.target.as_str(), w.reason.as_str()))
            .collect();
        assert_eq!(
            got,
            [(
                "https://unpkg.com/s.css",
                "kept as a link: the later @import of https://unpkg.com/x.css needed its bytes under the export's 8 MB total"
            )]
        );
    }

    #[test]
    fn room_made_around_an_import_frees_only_what_its_links_need() {
        // x.css's own try fits b.css by freeing a.css, so x.css writes a.css's
        // link and needs a.css's 607 bytes, 300 more than are left: s.css
        // alone goes, and t.css stays inlined.
        let s = format!("p{{}}/*{}*/", "x".repeat(600));
        let t = format!("t{{}}/*{}*/", "x".repeat(600));
        let a = format!("a{{}}/*{}*/", "x".repeat(600));
        let x = "@import \"a.css\";@import \"b.css\";";
        let b = format!("b{{}}/*{}*/", "x".repeat(MAX_ASSET_BYTES - 7 - 1553));
        let (s2, t2, a2, b2) = (s.clone(), t.clone(), a.clone(), b.clone());
        let fetch = move |url: &str, _: Duration| match url {
            "https://unpkg.com/s.css" => Ok(s2.clone().into_bytes()),
            "https://unpkg.com/t.css" => Ok(t2.clone().into_bytes()),
            "https://unpkg.com/x.css" => Ok(x.as_bytes().to_vec()),
            "https://unpkg.com/a.css" => Ok(a2.clone().into_bytes()),
            "https://unpkg.com/b.css" => Ok(b2.clone().into_bytes()),
            u if u.starts_with("https://unpkg.com/big") => Ok(vec![b'y'; MAX_ASSET_BYTES]),
            _ => Err("HTTP 404".to_string()),
        };
        let mut cdn = Cdn::new(fetch, Duration::from_secs(10));
        let mut warnings = Vec::new();
        for n in 0..MAX_INLINED_BYTES / MAX_ASSET_BYTES - 1 {
            assert!(keep(
                &mut cdn,
                &format!("https://unpkg.com/big{n}.css"),
                &mut warnings
            )
            .is_some());
        }
        let out = cdn.css(
            "@import \"s.css\";@import \"t.css\";@import \"x.css\";",
            Some("https://unpkg.com/page.css"),
            0,
            &mut warnings,
        );
        assert_eq!(
            out,
            format!(r#"@import url("https://unpkg.com/s.css");{t}{a}{b}"#)
        );
        assert_eq!(cdn.inlined(), MAX_INLINED_BYTES - 307);
        assert_eq!(linked(&cdn), ["https://unpkg.com/s.css"]);
        let got: Vec<_> = warnings.iter().map(|w| w.target.as_str()).collect();
        assert_eq!(got, ["https://unpkg.com/s.css"]);
    }

    #[test]
    fn an_import_whose_link_room_cannot_fix_frees_every_earlier_sheet_for_the_rest() {
        // c.css's 404.css stays a link whatever the room, so s.css and t.css
        // turn back into links; f.woff2, which missed the total, then fits:
        // its data: URI is 577 bytes under the asset of room the fill leaves,
        // fewer than s.css and t.css hold.
        let s = format!("p{{}}/*{}*/", "x".repeat(600));
        let t = format!("t{{}}/*{}*/", "x".repeat(600));
        let c = "@import \"404.css\";a{background:url(f.woff2)}";
        let (s2, t2) = (s.clone(), t.clone());
        let fetch = move |url: &str, _: Duration| match url {
            "https://unpkg.com/s.css" => Ok(s2.clone().into_bytes()),
            "https://unpkg.com/t.css" => Ok(t2.clone().into_bytes()),
            "https://unpkg.com/c.css" => Ok(c.as_bytes().to_vec()),
            "https://unpkg.com/f.woff2" => Ok(vec![b'f'; (MAX_ASSET_BYTES - 600) / 4 * 3]),
            u if u.starts_with("https://unpkg.com/big") => Ok(vec![b'y'; MAX_ASSET_BYTES]),
            _ => Err("HTTP 404".to_string()),
        };
        let mut cdn = Cdn::new(fetch, Duration::from_secs(10));
        let mut warnings = Vec::new();
        for n in 0..MAX_INLINED_BYTES / MAX_ASSET_BYTES - 1 {
            assert!(keep(
                &mut cdn,
                &format!("https://unpkg.com/big{n}.css"),
                &mut warnings
            )
            .is_some());
        }
        let out = cdn.css(
            "@import \"s.css\";@import \"t.css\";@import \"c.css\";",
            Some("https://unpkg.com/page.css"),
            0,
            &mut warnings,
        );
        assert!(
            out.starts_with(concat!(
                r#"@import url("https://unpkg.com/s.css");@import url("https://unpkg.com/t.css");"#,
                r#"@import url("https://unpkg.com/404.css");a{background:url("data:font/woff2;base64,"#
            )),
            "{}",
            &out[..200]
        );
        assert_eq!(
            linked(&cdn),
            [
                "https://unpkg.com/s.css",
                "https://unpkg.com/t.css",
                "https://unpkg.com/404.css"
            ]
        );
        let got: Vec<_> = warnings.iter().map(|w| w.target.as_str()).collect();
        assert_eq!(
            got,
            [
                "https://unpkg.com/404.css",
                "https://unpkg.com/s.css",
                "https://unpkg.com/t.css"
            ]
        );
    }

    #[test]
    fn a_conditional_import_whose_nested_import_missed_the_total_frees_only_its_room() {
        // m.css stays a link inside @layer only because its k.css misses the
        // total by 300 bytes, so s.css alone goes and t.css stays inlined.
        let s = format!("p{{}}/*{}*/", "x".repeat(600));
        let t = format!("t{{}}/*{}*/", "x".repeat(600));
        let m = "@import \"k.css\";";
        let k = format!("k{{}}/*{}*/", "x".repeat(MAX_ASSET_BYTES - 937));
        let (s2, t2, k2) = (s.clone(), t.clone(), k.clone());
        let fetch = move |url: &str, _: Duration| match url {
            "https://unpkg.com/s.css" => Ok(s2.clone().into_bytes()),
            "https://unpkg.com/t.css" => Ok(t2.clone().into_bytes()),
            "https://unpkg.com/m.css" => Ok(m.as_bytes().to_vec()),
            "https://unpkg.com/k.css" => Ok(k2.clone().into_bytes()),
            u if u.starts_with("https://unpkg.com/big") => Ok(vec![b'y'; MAX_ASSET_BYTES]),
            _ => Err("HTTP 404".to_string()),
        };
        let mut cdn = Cdn::new(fetch, Duration::from_secs(10));
        let mut warnings = Vec::new();
        for n in 0..MAX_INLINED_BYTES / MAX_ASSET_BYTES - 1 {
            assert!(keep(
                &mut cdn,
                &format!("https://unpkg.com/big{n}.css"),
                &mut warnings
            )
            .is_some());
        }
        let out = cdn.css(
            "@import \"s.css\";@import \"t.css\";@import \"m.css\" layer(x);",
            Some("https://unpkg.com/page.css"),
            0,
            &mut warnings,
        );
        assert_eq!(
            out,
            format!(r#"@import url("https://unpkg.com/s.css");{t}@layer x{{{k}}}"#)
        );
        assert_eq!(linked(&cdn), ["https://unpkg.com/s.css"]);
        let got: Vec<_> = warnings.iter().map(|w| w.target.as_str()).collect();
        assert_eq!(got, ["https://unpkg.com/s.css"]);
    }

    /// The URL of every `@import` written out as a link, in order.
    fn linked<F: cdn::Fetch>(cdn: &Cdn<F>) -> Vec<&str> {
        cdn.links().iter().map(|l| l.url.as_str()).collect()
    }

    #[test]
    fn downloads_past_the_time_budget_are_skipped() {
        let mut cdn = Cdn::new(|_: &str, _: Duration| Ok(b"x".to_vec()), Duration::ZERO);
        let mut warnings = Vec::new();
        assert_eq!(
            keep(&mut cdn, "https://unpkg.com/a.js", &mut warnings),
            None
        );
        assert_eq!(
            warnings[0].reason,
            "skipped: the export's 0s download time ran out"
        );
    }

    #[test]
    fn the_closers_an_imported_sheet_needs_count_toward_the_total() {
        // p.css, a block left open on MAX_ASSET_BYTES - 2 `(`, reads as one
        // byte short of twice its size once closed, so after 14 big sheets
        // it fits the total with a byte to spare, and after 15 it stays a
        // link though its own bytes would fit.
        let mut p = b"a{".to_vec();
        p.resize(MAX_ASSET_BYTES, b'(');
        let fetch = move |url: &str, _: Duration| match url {
            "https://unpkg.com/p.css" => Ok(p.clone()),
            u if u.starts_with("https://unpkg.com/big") => Ok(vec![b'y'; MAX_ASSET_BYTES]),
            _ => Err("HTTP 404".to_string()),
        };
        let fill = MAX_INLINED_BYTES / MAX_ASSET_BYTES - 2;
        for big in [fill, fill + 1] {
            let mut cdn = Cdn::new(fetch.clone(), Duration::from_secs(10));
            let mut warnings = Vec::new();
            for n in 0..big {
                let url = format!("https://unpkg.com/big{n}.css");
                assert!(keep(&mut cdn, &url, &mut warnings).is_some());
            }
            let out = cdn.css(
                "@import \"p.css\";",
                Some("https://unpkg.com/a.css"),
                0,
                &mut warnings,
            );
            if big == fill {
                assert_eq!(out.len(), 2 * MAX_ASSET_BYTES - 1);
                assert!(warnings.is_empty(), "{warnings:?}");
                assert_eq!(cdn.inlined(), MAX_INLINED_BYTES - 1);
            } else {
                assert_eq!(out, r#"@import url("https://unpkg.com/p.css");"#);
                let got: Vec<_> = warnings
                    .iter()
                    .map(|w| (w.target.as_str(), w.reason.as_str()))
                    .collect();
                assert_eq!(
                    got,
                    [(
                        "https://unpkg.com/p.css",
                        "skipped: the export already inlined 8 MB"
                    )]
                );
                assert_eq!(cdn.inlined(), (fill + 1) * MAX_ASSET_BYTES);
            }
        }
    }

    #[test]
    fn a_sheet_short_of_room_for_its_closers_frees_just_enough() {
        // After 13 big sheets, s1.css and s2.css leave p.css, closed, one
        // byte over the total; s1.css alone gives that byte back.
        let mut p = b"a{".to_vec();
        p.resize(MAX_ASSET_BYTES, b'(');
        let s1 = format!("p{{}}/*{}*/", "x".repeat(MAX_ASSET_BYTES - 8));
        let fetch = move |url: &str, _: Duration| match url {
            "https://unpkg.com/p.css" => Ok(p.clone()),
            "https://unpkg.com/s1.css" => Ok(s1.clone().into_bytes()),
            "https://unpkg.com/s2.css" => Ok(b"q{}".to_vec()),
            u if u.starts_with("https://unpkg.com/big") => Ok(vec![b'y'; MAX_ASSET_BYTES]),
            _ => Err("HTTP 404".to_string()),
        };
        let mut cdn = Cdn::new(fetch, Duration::from_secs(10));
        let mut warnings = Vec::new();
        for n in 0..MAX_INLINED_BYTES / MAX_ASSET_BYTES - 3 {
            let url = format!("https://unpkg.com/big{n}.css");
            assert!(keep(&mut cdn, &url, &mut warnings).is_some());
        }
        let out = cdn.css(
            "@import \"s1.css\";@import \"s2.css\";@import \"p.css\";",
            Some("https://unpkg.com/a.css"),
            0,
            &mut warnings,
        );
        assert!(
            out.starts_with(r#"@import url("https://unpkg.com/s1.css");q{}a{(("#),
            "{}",
            &out[..80]
        );
        let got: Vec<_> = warnings.iter().map(|w| w.target.as_str()).collect();
        assert_eq!(got, ["https://unpkg.com/s1.css"]);
        assert_eq!(cdn.inlined(), MAX_INLINED_BYTES - MAX_ASSET_BYTES + 2);
    }

    /// The body of `url`, kept in the page.
    fn keep<F: cdn::Fetch>(
        cdn: &mut Cdn<F>,
        url: &str,
        warnings: &mut Vec<ExportWarning>,
    ) -> Option<Vec<u8>> {
        cdn.inline(url, warnings, |_, bytes, _| Some(bytes))
    }

    #[test]
    fn a_body_thrown_away_leaves_nothing_in_the_record() {
        // a.css is thrown away after b.css went in from inside it, so
        // neither is recorded as in the page; c.css, kept, is.
        let fetch = |url: &str, _: Duration| match url {
            "https://unpkg.com/a.css" => Ok(b"aaa".to_vec()),
            "https://unpkg.com/b.css" => Ok(b"bb".to_vec()),
            "https://unpkg.com/c.css" => Ok(b"c".to_vec()),
            _ => Err("HTTP 404".to_string()),
        };
        let mut cdn = Cdn::new(fetch, Duration::from_secs(10));
        let mut warnings = Vec::new();
        let dropped = cdn.inline("https://unpkg.com/a.css", &mut warnings, |cdn, _, w| {
            assert!(keep(cdn, "https://unpkg.com/b.css", w).is_some());
            assert_eq!(cdn.present().len(), 2);
            None::<()>
        });
        assert_eq!(dropped, None);
        assert!(keep(&mut cdn, "https://unpkg.com/c.css", &mut warnings).is_some());
        assert_eq!(cdn.present(), ["https://unpkg.com/c.css"]);
        assert_eq!(cdn.inlined(), 1);
        assert!(warnings.is_empty());
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
