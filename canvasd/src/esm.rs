//! The module specifiers an ES module names, read with a real JavaScript
//! parser (oxc), so strings, comments, template literals and regular
//! expressions in minified code are never mistaken for an import.

use std::ops::Range;

use oxc_allocator::Allocator;
use oxc_parser::Parser;
use oxc_span::SourceType;

/// One specifier: the byte range of its string literal in the source, quotes
/// included, and its value.
#[derive(Debug, PartialEq, Eq)]
pub struct Specifier {
    pub range: Range<usize>,
    pub value: String,
}

/// Every specifier `source` names as a string: `import … from "x"`,
/// `import "x"`, `export … from "x"`, and `import("x")` or
/// `` import(`x`) `` with a plain string. An `import()` of any other
/// expression names nothing that can be read. In source order. `Err` with
/// the parser's first message when `source` isn't a module it can read.
pub fn specifiers(source: &str) -> Result<Vec<Specifier>, String> {
    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, source, SourceType::mjs()).parse();
    if !parsed.diagnostics.is_empty() {
        let message = parsed
            .diagnostics
            .iter()
            .next()
            .map(|d| d.message.to_string())
            .unwrap_or_else(|| "it doesn't parse".into());
        return Err(message);
    }
    let record = &parsed.module_record;
    let mut found: Vec<Specifier> = record
        .requested_modules
        .iter()
        .flat_map(|(value, requests)| {
            requests.iter().map(move |r| Specifier {
                range: r.span.start as usize..r.span.end as usize,
                value: value.to_string(),
            })
        })
        .collect();
    for dynamic in record.dynamic_imports.iter() {
        let range = dynamic.module_request.start as usize..dynamic.module_request.end as usize;
        if let Some(value) = plain_string(&source[range.clone()]) {
            found.push(Specifier {
                range,
                value: value.to_string(),
            });
        }
    }
    found.sort_by_key(|s| s.range.start);
    found.dedup_by_key(|s| s.range.start);
    Ok(found)
}

/// The text of a string or template literal holding no escape and no
/// substitution, so its value is its text.
fn plain_string(literal: &str) -> Option<&str> {
    let quote = literal.chars().next()?;
    if !matches!(quote, '"' | '\'' | '`') || literal.len() < 2 || !literal.ends_with(quote) {
        return None;
    }
    let inner = &literal[1..literal.len() - 1];
    (!inner.contains('\\') && !(quote == '`' && inner.contains("${"))).then_some(inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values(source: &str) -> Vec<(String, String)> {
        specifiers(source)
            .unwrap()
            .into_iter()
            .map(|s| (source[s.range].to_string(), s.value))
            .collect()
    }

    fn v(raw: &str, value: &str) -> (String, String) {
        (raw.to_string(), value.to_string())
    }

    #[test]
    fn reads_every_static_form() {
        let src = r#"import"/npm/a/+esm";import b from'./b.js';import*as c from"../c.js";
export*from"/npm/d/+esm";export{e}from"https://cdn.jsdelivr.net/npm/e";import{f}from"lit";"#;
        assert_eq!(
            values(src),
            [
                v(r#""/npm/a/+esm""#, "/npm/a/+esm"),
                v("'./b.js'", "./b.js"),
                v(r#""../c.js""#, "../c.js"),
                v(r#""/npm/d/+esm""#, "/npm/d/+esm"),
                v(
                    r#""https://cdn.jsdelivr.net/npm/e""#,
                    "https://cdn.jsdelivr.net/npm/e"
                ),
                v(r#""lit""#, "lit"),
            ]
        );
    }

    #[test]
    fn an_escaped_static_specifier_reads_as_its_value() {
        assert_eq!(
            values(r#"import"\x2fnpm/a";"#),
            [v(r#""\x2fnpm/a""#, "/npm/a")]
        );
    }

    #[test]
    fn dynamic_import_reads_only_a_plain_string() {
        let src =
            "import('/npm/a');import(`/npm/b`);import(`/npm/${x}`);import(name);import('\\x2fc');";
        assert_eq!(
            values(src),
            [v("'/npm/a'", "/npm/a"), v("`/npm/b`", "/npm/b")]
        );
    }

    #[test]
    fn strings_comments_templates_and_regexes_name_nothing() {
        let src = r#"const s="import'/npm/x'";/* import"/npm/y" */// export*from"/npm/z"
const t=`import "/npm/t"`;const r=/import"\/npm\/r"/g;const d=a/2/b;
import"/npm/real";"#;
        assert_eq!(values(src), [v(r#""/npm/real""#, "/npm/real")]);
    }

    #[test]
    fn source_that_isnt_a_module_is_an_error() {
        assert!(specifiers("import { from").is_err());
    }
}
