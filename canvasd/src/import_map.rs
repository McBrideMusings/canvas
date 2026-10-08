//! A card's import maps, merged, and the browser's resolution of a module
//! specifier through them (the HTML standard's "resolve a module
//! specifier"), so an export can follow the modules a card reaches by name.
//! The page's own URL isn't known at export time, so an entry whose key or
//! address is relative to the page never matches and never resolves: where
//! one would decide, the answer is that the page decides.

use serde_json::{Map, Value};
use url::Url;

/// The card's import maps merged into one, the first to name an entry
/// keeping it, as the page's single import map writes them. Scopes merge per
/// scope prefix, so a later map adds entries to a scope an earlier one has.
pub fn merge(card_maps: &[Map<String, Value>]) -> Map<String, Value> {
    let mut merged = Map::new();
    for map in card_maps {
        for (key, value) in map {
            // `scopes` holds one specifier map per prefix; `imports` is one.
            let depth = if key == "scopes" { 2 } else { 1 };
            merge_into(&mut merged, key, value, depth);
        }
    }
    merged
}

/// Puts `value` under `key` in `have` when it lacks one; when both are
/// objects, merges `value`'s entries in, `depth` levels deep.
fn merge_into(have: &mut Map<String, Value>, key: &str, value: &Value, depth: usize) {
    match (have.get_mut(key), value) {
        (None, _) => {
            have.insert(key.to_string(), value.clone());
        }
        (Some(Value::Object(inner)), Value::Object(add)) if depth > 0 => {
            for (k, v) in add {
                merge_into(inner, k, v, depth - 1);
            }
        }
        _ => {}
    }
}

/// Where an entry sends a specifier.
#[derive(Debug, Clone, PartialEq)]
enum Address {
    Url(Url),
    /// Relative to the page, or blocked (`null`, or an address that isn't
    /// one): the page decides.
    Page,
}

/// One specifier map, keys sorted the way the standard tries them:
/// descending by code unit, so a longer prefix is tried before a shorter one.
#[derive(Debug, Default)]
struct Specifiers(Vec<(String, Address)>);

impl Specifiers {
    fn parse(map: &Map<String, Value>) -> Self {
        let mut entries: Vec<(String, Address)> = map
            .iter()
            .filter_map(|(key, value)| {
                let key = normalize_key(key)?;
                let address = match value {
                    Value::String(s) => match url_like(s, None) {
                        Some(url) if !key.ends_with('/') || url.as_str().ends_with('/') => {
                            Address::Url(url)
                        }
                        _ => Address::Page,
                    },
                    _ => Address::Page,
                };
                Some((key, address))
            })
            .collect();
        entries.sort_by(|a, b| b.0.cmp(&a.0));
        Specifiers(entries)
    }

    /// The address this map gives `normalized`, None when no entry matches.
    fn lookup(&self, normalized: &str, as_url: Option<&Url>) -> Option<Option<Url>> {
        for (key, address) in &self.0 {
            if key == normalized {
                return Some(match address {
                    Address::Url(url) => Some(url.clone()),
                    Address::Page => None,
                });
            }
            let prefix = key.ends_with('/')
                && normalized.starts_with(key.as_str())
                && as_url.is_none_or(|u| u.is_special());
            if prefix {
                return Some(match address {
                    Address::Url(base) => base
                        .join(&normalized[key.len()..])
                        .ok()
                        // Backtracking out of the prefix is blocked.
                        .filter(|url| url.as_str().starts_with(base.as_str())),
                    Address::Page => None,
                });
            }
        }
        None
    }

    /// Each address that is a whole module rather than a prefix.
    fn modules(&self) -> impl Iterator<Item = &Url> {
        self.0.iter().filter_map(|(key, address)| match address {
            Address::Url(url) if !key.ends_with('/') => Some(url),
            _ => None,
        })
    }
}

/// A merged import map, read for resolution.
#[derive(Debug, Default)]
pub struct ImportMap {
    imports: Specifiers,
    /// Scope prefixes, sorted descending by code unit as the standard tries them.
    scopes: Vec<(String, Specifiers)>,
}

impl ImportMap {
    pub fn parse(merged: &Map<String, Value>) -> Self {
        let imports = match merged.get("imports") {
            Some(Value::Object(map)) => Specifiers::parse(map),
            _ => Specifiers::default(),
        };
        let mut scopes: Vec<(String, Specifiers)> = match merged.get("scopes") {
            Some(Value::Object(scopes)) => scopes
                .iter()
                .filter_map(|(prefix, map)| {
                    let prefix = Url::parse(prefix).ok()?.to_string();
                    match map {
                        Value::Object(map) => Some((prefix, Specifiers::parse(map))),
                        _ => None,
                    }
                })
                .collect(),
            _ => Vec::new(),
        };
        scopes.sort_by(|a, b| b.0.cmp(&a.0));
        ImportMap { imports, scopes }
    }

    /// The URL `specifier` loads when a module at `referrer` imports it (None
    /// for the page's own inline module, whose URL isn't known), or None when
    /// the page decides: a bare name with no entry, a relative specifier from
    /// the page, or an entry relative to the page or blocked.
    pub fn resolve(&self, specifier: &str, referrer: Option<&Url>) -> Option<Url> {
        let as_url = url_like(specifier, referrer);
        let normalized = match &as_url {
            Some(url) => url.to_string(),
            None if is_relative(specifier) => return None,
            None => specifier.to_string(),
        };
        if let Some(referrer) = referrer {
            let referrer = referrer.as_str();
            for (prefix, map) in &self.scopes {
                let applies =
                    prefix == referrer || (prefix.ends_with('/') && referrer.starts_with(prefix));
                if applies {
                    if let Some(found) = map.lookup(&normalized, as_url.as_ref()) {
                        return found;
                    }
                }
            }
        }
        match self.imports.lookup(&normalized, as_url.as_ref()) {
            Some(found) => found,
            None => as_url,
        }
    }

    /// Every address in the map, its imports' and its scopes', that names one
    /// module rather than a prefix, in the map's order.
    pub fn modules(&self) -> Vec<Url> {
        let mut all: Vec<Url> = self.imports.modules().cloned().collect();
        for (_, map) in &self.scopes {
            all.extend(map.modules().cloned());
        }
        all
    }
}

fn is_relative(specifier: &str) -> bool {
    ["/", "./", "../"].iter().any(|p| specifier.starts_with(p))
}

/// `specifier` as a URL: joined to `base` when it starts with `/`, `./` or
/// `../` (None with no base), else parsed whole; None for a bare name.
pub fn url_like(specifier: &str, base: Option<&Url>) -> Option<Url> {
    if is_relative(specifier) {
        base?.join(specifier).ok()
    } else {
        Url::parse(specifier).ok()
    }
}

/// A key as the standard normalizes it: an absolute URL serialized, a bare
/// name as written; None for an empty key or one relative to the page.
fn normalize_key(key: &str) -> Option<String> {
    if key.is_empty() || is_relative(key) {
        return None;
    }
    Some(match Url::parse(key) {
        Ok(url) => url.to_string(),
        Err(_) => key.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(json: &str) -> ImportMap {
        match serde_json::from_str(json).unwrap() {
            Value::Object(m) => ImportMap::parse(&m),
            _ => unreachable!(),
        }
    }

    fn at(url: &str) -> Url {
        Url::parse(url).unwrap()
    }

    fn resolved(m: &ImportMap, specifier: &str, referrer: Option<&str>) -> Option<String> {
        m.resolve(specifier, referrer.map(at).as_ref())
            .map(|u| u.to_string())
    }

    const CDN: &str = "https://cdn.jsdelivr.net/npm/";

    #[test]
    fn exact_and_longest_prefix_entries_resolve() {
        let m = map(&format!(
            r#"{{"imports":{{"lit":"{CDN}lit@3/+esm","lit/":"{CDN}lit@3/","lit/directives/":"{CDN}lit-dir/","lit/x.js":"{CDN}x/+esm"}}}}"#
        ));
        let r = |s| resolved(&m, s, None);
        assert_eq!(r("lit").as_deref(), Some(&*format!("{CDN}lit@3/+esm")));
        assert_eq!(
            r("lit/decorators.js").as_deref(),
            Some(&*format!("{CDN}lit@3/decorators.js"))
        );
        assert_eq!(
            r("lit/directives/a.js").as_deref(),
            Some(&*format!("{CDN}lit-dir/a.js"))
        );
        assert_eq!(r("lit/x.js").as_deref(), Some(&*format!("{CDN}x/+esm")));
        assert_eq!(r("react"), None);
        // A relative specifier from the page has no URL to resolve against.
        assert_eq!(r("./a.js"), None);
    }

    #[test]
    fn url_keys_remap_urls_and_relative_ones_resolve_from_the_referrer() {
        let m = map(&format!(
            r#"{{"imports":{{"{CDN}a/":"https://unpkg.com/a/","{CDN}b/+esm":"./mine.js"}}}}"#
        ));
        let from = Some("https://cdn.jsdelivr.net/npm/x/+esm");
        assert_eq!(
            resolved(&m, "/npm/a/i.js", from).as_deref(),
            Some("https://unpkg.com/a/i.js")
        );
        // An address relative to the page: the page decides.
        assert_eq!(resolved(&m, "/npm/b/+esm", from), None);
        assert_eq!(
            resolved(&m, "../c.js", from).as_deref(),
            Some("https://cdn.jsdelivr.net/npm/c.js")
        );
    }

    #[test]
    fn a_scope_matching_the_referrer_comes_before_the_imports() {
        let m = map(&format!(
            r#"{{"imports":{{"dep":"{CDN}dep@1/+esm"}},"scopes":{{"{CDN}lib/":{{"dep":"{CDN}dep@2/+esm"}},"{CDN}lib/old/":{{"other":"{CDN}o/+esm"}},"/rel/":{{"dep":"{CDN}never"}}}}}}"#
        ));
        assert_eq!(
            resolved(&m, "dep", Some(&format!("{CDN}lib/+esm"))).as_deref(),
            Some(&*format!("{CDN}dep@2/+esm"))
        );
        // The more specific scope has no entry, so the next one answers.
        assert_eq!(
            resolved(&m, "dep", Some(&format!("{CDN}lib/old/a.js"))).as_deref(),
            Some(&*format!("{CDN}dep@2/+esm"))
        );
        assert_eq!(
            resolved(&m, "dep", Some("https://unpkg.com/z")).as_deref(),
            Some(&*format!("{CDN}dep@1/+esm"))
        );
        assert_eq!(
            resolved(&m, "dep", None).as_deref(),
            Some(&*format!("{CDN}dep@1/+esm"))
        );
    }

    #[test]
    fn blocked_entries_and_backtracking_resolve_to_nothing() {
        let m = map(&format!(
            r#"{{"imports":{{"a":null,"b/":"{CDN}b","c/":"{CDN}c/","d":5}}}}"#
        ));
        assert_eq!(resolved(&m, "a", None), None);
        // A prefix whose address doesn't end in a slash is blocked.
        assert_eq!(resolved(&m, "b/x.js", None), None);
        assert_eq!(resolved(&m, "c/../../evil.js", None), None);
        assert_eq!(resolved(&m, "d", None), None);
    }

    #[test]
    fn modules_are_the_whole_module_addresses() {
        let m = map(&format!(
            r#"{{"imports":{{"lit":"{CDN}lit/+esm","lit/":"{CDN}lit/","rel":"./r.js"}},"scopes":{{"{CDN}s/":{{"d":"{CDN}d/+esm"}}}}}}"#
        ));
        let all: Vec<String> = m.modules().iter().map(|u| u.to_string()).collect();
        assert_eq!(all, [format!("{CDN}lit/+esm"), format!("{CDN}d/+esm")]);
    }

    #[test]
    fn the_first_map_to_name_an_entry_keeps_it_and_scopes_merge_per_prefix() {
        let maps: Vec<Map<String, Value>> = [
            r#"{"imports":{"a":"1"},"scopes":{"s/":{"x":"1"}}}"#,
            r#"{"imports":{"a":"2","b":"2"},"scopes":{"s/":{"x":"2","y":"2"},"t/":{"z":"2"}}}"#,
        ]
        .iter()
        .map(|j| serde_json::from_str(j).unwrap())
        .collect();
        assert_eq!(
            Value::Object(merge(&maps)),
            serde_json::json!({
                "imports": {"a": "1", "b": "2"},
                "scopes": {"s/": {"x": "1", "y": "2"}, "t/": {"z": "2"}},
            })
        );
    }
}
