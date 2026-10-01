//! The `stop-triggers` profile kind: which turns the plugin's prompt hook reminds
//! an agent to post. A profile's text is one directive per line; the parser
//! lives here, beside the profile store, so the daemon can refuse a bad
//! profile when it is saved and the hook parses the same text at run time.
//!
//! Directives, applied top to bottom (a later line overrides an earlier one,
//! which is how a repo profile joined after the global one edits it):
//!
//! - `image`, `file`, `report`, `verify` turn a trigger on
//! - `links [N]` and `long-block [N]` turn one on with a threshold
//! - `phrase <text>` adds a phrase that counts as asking the user to verify
//! - `scratch <prefix>` adds a path prefix whose files are not worth posting
//! - `no <trigger>`, `no phrase <text>`, `no scratch <prefix>` remove one
//! - `off` disables the hook; `on` re-enables it
//! - blank lines and lines starting with `#` are ignored

pub const DEFAULT_LINKS: usize = 3;
pub const DEFAULT_LONG_BLOCK: usize = 15;

/// Directive summary shown in the settings page and printed in errors.
pub const DIRECTIVES: &str = "image, file, report, verify, links [N], long-block [N], \
    phrase <text>, scratch <prefix>, no <directive>, off, on";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StopTriggers {
    pub enabled: bool,
    pub image: bool,
    pub file: bool,
    pub report: bool,
    pub verify: bool,
    /// Links in a reply that count as "several"; `None` turns the trigger off.
    pub links: Option<usize>,
    /// Lines in a fenced block or table that count as "long".
    pub long_block: Option<usize>,
    /// Lower-cased.
    pub phrases: Vec<String>,
    pub scratch: Vec<String>,
}

impl Default for StopTriggers {
    /// Nothing triggers: text with no directives asks for nothing.
    fn default() -> Self {
        StopTriggers {
            enabled: true,
            image: false,
            file: false,
            report: false,
            verify: false,
            links: None,
            long_block: None,
            phrases: Vec::new(),
            scratch: Vec::new(),
        }
    }
}

/// Parses profile text; the error names the first line that isn't a directive.
pub fn parse(text: &str) -> Result<StopTriggers, String> {
    let mut t = StopTriggers::default();
    for (i, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let bad = || {
            format!(
                "line {}: unknown directive {line:?} (use {DIRECTIVES})",
                i + 1
            )
        };
        let (negated, rest) = match line.strip_prefix("no ") {
            Some(rest) => (true, rest.trim()),
            None => (false, line),
        };
        let (word, arg) = match rest.split_once(char::is_whitespace) {
            Some((w, a)) => (w, a.trim()),
            None => (rest, ""),
        };
        let number = |default: usize| -> Result<usize, String> {
            if arg.is_empty() {
                Ok(default)
            } else {
                arg.parse().map_err(|_| bad())
            }
        };
        match (negated, word) {
            (false, "off") if arg.is_empty() => t.enabled = false,
            (false, "on") if arg.is_empty() => t.enabled = true,
            (neg, "image") if arg.is_empty() => t.image = !neg,
            (neg, "file") if arg.is_empty() => t.file = !neg,
            (neg, "report") if arg.is_empty() => t.report = !neg,
            (neg, "verify") if arg.is_empty() => t.verify = !neg,
            (false, "links") => t.links = Some(number(DEFAULT_LINKS)?),
            (true, "links") if arg.is_empty() => t.links = None,
            (false, "long-block") => t.long_block = Some(number(DEFAULT_LONG_BLOCK)?),
            (true, "long-block") if arg.is_empty() => t.long_block = None,
            (false, "phrase") if !arg.is_empty() => {
                let phrase = arg.to_lowercase();
                if !t.phrases.contains(&phrase) {
                    t.phrases.push(phrase);
                }
            }
            (true, "phrase") if !arg.is_empty() => {
                let phrase = arg.to_lowercase();
                t.phrases.retain(|p| *p != phrase);
            }
            (false, "scratch") if !arg.is_empty() => {
                if !t.scratch.iter().any(|s| s == arg) {
                    t.scratch.push(arg.to_string());
                }
            }
            (true, "scratch") if !arg.is_empty() => t.scratch.retain(|s| s != arg),
            _ => return Err(bad()),
        }
    }
    Ok(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_built_in_default_leaves_gate_shaped_triggers_off() {
        let t = parse(include_str!("../../plugin/stop-triggers.txt")).unwrap();
        assert!(t.enabled && t.image && t.file && !t.report && !t.verify);
        assert_eq!(t.links, Some(3));
        assert_eq!(t.long_block, Some(15));
        assert!(t.phrases.is_empty());
        assert!(t.scratch.contains(&"/tmp/".to_string()));
    }

    #[test]
    fn later_lines_override_earlier_ones() {
        let t = parse("image\nfile\nlinks 3\n\nno image\nlinks 5\nno long-block\nphrase Try It\nno phrase try it\noff\n")
            .unwrap();
        assert!(!t.image && t.file && !t.enabled);
        assert_eq!(t.links, Some(5));
        assert!(t.phrases.is_empty());
    }

    #[test]
    fn a_bad_line_is_named() {
        let err = parse("image\nlinks many\n").unwrap_err();
        assert!(err.starts_with("line 2:"), "{err}");
        assert!(parse("images").is_err());
        assert!(parse("phrase").is_err());
    }
}
