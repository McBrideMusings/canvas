//! One card as a standalone HTML page: images and videos become `data:` URIs
//! read from the card's stored paths, `#canvas-open-<n>` anchors become real
//! links or plain text from the card's targets, and the card's latest
//! `canvas data` value is delivered by a shim. None of the viewer's iframe
//! wrapping (theme rewrite, forced colours, contrast pass, resize script,
//! sandbox) is carried; `prefers-color-scheme` rules stay as written, so the
//! reader's system picks the palette. Pure apart from the injected file
//! reader, which is the test seam.

use std::io;
use std::path::Path;

use canvas_core::html::{find_attr_value_range, next_tag, tag_name};
use canvas_core::{base64, Card, ExportResult, ExportWarning, ExportWarningKind, MEDIA_EXTS};

pub fn export_card(
    card: &Card,
    data: Option<&serde_json::Value>,
    read: impl Fn(&str) -> io::Result<Vec<u8>>,
) -> ExportResult {
    let mut warnings = Vec::new();
    let body = rewrite(card, &read, &mut warnings);
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

fn rewrite(
    card: &Card,
    read: &impl Fn(&str) -> io::Result<Vec<u8>>,
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
                            let attr_start = tag[..vs]
                                .trim_end_matches(['"', '\''])
                                .trim_end()
                                .trim_end_matches('=')
                                .trim_end()
                                .len()
                                - "src".len();
                            out.push_str(tag[..attr_start].trim_end());
                            out.push_str(&tag[ve + 1..]);
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
            _ => out.push_str(tag),
        }
    }
    out.push_str(&html[pos..]);
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

/// The text of the first `<h1>`–`<h3>`, tags stripped, for the page title.
fn first_heading(html: &str) -> Option<String> {
    let mut pos = 0;
    while let Some((start, end)) = next_tag(html, pos) {
        pos = end;
        let name = tag_name(&html[start..end]);
        if let Some(level @ ("h1" | "h2" | "h3")) = name.as_deref() {
            let close = format!("</{level}");
            let rest = &html[end..];
            let stop = rest.to_ascii_lowercase().find(&close)?;
            let text = strip_tags(&rest[..stop]);
            let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
            return (!text.is_empty()).then_some(text);
        }
    }
    None
}

fn strip_tags(html: &str) -> String {
    let mut out = String::new();
    let mut pos = 0;
    while let Some((start, end)) = next_tag(html, pos) {
        out.push_str(&html[pos..start]);
        pos = end;
    }
    out.push_str(&html[pos..]);
    // `&amp;` last, so `&amp;lt;` decodes to `&lt;`, not `<`.
    out.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

fn escape_text(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn escape_attr(s: &str) -> String {
    escape_text(s).replace('"', "&quot;").replace('\'', "&#39;")
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

    #[test]
    fn image_becomes_data_uri() {
        let c = card(
            r#"<p><img src="/api/cards/c1/images/0" alt="a"></p>"#,
            &["/x/a.png"],
            &[],
        );
        let r = export_card(&c, None, files);
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
        let r = export_card(&c, None, files);
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
        let r = export_card(&c, None, files);
        assert_eq!(r.warnings.len(), 1);
        assert!(
            r.html.contains("<video controls muted></video>"),
            "{}",
            r.html
        );
    }

    #[test]
    fn title_decodes_entities_once() {
        let r = export_card(&card("<h1>a &amp;lt; b</h1>", &[], &[]), None, files);
        assert!(r.html.contains("<title>a &amp;lt; b</title>"), "{}", r.html);
    }

    #[test]
    fn links_become_real_or_plain() {
        let c = card(
            "<a href=\"#canvas-open-0\">site</a> and <a href='#canvas-open-1'><code>/x/f.rs</code></a>",
            &[],
            &["https://example.com/?a=1&b=2", "/x/f.rs"],
        );
        let r = export_card(&c, None, files);
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
        let r = export_card(&c, Some(&v), files);
        assert!(r.html.contains(r#"var v={"n":3,"s":"\u003c/script>"}"#));
        assert!(!r.html.contains("\"</script>"));
        assert!(r.html.contains("type:'canvas-data',value:v"));
        assert!(r.html.contains("<title>Load now</title>"));
    }

    #[test]
    fn no_data_no_shim() {
        let r = export_card(&card("<p>hi</p>", &[], &[]), None, files);
        assert!(!r.html.contains("canvas-data"));
        assert!(r.html.contains("<title>Canvas post</title>"));
    }
}
