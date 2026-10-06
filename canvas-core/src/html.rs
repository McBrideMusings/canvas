//! Tag-level HTML scanning shared by the CLI's post-time rewrites and
//! canvasd's export: no parser, just quote-aware tag boundaries and
//! attribute lookups over the byte string.

/// Finds the next `<...>` tag at or after `from`, tracking quote state so a
/// `>` inside a quoted attribute value doesn't end the tag early. Tag
/// boundaries follow the browser's tokenizer: `<` opens a tag only before a
/// letter, `/`, `!` or `?` (so the `<` in `a < b` is text), and a quote opens
/// a value only where a value starts, after the `=` that ends an attribute
/// name (so the `'` in `<b's>` or `<a href=p?q='x'>` is just a character).
/// `<!` and `<?` end at the first `>`. Returns the byte range `[start, end)`
/// including the angle brackets. A comment is one range from `<!--` through
/// `-->` (or the end of the HTML), so a quote or tag inside it is never read.
/// `None` past the last tag or if a tag is left unterminated.
pub fn next_tag(html: &str, from: usize) -> Option<(usize, usize)> {
    let bytes = html.as_bytes();
    let mut i = from;
    while i < bytes.len() {
        if bytes[i] == b'<'
            && bytes
                .get(i + 1)
                .is_some_and(|&c| c.is_ascii_alphabetic() || matches!(c, b'/' | b'!' | b'?'))
        {
            if html[i..].starts_with("<!--") {
                // From the opener's dashes, so `<!-->` and `<!--->` end at once.
                let end = html[i + 2..]
                    .find("-->")
                    .map_or(html.len(), |e| i + 2 + e + 3);
                return Some((i, end));
            }
            if matches!(bytes[i + 1], b'!' | b'?') {
                return html[i..].find('>').map(|e| (i, i + e + 1));
            }
            let mut state = Tok::Name;
            for (j, &c) in bytes.iter().enumerate().skip(i + 1) {
                let space = c.is_ascii_whitespace();
                state = match (state, c) {
                    (Tok::Quoted(q), _) if c == q => Tok::BeforeAttr,
                    (Tok::Quoted(q), _) => Tok::Quoted(q),
                    (_, b'>') => return Some((i, j + 1)),
                    (Tok::Name, b'/') => Tok::BeforeAttr,
                    (Tok::Name | Tok::Unquoted, _) if space => Tok::BeforeAttr,
                    (Tok::Name | Tok::Unquoted, _) => state,
                    (Tok::BeforeAttr, b'/') => Tok::BeforeAttr,
                    (Tok::BeforeAttr, _) if space => Tok::BeforeAttr,
                    (Tok::BeforeAttr, _) => Tok::AttrName,
                    (Tok::AttrName, b'=') => Tok::BeforeValue,
                    (Tok::AttrName, b'/') => Tok::BeforeAttr,
                    (Tok::AttrName, _) => Tok::AttrName,
                    (Tok::BeforeValue, b'"' | b'\'') => Tok::Quoted(c),
                    (Tok::BeforeValue, _) if space => Tok::BeforeValue,
                    (Tok::BeforeValue, _) => Tok::Unquoted,
                };
            }
            return None;
        }
        i += 1;
    }
    None
}

/// Where [`next_tag`] stands inside a tag: the tokenizer's states, merged
/// where they end a tag the same way. `AttrName` also covers the spaces after
/// a name, since `=` there still starts a value; `BeforeAttr` also covers
/// self-closing and the end of a quoted value.
#[derive(Clone, Copy)]
enum Tok {
    Name,
    BeforeAttr,
    AttrName,
    BeforeValue,
    Unquoted,
    Quoted(u8),
}

/// Every tag in `html`, in order. The text of a `<script>`, `<style>`,
/// `<title>` or `<textarea>` element holds no tags, so after its opening tag
/// the scan resumes at its closing tag, which the tag's `text_end` names.
pub fn tags(html: &str) -> Tags<'_> {
    Tags {
        html,
        pos: 0,
        foreign: 0,
    }
}

/// One tag from [`tags`]: the [`next_tag`] range `[start, end)`, and
/// `text_end`, where the scan resumes. For a raw-text element's opening tag
/// that is its closing tag (or the end of the HTML), so `[end, text_end)` is
/// the element's text; for any other tag it is `end`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tag {
    pub start: usize,
    pub end: usize,
    pub text_end: usize,
}

pub struct Tags<'a> {
    html: &'a str,
    pos: usize,
    /// How many `<svg>` and `<math>` elements are open: inside one a `/>`
    /// closes an element. HTML islands within them (`<foreignObject>`) and an
    /// HTML tag that ends one early are not tracked.
    foreign: usize,
}

impl Tags<'_> {
    /// Resumes the scan at `pos`, past whatever the caller consumed itself.
    pub fn skip_to(&mut self, pos: usize) {
        self.pos = self.pos.max(pos);
    }
}

impl Iterator for Tags<'_> {
    type Item = Tag;

    fn next(&mut self) -> Option<Tag> {
        let (start, end) = next_tag(self.html, self.pos)?;
        let tag = &self.html[start..end];
        let self_closed = self.foreign > 0 && tag.ends_with("/>");
        let text_end = match tag_name(tag).as_deref() {
            Some("svg" | "math") if !tag.ends_with("/>") => {
                self.foreign += 1;
                end
            }
            // In HTML `<style/>` still opens its text; inside SVG or MathML
            // (`<title/>`) it is closed and has none.
            Some(name @ ("script" | "style" | "title" | "textarea")) if !self_closed => {
                closing_tag_start(self.html, end, name)
            }
            None if tag.starts_with("</")
                && matches!(
                    tag_name(&tag.replacen("</", "<", 1)).as_deref(),
                    Some("svg" | "math")
                ) =>
            {
                self.foreign = self.foreign.saturating_sub(1);
                end
            }
            _ => end,
        };
        self.pos = text_end;
        Some(Tag {
            start,
            end,
            text_end,
        })
    }
}

/// The offset of the first `</name` at or after `from`, any case, or the end
/// of the HTML. It reads only as far as the match, so a page of many raw-text
/// elements costs its length once, not once per element.
fn closing_tag_start(html: &str, from: usize, name: &str) -> usize {
    let bytes = html.as_bytes();
    let mut pos = from;
    while let Some(rel) = bytes[pos..].iter().position(|&b| b == b'<') {
        let lt = pos + rel;
        let closes = bytes[lt + 1..]
            .strip_prefix(b"/")
            .and_then(|rest| rest.get(..name.len()))
            .is_some_and(|n| n.eq_ignore_ascii_case(name.as_bytes()));
        if closes {
            return lt;
        }
        pos = lt + 1;
    }
    html.len()
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

/// Finds the attribute `attr` (case-insensitive name) in the opening tag
/// `tag`, reading its attributes as the browser's tokenizer does, and returns
/// the byte range of its value relative to `tag`: between the quotes of
/// `name="v"` or `name='v'`, or for an unquoted `name=v` up to whitespace or
/// `>`. Spaces around `=` are allowed. Text inside another attribute's value
/// is never read as an attribute, so `alt="a src=x"` holds no `src`. The
/// first attribute of a name wins, as in the browser, and one written without
/// `=` has no value: `None`.
pub fn find_attr_value_range(tag: &str, attr: &str) -> Option<(usize, usize)> {
    let b = tag.as_bytes();
    let space = |i: usize| b.get(i).is_some_and(u8::is_ascii_whitespace);
    let name_ends = |i: usize| i >= b.len() || space(i) || matches!(b[i], b'/' | b'>' | b'=');
    // Past `<` and the tag name.
    let mut i = 1;
    while !name_ends(i) {
        i += 1;
    }
    loop {
        while space(i) || b.get(i) == Some(&b'/') {
            i += 1;
        }
        if i >= b.len() || b[i] == b'>' {
            return None;
        }
        let name_start = i;
        // A leading `=` is part of the name.
        i += 1;
        while !name_ends(i) {
            i += 1;
        }
        let is_attr = tag[name_start..i].eq_ignore_ascii_case(attr);
        while space(i) {
            i += 1;
        }
        if b.get(i) != Some(&b'=') {
            if is_attr {
                return None;
            }
            continue;
        }
        i += 1;
        while space(i) {
            i += 1;
        }
        let (vs, ve) = match b.get(i) {
            Some(&q @ (b'"' | b'\'')) => {
                let vs = i + 1;
                let ve = vs + tag[vs..].find(q as char)?;
                i = ve + 1;
                (vs, ve)
            }
            _ => {
                let vs = i;
                while i < b.len() && !space(i) && b[i] != b'>' {
                    i += 1;
                }
                (vs, i)
            }
        };
        if is_attr {
            return Some((vs, ve));
        }
    }
}

/// `tag` with the attribute value at `[vs, ve)` (a [`find_attr_value_range`]
/// range) replaced by `value`, which must already be safe inside a quoted
/// value ([`escape_attr`]). An unquoted value is written in double quotes, so
/// the new one stays one value whatever it holds.
pub fn replace_attr_value(tag: &str, (vs, ve): (usize, usize), value: &str) -> String {
    let quote = if attr_value_is_quoted(tag, vs) {
        ""
    } else {
        "\""
    };
    format!("{}{quote}{value}{quote}{}", &tag[..vs], &tag[ve..])
}

/// Whether the [`find_attr_value_range`] value starting at `vs` is quoted.
/// The byte before an unquoted value is `=` or whitespace, never a quote.
pub fn attr_value_is_quoted(tag: &str, vs: usize) -> bool {
    tag[..vs].ends_with(['"', '\''])
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
    for Tag { start, end, .. } in tags(html) {
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
    for Tag { start, end, .. } in tags(html) {
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
    while let Some(Tag {
        start,
        end,
        text_end,
    }) = scan.next()
    {
        out.push_str(&html[pos..start]);
        pos = end;
        let tag = &html[start..end];
        let name = tag_name(&tag.replacen("</", "<", 1));
        match name.as_deref() {
            Some("script" | "style") => pos = text_end,
            // Not raw text, but its contents are never shown.
            Some("template") if !tag.starts_with("</") => {
                pos = closing_tag_start(html, end, "template");
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
        let names: Vec<&str> = tags(html).map(|t| &html[t.start..t.end]).collect();
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
    fn closing_tag_start_finds_the_name_in_any_case_as_a_prefix() {
        let html = "é<script>a</b>< /script></ScRiPtx>z";
        assert_eq!(
            closing_tag_start(html, 10, "script"),
            html.find("</Sc").unwrap()
        );
        assert_eq!(closing_tag_start(html, 0, "style"), html.len());
        assert_eq!(closing_tag_start("x</scrip", 0, "script"), 8);
        assert_eq!(closing_tag_start("x<", 1, "script"), 2);
    }

    #[test]
    fn many_script_blocks_scan_in_one_pass() {
        // 20,000 blocks in 780 KB: the scan reads the page once. A search that
        // reread the rest of the page per block would be about 8 GB of work.
        let html = "<script>let a = 1;</script><p>text</p>".repeat(20_000) + "<h2>end</h2>";
        assert_eq!(tags(&html).count(), 20_000 * 4 + 2);
        assert_eq!(card_label(&html).as_deref(), Some("end"));
    }

    #[test]
    fn a_comment_is_one_tag_whatever_it_holds() {
        let html = "<!-- don't <script> --><img src=x><title>a<b's</title><!--><!---><svg><title/><b><!-- open";
        let names: Vec<&str> = tags(html).map(|t| &html[t.start..t.end]).collect();
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
    fn a_self_closing_slash_ends_raw_text_only_inside_svg() {
        let html = "<style/><b>x</b></style><svg><title/></svg><b>y</b>";
        let names: Vec<&str> = tags(html).map(|t| &html[t.start..t.end]).collect();
        assert_eq!(
            names,
            ["<style/>", "</style>", "<svg>", "<title/>", "</svg>", "<b>", "</b>"]
        );
    }

    #[test]
    fn card_label_skips_the_text_of_a_self_closed_script_or_style() {
        for html in [
            "<style/><b>x</b></style><p>after</p>",
            "<script/><b>x</b></script><p>after</p>",
        ] {
            assert_eq!(card_label(html).as_deref(), Some("after"), "{html}");
        }
        assert_eq!(
            card_label("<svg><title/></svg><b>y</b>").as_deref(),
            Some("y")
        );
    }

    #[test]
    fn card_label_tracks_where_svg_and_math_end() {
        for (html, label) in [
            (
                "<math><mi/></math><style/><b>x</b></style><p>after</p>",
                "after",
            ),
            ("<svg><svg></svg><title/></svg><b>y</b>", "y"),
            ("</svg><style/><b>x</b></style><p>after</p>", "after"),
            (
                "<SVG><title/></SVG><style/><b>x</b></style><p>after</p>",
                "after",
            ),
        ] {
            assert_eq!(card_label(html).as_deref(), Some(label), "{html}");
        }
    }

    #[test]
    fn a_quote_opens_a_value_only_after_an_equals_sign() {
        let html = "a < b's <x <'s<b's<i src=x><p title=x'y a = '>'>";
        let names: Vec<&str> = tags(html).map(|t| &html[t.start..t.end]).collect();
        assert_eq!(names, ["<x <'s<b's<i src=x>", "<p title=x'y a = '>'>"]);

        let html = "<a=\"><a href=p?q='x'><a =\"x><a/b=\"x>y\"><?x a=\"x><!doctype a=\"b>";
        let names: Vec<&str> = tags(html).map(|t| &html[t.start..t.end]).collect();
        assert_eq!(
            names,
            [
                "<a=\">",
                "<a href=p?q='x'>",
                "<a =\"x>",
                "<a/b=\"x>y\">",
                "<?x a=\"x>",
                "<!doctype a=\"b>"
            ]
        );
    }

    #[test]
    fn attr_values_are_read_as_the_tokenizer_reads_them() {
        fn value<'a>(tag: &'a str, attr: &str) -> Option<&'a str> {
            find_attr_value_range(tag, attr).map(|(s, e)| &tag[s..e])
        }
        assert_eq!(value("<img src=/a.png>", "src"), Some("/a.png"));
        assert_eq!(value("<img src = /a.png/>", "src"), Some("/a.png/"));
        assert_eq!(value("<img SRC=/a\talt=x>", "src"), Some("/a"));
        assert_eq!(value("<img src=>", "src"), Some(""));
        assert_eq!(value("<img/src='/a b'>", "src"), Some("/a b"));
        assert_eq!(value("<img alt=\"a src=x\" src=\"/y\">", "src"), Some("/y"));
        assert_eq!(value("<img alt=a src=x>", "src"), Some("x"));
        assert_eq!(value("<img xsrc=x data-src=y>", "src"), None);
        assert_eq!(value("<img src=x src=y>", "src"), Some("x"));
        assert_eq!(value("<img src src=y>", "src"), None);
        assert_eq!(value("<img =src=x>", "src"), None);
    }

    #[test]
    fn replace_attr_value_quotes_an_unquoted_value() {
        let tag = "<img src=/a.png alt=x>";
        let range = find_attr_value_range(tag, "src").unwrap();
        assert_eq!(
            replace_attr_value(tag, range, "a b"),
            "<img src=\"a b\" alt=x>"
        );
        let tag = "<img src='/a.png'>";
        let range = find_attr_value_range(tag, "src").unwrap();
        assert_eq!(replace_attr_value(tag, range, "x"), "<img src='x'>");
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

    #[test]
    fn strip_tags_keeps_literal_tags_in_title_text() {
        assert_eq!(strip_tags("<title>a<b>c</title>"), "a<b>c");
    }

    #[test]
    fn card_label_ignores_a_heading_in_script_text() {
        assert_eq!(
            card_label("<script>s='<h1>no</h1>'</script><h2>yes</h2>").as_deref(),
            Some("yes")
        );
    }

    #[test]
    fn card_label_ignores_a_heading_in_a_comment() {
        assert_eq!(
            card_label("<!-- x > <h1>old</h1> --><h2>new</h2>").as_deref(),
            Some("new")
        );
    }

    #[test]
    fn card_label_reads_a_bare_less_than_and_quote_as_text() {
        assert_eq!(card_label("<p>a < b's</p>").as_deref(), Some("a < b's"));
    }
}
