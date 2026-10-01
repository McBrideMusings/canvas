//! Pure scan over converted post HTML: finds `<img src>` and `<a href>`
//! attributes that name a local file or a link worth making clickable
//! inside the sandboxed iframe, and rewrites them to placeholders the
//! daemon and viewer resolve. No I/O of its own — callers inject an
//! `exists` predicate, which is the test seam.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scanned {
    pub html: String,
    pub images: Vec<String>,
    pub targets: Vec<String>,
    pub warnings: Vec<String>,
}

/// Scans `html` for `<img src="...">` and `<a href="...">` attributes.
///
/// - An `<img src>` that is an absolute path (`/...`) for which `exists`
///   returns true is appended to `images` and its `src` rewritten to
///   `canvas-image:<n>` (`n` = its index in `images`), a placeholder
///   canvasd resolves to `/api/cards/<id>/images/<n>` on card creation.
/// - An `<a href>` that is an absolute path for which `exists` returns
///   true, or that starts with `http://`/`https://`, is appended to
///   `targets` and its `href` rewritten to `#canvas-open-<n>` (`n` = its
///   index in `targets`), which the viewer's click bridge turns into an
///   open request.
/// - An `<a>` to an existing local file whose only content is an existing
///   local `<img>` is dropped first, leaving the `<img>`, so a click on that
///   image opens the viewer's lightbox and not the file.
/// - An absolute path (img or a) that does not exist produces one warning
///   string and is left untouched.
/// - Everything else — relative paths, `data:`, other schemes, non-img/a
///   tags — is left untouched.
///
/// Document order is preserved as index order in both `images` and
/// `targets`.
pub fn scan(html: &str, exists: impl Fn(&str) -> bool) -> Scanned {
    let unwrapped = unwrap_image_links(html, &exists);
    let html = unwrapped.as_str();
    let mut out = String::with_capacity(html.len());
    let mut images = Vec::new();
    let mut targets = Vec::new();
    let mut warnings = Vec::new();
    let mut pos = 0usize;

    while let Some((tag_start, tag_end)) = next_tag(html, pos) {
        out.push_str(&html[pos..tag_start]);
        let tag = &html[tag_start..tag_end];
        let name = if tag.starts_with("</") {
            None
        } else {
            tag_name(tag)
        };

        match name.as_deref() {
            Some("img") => match find_attr_value_range(tag, "src") {
                Some((vs, ve)) => {
                    let value = &tag[vs..ve];
                    if value.starts_with('/') {
                        if exists(value) {
                            let idx = images.len();
                            images.push(value.to_string());
                            // Written as exactly `src="canvas-image:<n>"`, with no
                            // whitespace around `=`: canvasd matches that one shape.
                            let quote = &tag[vs - 1..vs];
                            let before_quote = tag[..vs - 1].trim_end();
                            let name_end = before_quote
                                .strip_suffix('=')
                                .unwrap_or(before_quote)
                                .trim_end();
                            out.push_str(name_end);
                            out.push('=');
                            out.push_str(quote);
                            out.push_str(&format!("canvas-image:{idx}"));
                            out.push_str(&tag[ve..]);
                        } else {
                            warnings
                                .push(format!("canvas post: image not found, left as-is: {value}"));
                            out.push_str(tag);
                        }
                    } else {
                        out.push_str(tag);
                    }
                }
                None => out.push_str(tag),
            },
            Some("a") => match find_attr_value_range(tag, "href") {
                Some((vs, ve)) => {
                    let value = &tag[vs..ve];
                    let is_url = value.starts_with("http://") || value.starts_with("https://");
                    let is_abs_path = value.starts_with('/');
                    if is_url || (is_abs_path && exists(value)) {
                        let idx = targets.len();
                        targets.push(value.to_string());
                        out.push_str(&tag[..vs]);
                        out.push_str(&format!("#canvas-open-{idx}"));
                        out.push_str(&tag[ve..]);
                    } else if is_abs_path {
                        warnings.push(format!("canvas post: path not found, left as-is: {value}"));
                        out.push_str(tag);
                    } else {
                        out.push_str(tag);
                    }
                }
                None => out.push_str(tag),
            },
            _ => out.push_str(tag),
        }

        pos = tag_end;
    }
    out.push_str(&html[pos..]);

    Scanned {
        html: out,
        images,
        targets,
        warnings,
    }
}

/// Removes the `<a>` around an existing local image when that link points at
/// an existing local file and holds nothing but the image: the viewer opens a
/// linked image's file instead of its lightbox, and a click on a card image
/// should always open the lightbox.
fn unwrap_image_links(html: &str, exists: &impl Fn(&str) -> bool) -> String {
    let mut out = String::with_capacity(html.len());
    let mut pos = 0usize;
    while let Some((start, end)) = next_tag(html, pos) {
        let tag = &html[start..end];
        if tag_name(tag).as_deref() == Some("a") && has_existing_local(tag, "href", exists) {
            if let Some((img_start, img_end, close_end)) = sole_image_in_link(html, end, exists) {
                out.push_str(&html[pos..start]);
                out.push_str(&html[img_start..img_end]);
                pos = close_end;
                continue;
            }
        }
        out.push_str(&html[pos..end]);
        pos = end;
    }
    out.push_str(&html[pos..]);
    out
}

/// After an `<a>` opening tag ending at `after_open`, the byte ranges of the
/// image tag and the end of `</a>`, when the link holds only an existing
/// local `<img>` (whitespace aside).
fn sole_image_in_link(
    html: &str,
    after_open: usize,
    exists: &impl Fn(&str) -> bool,
) -> Option<(usize, usize, usize)> {
    let (img_start, img_end) = next_tag(html, after_open)?;
    if !html[after_open..img_start].trim().is_empty() {
        return None;
    }
    let img = &html[img_start..img_end];
    if tag_name(img).as_deref() != Some("img") || !has_existing_local(img, "src", exists) {
        return None;
    }
    let (close_start, close_end) = next_tag(html, img_end)?;
    if !html[img_end..close_start].trim().is_empty()
        || !html[close_start..close_end].eq_ignore_ascii_case("</a>")
    {
        return None;
    }
    Some((img_start, img_end, close_end))
}

fn has_existing_local(tag: &str, attr: &str, exists: &impl Fn(&str) -> bool) -> bool {
    find_attr_value_range(tag, attr).is_some_and(|(vs, ve)| {
        let value = &tag[vs..ve];
        value.starts_with('/') && exists(value)
    })
}

/// Finds the next `<...>` tag at or after `from`, tracking quote state so a
/// `>` inside a quoted attribute value doesn't end the tag early. Returns
/// the byte range `[start, end)` including the angle brackets. `None` past
/// the last `<` or if a tag is left unterminated.
fn next_tag(html: &str, from: usize) -> Option<(usize, usize)> {
    let bytes = html.as_bytes();
    let mut i = from;
    while i < bytes.len() {
        if bytes[i] == b'<' {
            let mut j = i + 1;
            let mut in_quote: Option<u8> = None;
            while j < bytes.len() {
                let c = bytes[j];
                match in_quote {
                    Some(q) => {
                        if c == q {
                            in_quote = None;
                        }
                    }
                    None => {
                        if c == b'"' || c == b'\'' {
                            in_quote = Some(c);
                        } else if c == b'>' {
                            return Some((i, j + 1));
                        }
                    }
                }
                j += 1;
            }
            return None;
        }
        i += 1;
    }
    None
}

/// The lowercased tag name of an opening tag, e.g. `"img"` or `"a"`.
fn tag_name(tag: &str) -> Option<String> {
    let inner = tag.strip_prefix('<')?;
    let end = inner
        .find(|c: char| c.is_whitespace() || c == '>' || c == '/')
        .unwrap_or(inner.len());
    if end == 0 {
        return None;
    }
    Some(inner[..end].to_ascii_lowercase())
}

/// Finds `name="value"` or `name='value'` (case-insensitive attribute name,
/// whitespace-tolerant around `=`) inside `tag`, and returns the byte range
/// of `value` (excluding the quotes), both relative to `tag`. The attribute
/// name must be preceded by whitespace, so `xsrc="..."` never matches `src`.
fn find_attr_value_range(tag: &str, attr: &str) -> Option<(usize, usize)> {
    let lower = tag.to_ascii_lowercase();
    let bytes = tag.as_bytes();
    let mut search_from = 0usize;

    while let Some(rel) = lower[search_from..].find(attr) {
        let abs = search_from + rel;
        let preceded_by_boundary = abs > 0 && bytes[abs - 1].is_ascii_whitespace();
        if preceded_by_boundary {
            let mut i = abs + attr.len();
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            if i < bytes.len() && bytes[i] == b'=' {
                i += 1;
                while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                    i += 1;
                }
                if i < bytes.len() && (bytes[i] == b'"' || bytes[i] == b'\'') {
                    let quote = bytes[i];
                    let val_start = i + 1;
                    if let Some(rel_end) = tag[val_start..].find(quote as char) {
                        return Some((val_start, val_start + rel_end));
                    }
                }
            }
        }
        search_from = abs + attr.len();
        if search_from >= lower.len() {
            break;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn exists_in<'a>(paths: &'a [&'a str]) -> impl Fn(&str) -> bool + 'a {
        let set: HashSet<&str> = paths.iter().copied().collect();
        move |p: &str| set.contains(p)
    }

    #[test]
    fn existing_local_image_is_rewritten_and_recorded() {
        let html = r#"<p><img src="/abs/x.png" alt=""></p>"#;
        let s = scan(html, exists_in(&["/abs/x.png"]));
        assert_eq!(s.images, vec!["/abs/x.png".to_string()]);
        assert!(s.warnings.is_empty());
        assert!(s.html.contains(r#"src="canvas-image:0""#));
        assert!(!s.html.contains("/abs/x.png"));
    }

    #[test]
    fn image_placeholder_has_no_whitespace_around_equals() {
        let html = r#"<img src = '/abs/x.png'>"#;
        let s = scan(html, exists_in(&["/abs/x.png"]));
        assert_eq!(s.html, "<img src='canvas-image:0'>");
    }

    #[test]
    fn existing_local_link_is_rewritten_and_recorded() {
        let html = r#"<a href="/abs/file">f</a>"#;
        let s = scan(html, exists_in(&["/abs/file"]));
        assert_eq!(s.targets, vec!["/abs/file".to_string()]);
        assert!(s.html.contains(r##"href="#canvas-open-0""##));
    }

    #[test]
    fn link_holding_only_a_local_image_is_unwrapped() {
        let html = "<figure><a href=\"/abs/x.png\">\n <img src=\"/abs/x.png\" width=\"190\">\n</a></figure>";
        let s = scan(html, exists_in(&["/abs/x.png"]));
        assert_eq!(
            s.html,
            "<figure><img src=\"canvas-image:0\" width=\"190\"></figure>"
        );
        assert_eq!(s.images, vec!["/abs/x.png".to_string()]);
        assert!(s.targets.is_empty());
    }

    #[test]
    fn links_with_other_content_or_a_url_keep_their_image() {
        let html = concat!(
            r#"<a href="/abs/x.png"><img src="/abs/x.png"> caption</a>"#,
            r#"<a href="https://example.com"><img src="/abs/x.png"></a>"#,
        );
        let s = scan(html, exists_in(&["/abs/x.png"]));
        assert_eq!(s.targets.len(), 2);
        assert_eq!(s.images.len(), 2);
    }

    #[test]
    fn http_and_https_links_are_recorded_without_an_existence_check() {
        let html = r#"<a href="https://example.com">e</a><a href="http://x.test">x</a>"#;
        let s = scan(html, exists_in(&[]));
        assert_eq!(
            s.targets,
            vec![
                "https://example.com".to_string(),
                "http://x.test".to_string()
            ]
        );
        assert!(s.html.contains(r##"href="#canvas-open-0""##));
        assert!(s.html.contains(r##"href="#canvas-open-1""##));
    }

    #[test]
    fn missing_local_image_warns_and_is_untouched() {
        let html = r#"<img src="/nope.png">"#;
        let s = scan(html, exists_in(&[]));
        assert!(s.images.is_empty());
        assert_eq!(s.warnings.len(), 1);
        assert!(s.warnings[0].contains("/nope.png"));
        assert_eq!(s.html, html);
    }

    #[test]
    fn missing_local_link_warns_and_is_untouched() {
        let html = r#"<a href="/nope/file">f</a>"#;
        let s = scan(html, exists_in(&[]));
        assert!(s.targets.is_empty());
        assert_eq!(s.warnings.len(), 1);
        assert_eq!(s.html, html);
    }

    #[test]
    fn relative_paths_and_other_schemes_are_untouched() {
        let html = concat!(
            r#"<img src="relative.png">"#,
            r#"<img src="data:image/png;base64,AAA">"#,
            r#"<a href="relative/page">r</a>"#,
            r#"<a href="mailto:x@y.test">m</a>"#,
            r#"<a href="ftp://host/f">ftp</a>"#,
        );
        let s = scan(html, exists_in(&[]));
        assert!(s.images.is_empty());
        assert!(s.targets.is_empty());
        assert!(s.warnings.is_empty());
        assert_eq!(s.html, html);
    }

    #[test]
    fn document_order_is_preserved_across_mixed_content() {
        let html = concat!(
            r#"<a href="/a1">1</a>"#,
            r#"<img src="/i1.png">"#,
            r#"<a href="https://example.com">2</a>"#,
            r#"<img src="/i2.png">"#,
        );
        let s = scan(html, exists_in(&["/a1", "/i1.png", "/i2.png"]));
        assert_eq!(s.images, vec!["/i1.png".to_string(), "/i2.png".to_string()]);
        assert_eq!(
            s.targets,
            vec!["/a1".to_string(), "https://example.com".to_string()]
        );
    }

    #[test]
    fn single_quoted_attributes_are_handled() {
        let html = r#"<img src='/abs/x.png'>"#;
        let s = scan(html, exists_in(&["/abs/x.png"]));
        assert_eq!(s.images, vec!["/abs/x.png".to_string()]);
        assert!(
            s.html.contains("src='canvas-image:0'") || s.html.contains(r#"src="canvas-image:0""#)
        );
    }

    #[test]
    fn uppercase_tag_and_attribute_names_are_handled() {
        let html = r#"<IMG SRC="/abs/x.png">"#;
        let s = scan(html, exists_in(&["/abs/x.png"]));
        assert_eq!(s.images, vec!["/abs/x.png".to_string()]);
    }
}
