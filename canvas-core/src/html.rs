//! Tag-level HTML scanning shared by the CLI's post-time rewrites and
//! canvasd's export: no parser, just quote-aware tag boundaries and
//! attribute lookups over the byte string.

/// Finds the next `<...>` tag at or after `from`, tracking quote state so a
/// `>` inside a quoted attribute value doesn't end the tag early. Returns
/// the byte range `[start, end)` including the angle brackets. A comment
/// is one range from `<!--` through `-->` (or the end of the HTML), so a
/// quote or tag inside it is never read. `None` past the last `<` or if a
/// tag is left unterminated.
pub fn next_tag(html: &str, from: usize) -> Option<(usize, usize)> {
    let bytes = html.as_bytes();
    let mut i = from;
    while i < bytes.len() {
        if bytes[i] == b'<' {
            if html[i..].starts_with("<!--") {
                // From the opener's dashes, so `<!-->` and `<!--->` end at once.
                let end = html[i + 2..]
                    .find("-->")
                    .map_or(html.len(), |e| i + 2 + e + 3);
                return Some((i, end));
            }
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

/// Every tag in `html`, in order, as [`next_tag`] ranges. The text of a
/// `<script>`, `<style>`, `<title>` or `<textarea>` element holds no tags,
/// so after its opening tag the scan resumes at its closing tag.
pub fn tags(html: &str) -> Tags<'_> {
    Tags { html, pos: 0 }
}

pub struct Tags<'a> {
    html: &'a str,
    pos: usize,
}

impl Tags<'_> {
    /// Resumes the scan at `pos`, past whatever the caller consumed itself.
    pub fn skip_to(&mut self, pos: usize) {
        self.pos = self.pos.max(pos);
    }
}

impl Iterator for Tags<'_> {
    type Item = (usize, usize);

    fn next(&mut self) -> Option<(usize, usize)> {
        let (start, end) = next_tag(self.html, self.pos)?;
        let tag = &self.html[start..end];
        self.pos = match tag_name(tag).as_deref() {
            // A self-closed one (SVG's `<title/>`) has no text to skip.
            Some(name @ ("script" | "style" | "title" | "textarea")) if !tag.ends_with("/>") => {
                raw_text_end(self.html, end, name)
            }
            _ => end,
        };
        Some((start, end))
    }
}

/// Where the text of a `<script>`, `<style>`, `<title>` or `<textarea>` element opened before
/// `from` ends: its closing tag, or the end of the HTML.
pub fn raw_text_end(html: &str, from: usize, name: &str) -> usize {
    html[from..]
        .to_ascii_lowercase()
        .find(&format!("</{name}"))
        .map(|i| from + i)
        .unwrap_or(html.len())
}

/// The lowercased tag name of an opening tag, e.g. `"img"` or `"a"`.
pub fn tag_name(tag: &str) -> Option<String> {
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
pub fn find_attr_value_range(tag: &str, attr: &str) -> Option<(usize, usize)> {
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

/// What names a card in a list or a page title: the text of its first
/// `<h1>`–`<h3>`, else its first line of visible text (script, style and
/// template contents skipped), whitespace collapsed. `None` for a card with
/// no text at all.
pub fn card_label(html: &str) -> Option<String> {
    first_heading(html).or_else(|| {
        visible_text(html)
            .lines()
            .map(collapse_whitespace)
            .find(|line| !line.is_empty())
    })
}

/// The text of the first `<h1>`–`<h3>`, tags stripped.
pub fn first_heading(html: &str) -> Option<String> {
    for (start, end) in tags(html) {
        let name = tag_name(&html[start..end]);
        if let Some(level @ ("h1" | "h2" | "h3")) = name.as_deref() {
            let close = format!("</{level}");
            let rest = &html[end..];
            let stop = rest.to_ascii_lowercase().find(&close)?;
            let text = collapse_whitespace(&strip_tags(&rest[..stop]));
            return (!text.is_empty()).then_some(text);
        }
    }
    None
}

/// `html` with every tag removed and the escapes [`escape_attr`] writes
/// decoded once.
pub fn strip_tags(html: &str) -> String {
    let mut out = String::new();
    let mut pos = 0;
    for (start, end) in tags(html) {
        out.push_str(&html[pos..start]);
        pos = end;
    }
    out.push_str(&html[pos..]);
    decode_entities(&out)
}

/// Tags that start a new line of text, opening or closing.
const BLOCK_TAGS: &[&str] = &[
    "address",
    "article",
    "aside",
    "blockquote",
    "br",
    "dd",
    "div",
    "dl",
    "dt",
    "figcaption",
    "figure",
    "footer",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "header",
    "hr",
    "li",
    "main",
    "nav",
    "ol",
    "p",
    "pre",
    "section",
    "table",
    "td",
    "th",
    "tr",
    "ul",
];

/// Like [`strip_tags`], but the contents of `<script>`, `<style>` and
/// `<template>` are dropped and each block tag starts a new line.
fn visible_text(html: &str) -> String {
    let mut out = String::new();
    let mut pos = 0;
    let mut scan = tags(html);
    while let Some((start, end)) = scan.next() {
        out.push_str(&html[pos..start]);
        pos = end;
        let tag = &html[start..end];
        let name = tag_name(&tag.replacen("</", "<", 1));
        match name.as_deref() {
            Some(name @ ("script" | "style" | "template")) if !tag.starts_with("</") => {
                let close = format!("</{name}");
                pos = html[end..]
                    .to_ascii_lowercase()
                    .find(&close)
                    .map_or(html.len(), |stop| end + stop);
                scan.skip_to(pos);
            }
            Some(name) if BLOCK_TAGS.contains(&name) => out.push('\n'),
            _ => {}
        }
    }
    if pos < html.len() {
        out.push_str(&html[pos..]);
    }
    decode_entities(&out)
}

fn decode_entities(s: &str) -> String {
    // `&amp;` last, so `&amp;lt;` decodes to `&lt;`, not `<`.
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

fn collapse_whitespace(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Text made safe to place between tags.
pub fn escape_text(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Text made safe to place inside a quoted attribute value.
pub fn escape_attr(s: &str) -> String {
    escape_text(s).replace('"', "&quot;").replace('\'', "&#39;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_skip_script_and_style_text() {
        let html =
            "<script src=a.js>i<n; s='x</SCRIPT><style>a<b{content:\"'\"}</style><img src=x>";
        let names: Vec<&str> = tags(html).map(|(s, e)| &html[s..e]).collect();
        assert_eq!(
            names,
            [
                "<script src=a.js>",
                "</SCRIPT>",
                "<style>",
                "</style>",
                "<img src=x>"
            ]
        );
    }

    #[test]
    fn a_comment_is_one_tag_whatever_it_holds() {
        let html = "<!-- don't <script> --><img src=x><title>a<b's</title><!--><!---><svg><title/><b><!-- open";
        let names: Vec<&str> = tags(html).map(|(s, e)| &html[s..e]).collect();
        assert_eq!(
            names,
            [
                "<!-- don't <script> -->",
                "<img src=x>",
                "<title>",
                "</title>",
                "<!-->",
                "<!--->",
                "<svg>",
                "<title/>",
                "<b>",
                "<!-- open"
            ]
        );
    }

    #[test]
    fn card_label_prefers_the_first_heading() {
        assert_eq!(
            card_label("<p>intro</p><h2>The <b>plan</b>\n now</h2>").as_deref(),
            Some("The plan now")
        );
    }

    #[test]
    fn card_label_falls_back_to_the_first_visible_line() {
        assert_eq!(
            card_label(
                "<style>p{}</style><script>let a = 1;</script>\n<p>  first   line</p>\n<p>second</p>"
            )
            .as_deref(),
            Some("first line")
        );
        assert_eq!(
            card_label("<p>a &amp;lt; b</p>").as_deref(),
            Some("a &lt; b")
        );
        assert_eq!(card_label("<p>one</p><p>two</p>").as_deref(), Some("one"));
        assert_eq!(card_label("<img src=x><script>x</script>"), None);
    }
}
