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

/// Every tag in `html`, in order. The text of an HTML `<script>`, `<style>`,
/// `<title>` or `<textarea>` element holds no tags, so after its opening tag
/// the scan resumes at its closing tag, which the tag's `text_end` names. In
/// SVG or MathML those elements hold markup like any other, and a
/// `<![CDATA[` section there is one tag through its `]]>`.
pub fn tags(html: &str) -> Tags<'_> {
    Tags {
        html,
        pos: 0,
        open: Vec::new(),
        mode: Mode::Body,
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
    /// The open elements, innermost last, as the browser's tree builder keeps
    /// them. A tag is read as foreign content (where `/>` closes an element)
    /// while the innermost is SVG or MathML and not an integration point; at
    /// one, a start tag is HTML again. A breakout tag such as `<p>` closes
    /// every foreign element down to the nearest integration point. An end
    /// tag closes elements by the tree builder's rules for it, so an HTML end
    /// tag can close the SVG opened inside its element. An HTML start tag
    /// first closes what the tree builder closes for it (a `<p>` before a
    /// block, a sibling list item or cell), and a formatting end tag runs the
    /// adoption agency's rounds. The list of active formatting elements is
    /// not kept, so its marker is read off the open cells and captions, and
    /// the elements the browser reopens from it before text are not reopened.
    /// Like WebKit's, the stack holds at most [`MAX_OPEN`] elements: see
    /// [`Tags::insert`].
    open: Vec<Element>,
    /// The tree builder's insertion mode. Like WebKit's it is kept, not read
    /// off the open elements each time: the depth cap can close the table,
    /// section, row or cell that set it, and the mode stays.
    mode: Mode,
}

/// WebKit's cap on its stack of open elements
/// (`defaultMaximumHTMLParserDOMTreeDepth`), less the `<html>`, `<body>` and
/// wrapper `<div>` that the viewer's card frame and an exported page both put
/// around a card.
const MAX_OPEN: usize = 512 - 3;

/// One open element.
struct Element {
    name: String,
    ns: Ns,
    point: Point,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Ns {
    Html,
    Svg,
    Math,
}

impl Element {
    fn is_foreign(&self) -> bool {
        self.ns != Ns::Html
    }

    /// Whether the tree builder counts this element as special: an end tag
    /// for another element never closes past it.
    fn is_special(&self) -> bool {
        match self.ns {
            Ns::Html => SPECIAL_TAGS.contains(&self.name.as_str()),
            _ => self.ends_foreign_scope(),
        }
    }

    /// The SVG and MathML elements that bound every scope but table scope.
    fn ends_foreign_scope(&self) -> bool {
        match self.ns {
            Ns::Html => false,
            Ns::Svg => matches!(self.name.as_str(), "foreignobject" | "desc" | "title"),
            Ns::Math => matches!(
                self.name.as_str(),
                "mi" | "mo" | "mn" | "ms" | "mtext" | "annotation-xml"
            ),
        }
    }

    /// Whether this element bounds `scope`, so a search for an element in
    /// scope stops here.
    fn ends_scope(&self, scope: Scope) -> bool {
        let name = self.name.as_str();
        match (self.ns, scope) {
            (Ns::Html, Scope::Table) => matches!(name, "html" | "table" | "template"),
            (Ns::Html, _) => {
                matches!(
                    name,
                    "applet"
                        | "caption"
                        | "html"
                        | "table"
                        | "td"
                        | "th"
                        | "marquee"
                        | "object"
                        | "template"
                ) || (scope == Scope::ListItem && matches!(name, "ol" | "ul"))
                    || (scope == Scope::Button && name == "button")
            }
            (_, Scope::Table) => false,
            _ => self.ends_foreign_scope(),
        }
    }
}

/// The tree builder's element scopes, each bounded by its own set of
/// elements.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Scope {
    Default,
    ListItem,
    Button,
    Table,
}

/// The tree builder's insertion modes that change what a start tag closes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Body,
    Template,
    Table,
    TableBody,
    Row,
    Cell,
    Caption,
    ColumnGroup,
}

/// The insertion mode the HTML element `name` starts when it opens, and
/// that the "reset the insertion mode" step reads off it.
fn mode_of(name: &str) -> Option<Mode> {
    Some(match name {
        "td" | "th" => Mode::Cell,
        "tr" => Mode::Row,
        "tbody" | "thead" | "tfoot" => Mode::TableBody,
        "caption" => Mode::Caption,
        "colgroup" => Mode::ColumnGroup,
        "table" => Mode::Table,
        "template" => Mode::Template,
        _ => return None,
    })
}

/// A table's sections.
const SECTIONS: &[&str] = &["tbody", "tfoot", "thead"];

/// Which HTML integration point a foreign [`Element`] is: where the browser
/// reads a start tag as HTML.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Point {
    None,
    /// SVG `<foreignObject>`, `<desc>` and `<title>`, and MathML
    /// `<annotation-xml>` whose `encoding` is HTML: every start tag.
    Html,
    /// MathML `<mi>`, `<mo>`, `<mn>`, `<ms>` and `<mtext>`: every start tag
    /// but `<mglyph>` and `<malignmark>`.
    MathText,
}

impl Tags<'_> {
    /// Resumes the scan at `pos`, past whatever the caller consumed itself.
    pub fn skip_to(&mut self, pos: usize) {
        self.pos = self.pos.max(pos);
    }

    /// Updates the open elements for the start tag `tag`, and returns where
    /// the scan resumes.
    fn start_tag(&mut self, tag: &str, name: &str, end: usize) -> usize {
        let self_closing = tag.ends_with("/>");
        let mut as_html = match self.open.last() {
            None => true,
            Some(top) if !top.is_foreign() => true,
            Some(top) => match top.point {
                // Any `<annotation-xml>` holds an `<svg>` as HTML would.
                Point::None => top.ns == Ns::Math && top.name == "annotation-xml" && name == "svg",
                Point::Html => true,
                Point::MathText => !matches!(name, "mglyph" | "malignmark"),
            },
        };
        if !as_html && breaks_out(tag, name) {
            self.pop_foreign_to_point();
            as_html = true;
        }
        let ns = if as_html {
            match name {
                "svg" => Ns::Svg,
                "math" => Ns::Math,
                _ => return self.html_start_tag(tag, name, end),
            }
        } else if self.open.last().is_some_and(|e| e.ns == Ns::Math) {
            Ns::Math
        } else {
            Ns::Svg
        };
        // An `<svg>` or `<math>` read as HTML meets the table modes first,
        // which can close a `<colgroup>`.
        if as_html && !self.table_start_tag(name) {
            return end;
        }
        self.insert(tag, name);
        if self_closing {
            return end;
        }
        self.open.push(Element {
            name: name.to_string(),
            ns,
            point: integration_point(tag, name, ns == Ns::Math),
        });
        end
    }

    /// Updates the open elements for the HTML start tag `<name>`, closing
    /// what the tree builder closes before inserting it, and returns where
    /// the scan resumes.
    fn html_start_tag(&mut self, tag: &str, name: &str, end: usize) -> usize {
        // A card is already in a body, where `<html>` and `<body>` only add
        // attributes to the open ones and `<head>` is dropped.
        if !self.table_start_tag(name) || matches!(name, "html" | "body" | "head") {
            return end;
        }
        self.body_start_tag(name);
        // A `<frame>` in a body is dropped, so nothing is inserted.
        if name != "frame" {
            self.insert(tag, name);
        }
        if !VOID_TAGS.contains(&name) {
            self.push_html(name);
        }
        match name {
            // In HTML `<style/>` still opens its text.
            "script" | "style" | "title" | "textarea" => closing_tag_start(self.html, end, name),
            _ => end,
        }
    }

    /// Opens the HTML element `<name>`, switching to the insertion mode a
    /// table, one of its parts or a `<template>` starts.
    fn push_html(&mut self, name: &str) {
        self.open.push(Element {
            name: name.to_string(),
            ns: Ns::Html,
            point: Point::None,
        });
        if let Some(mode) = mode_of(name) {
            self.mode = mode;
        }
    }

    /// Inserts and opens the element `<name>` a table mode implies, such as
    /// the `<tbody>` before a `<tr>`.
    fn imply(&mut self, name: &str) {
        self.insert("", name);
        self.push_html(name);
    }

    /// Applies WebKit's depth cap before the tree builder attaches a node to
    /// the innermost element: with the stack full, that element is closed and
    /// the node goes to its parent instead. Every element counts (void and
    /// self-closed ones too, which push nothing), as does a comment, but not
    /// text or a node foster-parented out of a table. `tag` and `name` are
    /// the start tag being inserted, empty for a comment.
    fn insert(&mut self, tag: &str, name: &str) {
        if self.open.len() >= MAX_OPEN && !self.fostered(tag, name) {
            self.open.pop();
        }
    }

    /// Whether the start tag `tag` named `name` is foster-parented: in a
    /// table, section or row, while the innermost element is one, a tag the
    /// table holds no element for goes before the table instead.
    fn fostered(&self, tag: &str, name: &str) -> bool {
        let in_table = matches!(self.mode, Mode::Table | Mode::TableBody | Mode::Row)
            && self.open.last().is_some_and(|e| {
                !e.is_foreign()
                    && matches!(
                        e.name.as_str(),
                        "table" | "tbody" | "tfoot" | "thead" | "tr"
                    )
            });
        let held = name.is_empty()
            || TABLE_PARTS.contains(&name)
            || matches!(name, "table" | "style" | "script" | "template" | "form")
            || (name == "input"
                && find_attr_value_range(tag, "type")
                    .is_some_and(|(s, e)| tag[s..e].eq_ignore_ascii_case("hidden")));
        in_table && !held
    }

    /// Sets the insertion mode from the open elements, as the tree builder's
    /// "reset the insertion mode appropriately" step does.
    fn reset_mode(&mut self) {
        self.mode = self
            .open
            .iter()
            .rev()
            .filter(|e| !e.is_foreign())
            .find_map(|e| match e.name.as_str() {
                "html" => Some(Mode::Body),
                name => mode_of(name),
            })
            .unwrap_or(Mode::Body);
    }

    /// The index of the innermost open HTML element named in `names`, when
    /// it is in table scope.
    fn in_table_scope(&self, names: &[&str]) -> Option<usize> {
        for (i, e) in self.open.iter().enumerate().rev() {
            if !e.is_foreign() && names.contains(&e.name.as_str()) {
                return Some(i);
            }
            if e.ends_scope(Scope::Table) {
                return None;
            }
        }
        None
    }

    /// Closes elements down to the innermost open HTML element named in
    /// `names` or a `<template>`, as the table modes' "clear the stack back
    /// to a context" steps do. With neither open, that is every element.
    fn clear_to(&mut self, names: &[&str]) {
        let keep = self.open.iter().rposition(|e| {
            !e.is_foreign() && (names.contains(&e.name.as_str()) || e.name == "template")
        });
        self.open.truncate(keep.map_or(0, |i| i + 1));
    }

    /// Applies the table insertion modes' rules for the start tag `<name>`:
    /// a cell, row, section or table part closes the open one it replaces,
    /// and a missing `<tbody>`, `<tr>` or `<colgroup>` is opened for it.
    /// Returns whether the tag still opens an element; a table part outside
    /// any table is dropped.
    fn table_start_tag(&mut self, name: &str) -> bool {
        loop {
            let part = TABLE_PARTS.contains(&name);
            match self.mode {
                Mode::Body => return !part,
                Mode::Cell | Mode::Caption if part => {
                    let (closes, next): (&[&str], _) = if self.mode == Mode::Cell {
                        (&["td", "th"], Mode::Row)
                    } else {
                        (&["caption"], Mode::Table)
                    };
                    let Some(i) = self.in_table_scope(closes) else {
                        return false;
                    };
                    self.open.truncate(i);
                    self.mode = next;
                }
                Mode::Row if matches!(name, "td" | "th") => {
                    self.clear_to(&["tr"]);
                    return true;
                }
                Mode::Row if part => {
                    let Some(i) = self.in_table_scope(&["tr"]) else {
                        return false;
                    };
                    self.open.truncate(i);
                    self.mode = Mode::TableBody;
                }
                Mode::TableBody if name == "tr" => {
                    self.clear_to(SECTIONS);
                    return true;
                }
                Mode::TableBody if matches!(name, "td" | "th") => {
                    self.clear_to(SECTIONS);
                    self.imply("tr");
                }
                Mode::TableBody if part => {
                    let Some(i) = self.in_table_scope(SECTIONS) else {
                        return false;
                    };
                    self.open.truncate(i);
                    self.mode = Mode::Table;
                }
                Mode::ColumnGroup if !matches!(name, "col" | "template") => {
                    // Only a `<colgroup>` still current is closed; under
                    // anything else the tag is dropped.
                    if !self
                        .open
                        .last()
                        .is_some_and(|e| !e.is_foreign() && e.name == "colgroup")
                    {
                        return false;
                    }
                    self.open.pop();
                    self.mode = Mode::Table;
                }
                Mode::Table | Mode::TableBody | Mode::Row => match name {
                    "caption" | "colgroup" | "tbody" | "tfoot" | "thead" => {
                        self.clear_to(&["table"]);
                        return true;
                    }
                    "col" => {
                        self.clear_to(&["table"]);
                        self.imply("colgroup");
                    }
                    "td" | "th" | "tr" => {
                        self.clear_to(&["table"]);
                        self.imply("tbody");
                    }
                    "table" => {
                        let Some(i) = self.in_table_scope(&["table"]) else {
                            return false;
                        };
                        self.open.truncate(i);
                        self.reset_mode();
                    }
                    _ => return true,
                },
                _ => return true,
            }
        }
    }

    /// Applies the table insertion modes' rules for the end tag `</name>`,
    /// which close a cell, row, section, caption, column group or table by
    /// the mode rather than by the elements open, and returns whether it
    /// handled the tag; any other goes on to the "in body" rules.
    fn table_end_tag(&mut self, name: &str) -> bool {
        loop {
            match (self.mode, name) {
                (Mode::Body | Mode::Template, _) => return false,
                (Mode::ColumnGroup, "template") => return false,
                (Mode::ColumnGroup, _) => {
                    let current = self
                        .open
                        .last()
                        .is_some_and(|e| !e.is_foreign() && e.name == "colgroup");
                    if name == "col" || !current {
                        return true;
                    }
                    self.open.pop();
                    self.mode = Mode::Table;
                    if name == "colgroup" {
                        return true;
                    }
                }
                (Mode::Table, "table") => {
                    if let Some(i) = self.in_table_scope(&["table"]) {
                        self.open.truncate(i);
                        self.reset_mode();
                    }
                    return true;
                }
                (Mode::TableBody, "tbody" | "tfoot" | "thead") => {
                    if self.in_table_scope(&[name]).is_some() {
                        self.clear_to(SECTIONS);
                        self.open.pop();
                        self.mode = Mode::Table;
                    }
                    return true;
                }
                (Mode::TableBody, "table") => {
                    if self.in_table_scope(SECTIONS).is_none() {
                        return true;
                    }
                    self.clear_to(SECTIONS);
                    self.open.pop();
                    self.mode = Mode::Table;
                }
                (Mode::Row, "tr" | "table" | "tbody" | "tfoot" | "thead") => {
                    if self.in_table_scope(&["tr"]).is_none()
                        || (name != "table" && self.in_table_scope(&[name]).is_none())
                    {
                        return true;
                    }
                    self.clear_to(&["tr"]);
                    self.open.pop();
                    self.mode = Mode::TableBody;
                    if name == "tr" {
                        return true;
                    }
                }
                (Mode::Cell, "td" | "th") => {
                    if let Some(i) = self.in_table_scope(&[name]) {
                        self.open.truncate(i);
                        self.mode = Mode::Row;
                    }
                    return true;
                }
                (Mode::Cell, "table" | "tbody" | "tfoot" | "thead" | "tr") => {
                    if self.in_table_scope(&[name]).is_none() {
                        return true;
                    }
                    if let Some(i) = self.in_table_scope(&["td", "th"]) {
                        self.open.truncate(i);
                    }
                    self.mode = Mode::Row;
                }
                (Mode::Caption, "caption" | "table") => {
                    let Some(i) = self.in_table_scope(&["caption"]) else {
                        return true;
                    };
                    self.open.truncate(i);
                    self.mode = Mode::Table;
                    if name == "caption" {
                        return true;
                    }
                }
                (
                    Mode::Table | Mode::TableBody | Mode::Row | Mode::Cell | Mode::Caption,
                    "body" | "caption" | "col" | "colgroup" | "html" | "tbody" | "td" | "tfoot"
                    | "th" | "thead" | "tr",
                ) => return true,
                _ => return false,
            }
        }
    }

    /// Closes what the "in body" rules close for the start tag `<name>`:
    /// an open `<p>` before a block, a list item before its sibling, a
    /// heading before a heading, and the like.
    fn body_start_tag(&mut self, name: &str) {
        match name {
            _ if CLOSES_P.contains(&name) => self.close_p(),
            _ if is_heading(name) => {
                self.close_p();
                if self
                    .open
                    .last()
                    .is_some_and(|e| !e.is_foreign() && is_heading(&e.name))
                {
                    self.open.pop();
                }
            }
            "li" | "dd" | "dt" => {
                let siblings: &[&str] = if name == "li" { &["li"] } else { &["dd", "dt"] };
                for i in (0..self.open.len()).rev() {
                    let e = &self.open[i];
                    if !e.is_foreign() && siblings.contains(&e.name.as_str()) {
                        self.open.truncate(i);
                        break;
                    }
                    if e.is_special()
                        && (e.is_foreign() || !matches!(e.name.as_str(), "address" | "div" | "p"))
                    {
                        break;
                    }
                }
                self.close_p();
            }
            "button" => {
                if let Some(i) = self.in_scope(name, Scope::Default) {
                    self.open.truncate(i);
                }
            }
            "option" | "optgroup"
                if self
                    .open
                    .last()
                    .is_some_and(|e| !e.is_foreign() && e.name == "option") =>
            {
                self.open.pop();
            }
            "rb" | "rp" | "rt" | "rtc" if self.in_scope("ruby", Scope::Default).is_some() => {
                let keep = if matches!(name, "rp" | "rt") {
                    "rtc"
                } else {
                    ""
                };
                while self.open.last().is_some_and(|e| {
                    !e.is_foreign() && e.name != keep && IMPLIED_END_TAGS.contains(&e.name.as_str())
                }) {
                    self.open.pop();
                }
            }
            "a" if self.formatting_element(name).is_some() => {
                // A second `<a>` ends the first, which leaves the stack even
                // when the adoption agency keeps it.
                self.adopt(name);
                if let Some(i) = self.formatting_element(name) {
                    self.open.remove(i);
                }
            }
            "nobr" if self.in_scope(name, Scope::Default).is_some() => {
                self.adopt(name);
            }
            _ => {}
        }
    }

    /// Closes an open `<p>` in button scope.
    fn close_p(&mut self) {
        if let Some(i) = self.in_scope("p", Scope::Button) {
            self.open.truncate(i);
        }
    }

    /// The innermost open formatting element `name` since the last marker
    /// (a cell, caption, `<applet>`, `<marquee>`, `<object>` or
    /// `<template>`), where the adoption agency looks for it.
    fn formatting_element(&self, name: &str) -> Option<usize> {
        for (i, e) in self.open.iter().enumerate().rev() {
            if e.is_foreign() {
                continue;
            }
            if e.name == name {
                return Some(i);
            }
            if MARKERS.contains(&e.name.as_str()) {
                return None;
            }
        }
        None
    }

    /// Applies the adoption agency's rounds to the open elements for the
    /// formatting element `name`, which callers have found open since the
    /// last marker. Each round moves the formatting element above the first
    /// special element open inside it, keeping between them only the
    /// formatting elements within three of that one; a round that finds no
    /// special element closes everything from the formatting element up.
    fn adopt(&mut self, name: &str) {
        for _ in 0..8 {
            let Some(fe) = self.formatting_element(name) else {
                return;
            };
            if self.open[fe + 1..]
                .iter()
                .any(|e| e.ends_scope(Scope::Default))
            {
                return;
            }
            let Some(block) = self.open[fe + 1..]
                .iter()
                .position(Element::is_special)
                .map(|j| fe + 1 + j)
            else {
                self.open.truncate(fe);
                return;
            };
            let mut removed = 0;
            for j in (fe + 1..block).rev() {
                let e = &self.open[j];
                let formatting = !e.is_foreign() && FORMATTING_TAGS.contains(&e.name.as_str());
                if !(formatting && block - j <= 3) {
                    self.open.remove(j);
                    removed += 1;
                }
            }
            // Removing the formatting element shifts the block down one
            // more; it goes back in just above the block.
            let element = self.open.remove(fe);
            self.open.insert(block - removed, element);
        }
    }

    /// Pops foreign elements until the innermost is HTML or an integration
    /// point.
    fn pop_foreign_to_point(&mut self) {
        while self
            .open
            .last()
            .is_some_and(|e| e.is_foreign() && e.point == Point::None)
        {
            self.open.pop();
        }
    }

    /// Whether a `<![CDATA[` here opens a CDATA section, which runs to `]]>`:
    /// only while the browser's current node is SVG or MathML, as WebKit
    /// reads it. Elsewhere it is a bogus comment, ending at the first `>`.
    fn cdata_allowed(&self) -> bool {
        self.open.last().is_some_and(Element::is_foreign)
    }

    /// Closes elements for the end tag `</name>`. While the innermost element
    /// is foreign, the tag closes the innermost foreign element of its name
    /// above the nearest HTML one; failing that, it is read as HTML, which can
    /// close foreign elements opened inside the HTML element it ends.
    fn end_tag(&mut self, name: &str) {
        if self.open.last().is_some_and(Element::is_foreign) {
            if matches!(name, "p" | "br") {
                self.pop_foreign_to_point();
            } else {
                let html = self.open.iter().rposition(|e| !e.is_foreign());
                let from = html.map_or(0, |i| i + 1);
                if let Some(i) = self.open[from..].iter().rposition(|e| e.name == name) {
                    self.open.truncate(from + i);
                    return;
                }
                if html.is_none() {
                    return;
                }
            }
        }
        self.html_end_tag(name);
    }

    /// Closes elements for the end tag `</name>` as the tree builder's "in
    /// body" rules do.
    fn html_end_tag(&mut self, name: &str) {
        if self.table_end_tag(name) {
            return;
        }
        let scope = match name {
            "body" | "html" => return,
            // `</br>` inserts a `<br>`, and a `</p>` with no `<p>` to close
            // an empty `<p>`, which can close the innermost element.
            "br" => return self.insert("", name),
            "p" if self.in_scope(name, Scope::Button).is_none() => {
                return self.insert("", name);
            }
            "p" => Scope::Button,
            "li" => Scope::ListItem,
            "table" | "caption" | "tbody" | "thead" | "tfoot" | "tr" | "td" | "th" => Scope::Table,
            "form" => {
                // `</form>` removes the form alone, leaving open what it holds.
                if let Some(i) = self.in_scope(name, Scope::Default) {
                    self.open.remove(i);
                }
                return;
            }
            _ if SCOPED_END_TAGS.contains(&name) || is_heading(name) => Scope::Default,
            _ if FORMATTING_TAGS.contains(&name) && self.formatting_element(name).is_some() => {
                self.adopt(name);
                return;
            }
            _ => {
                for i in (0..self.open.len()).rev() {
                    let e = &self.open[i];
                    if !e.is_foreign() && e.name == name {
                        self.open.truncate(i);
                        if name == "template" {
                            self.reset_mode();
                        }
                        return;
                    }
                    if e.is_special() {
                        return;
                    }
                }
                return;
            }
        };
        if let Some(i) = self.in_scope(name, scope) {
            self.open.truncate(i);
        }
    }

    /// The index of the innermost HTML element `</name>` ends, when it is in
    /// `scope`. A heading's end tag ends any heading.
    fn in_scope(&self, name: &str, scope: Scope) -> Option<usize> {
        for i in (0..self.open.len()).rev() {
            let e = &self.open[i];
            let ends = !e.is_foreign()
                && if is_heading(name) {
                    is_heading(&e.name)
                } else {
                    e.name == name
                };
            if ends {
                return Some(i);
            }
            if e.ends_scope(scope) {
                return None;
            }
        }
        None
    }
}

fn is_heading(name: &str) -> bool {
    matches!(name, "h1" | "h2" | "h3" | "h4" | "h5" | "h6")
}

/// HTML end tags the "in body" rules close only when their element is in
/// scope, besides `p`, `li`, headings, formatting elements and tables.
const SCOPED_END_TAGS: &[&str] = &[
    "address",
    "applet",
    "article",
    "aside",
    "blockquote",
    "button",
    "center",
    "dd",
    "details",
    "dialog",
    "dir",
    "div",
    "dl",
    "dt",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "header",
    "hgroup",
    "listing",
    "main",
    "marquee",
    "menu",
    "nav",
    "object",
    "ol",
    "pre",
    "search",
    "section",
    "summary",
    "ul",
];

/// HTML start tags that close an open `<p>` in button scope, besides
/// headings and list items. `<table>` does so only in standards mode, which
/// a card's `<!doctype html>` frame sets.
const CLOSES_P: &[&str] = &[
    "address",
    "article",
    "aside",
    "blockquote",
    "center",
    "details",
    "dialog",
    "dir",
    "div",
    "dl",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "form",
    "header",
    "hgroup",
    "hr",
    "listing",
    "main",
    "menu",
    "nav",
    "ol",
    "p",
    "plaintext",
    "pre",
    "search",
    "section",
    "summary",
    "table",
    "ul",
    "xmp",
];

/// The parts of a table, each only ever opened inside one.
const TABLE_PARTS: &[&str] = &[
    "caption", "col", "colgroup", "tbody", "td", "tfoot", "th", "thead", "tr",
];

/// The elements the tree builder closes without an end tag.
const IMPLIED_END_TAGS: &[&str] = &[
    "dd", "dt", "li", "optgroup", "option", "p", "rb", "rp", "rt", "rtc",
];

/// The elements that put a marker in the list of active formatting elements,
/// past which the adoption agency never looks.
const MARKERS: &[&str] = &[
    "applet", "caption", "marquee", "object", "td", "template", "th",
];

/// HTML formatting elements, whose end tags run the adoption agency.
const FORMATTING_TAGS: &[&str] = &[
    "a", "b", "big", "code", "em", "font", "i", "nobr", "s", "small", "strike", "strong", "tt", "u",
];

/// The tree builder's special HTML elements.
const SPECIAL_TAGS: &[&str] = &[
    "address",
    "applet",
    "area",
    "article",
    "aside",
    "base",
    "basefont",
    "bgsound",
    "blockquote",
    "body",
    "br",
    "button",
    "caption",
    "center",
    "col",
    "colgroup",
    "dd",
    "details",
    "dir",
    "div",
    "dl",
    "dt",
    "embed",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "form",
    "frame",
    "frameset",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "head",
    "header",
    "hgroup",
    "hr",
    "html",
    "iframe",
    "img",
    "input",
    "keygen",
    "li",
    "link",
    "listing",
    "main",
    "marquee",
    "menu",
    "meta",
    "nav",
    "noembed",
    "noframes",
    "noscript",
    "object",
    "ol",
    "p",
    "param",
    "plaintext",
    "pre",
    "script",
    "search",
    "section",
    "select",
    "source",
    "style",
    "summary",
    "table",
    "tbody",
    "td",
    "template",
    "textarea",
    "tfoot",
    "th",
    "thead",
    "title",
    "tr",
    "track",
    "ul",
    "wbr",
    "xmp",
];

/// Whether the foreign element `name` (opened by `tag`) is an integration
/// point.
fn integration_point(tag: &str, name: &str, math: bool) -> Point {
    match (math, name) {
        (false, "foreignobject" | "desc" | "title") => Point::Html,
        (true, "mi" | "mo" | "mn" | "ms" | "mtext") => Point::MathText,
        (true, "annotation-xml")
            if find_attr_value_range(tag, "encoding").is_some_and(|(s, e)| {
                let encoding = &tag[s..e];
                encoding.eq_ignore_ascii_case("text/html")
                    || encoding.eq_ignore_ascii_case("application/xhtml+xml")
            }) =>
        {
            Point::Html
        }
        _ => Point::None,
    }
}

/// Whether the start tag `tag` named `name` ends foreign content: the HTML
/// tags the tree builder never nests inside SVG or MathML.
fn breaks_out(tag: &str, name: &str) -> bool {
    match name {
        "font" => ["color", "face", "size"]
            .iter()
            .any(|a| find_attr(tag, a).is_some()),
        _ => BREAKOUT_TAGS.contains(&name),
    }
}

const BREAKOUT_TAGS: &[&str] = &[
    "b",
    "big",
    "blockquote",
    "body",
    "br",
    "center",
    "code",
    "dd",
    "div",
    "dl",
    "dt",
    "em",
    "embed",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "head",
    "hr",
    "i",
    "img",
    "li",
    "listing",
    "menu",
    "meta",
    "nobr",
    "ol",
    "p",
    "pre",
    "ruby",
    "s",
    "small",
    "span",
    "strong",
    "strike",
    "sub",
    "sup",
    "table",
    "tt",
    "u",
    "ul",
    "var",
];

/// HTML start tags that leave no element open: void elements, `<image>`
/// (read as `<img>`) and `<frame>` (dropped in a body).
const VOID_TAGS: &[&str] = &[
    "area", "base", "basefont", "bgsound", "br", "col", "embed", "frame", "hr", "image", "img",
    "input", "keygen", "link", "meta", "param", "source", "track", "wbr",
];

impl Iterator for Tags<'_> {
    type Item = Tag;

    fn next(&mut self) -> Option<Tag> {
        let (start, mut end) = next_tag(self.html, self.pos)?;
        let cdata = self.html[start..].starts_with("<![CDATA[") && self.cdata_allowed();
        if cdata {
            let body = start + "<![CDATA[".len();
            end = self.html[body..]
                .find("]]>")
                .map_or(self.html.len(), |e| body + e + "]]>".len());
        }
        let tag = &self.html[start..end];
        let text_end = if tag.as_bytes()[1].is_ascii_alphabetic() {
            match tag_name(tag) {
                Some(name) => self.start_tag(tag, &name, end),
                None => end,
            }
        } else {
            match tag.strip_prefix("</") {
                // An end tag starts with a letter; `</>` is dropped, and any
                // other is a bogus comment.
                Some(rest) if rest.starts_with(|c: char| c.is_ascii_alphabetic()) => {
                    if let Some(name) = tag_name(&format!("<{rest}")) {
                        self.end_tag(&name);
                    }
                }
                Some(">") => {}
                Some(_) => self.insert("", ""),
                // A comment, bogus or not; a doctype and a CDATA section
                // insert nothing.
                None if !cdata
                    && !tag
                        .get(..9)
                        .is_some_and(|t| t.eq_ignore_ascii_case("<!doctype")) =>
                {
                    self.insert("", "")
                }
                None => {}
            }
            end
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
    find_attr(tag, attr).flatten()
}

/// [`find_attr_value_range`]'s lookup, telling a missing attribute (`None`)
/// from one written without `=` (`Some(None)`).
fn find_attr(tag: &str, attr: &str) -> Option<Option<(usize, usize)>> {
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
                return Some(None);
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
            return Some(Some((vs, ve)));
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

/// `s` with the five entities [`escape_attr`] writes decoded.
pub fn decode_entities(s: &str) -> String {
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
    fn deep_nesting_scans_in_one_pass() {
        // Each stray end tag walks the open `<g>`s, which WebKit's depth cap
        // holds to 509 inside a card: 20,000 of them cost about 10 million
        // steps, not the 400 million an uncapped stack would.
        let html = format!(
            "<svg>{}{}<h2>end</h2>",
            "<g>".repeat(20_000),
            "</x>".repeat(20_000)
        );
        assert_eq!(tags(&html).count(), 40_003);
        assert_eq!(card_label(&html).as_deref(), Some("end"));
        // Whatever opens the elements, the stack never holds more than the
        // cap, so no walk over it costs more than 509 steps.
        for unit in [
            "<div>",
            "<span>",
            "<b>",
            "<svg><g>",
            "<table><tr><td>",
            "<table><caption>",
            "<math><mi>",
        ] {
            let html = unit.repeat(2_000);
            let mut scan = tags(&html);
            while scan.next().is_some() {
                assert!(scan.open.len() <= MAX_OPEN, "{unit}: {}", scan.open.len());
            }
        }
    }

    #[test]
    fn a_node_inserted_at_webkits_depth_cap_closes_the_innermost_element() {
        // WebKit's stack holds 512 elements, three of them the frame's
        // `<html>`, `<body>` and wrapper `<div>`. Each case runs inside `k`
        // more `<div>`s: at 508 its `<svg>` fills the stack (at 507, its
        // `<foreignObject>` does), and the next node inserted closes that
        // element. The `<style>` after it shows whether an `<svg>` is still
        // open: inside one, `<style>` holds markup and its `<b>` is a tag;
        // otherwise it is HTML raw text. Each count is the `<b>` elements
        // WebKit builds.
        for (k, inner, bs) in [
            (508, "<svg>", 1),
            (507, "<svg><!--c-->", 1),
            (508, "<svg><!--c-->", 0),
            (508, "<svg></3>", 0),
            (508, "<svg><!x>", 0),
            (508, "<svg><?x>", 0),
            (508, "<svg><g/>", 0),
            // Nothing is inserted for these.
            (508, "<svg><!doctype html>", 1),
            (508, "<svg><![CDATA[a]]>", 1),
            (508, "<svg>text", 1),
            (508, "<svg></>", 1),
            // `<html>` and `<body>` open nothing in a body.
            (507, "<body><svg><!--c-->", 1),
            (507, "<html><svg><!--c-->", 1),
            // A node foster-parented out of a table leaves it open.
            (508, "<table><svg></svg></table>", 0),
            (508, "<table><input><svg></table>", 0),
            (508, "<table><input type=hidden><svg></table>", 1),
            (508, "<table><col><svg></table>", 1),
            // With the table closed, its row's mode still closes the row.
            (508, "<table><tr><svg></table>", 0),
            (506, "<svg><foreignObject></p>", 0),
            (507, "<svg><foreignObject></p>", 1),
            (507, "<svg><foreignObject></br>", 1),
            (507, "<svg><foreignObject><img>", 1),
            (507, "<svg><foreignObject><frame>", 0),
        ] {
            let html = format!("{}{inner}<style><b>x</b></style>", "<div>".repeat(k));
            let names: Vec<&str> = tags(&html).map(|t| &html[t.start..t.end]).collect();
            assert_eq!(
                names.iter().filter(|n| **n == "<b>").count(),
                bs,
                "{k} {inner}"
            );
        }
        // Past the cap the `<g>` opens in place of the `<svg>`, so `</svg>`
        // finds nothing to close and the `<g>` stays open.
        for n in [600, 2000] {
            let html = format!("{}<svg><g></svg><style><b>x</b></style>", "<div>".repeat(n));
            assert_eq!(
                tags(&html)
                    .filter(|t| &html[t.start..t.end] == "<b>")
                    .count(),
                1
            );
        }
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
    fn an_html_island_inside_svg_reads_tags_as_html() {
        let html =
            "<svg><foreignObject><style/><b>x</b></style></foreignObject><title/></svg><b>y</b>";
        let names: Vec<&str> = tags(html).map(|t| &html[t.start..t.end]).collect();
        assert_eq!(
            names,
            [
                "<svg>",
                "<foreignObject>",
                "<style/>",
                "</style>",
                "</foreignObject>",
                "<title/>",
                "</svg>",
                "<b>",
                "</b>"
            ]
        );

        let html = "<math><mi><style/><b>x</b></style></mi><annotation-xml encoding=\"Text/HTML\"><style/><b>x</b></style></annotation-xml><annotation-xml><title/></annotation-xml></math>";
        let names: Vec<&str> = tags(html).map(|t| &html[t.start..t.end]).collect();
        assert_eq!(
            names,
            [
                "<math>",
                "<mi>",
                "<style/>",
                "</style>",
                "</mi>",
                "<annotation-xml encoding=\"Text/HTML\">",
                "<style/>",
                "</style>",
                "</annotation-xml>",
                "<annotation-xml>",
                "<title/>",
                "</annotation-xml>",
                "</math>"
            ]
        );
    }

    #[test]
    fn raw_text_elements_hold_markup_inside_svg_and_math() {
        let html = "<svg><style><b>x</b></style></svg>";
        let names: Vec<&str> = tags(html).map(|t| &html[t.start..t.end]).collect();
        assert_eq!(
            names,
            ["<svg>", "<style>", "<b>", "</b>", "</style>", "</svg>"]
        );
        // How many `<b>` elements Chromium builds from each.
        for (html, bs) in [
            ("<svg><script><b>x</b></script></svg>", 1),
            ("<svg><textarea><b>x</b></textarea></svg>", 1),
            ("<math><style><b>x</b></style></math>", 1),
            ("<svg><g><style><b>x</b></style></g></svg><b>y</b>", 2),
            // An svg `<title>` is an HTML island, so its start tags are HTML.
            ("<svg><title><b>x</b></title></svg>", 1),
            ("<svg><title><style/><b>x</b></style></title></svg>", 0),
            ("<math><mtext><title><b>x</b></title></mtext></math>", 0),
            (
                "<svg><style>.a{fill:red}</style><rect/></svg><style/><b>x</b></style>",
                0,
            ),
        ] {
            let names: Vec<&str> = tags(html).map(|t| &html[t.start..t.end]).collect();
            assert_eq!(
                names.iter().filter(|n| **n == "<b>").count(),
                bs,
                "{html}: {names:?}"
            );
        }
    }

    #[test]
    fn a_cdata_section_is_text_while_the_current_node_is_foreign() {
        let html = "<svg><script><![CDATA[if(a>b){s='<b>x</b>'}]]></script></svg>";
        let names: Vec<&str> = tags(html).map(|t| &html[t.start..t.end]).collect();
        assert_eq!(
            names,
            [
                "<svg>",
                "<script>",
                "<![CDATA[if(a>b){s='<b>x</b>'}]]>",
                "</script>",
                "</svg>"
            ]
        );
        // How many `<b>` elements WebKit builds from each. Chromium reads a
        // CDATA section at any integration point as a bogus comment.
        for (html, bs) in [
            ("<svg><style><![CDATA[a>b]]><b>x</b></style></svg>", 1),
            ("<svg><![CDATA[x>y<b>z</b>", 0),
            (
                "<math><annotation-xml><![CDATA[x>y<b>z</b>]]></annotation-xml></math>",
                0,
            ),
            (
                "<svg><foreignObject><![CDATA[x>y<b>z</b>]]></foreignObject></svg>",
                0,
            ),
            ("<math><mi><![CDATA[x>y<b>z</b>]]></mi></math>", 0),
            (
                "<svg><foreignObject><div></div><![CDATA[x>y<b>z</b>]]></foreignObject></svg>",
                0,
            ),
            (
                "<svg><foreignObject><br><![CDATA[x>y<b>z</b>]]></foreignObject></svg>",
                0,
            ),
            (
                "<svg><title><style/>a</style><![CDATA[x>y<b>z</b>]]></title></svg>",
                0,
            ),
            // A bogus comment, ending at the first `>`, where the current
            // node is HTML.
            ("<p><![CDATA[x>y<b>z</b>]]>", 1),
            (
                "<svg><foreignObject><div><![CDATA[x>y<b>z</b>]]></div></foreignObject></svg>",
                1,
            ),
        ] {
            let names: Vec<&str> = tags(html).map(|t| &html[t.start..t.end]).collect();
            assert_eq!(
                names.iter().filter(|n| **n == "<b>").count(),
                bs,
                "{html}: {names:?}"
            );
        }
        // Start tags that leave no HTML element open inside the island.
        for tag in ["image", "frame", "keygen", "basefont", "bgsound"] {
            let html =
                format!("<svg><foreignObject><{tag}><![CDATA[x>y<b>z</b>]]></foreignObject></svg>");
            let names: Vec<&str> = tags(&html).map(|t| &html[t.start..t.end]).collect();
            assert!(!names.contains(&"<b>"), "{html}: {names:?}");
        }
    }

    #[test]
    fn a_start_tag_closes_what_the_browser_closes_before_it() {
        // Each runs inside `<svg><foreignObject>`, then a CDATA section: one
        // element left open that WebKit closed makes it a bogus comment, and
        // the `<b>` inside it a tag. The count is WebKit's `<b>` elements.
        for (inner, bs) in [
            ("<p>a<p>b</p>", 0),
            ("<li>a<li>b</li>", 0),
            ("<li>a<div><li>b</li></div>", 0),
            ("<li>a<span><li>b</li>", 0),
            ("<li><ul><li>b</li></ul>", 1),
            ("<dd>a<dt>b</dt>", 0),
            ("<dt>a<dd>b</dd>", 0),
            ("<option>a<option>b</option>", 0),
            ("<optgroup><option>a<optgroup>b</optgroup>", 1),
            ("<p>a<div>b</div>", 0),
            ("<p>a<h1>b</h1>", 0),
            ("<h1>a<h2>b</h2>", 0),
            ("<button>a<button>b</button>", 0),
            ("<p>a<table></table>", 0),
            ("<p>a<form></form>", 0),
            ("<table><tr><td>a<td>b</td></tr></table>", 0),
            ("<table><tr><td>a<tr><td>b</td></tr></table>", 0),
            ("<table><tr><td>a<th>b</table>", 0),
            ("<table><tbody><tr><td>a<tbody><tr><td>b</table>", 0),
            ("<table><caption>a<tr><td>b</table>", 0),
            ("<table><tr><td><table><tr><td>b</table></table>", 0),
            (
                "<table><tr><td><svg><foreignObject><td>b</td></tr></table>",
                0,
            ),
            ("<table><tr><td><math><mi><td>b</td></tr></table>", 0),
            ("<p><svg><foreignObject><p>a</foreignObject></svg>", 1),
            ("<li><svg><foreignObject><li>a</foreignObject></svg>", 1),
            // A table part outside a table opens nothing.
            ("<td>a", 0),
            ("<tr>a", 0),
            ("<caption>a", 0),
            // The adoption agency.
            ("<em><div></em></div>", 0),
            ("<em><i><div></em></div></i>", 0),
            ("<em><span><div></em></div>", 0),
            ("<em><div><span></em></span></div>", 0),
            ("<em><i><s><u><div></em></div></u></s></i>", 0),
            ("<a><div></a></div>", 0),
            ("<a>x<a>y</a>", 0),
            ("<a><div><a></a><svg></a>", 0),
            ("<em><table><tr><td></em></td></tr></table>", 1),
        ] {
            let html =
                format!("<svg><foreignObject>{inner}<![CDATA[x>y<b>z</b>]]></foreignObject></svg>");
            let names: Vec<&str> = tags(&html).map(|t| &html[t.start..t.end]).collect();
            assert_eq!(
                names.iter().filter(|n| **n == "<b>").count(),
                bs,
                "{html}: {names:?}"
            );
        }
        // Closing what holds an `<svg>` closes it, and `<style/>` then
        // opens HTML raw text. The count is WebKit's `<p>` elements, since the
        // agency clones a `<b>`.
        for (html, ps) in [
            ("<b><div><svg></b><style/><p>x</p></style>", 0),
            ("<b><svg></b><style/><p>x</p></style>", 0),
            ("<li><svg><li><style/><p>x</p></style>", 0),
            (
                "<table><tr><td><svg><foreignObject><tr><style/><p>x</p></style>",
                0,
            ),
            ("<table><tr><td><svg><td><style/><p>x</p></style>", 1),
        ] {
            let names: Vec<&str> = tags(html).map(|t| &html[t.start..t.end]).collect();
            assert_eq!(
                names.iter().filter(|n| **n == "<p>").count(),
                ps,
                "{html}: {names:?}"
            );
        }
    }

    #[test]
    fn an_html_tag_ends_svg_early() {
        for html in [
            "<svg><p>x</p><style/><b>x</b></style>",
            "<svg><g><font color=red>x</font><style/><b>x</b></style>",
            "<svg><font face>x</font><style/><b>x</b></style>",
            "<svg><g></p><style/><b>x</b></style>",
        ] {
            let names: Vec<&str> = tags(html).map(|t| &html[t.start..t.end]).collect();
            assert!(!names.contains(&"<b>"), "{html}: {names:?}");
        }
        // How many `<b>` elements Chromium builds from each.
        for (html, bs) in [
            // `</br>` ends the svg, so `<title/>` opens HTML title text.
            ("<svg><g></br><title/></svg><b>y</b>", 0),
            // `<mglyph>` stays MathML inside `<mi>`, so `<style/>` is closed.
            ("<math><mi><mglyph><style/></mglyph></mi></math><b>y</b>", 1),
            // An `<svg>` in any `<annotation-xml>` is SVG, with its islands.
            ("<math><annotation-xml><svg><foreignObject><b>x</b></foreignObject><title/></svg></annotation-xml></math><b>y</b>", 2),
            ("<math><annotation-xml encoding='application/xhtml+xml'><style/><b>x</b></style></annotation-xml></math>", 0),
            // One `</svg>` closes every element inside it.
            ("<svg><g><g><a></svg><title/><b>y</b>", 0),
        ] {
            let names: Vec<&str> = tags(html).map(|t| &html[t.start..t.end]).collect();
            assert_eq!(names.iter().filter(|n| **n == "<b>").count(), bs, "{html}: {names:?}");
        }
        // `<font>` without a colour, face or size stays inside the svg.
        let html = "<svg><font><title/></font></svg>";
        let names: Vec<&str> = tags(html).map(|t| &html[t.start..t.end]).collect();
        assert_eq!(names, ["<svg>", "<font>", "<title/>", "</font>", "</svg>"]);
    }

    #[test]
    fn an_html_end_tag_closes_the_foreign_content_inside_it() {
        // How many `<b>` elements WebKit builds from each.
        for (html, bs) in [
            ("<div><svg></div><style/><b>x</b></style>", 0),
            ("<div><svg><g></div><style/><b>x</b></style>", 0),
            ("<span><svg></span><style/><b>x</b></style>", 0),
            ("<b><svg></b><style/><b>x</b></style>", 1),
            ("<a><svg></a><style/><b>x</b></style>", 0),
            ("<li><svg></li><style/><b>x</b></style>", 0),
            ("<ul><li><svg></ul><style/><b>x</b></style>", 0),
            ("<h1><svg></h2><style/><b>x</b></style>", 0),
            ("<button><p><svg></button><style/><b>x</b></style>", 0),
            (
                "<table><tr><td><svg></td></tr></table><style/><b>x</b></style>",
                0,
            ),
            ("<div><form><svg></div><style/><b>x</b></style>", 0),
            ("<form><div><svg></form></div><style/><b>x</b></style>", 0),
            ("<svg><foreignObject></svg><style/><b>x</b></style>", 0),
            // `</p>` and `</br>` close the foreign elements, then read as
            // HTML.
            ("<p><svg><g></p><style/><b>x</b></style>", 0),
            ("<div><svg><g></br><style/><b>x</b></style>", 0),
            // An end tag that names no open element, or one outside its
            // scope, leaves the svg open.
            ("<div><svg></span><style/><b>x</b></style>", 1),
            ("<div><svg></foo><style/><b>x</b></style>", 1),
            ("<li><ol><svg></li><style/><b>x</b></style>", 1),
            // `</form>` removes the form alone.
            ("<form><svg></form><style/><b>x</b></style>", 1),
            // An integration point bounds the scope.
            (
                "<div><svg><foreignObject></div></foreignObject><style/><b>x</b></style>",
                1,
            ),
            ("<div><math><mi></div></mi><style/><b>x</b></style>", 1),
            (
                "<div><math><annotation-xml></div><style/><b>x</b></style>",
                1,
            ),
            (
                "<p><svg><foreignObject><div></p></div></foreignObject><style/><b>x</b></style>",
                1,
            ),
            (
                "<math><annotation-xml></div></annotation-xml></math><style/><b>x</b></style>",
                0,
            ),
            // `</svg>` stops at the special element open in the island.
            (
                "<svg><foreignObject><div></svg></div></foreignObject><style/><b>x</b></style>",
                1,
            ),
            (
                "<svg><foreignObject><span></svg></span></foreignObject><style/><b>x</b></style>",
                1,
            ),
            ("<svg><foreignObject><div></svg><title/><b>y</b>", 0),
        ] {
            let names: Vec<&str> = tags(html).map(|t| &html[t.start..t.end]).collect();
            assert_eq!(
                names.iter().filter(|n| **n == "<b>").count(),
                bs,
                "{html}: {names:?}"
            );
        }
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
