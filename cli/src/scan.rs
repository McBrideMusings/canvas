//! Pure scan over converted post HTML: finds `<img src>`, `<video src>`,
//! `<source src>` and `<a href>` attributes that name a local file or a link worth making clickable
//! inside the sandboxed iframe, and rewrites them to placeholders the
//! daemon and viewer resolve. It also finds media files a post names only as
//! text, for the hints `canvas post` returns. No I/O of its own — callers
//! inject an `exists` predicate, which is the test seam, and the home folder
//! a `~/` path expands to.

use canvas_core::html::{
    decode_entities, find_attr_value, next_tag, replace_attr_value, tag_name, tags, Tag,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scanned {
    pub html: String,
    pub images: Vec<String>,
    pub targets: Vec<String>,
    pub warnings: Vec<String>,
    /// One per existing media file the post names as plain text.
    pub hints: Vec<String>,
}

/// The extensions a path in text needs to earn a hint.
const MEDIA_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "webp", "apng", "svg", "avif", "mp4", "webm", "mov",
];

/// The hints for a `--format text` post, checked on its raw input, which
/// becomes one `<pre>` the HTML scan skips.
pub fn text_hints(input: &str, home: Option<&str>, exists: impl Fn(&str) -> bool) -> Vec<String> {
    media_paths(input, home, &exists, &mut Vec::new())
        .into_iter()
        .map(|(written, abs)| {
            let markup = media_markup(&abs);
            format!("\"{written}\" shows as plain text; post as Markdown with {markup}")
        })
        .collect()
}

/// The markup that shows `abs`: a Markdown image, or for a video the
/// `<video>` tag Markdown passes through, since an image of a video is a
/// broken image.
fn media_markup(abs: &str) -> String {
    let is_video = abs.rsplit_once('.').is_some_and(|(_, ext)| {
        ["mp4", "webm", "mov"]
            .iter()
            .any(|v| v.eq_ignore_ascii_case(ext))
    });
    if is_video {
        format!("<video src=\"{abs}\" controls></video>")
    } else {
        format!("![]({abs})")
    }
}

/// Each existing media file `text` names by an absolute or `~/` path, as
/// written and as the absolute path it resolves to, skipping one already in
/// `seen` (which it joins).
fn media_paths(
    text: &str,
    home: Option<&str>,
    exists: &impl Fn(&str) -> bool,
    seen: &mut Vec<String>,
) -> Vec<(String, String)> {
    let is_end = |c: char| c.is_whitespace() || "\"'`<>()[]{}|,;".contains(c);
    let mut found = Vec::new();
    let mut prev: Option<char> = None;
    for (i, c) in text.char_indices() {
        let at_boundary = prev.is_none_or(|p| p.is_whitespace() || "([{\"'`<".contains(p));
        prev = Some(c);
        let rest = &text[i..];
        if !at_boundary || !(rest.starts_with('/') || rest.starts_with("~/")) {
            continue;
        }
        let len = rest.find(is_end).unwrap_or(rest.len());
        let written = rest[..len].trim_end_matches(['.', ':', '!', '?']);
        if !has_media_extension(written) {
            continue;
        }
        let abs = match written.strip_prefix("~/") {
            Some(tail) => match home {
                Some(home) => format!("{}/{tail}", home.trim_end_matches('/')),
                None => continue,
            },
            None => written.to_string(),
        };
        if !seen.contains(&abs) && exists(&abs) {
            seen.push(abs.clone());
            found.push((written.to_string(), abs));
        }
    }
    found
}

fn has_media_extension(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or("");
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => {
            MEDIA_EXTENSIONS.iter().any(|m| m.eq_ignore_ascii_case(ext))
        }
        _ => false,
    }
}

/// Scans `html` for `<img src="...">`, `<video src="...">`, `<source src="...">`
/// and `<a href="...">` attributes.
///
/// - An `<img>`, `<video>` or `<source>` `src` that is an absolute path (`/...`) for which `exists`
///   returns true is appended to `images` and its `src` rewritten to
///   `canvas-image:<n>` (`n` = its index in `images`), a placeholder
///   canvasd resolves to `/api/cards/<id>/images/<n>` on card creation. A
///   video shares that list and route, so one mechanism serves both.
/// - An `<a href>` that is an absolute path for which `exists` returns
///   true, or that starts with `http://`/`https://`, is appended to
///   `targets` and its `href` rewritten to `#canvas-open-<n>` (`n` = its
///   index in `targets`), which the viewer's click bridge turns into an
///   open request.
/// - An `<a>` to an existing local file whose only content is an existing
///   local `<img>` is dropped first, leaving the `<img>`, so a click on that
///   image opens the viewer's lightbox and not the file.
/// - An absolute path (img, video, source or a) that does not exist produces one warning
///   string and is left untouched.
/// - Everything else — relative paths, `data:`, other schemes, non-img/a
///   tags — is left untouched.
///
/// - An existing media file (an absolute or `~/` path, `home` expanding the
///   `~`, with an extension in `MEDIA_EXTENSIONS`) named in text produces one
///   hint, once per file. Text inside `<pre>`, `<a>` or a raw-text element
///   such as `<script>` is not checked; inline `<code>` is.
///
/// Document order is preserved as index order in both `images` and
/// `targets`.
pub fn scan(html: &str, home: Option<&str>, exists: impl Fn(&str) -> bool) -> Scanned {
    let unwrapped = unwrap_image_links(html, &exists);
    let html = unwrapped.as_str();
    let mut out = String::with_capacity(html.len());
    let mut images = Vec::new();
    let mut targets = Vec::new();
    let mut warnings = Vec::new();
    let mut hints = Vec::new();
    // A file the post already shows needs no hint where its path is also text.
    let mut seen: Vec<String> = tags(html)
        .filter_map(|t| {
            let tag = &html[t.start..t.end];
            let name = tag_name(tag)?;
            let src = find_attr_value(tag, "src")
                .filter(|_| matches!(name.as_str(), "img" | "video" | "source"))?;
            let value = &tag[src.range()];
            (value.starts_with('/') && exists(value)).then(|| value.to_string())
        })
        .collect();
    let mut pos = 0usize;
    // The `<pre>` and `<a>` elements still open, whose text is never checked,
    // and whether the text since the last tag is a raw-text element's.
    let mut unchecked: Vec<usize> = Vec::new();
    let mut raw_text = false;
    let mut check = |text: &str, hints: &mut Vec<String>| {
        for (written, abs) in media_paths(&decode_entities(text), home, &exists, &mut seen) {
            let markup = media_markup(&abs);
            hints.push(format!(
                "\"{written}\" shows as plain text; write {markup} to show it"
            ));
        }
    };

    let mut walk = tags(html);
    while let Some(Tag {
        start: tag_start,
        end: tag_end,
        text,
        opened,
        ..
    }) = walk.next()
    {
        if !raw_text && unchecked.is_empty() {
            check(&html[pos..tag_start], &mut hints);
        }
        out.push_str(&html[pos..tag_start]);
        let tag = &html[tag_start..tag_end];
        let name = if tag.starts_with("</") {
            None
        } else {
            tag_name(tag)
        };

        match name.as_deref() {
            Some(tag_kind @ ("img" | "video" | "source")) => {
                match find_attr_value(tag, "src") {
                    Some(src) => {
                        let value = &tag[src.range()];
                        if value.starts_with('/') {
                            if exists(value) {
                                let idx = images.len();
                                images.push(value.to_string());
                                // Written as exactly `src="canvas-image:<n>"`, with no
                                // whitespace around `=` and an unquoted value given
                                // double quotes: canvasd matches that one shape.
                                let quote = src.quote.unwrap_or('"');
                                let before_value = tag[..src.outer().start].trim_end();
                                let name_end = before_value
                                    .strip_suffix('=')
                                    .unwrap_or(before_value)
                                    .trim_end();
                                out.push_str(name_end);
                                out.push('=');
                                out.push(quote);
                                out.push_str(&format!("canvas-image:{idx}"));
                                out.push(quote);
                                out.push_str(&tag[src.outer().end..]);
                            } else {
                                let what = if tag_kind == "img" { "image" } else { "video" };
                                warnings.push(format!(
                                    "canvas post: {what} not found, left as-is: {value}"
                                ));
                                out.push_str(tag);
                            }
                        } else {
                            out.push_str(tag);
                        }
                    }
                    None => out.push_str(tag),
                }
            }
            Some("a") => match find_attr_value(tag, "href") {
                Some(href) => {
                    let value = &tag[href.range()];
                    let is_url = value.starts_with("http://") || value.starts_with("https://");
                    let is_abs_path = value.starts_with('/');
                    if is_url || (is_abs_path && exists(value)) {
                        let idx = targets.len();
                        targets.push(value.to_string());
                        out.push_str(&replace_attr_value(
                            tag,
                            href,
                            &format!("#canvas-open-{idx}"),
                        ));
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

        unchecked.retain(|id| walk.is_open(*id));
        if let (Some(id), Some("pre" | "a")) = (opened, name.as_deref()) {
            unchecked.push(id);
        }
        raw_text = text.is_some();
        pos = tag_end;
    }
    if !raw_text && unchecked.is_empty() {
        check(&html[pos..], &mut hints);
    }
    out.push_str(&html[pos..]);

    Scanned {
        html: out,
        images,
        targets,
        warnings,
        hints,
    }
}

/// Removes the `<a>` around an existing local image when that link points at
/// an existing local file and holds nothing but the image: the viewer opens a
/// linked image's file instead of its lightbox, and a click on a card image
/// should always open the lightbox.
fn unwrap_image_links(html: &str, exists: &impl Fn(&str) -> bool) -> String {
    let mut out = String::with_capacity(html.len());
    let mut pos = 0usize;
    let mut walk = tags(html);
    while let Some(Tag { start, end, .. }) = walk.next() {
        let tag = &html[start..end];
        if tag_name(tag).as_deref() == Some("a") && has_existing_local(tag, "href", exists) {
            if let Some((img_start, img_end, close_end)) = sole_image_in_link(html, end, exists) {
                out.push_str(&html[pos..start]);
                out.push_str(&html[img_start..img_end]);
                pos = close_end;
                walk.skip_to(close_end);
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
    find_attr_value(tag, attr).is_some_and(|v| {
        let value = &tag[v.range()];
        value.starts_with('/') && exists(value)
    })
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
        let s = scan(html, None, exists_in(&["/abs/x.png"]));
        assert_eq!(s.images, vec!["/abs/x.png".to_string()]);
        assert!(s.warnings.is_empty());
        assert!(s.html.contains(r#"src="canvas-image:0""#));
        assert!(!s.html.contains("/abs/x.png"));
    }

    #[test]
    fn script_text_that_looks_like_a_tag_does_not_stop_the_scan() {
        let html = "<script>if(i<n)s='it\\'s';</script><img src=\"/abs/x.png\">";
        let s = scan(html, None, exists_in(&["/abs/x.png"]));
        assert_eq!(s.images, vec!["/abs/x.png".to_string()]);
        assert!(s.html.starts_with("<script>if(i<n)s='it\\'s';</script>"));
        assert!(s.html.contains(r#"src="canvas-image:0""#));
    }

    #[test]
    fn a_quote_in_text_does_not_stop_the_scan() {
        let html = "a < b's <img src=\"/abs/x.png\">";
        let s = scan(html, None, exists_in(&["/abs/x.png"]));
        assert_eq!(s.images, vec!["/abs/x.png".to_string()]);
        assert!(s.html.contains(r#"src="canvas-image:0""#));
    }

    #[test]
    fn existing_local_video_and_source_share_the_image_list() {
        let html = r#"<video src="/abs/a.webm" controls></video><video><source src="/abs/b.mp4" type="video/mp4"></video><img src="/abs/x.png">"#;
        let s = scan(
            html,
            None,
            exists_in(&["/abs/a.webm", "/abs/b.mp4", "/abs/x.png"]),
        );
        assert_eq!(s.images, vec!["/abs/a.webm", "/abs/b.mp4", "/abs/x.png"]);
        assert!(s.html.contains(r#"<video src="canvas-image:0" controls>"#));
        assert!(s
            .html
            .contains(r#"<source src="canvas-image:1" type="video/mp4">"#));
    }

    #[test]
    fn missing_local_video_warns_and_is_untouched() {
        let html = r#"<video src="/nope.webm"></video>"#;
        let s = scan(html, None, exists_in(&[]));
        assert!(s.images.is_empty());
        assert_eq!(s.warnings.len(), 1);
        assert!(s.warnings[0].contains("video not found"));
        assert_eq!(s.html, html);
    }

    #[test]
    fn image_placeholder_has_no_whitespace_around_equals() {
        let html = r#"<img src = '/abs/x.png'>"#;
        let s = scan(html, None, exists_in(&["/abs/x.png"]));
        assert_eq!(s.html, "<img src='canvas-image:0'>");
    }

    #[test]
    fn unquoted_local_image_and_link_are_rewritten_in_double_quotes() {
        let html = "<img src=/abs/x.png alt=x><img src = /abs/x.png/><a href=/abs/file>f</a>";
        let s = scan(
            html,
            None,
            exists_in(&["/abs/x.png", "/abs/x.png/", "/abs/file"]),
        );
        assert_eq!(s.images, vec!["/abs/x.png", "/abs/x.png/"]);
        assert_eq!(s.targets, vec!["/abs/file"]);
        assert_eq!(
            s.html,
            r##"<img src="canvas-image:0" alt=x><img src="canvas-image:1"><a href="#canvas-open-0">f</a>"##
        );
    }

    #[test]
    fn existing_local_link_is_rewritten_and_recorded() {
        let html = r#"<a href="/abs/file">f</a>"#;
        let s = scan(html, None, exists_in(&["/abs/file"]));
        assert_eq!(s.targets, vec!["/abs/file".to_string()]);
        assert!(s.html.contains(r##"href="#canvas-open-0""##));
    }

    #[test]
    fn link_holding_only_a_local_image_is_unwrapped() {
        let html = "<figure><a href=\"/abs/x.png\">\n <img src=\"/abs/x.png\" width=\"190\">\n</a></figure>";
        let s = scan(html, None, exists_in(&["/abs/x.png"]));
        assert_eq!(
            s.html,
            "<figure><img src=\"canvas-image:0\" width=\"190\"></figure>"
        );
        assert_eq!(s.images, vec!["/abs/x.png".to_string()]);
        assert!(s.targets.is_empty());
    }

    #[test]
    fn a_comment_in_a_link_keeps_its_image_and_the_next_link_unwraps() {
        let html = concat!(
            r#"<a href="/abs/x.png"><!-- it's --><img src="/abs/x.png"></a>"#,
            r#"<a href="/abs/x.png"><img src="/abs/x.png"></a>"#,
        );
        assert_eq!(
            unwrap_image_links(html, &exists_in(&["/abs/x.png"])),
            concat!(
                r#"<a href="/abs/x.png"><!-- it's --><img src="/abs/x.png"></a>"#,
                r#"<img src="/abs/x.png">"#,
            )
        );
    }

    #[test]
    fn links_with_other_content_or_a_url_keep_their_image() {
        let html = concat!(
            r#"<a href="/abs/x.png"><img src="/abs/x.png"> caption</a>"#,
            r#"<a href="https://example.com"><img src="/abs/x.png"></a>"#,
        );
        let s = scan(html, None, exists_in(&["/abs/x.png"]));
        assert_eq!(s.targets.len(), 2);
        assert_eq!(s.images.len(), 2);
    }

    #[test]
    fn http_and_https_links_are_recorded_without_an_existence_check() {
        let html = r#"<a href="https://example.com">e</a><a href="http://x.test">x</a>"#;
        let s = scan(html, None, exists_in(&[]));
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
        let s = scan(html, None, exists_in(&[]));
        assert!(s.images.is_empty());
        assert_eq!(s.warnings.len(), 1);
        assert!(s.warnings[0].contains("/nope.png"));
        assert_eq!(s.html, html);
    }

    #[test]
    fn missing_local_link_warns_and_is_untouched() {
        let html = r#"<a href="/nope/file">f</a>"#;
        let s = scan(html, None, exists_in(&[]));
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
        let s = scan(html, None, exists_in(&[]));
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
        let s = scan(html, None, exists_in(&["/a1", "/i1.png", "/i2.png"]));
        assert_eq!(s.images, vec!["/i1.png".to_string(), "/i2.png".to_string()]);
        assert_eq!(
            s.targets,
            vec!["/a1".to_string(), "https://example.com".to_string()]
        );
    }

    #[test]
    fn single_quoted_attributes_are_handled() {
        let html = r#"<img src='/abs/x.png'>"#;
        let s = scan(html, None, exists_in(&["/abs/x.png"]));
        assert_eq!(s.images, vec!["/abs/x.png".to_string()]);
        assert!(
            s.html.contains("src='canvas-image:0'") || s.html.contains(r#"src="canvas-image:0""#)
        );
    }

    #[test]
    fn a_bare_media_path_in_text_gets_one_hint() {
        let html = "<p>Saved /abs/shot.png. Also /abs/shot.png and ~/clip.webm</p>";
        let s = scan(
            html,
            Some("/Users/me"),
            exists_in(&["/abs/shot.png", "/Users/me/clip.webm"]),
        );
        assert_eq!(
            s.hints,
            vec![
                "\"/abs/shot.png\" shows as plain text; write ![](/abs/shot.png) to show it",
                "\"~/clip.webm\" shows as plain text; write <video src=\"/Users/me/clip.webm\" controls></video> to show it",
            ]
        );
        assert_eq!(s.html, html);
    }

    #[test]
    fn inline_code_is_checked_and_pre_is_skipped() {
        let html = "<pre><code>/abs/a.png</code></pre><p><code>/abs/b.png</code></p>";
        let s = scan(html, None, exists_in(&["/abs/a.png", "/abs/b.png"]));
        assert_eq!(s.hints.len(), 1);
        assert!(s.hints[0].starts_with("\"/abs/b.png\""), "{:?}", s.hints);
    }

    #[test]
    fn shown_linked_and_script_paths_get_no_hint() {
        let html = concat!(
            r#"<img src="/abs/a.png" alt="/abs/a.png">"#,
            r#"<a href="/abs/a.png">/abs/a.png</a>"#,
            r#"<script>const s = "/abs/a.png";</script>"#,
        );
        let s = scan(html, None, exists_in(&["/abs/a.png"]));
        assert!(s.hints.is_empty(), "{:?}", s.hints);
    }

    #[test]
    fn a_path_the_post_already_shows_gets_no_hint() {
        let html = r#"<p>Saved to /abs/a.png</p><img src="/abs/a.png">"#;
        let s = scan(html, None, exists_in(&["/abs/a.png"]));
        assert!(s.hints.is_empty(), "{:?}", s.hints);
    }

    #[test]
    fn a_missing_file_or_other_extension_gets_no_hint() {
        let html = "<p>/abs/gone.png /abs/notes.md /abs/x.PNGs https://x.test/abs/a.png</p>";
        let s = scan(
            html,
            None,
            exists_in(&["/abs/notes.md", "/abs/x.PNGs", "/abs/a.png"]),
        );
        assert!(s.hints.is_empty(), "{:?}", s.hints);
    }

    #[test]
    fn a_text_post_hint_says_to_post_markdown() {
        let hints = text_hints("shot at /abs/s.JPG\n", None, exists_in(&["/abs/s.JPG"]));
        assert_eq!(
            hints,
            vec!["\"/abs/s.JPG\" shows as plain text; post as Markdown with ![](/abs/s.JPG)"]
        );
    }

    #[test]
    fn uppercase_tag_and_attribute_names_are_handled() {
        let html = r#"<IMG SRC="/abs/x.png">"#;
        let s = scan(html, None, exists_in(&["/abs/x.png"]));
        assert_eq!(s.images, vec!["/abs/x.png".to_string()]);
    }
}
