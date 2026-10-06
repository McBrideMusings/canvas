//! Makes a local stylesheet part of the post. A card renders in a sandboxed
//! iframe that cannot fetch, so `<link rel="stylesheet" href="/abs/x.css">`
//! becomes a `<style>` block and the `url(...)` assets inside it (fonts,
//! images) become `data:` URIs. File reads are injected, which is the test
//! seam, as `scan`'s `exists` is.

use std::path::Path;

use canvas_core::html::{find_attr_value_range, tag_name, tags, Tag};
use canvas_core::{base64, MAX_ASSET_BYTES};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inlined {
    pub html: String,
    pub warnings: Vec<String>,
}

/// Replaces each `<link rel="stylesheet" href="/abs/path.css">` whose file
/// `read` returns with `<style>` holding that CSS, and rewrites its local
/// `url(...)` references (absolute, or relative to the stylesheet) to `data:`
/// URIs. A file that cannot be read, or an asset over `MAX_ASSET_BYTES`,
/// produces one warning and is left as written. `http(s)` links, `data:` and
/// `#fragment` references are never touched.
pub fn inline_stylesheets(html: &str, read: impl Fn(&str) -> Option<Vec<u8>>) -> Inlined {
    let mut out = String::with_capacity(html.len());
    let mut warnings = Vec::new();
    let mut pos = 0usize;
    for Tag { start, end, .. } in tags(html) {
        out.push_str(&html[pos..start]);
        let tag = &html[start..end];
        pos = end;
        let Some(href) = local_stylesheet_href(tag) else {
            out.push_str(tag);
            continue;
        };
        match read(&href) {
            Some(bytes) => {
                let css = String::from_utf8_lossy(&bytes);
                let dir = Path::new(&href).parent().unwrap_or(Path::new("/"));
                let css = inline_urls(&css, dir, &read, &mut warnings);
                out.push_str("<style>");
                out.push_str(&css.replace("</style", "<\\/style"));
                out.push_str("</style>");
            }
            None => {
                warnings.push(format!(
                    "canvas post: stylesheet not found, left as-is: {href}"
                ));
                out.push_str(tag);
            }
        }
    }
    out.push_str(&html[pos..]);
    Inlined {
        html: out,
        warnings,
    }
}

fn local_stylesheet_href(tag: &str) -> Option<String> {
    if tag_name(tag).as_deref() != Some("link") {
        return None;
    }
    let (rs, re) = find_attr_value_range(tag, "rel")?;
    if !tag[rs..re]
        .split_whitespace()
        .any(|w| w.eq_ignore_ascii_case("stylesheet"))
    {
        return None;
    }
    let (hs, he) = find_attr_value_range(tag, "href")?;
    let href = &tag[hs..he];
    href.starts_with('/').then(|| href.to_string())
}

fn inline_urls(
    css: &str,
    dir: &Path,
    read: &impl Fn(&str) -> Option<Vec<u8>>,
    warnings: &mut Vec<String>,
) -> String {
    let mut out = String::with_capacity(css.len());
    let mut rest = css;
    while let Some(idx) = rest.find("url(") {
        let after = &rest[idx + 4..];
        let Some(close) = after.find(')') else { break };
        out.push_str(&rest[..idx]);
        let raw = &after[..close];
        let reference = raw.trim().trim_matches(|c| c == '"' || c == '\'');
        match data_uri(reference, dir, read, warnings) {
            Some(uri) => out.push_str(&format!("url(\"{uri}\")")),
            None => out.push_str(&rest[idx..idx + 4 + close + 1]),
        }
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    out
}

fn data_uri(
    reference: &str,
    dir: &Path,
    read: &impl Fn(&str) -> Option<Vec<u8>>,
    warnings: &mut Vec<String>,
) -> Option<String> {
    if reference.is_empty()
        || reference.starts_with("data:")
        || reference.starts_with('#')
        || reference.starts_with("//")
        || reference.contains("://")
    {
        return None;
    }
    let file = reference.split(['?', '#']).next().unwrap_or(reference);
    let path = if file.starts_with('/') {
        Path::new(file).to_path_buf()
    } else {
        dir.join(file)
    };
    let path = path.to_string_lossy().to_string();
    let ext = Path::new(&path)
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let Some(mime) = asset_mime(&ext) else {
        warnings.push(format!(
            "canvas post: stylesheet asset has an unknown type, left as-is: {path}"
        ));
        return None;
    };
    let Some(bytes) = read(&path) else {
        warnings.push(format!(
            "canvas post: stylesheet asset not found, left as-is: {path}"
        ));
        return None;
    };
    if bytes.len() > MAX_ASSET_BYTES {
        warnings.push(format!(
            "canvas post: stylesheet asset over {} KB, left as-is: {path}",
            MAX_ASSET_BYTES / 1024
        ));
        return None;
    }
    Some(format!("data:{mime};base64,{}", base64(&bytes)))
}

fn asset_mime(ext: &str) -> Option<&'static str> {
    Some(match ext {
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "svg" => "image/svg+xml",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn files<'a>(entries: &'a [(&'a str, &'a [u8])]) -> impl Fn(&str) -> Option<Vec<u8>> + 'a {
        let map: HashMap<&str, &[u8]> = entries.iter().copied().collect();
        move |p: &str| map.get(p).map(|b| b.to_vec())
    }

    #[test]
    fn local_stylesheet_becomes_a_style_block() {
        let html = r#"<link rel="stylesheet" href="/p/tokens.css"><p>x</p>"#;
        let r = inline_stylesheets(html, files(&[("/p/tokens.css", b":root{--a:1}")]));
        assert_eq!(r.html, "<style>:root{--a:1}</style><p>x</p>");
        assert!(r.warnings.is_empty());
    }

    #[test]
    fn urls_inside_become_data_uris_relative_to_the_stylesheet() {
        let css =
            b"@font-face{src:url('fonts/a.woff2') format('woff2')}.b{background:url(/p/i.png)}";
        let r = inline_stylesheets(
            r#"<link href="/p/s.css" rel="stylesheet">"#,
            files(&[
                ("/p/s.css", css),
                ("/p/fonts/a.woff2", b"ab"),
                ("/p/i.png", b"abc"),
            ]),
        );
        assert!(r
            .html
            .contains(r#"url("data:font/woff2;base64,YWI=") format('woff2')"#));
        assert!(r.html.contains(r#"url("data:image/png;base64,YWJj")"#));
    }

    #[test]
    fn missing_files_warn_and_stay_as_written() {
        let r = inline_stylesheets(r#"<link rel="stylesheet" href="/nope.css">"#, files(&[]));
        assert_eq!(r.html, r#"<link rel="stylesheet" href="/nope.css">"#);
        assert_eq!(r.warnings.len(), 1);

        let r = inline_stylesheets(
            r#"<link rel="stylesheet" href="/p/s.css">"#,
            files(&[("/p/s.css", b".a{background:url(gone.png)}")]),
        );
        assert!(r.html.contains("url(gone.png)"));
        assert!(r.warnings[0].contains("/p/gone.png"));
    }

    #[test]
    fn urls_and_data_references_are_not_touched() {
        let css = b".a{background:url(https://x.test/a.png)}.b{background:url(data:image/png;base64,AA)}.c{fill:url(#g)}";
        let r = inline_stylesheets(
            r#"<link rel="stylesheet" href="/p/s.css"><link rel="stylesheet" href="https://x.test/c.css">"#,
            files(&[("/p/s.css", css)]),
        );
        assert!(r.html.contains("url(https://x.test/a.png)"));
        assert!(r.html.contains("url(data:image/png;base64,AA)"));
        assert!(r.html.contains("url(#g)"));
        assert!(r
            .html
            .contains(r#"<link rel="stylesheet" href="https://x.test/c.css">"#));
        assert!(r.warnings.is_empty());
    }

    #[test]
    fn an_oversized_asset_warns_and_stays_a_url() {
        let big = vec![0u8; MAX_ASSET_BYTES + 1];
        let r = inline_stylesheets(
            r#"<link rel="stylesheet" href="/p/s.css">"#,
            files(&[
                ("/p/s.css", b".a{background:url(big.png)}"),
                ("/p/big.png", &big),
            ]),
        );
        assert!(r.html.contains("url(big.png)"));
        assert!(r.warnings[0].contains("over 512 KB"));
    }
}
