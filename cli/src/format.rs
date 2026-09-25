//! `canvas post`'s input format: Markdown, plain text, or HTML, converted to
//! the HTML the daemon and viewer always receive.

use pulldown_cmark::{html as md_html, Options, Parser};

/// Default styling for Markdown and text posts, prepended as a `<style>`
/// block before the converted body so a post's own raw `<style>` comes later
/// in the document and wins at equal specificity. Plain element selectors
/// only — no ids, no `!important`.
const DEFAULT_STYLE: &str = "<style>\
body { font-family: -apple-system, BlinkMacSystemFont, \"Segoe UI\", sans-serif; }\
table { border-collapse: collapse; }\
th, td { border: 1px solid #ccc; padding: 4px 8px; }\
th { font-weight: 600; background: #f0f0f0; }\
code, pre { font-family: ui-monospace, Menlo, Consolas, monospace; }\
code { background: #f0f0f0; padding: 0.1em 0.3em; }\
pre { background: #f0f0f0; padding: 8px; overflow-x: auto; }\
</style>";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Markdown,
    Text,
    Html,
}

impl Format {
    /// Parses an explicit `--format` value. Anything else is a usage error
    /// the caller reports.
    pub fn parse(value: &str) -> Option<Format> {
        match value {
            "md" => Some(Format::Markdown),
            "text" => Some(Format::Text),
            "html" => Some(Format::Html),
            _ => None,
        }
    }

    /// Format from a file extension. `None` for an argument-less/stdin post
    /// or an unrecognised extension — both fall back to Markdown at the call
    /// site.
    pub fn from_extension(path: &str) -> Option<Format> {
        let ext = std::path::Path::new(path)
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())?;
        match ext.as_str() {
            "md" | "markdown" => Some(Format::Markdown),
            "txt" => Some(Format::Text),
            "html" | "htm" => Some(Format::Html),
            _ => None,
        }
    }
}

/// Converts `input` to HTML per `format`. Markdown renders with tables and
/// strikethrough enabled; raw HTML embedded in the Markdown source passes
/// through unchanged, so `echo '<p>done</p>' | canvas post` still renders as
/// HTML. Plain text becomes an HTML-escaped `<pre>` block. HTML passes
/// through as-is — untouched, with no style injected.
///
/// Markdown and Text output is prefixed with `DEFAULT_STYLE`, a small
/// stylesheet covering tables, code/pre blocks and body font. Any `<style>`
/// of the post's own — raw HTML in Markdown source, or literal text —
/// lands later in the returned string and so wins the cascade at equal
/// specificity.
pub fn convert(input: &str, format: Format) -> String {
    match format {
        Format::Html => input.to_string(),
        Format::Text => format!("{}<pre>{}</pre>", DEFAULT_STYLE, escape_html(input)),
        Format::Markdown => {
            let mut options = Options::empty();
            options.insert(Options::ENABLE_TABLES);
            options.insert(Options::ENABLE_STRIKETHROUGH);
            let parser = Parser::new_ext(input, options);
            let mut out = String::new();
            md_html::push_html(&mut out, parser);
            format!("{}{}", DEFAULT_STYLE, out)
        }
    }
}

fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_maps_to_format() {
        assert_eq!(Format::from_extension("a.md"), Some(Format::Markdown));
        assert_eq!(Format::from_extension("a.markdown"), Some(Format::Markdown));
        assert_eq!(Format::from_extension("a.txt"), Some(Format::Text));
        assert_eq!(Format::from_extension("a.html"), Some(Format::Html));
        assert_eq!(Format::from_extension("a.htm"), Some(Format::Html));
        assert_eq!(Format::from_extension("a.xyz"), None);
        assert_eq!(Format::from_extension("noext"), None);
    }

    #[test]
    fn explicit_format_parses() {
        assert_eq!(Format::parse("md"), Some(Format::Markdown));
        assert_eq!(Format::parse("text"), Some(Format::Text));
        assert_eq!(Format::parse("html"), Some(Format::Html));
        assert_eq!(Format::parse("bogus"), None);
    }

    #[test]
    fn markdown_converts_to_html() {
        let out = convert("# hi\n\n*there*", Format::Markdown);
        assert!(out.contains("<h1>hi</h1>"));
        assert!(out.contains("<em>there</em>"));
    }

    #[test]
    fn markdown_output_starts_with_default_style_and_own_style_comes_after() {
        let out = convert(
            "# hi\n\n<style>h1 { color: red; }</style>",
            Format::Markdown,
        );
        assert!(out.starts_with(DEFAULT_STYLE));
        let default_pos = out.find(DEFAULT_STYLE).unwrap();
        let own_pos = out.find("h1 { color: red; }").unwrap();
        assert!(own_pos > default_pos);
    }

    #[test]
    fn markdown_tables_and_strikethrough_are_enabled() {
        let out = convert("~~gone~~", Format::Markdown);
        assert!(out.contains("<del>gone</del>"));

        let out = convert("| a | b |\n|---|---|\n| 1 | 2 |\n", Format::Markdown);
        assert!(out.contains("<table>"));
    }

    #[test]
    fn raw_html_in_markdown_passes_through() {
        let out = convert("<p>done</p>", Format::Markdown);
        assert!(out.contains("<p>done</p>"));
    }

    #[test]
    fn text_becomes_escaped_pre() {
        let out = convert("<b>not bold</b> & stuff", Format::Text);
        assert_eq!(
            out,
            format!(
                "{}<pre>&lt;b&gt;not bold&lt;/b&gt; &amp; stuff</pre>",
                DEFAULT_STYLE
            )
        );
    }

    #[test]
    fn html_passes_through_unchanged() {
        let out = convert("<p>already html</p>", Format::Html);
        assert_eq!(out, "<p>already html</p>");
    }
}
