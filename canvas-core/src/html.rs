//! Tag-level HTML scanning shared by the CLI's post-time rewrites and
//! canvasd's export: no parser, just quote-aware tag boundaries and
//! attribute lookups over the byte string.

use std::collections::hash_map::DefaultHasher;
use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::ops::Range;

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
/// `<title>`, `<textarea>`, `<xmp>`, `<iframe>`, `<noembed>` or `<noframes>`
/// element holds no tags, so after its opening tag the scan resumes at its
/// closing tag, which the tag's `text_end` holds; after `<plaintext>` the
/// rest of the HTML is its text. In
/// SVG or MathML those elements hold markup like any other, and a
/// `<![CDATA[` section there is one tag through its `]]>`, except at an
/// integration point such as `<foreignObject>`.
pub fn tags(html: &str) -> Tags<'_> {
    Tags {
        html,
        pos: 0,
        open: OpenElements::default(),
        formatting: Vec::new(),
        next_id: 0,
        form: None,
        mode: Mode::Body,
        templates: Vec::new(),
        freezes: false,
    }
}

/// One tag from [`tags`]: the [`next_tag`] range `[start, end)`, and for
/// the opening tag of an element the scan read as raw text, `text_end`: its
/// closing tag (or the end of the HTML), where the scan resumes, so
/// `[end, text_end)` is the element's text. Any other tag, an SVG or MathML
/// `<style>` included, has none and the scan resumes at `end`. `foreign`
/// is true for a start tag that opened an SVG or MathML element, `<svg>` and
/// `<math>` themselves included: the browser gives such an element none of
/// its HTML namesake's behavior, so an SVG `<link>` loads no stylesheet and
/// an SVG `<script>` ignores `src`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tag {
    pub start: usize,
    pub end: usize,
    pub text_end: Option<usize>,
    pub foreign: bool,
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
    /// adoption agency's rounds. Like WebKit's, the stack holds at most
    /// [`MAX_OPEN`] elements: see [`Tags::insert`].
    open: OpenElements,
    /// The tree builder's list of active formatting elements, oldest first,
    /// `None` for a marker. An entry outlives its element's place on the
    /// stack: text, and most start tags, first reopen every entry since the
    /// last marker whose element was closed, as the browser does.
    formatting: Vec<Option<Formatting>>,
    /// The id the next opened element gets.
    next_id: usize,
    /// The tree builder's form element pointer: the id of the last `<form>`
    /// opened outside a `<template>`. Only a `</form>` outside a template
    /// clears it, so it outlives the form's place on the stack, and while it
    /// is set a `<form>` outside a template is dropped.
    form: Option<usize>,
    /// The tree builder's insertion mode. Like WebKit's it is kept, not read
    /// off the open elements each time: the depth cap can close the table,
    /// section, row or cell that set it, and the mode stays.
    mode: Mode,
    /// The tree builder's stack of template insertion modes, one per open
    /// `<template>`, innermost last. A template starts in [`Mode::Template`],
    /// and its first start tag that the "in head" rules don't handle picks
    /// the mode its contents keep: a table part's, else [`Mode::Body`].
    /// Only `</template>` pops it, so like WebKit's it outlives a template
    /// the depth cap closed.
    templates: Vec<Mode>,
    /// Set once an end tag that closes a table, section or row reached the
    /// cell mode with no cell in table scope, the depth cap having closed it.
    /// The spec has the end tag close the cell first, and WebKit's "close
    /// the cell" finds none, so the older WebKit Canvas.app runs reprocesses
    /// that end tag in the same mode forever. The scan goes on as a newer
    /// WebKit does, in the row mode.
    freezes: bool,
}

/// WebKit's cap on its stack of open elements
/// (`defaultMaximumHTMLParserDOMTreeDepth`), less the `<html>`, `<body>` and
/// wrapper `<div>` that the viewer's card frame and an exported page both put
/// around a card.
const MAX_OPEN: usize = 512 - 3;

/// The stack of open elements, innermost last, and how many of them are HTML
/// `<template>`s, kept as it changes so asking whether the scan is inside one
/// costs nothing: card_label asks after every tag. Reads go through the
/// slice; every change goes through a method here, which keeps the count.
#[derive(Default)]
struct OpenElements {
    elements: Vec<Element>,
    templates: usize,
}

impl std::ops::Deref for OpenElements {
    type Target = [Element];

    fn deref(&self) -> &[Element] {
        &self.elements
    }
}

impl OpenElements {
    fn push(&mut self, element: Element) {
        self.insert(self.elements.len(), element);
    }

    fn insert(&mut self, i: usize, element: Element) {
        self.templates += usize::from(element.is_template());
        self.elements.insert(i, element);
    }

    fn pop(&mut self) -> Option<Element> {
        let element = self.elements.pop()?;
        self.templates -= usize::from(element.is_template());
        Some(element)
    }

    fn remove(&mut self, i: usize) -> Element {
        let element = self.elements.remove(i);
        self.templates -= usize::from(element.is_template());
        element
    }

    fn truncate(&mut self, len: usize) {
        while self.elements.len() > len {
            self.pop();
        }
    }

    fn retain(&mut self, keep: impl Fn(&Element) -> bool) {
        let templates = &mut self.templates;
        self.elements.retain(|e| {
            let kept = keep(e);
            *templates -= usize::from(!kept && e.is_template());
            kept
        });
    }

    fn set_id(&mut self, i: usize, id: usize) {
        self.elements[i].id = id;
    }
}

/// One open element. `id` tells it from a copy the adoption agency or a
/// reopening made of it. `special` is whether the tree builder counts it as
/// special: an end tag for another element never closes past it. It is read
/// on every walk down the stack, so it is worked out once.
struct Element {
    id: usize,
    name: String,
    ns: Ns,
    point: Point,
    special: bool,
}

/// An entry in the list of active formatting elements: the open or closed
/// element `id`, and what tells it from another of its name, its attributes
/// (lowercased names, decoded values, sorted), with `hash` a hash of both.
struct Formatting {
    id: usize,
    name: String,
    attrs: Vec<(String, String)>,
    hash: u64,
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

    /// Whether the browser reads what follows this element, as the current
    /// node, as foreign content: it is SVG or MathML and no integration point.
    fn holds_foreign_content(&self) -> bool {
        self.is_foreign() && self.point == Point::None
    }

    fn is_template(&self) -> bool {
        !self.is_foreign() && self.name == "template"
    }

    fn is_special(&self) -> bool {
        self.special
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
                        | "select"
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
    /// its text ends when the scan reads it as raw text, and whether it
    /// opened an SVG or MathML element.
    fn start_tag(&mut self, tag: &str, name: &str, end: usize) -> (Option<usize>, bool) {
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
                _ => return (self.html_start_tag(tag, name, end), false),
            }
        } else if self.open.last().is_some_and(|e| e.ns == Ns::Math) {
            Ns::Math
        } else {
            Ns::Svg
        };
        // An `<svg>` or `<math>` read as HTML meets the table modes first,
        // which can close a `<colgroup>`.
        if as_html {
            if !self.table_start_tag(name) {
                return (None, false);
            }
            self.reconstruct();
        }
        self.insert(tag, name);
        if !self_closing {
            self.open_element(name, ns, integration_point(tag, name, ns == Ns::Math));
        }
        (None, true)
    }

    /// Updates the open elements for the HTML start tag `<name>`, closing
    /// what the tree builder closes before inserting it, and returns where
    /// its text ends when it is a raw-text element.
    fn html_start_tag(&mut self, tag: &str, name: &str, end: usize) -> Option<usize> {
        // A card is already in a body, where `<html>` and `<body>` only add
        // attributes to the open ones and `<head>` is dropped.
        if !self.table_start_tag(name) || matches!(name, "html" | "body" | "head") {
            return None;
        }
        let sets_pointer = name == "form" && !self.in_template();
        if sets_pointer && self.form.is_some() {
            return None;
        }
        // A table's rules insert a `<form>` where it is and close it at once.
        if name == "form" && matches!(self.mode, Mode::Table | Mode::TableBody | Mode::Row) {
            self.insert(tag, name);
            let id = self.push_html(name);
            if sets_pointer {
                self.form = Some(id);
            }
            self.open.pop();
            return None;
        }
        if !self.body_start_tag(name) {
            return None;
        }
        if self.reconstructs(tag, name) {
            self.reconstruct();
        }
        // A `<frame>` in a body is dropped, so nothing is inserted.
        if name != "frame" {
            self.insert(tag, name);
        }
        if !VOID_TAGS.contains(&name) {
            let id = self.push_html(name);
            if FORMATTING_TAGS.contains(&name) {
                self.push_formatting(id, name, tag);
            }
            if sets_pointer {
                self.form = Some(id);
            }
        }
        match name {
            // In HTML `<style/>` still opens its text.
            "iframe" | "noembed" | "noframes" | "script" | "style" | "textarea" | "title"
            | "xmp" => Some(closing_tag_start(self.html, end, name)),
            // Nothing after `<plaintext>` is a tag, its own end tag included.
            "plaintext" => Some(self.html.len()),
            _ => None,
        }
    }

    /// Whether the "in body" rules reopen the formatting elements before
    /// inserting the start tag `tag` named `name`. A block, a heading, a list
    /// item, a table part and what the "in head" rules insert do not, nor
    /// does a hidden `<input>` while a table's rules read it.
    fn reconstructs(&self, tag: &str, name: &str) -> bool {
        let table = matches!(self.mode, Mode::Table | Mode::TableBody | Mode::Row);
        !((CLOSES_P.contains(&name) && name != "xmp")
            || is_heading(name)
            || TABLE_PARTS.contains(&name)
            || KEEPS_FORMATTING_CLOSED.contains(&name)
            || (table && hidden_input(tag, name)))
    }

    /// Reopens the formatting elements since the last marker whose elements
    /// are closed, in order, each a copy of the one closed, as the tree
    /// builder's "reconstruct the active formatting elements" step does.
    fn reconstruct(&mut self) {
        let Some(Some(newest)) = self.formatting.last() else {
            return;
        };
        if self.open_index(newest.id).is_some() {
            return;
        }
        let open: HashSet<usize> = self.open.iter().map(|e| e.id).collect();
        let last = self.formatting.len() - 1;
        let mut first = last;
        while let Some(Some(entry)) = first.checked_sub(1).map(|i| &self.formatting[i]) {
            if open.contains(&entry.id) {
                break;
            }
            first -= 1;
        }
        let mut i = first;
        while i <= last {
            let Some(entry) = &self.formatting[i] else {
                return;
            };
            let name = entry.name.clone();
            self.insert("", &name);
            let id = self.push_html(&name);
            if let Some(entry) = &mut self.formatting[i] {
                entry.id = id;
            }
            // With the stack full, each later entry's insertion closes the
            // one before it, so only the newest stays open.
            if self.open.len() >= MAX_OPEN && i + 1 < last {
                self.open.pop();
                i = last;
            } else {
                i += 1;
            }
        }
    }

    /// Adds the formatting element `id`, opened by `tag`, to the list. As
    /// in the browser, a fourth entry since the last marker with the same
    /// name and attributes removes the oldest of those.
    fn push_formatting(&mut self, id: usize, name: &str, tag: &str) {
        let attrs = attr_key(tag);
        let mut hasher = DefaultHasher::new();
        (name, &attrs).hash(&mut hasher);
        let hash = hasher.finish();
        let mut same = 0;
        let mut oldest = None;
        for (i, entry) in self.formatting.iter().enumerate().rev() {
            let Some(f) = entry else {
                break;
            };
            if f.hash == hash && f.name == name && f.attrs == attrs {
                same += 1;
                oldest = Some(i);
            }
        }
        if let Some(i) = oldest.filter(|_| same >= 3) {
            self.formatting.remove(i);
        }
        self.formatting.push(Some(Formatting {
            id,
            name: name.to_string(),
            attrs,
            hash,
        }));
    }

    /// Removes the list's entries back to and including the last marker,
    /// as closing a cell, caption, `<applet>`, `<marquee>`, `<object>` or
    /// `<template>` does.
    fn clear_to_marker(&mut self) {
        let marker = self.formatting.iter().rposition(Option::is_none);
        self.formatting.truncate(marker.unwrap_or(0));
    }

    /// The index on the stack of the open element `id`.
    fn open_index(&self, id: usize) -> Option<usize> {
        self.open.iter().rposition(|e| e.id == id)
    }

    /// Whether the element `id` has an entry in the list.
    fn listed(&self, id: usize) -> bool {
        self.formatting.iter().flatten().any(|f| f.id == id)
    }

    /// Opens an element and returns its id.
    fn open_element(&mut self, name: &str, ns: Ns, point: Point) -> usize {
        let id = self.next_id;
        self.next_id += 1;
        let mut element = Element {
            id,
            name: name.to_string(),
            ns,
            point,
            special: false,
        };
        element.special = match ns {
            Ns::Html => SPECIAL_TAGS.contains(&name),
            _ => element.ends_foreign_scope(),
        };
        self.open.push(element);
        id
    }

    /// Opens the HTML element `<name>`, switching to the insertion mode a
    /// table, one of its parts or a `<template>` starts, and adding the
    /// marker a cell, caption, `<applet>`, `<marquee>`, `<object>` or
    /// `<template>` puts in the list. Returns its id.
    fn push_html(&mut self, name: &str) -> usize {
        let id = self.open_element(name, Ns::Html, Point::None);
        if let Some(mode) = mode_of(name) {
            self.mode = mode;
        }
        if name == "template" {
            self.templates.push(Mode::Template);
        }
        if MARKERS.contains(&name) {
            self.formatting.push(None);
        }
        id
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
            || hidden_input(tag, name);
        in_table && !held
    }

    /// Whether the open element at the top of the stack is a table or one of
    /// the parts that holds rows, where text that is only whitespace goes in
    /// place and any other is foster-parented.
    fn at_table(&self) -> bool {
        self.open.last().is_some_and(|e| {
            !e.is_foreign()
                && matches!(
                    e.name.as_str(),
                    "table" | "tbody" | "template" | "tfoot" | "thead" | "tr"
                )
        })
    }

    /// Applies the text from the scan's position up to `to`: in an HTML
    /// context, the tree builder reopens the closed formatting elements
    /// before inserting it. A table, section or row takes text that is only
    /// whitespace without reopening them, and a column group closes for any
    /// other text. A NUL is dropped, and in SVG or MathML text goes in as is.
    fn text(&mut self, to: usize) {
        let text = &self.html[self.pos.min(to)..to];
        let chars = text.trim_matches('\0');
        if chars.is_empty() || self.open.last().is_some_and(Element::holds_foreign_content) {
            return;
        }
        let space = chars
            .bytes()
            .all(|b| matches!(b, b'\t' | b'\n' | 0x0c | b'\r' | b' ' | 0));
        match self.mode {
            Mode::Table | Mode::TableBody | Mode::Row if space && self.at_table() => return,
            Mode::ColumnGroup if space => return,
            Mode::ColumnGroup => {
                if !self
                    .open
                    .last()
                    .is_some_and(|e| !e.is_foreign() && e.name == "colgroup")
                {
                    return;
                }
                self.open.pop();
                self.mode = Mode::Table;
            }
            _ => {}
        }
        self.reconstruct();
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
                "template" => Some(self.templates.last().copied().unwrap_or(Mode::Template)),
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
                Mode::Template => {
                    let next = match name {
                        "base" | "basefont" | "bgsound" | "link" | "meta" | "noframes"
                        | "script" | "style" | "template" | "title" => return true,
                        "caption" | "colgroup" | "tbody" | "tfoot" | "thead" => Mode::Table,
                        "col" => Mode::ColumnGroup,
                        "tr" => Mode::TableBody,
                        "td" | "th" => Mode::Row,
                        _ => Mode::Body,
                    };
                    if let Some(mode) = self.templates.last_mut() {
                        *mode = next;
                    }
                    self.mode = next;
                }
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
                    self.clear_to_marker();
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
                (Mode::Body, _) => return false,
                // "In template" drops every end tag but `</template>`.
                (Mode::Template, _) => return true,
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
                        self.clear_to_marker();
                        self.mode = Mode::Row;
                    }
                    return true;
                }
                (Mode::Cell, "table" | "tbody" | "tfoot" | "thead" | "tr") => {
                    if self.in_table_scope(&[name]).is_none() {
                        return true;
                    }
                    match self.in_table_scope(&["td", "th"]) {
                        Some(i) => {
                            self.open.truncate(i);
                            self.clear_to_marker();
                        }
                        None => self.freezes = true,
                    }
                    self.mode = Mode::Row;
                }
                (Mode::Caption, "caption" | "table") => {
                    let Some(i) = self.in_table_scope(&["caption"]) else {
                        return true;
                    };
                    self.open.truncate(i);
                    self.clear_to_marker();
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
    /// heading before a heading, and the like. Returns whether the tag still
    /// opens an element; a `<select>` inside a select closes it instead.
    fn body_start_tag(&mut self, name: &str) -> bool {
        // Only these read whether a select is in scope.
        let select = matches!(name, "hr" | "input" | "optgroup" | "option" | "select")
            .then(|| self.in_scope("select", Scope::Default))
            .flatten();
        match name {
            "select" | "input" => {
                if let Some(i) = select {
                    self.open.truncate(i);
                    return name == "input";
                }
            }
            "hr" => {
                self.close_p();
                if select.is_some() {
                    self.imply_end_tags("");
                }
            }
            // In a select, an `<option>` closes an option and what is open
            // inside it, and an `<optgroup>` an optgroup too; elsewhere they
            // close only an option that is the current node.
            "option" | "optgroup" => {
                if select.is_some() {
                    self.imply_end_tags(if name == "option" { "optgroup" } else { "" });
                } else if self
                    .open
                    .last()
                    .is_some_and(|e| !e.is_foreign() && e.name == "option")
                {
                    self.open.pop();
                }
            }
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
            "rb" | "rp" | "rt" | "rtc" if self.in_scope("ruby", Scope::Default).is_some() => {
                let keep = if matches!(name, "rp" | "rt") {
                    "rtc"
                } else {
                    ""
                };
                self.imply_end_tags(keep);
            }
            "a" => {
                // A second `<a>` ends the first, which leaves the list and
                // the stack even when the adoption agency keeps it.
                if let Some(i) = self.formatting_element(name) {
                    let id = self.formatting[i].as_ref().map(|f| f.id);
                    self.adopt(name);
                    self.formatting.retain(|f| f.as_ref().map(|f| f.id) != id);
                    self.open.retain(|e| Some(e.id) != id);
                }
            }
            "nobr" => {
                // Reopening can open a `<nobr>`; the start tag reopens again
                // after the agency closes it.
                self.reconstruct();
                if self.in_scope(name, Scope::Default).is_some() {
                    self.adopt(name);
                }
            }
            _ => {}
        }
        true
    }

    /// Closes the `<p>`, list items, options and the like at the top of the
    /// stack, but never an `<except>`, as the tree builder's "generate
    /// implied end tags" step does.
    fn imply_end_tags(&mut self, except: &str) {
        while self.open.last().is_some_and(|e| {
            !e.is_foreign() && e.name != except && IMPLIED_END_TAGS.contains(&e.name.as_str())
        }) {
            self.open.pop();
        }
    }

    /// Closes an open `<p>` in button scope.
    fn close_p(&mut self) {
        if let Some(i) = self.in_scope("p", Scope::Button) {
            self.open.truncate(i);
        }
    }

    /// The index in the list of the newest entry for the formatting element
    /// `name` since the last marker, where the adoption agency looks for it.
    fn formatting_element(&self, name: &str) -> Option<usize> {
        for (i, entry) in self.formatting.iter().enumerate().rev() {
            match entry {
                None => return None,
                Some(f) if f.name == name => return Some(i),
                Some(_) => {}
            }
        }
        None
    }

    /// Runs the adoption agency for the formatting end tag `</name>`. Each
    /// round takes the list's entry for `name`: one whose element is closed
    /// leaves the list, and one with no special element open inside it
    /// closes everything from it up. Otherwise the formatting element moves
    /// above the first special element inside it, a copy of it replacing it
    /// in the list; of the elements between them, only those listed within
    /// three of that special element stay open, each replaced by a copy.
    /// With no entry for `name` the tag closes as any other end tag does.
    fn adopt(&mut self, name: &str) {
        if self
            .open
            .last()
            .is_some_and(|e| !e.is_foreign() && e.name == name && !self.listed(e.id))
        {
            self.open.pop();
            return;
        }
        for _ in 0..8 {
            let Some(i) = self.formatting_element(name) else {
                return self.other_end_tag(name);
            };
            let Some(fe_id) = self.formatting[i].as_ref().map(|f| f.id) else {
                return;
            };
            let Some(fe) = self.open_index(fe_id) else {
                self.formatting.remove(i);
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
                self.formatting.remove(i);
                return;
            };
            // The copy of the formatting element goes into the list after
            // the copy of the element nearest the block, else in its place.
            let mut after = None;
            let mut removed = 0;
            for (inner, node) in (fe + 1..block).rev().enumerate() {
                let id = self.open[node].id;
                let mut entry = self
                    .formatting
                    .iter()
                    .position(|f| f.as_ref().is_some_and(|f| f.id == id));
                if inner >= 3 {
                    if let Some(j) = entry.take() {
                        self.formatting.remove(j);
                    }
                }
                let Some(j) = entry else {
                    self.open.remove(node);
                    removed += 1;
                    continue;
                };
                let copy = self.next_id;
                self.next_id += 1;
                self.open.set_id(node, copy);
                if let Some(f) = &mut self.formatting[j] {
                    f.id = copy;
                }
                after.get_or_insert(copy);
            }
            let copy = self.next_id;
            self.next_id += 1;
            let Some(i) = self
                .formatting
                .iter()
                .position(|f| f.as_ref().is_some_and(|f| f.id == fe_id))
            else {
                return;
            };
            if let Some(mut entry) = self.formatting.remove(i) {
                entry.id = copy;
                let at = after
                    .and_then(|a| {
                        self.formatting
                            .iter()
                            .position(|f| f.as_ref().is_some_and(|f| f.id == a))
                    })
                    .map_or(i, |j| j + 1);
                self.formatting.insert(at, Some(entry));
            }
            // Removing the formatting element shifts the block down one
            // more; its copy goes back in just above the block.
            let mut element = self.open.remove(fe);
            element.id = copy;
            self.open.insert(block - removed, element);
        }
    }

    /// The index of the innermost open HTML `<template>`.
    fn template_index(&self) -> Option<usize> {
        self.open.iter().rposition(Element::is_template)
    }

    /// Whether the scan is inside an HTML `<template>`, whose contents the
    /// browser never shows.
    fn in_template(&self) -> bool {
        self.open.templates > 0
    }

    /// Pops foreign elements until the innermost is HTML or an integration
    /// point.
    fn pop_foreign_to_point(&mut self) {
        while self.open.last().is_some_and(Element::holds_foreign_content) {
            self.open.pop();
        }
    }

    /// Whether a `<![CDATA[` here opens a CDATA section, which runs to `]]>`:
    /// only while the browser's current node is SVG or MathML and not an
    /// integration point, as Canvas.app's WebKit reads it (Playwright's
    /// WebKit build opens a section at an integration point too). Elsewhere
    /// it is a bogus comment, ending at the first `>`.
    fn cdata_allowed(&self) -> bool {
        self.open.last().is_some_and(Element::holds_foreign_content)
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
                // With no HTML element open here, the card's wrapper `<div>`
                // is the nearest, and the tag still reads as HTML.
                let from = self
                    .open
                    .iter()
                    .rposition(|e| !e.is_foreign())
                    .map_or(0, |i| i + 1);
                if let Some(i) = self.open[from..].iter().rposition(|e| e.name == name) {
                    self.open.truncate(from + i);
                    return;
                }
            }
        }
        self.html_end_tag(name);
    }

    /// Closes elements for the end tag `</name>` as the tree builder's "in
    /// body" rules do.
    fn html_end_tag(&mut self, name: &str) {
        // Every insertion mode reads `</template>` by the "in head" rules:
        // it closes the innermost template and everything it holds, past any
        // special element, and is dropped when no template is open.
        if name == "template" {
            if let Some(i) = self.template_index() {
                self.open.truncate(i);
                self.clear_to_marker();
                self.templates.pop();
                self.reset_mode();
            }
            return;
        }
        if self.table_end_tag(name) {
            return;
        }
        let scope = match name {
            "body" | "html" => return,
            // `</br>` inserts a `<br>`, and a `</p>` with no `<p>` to close
            // an empty `<p>`, which can close the innermost element. Only the
            // `<br>` reopens the formatting elements first.
            "br" => {
                self.reconstruct();
                return self.insert("", name);
            }
            "p" if self.in_scope(name, Scope::Button).is_none() => {
                return self.insert("", name);
            }
            "p" => Scope::Button,
            "li" => Scope::ListItem,
            "table" | "caption" | "tbody" | "thead" | "tfoot" | "tr" | "td" | "th" => Scope::Table,
            "form" => return self.form_end_tag(),
            _ if SCOPED_END_TAGS.contains(&name) || is_heading(name) => Scope::Default,
            _ if FORMATTING_TAGS.contains(&name) => return self.adopt(name),
            _ => return self.other_end_tag(name),
        };
        if let Some(i) = self.in_scope(name, scope) {
            self.open.truncate(i);
            if matches!(name, "applet" | "marquee" | "object") {
                self.clear_to_marker();
            }
        }
    }

    /// Closes elements for `</form>`. Outside a `<template>` it clears the
    /// form element pointer and, when that form is in scope, closes the
    /// `<p>`, list items and the like at the top of the stack, then removes
    /// the form alone, leaving open what it holds. Inside one it closes the
    /// innermost form in scope and everything inside it.
    fn form_end_tag(&mut self) {
        if self.in_template() {
            if let Some(i) = self.in_scope("form", Scope::Default) {
                self.open.truncate(i);
            }
            return;
        }
        let Some(i) = self.form.take().and_then(|id| self.open_index(id)) else {
            return;
        };
        if self.open[i + 1..]
            .iter()
            .any(|e| e.ends_scope(Scope::Default))
        {
            return;
        }
        self.imply_end_tags("");
        self.open.remove(i);
    }

    /// Closes elements for an end tag the "in body" rules name no step for:
    /// the innermost open HTML element `name` and everything inside it,
    /// unless a special element is open inside it.
    fn other_end_tag(&mut self, name: &str) {
        for i in (0..self.open.len()).rev() {
            let e = &self.open[i];
            if !e.is_foreign() && e.name == name {
                self.open.truncate(i);
                return;
            }
            if e.is_special() {
                return;
            }
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
    "select",
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

/// HTML start tags, besides blocks, headings and table parts, before which
/// the tree builder leaves the closed formatting elements closed: what the
/// "in head" rules insert, list items, raw text and ruby text.
const KEEPS_FORMATTING_CLOSED: &[&str] = &[
    "base", "basefont", "bgsound", "dd", "dt", "frame", "frameset", "iframe", "li", "link", "meta",
    "noembed", "noframes", "noscript", "param", "rb", "rp", "rt", "rtc", "script", "source",
    "style", "template", "textarea", "title", "track",
];

/// HTML formatting elements, whose end tags run the adoption agency and
/// which the tree builder lists as active formatting elements.
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
            if find_attr_value(tag, "encoding").is_some_and(|v| {
                let encoding = &tag[v.range()];
                encoding.eq_ignore_ascii_case("text/html")
                    || encoding.eq_ignore_ascii_case("application/xhtml+xml")
            }) =>
        {
            Point::Html
        }
        _ => Point::None,
    }
}

/// Whether the start tag `tag` named `name` is an `<input type=hidden>`.
fn hidden_input(tag: &str, name: &str) -> bool {
    name == "input"
        && find_attr_value(tag, "type")
            .is_some_and(|v| tag[v.range()].eq_ignore_ascii_case("hidden"))
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
        self.text(start);
        let cdata = self.html[start..].starts_with("<![CDATA[") && self.cdata_allowed();
        if cdata {
            // The section's text is text like any other.
            let body = start + "<![CDATA[".len();
            let text_end = self.html[body..]
                .find("]]>")
                .map_or(self.html.len(), |e| body + e);
            end = (text_end + "]]>".len()).min(self.html.len());
            self.pos = body;
            self.text(text_end);
        }
        let tag = &self.html[start..end];
        let (text_end, foreign) = if tag.as_bytes()[1].is_ascii_alphabetic() {
            tag_name(tag).map_or((None, false), |name| self.start_tag(tag, &name, end))
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
            (None, false)
        };
        self.pos = text_end.unwrap_or(end);
        Some(Tag {
            start,
            end,
            text_end,
            foreign,
        })
    }
}

/// The offset of the first end tag in `html` that freezes Canvas.app's
/// WebKit (see `Tags::freezes`): a card holding one must never reach a
/// viewer, and `None` when it holds none.
pub fn webkit_freeze(html: &str) -> Option<usize> {
    let mut scan = tags(html);
    while let Some(tag) = scan.next() {
        if scan.freezes {
            return Some(tag.start);
        }
    }
    None
}

/// The offset of the first end tag for `name` at or after `from`, any case,
/// or the end of the HTML: `</name` followed by a space, tab, line feed, form
/// feed, carriage return, `/` or `>`, as the tokenizer's appropriate end tag
/// needs, so `</scriptx>` stays text. It reads only as far as the match, so a
/// page of many raw-text elements costs its length once, not once per element.
fn closing_tag_start(html: &str, from: usize, name: &str) -> usize {
    let bytes = html.as_bytes();
    let mut pos = from;
    while let Some(rel) = bytes[pos..].iter().position(|&b| b == b'<') {
        let lt = pos + rel;
        let closes = bytes[lt + 1..]
            .strip_prefix(b"/")
            .and_then(|rest| rest.get(..=name.len()))
            .is_some_and(|n| {
                n[..name.len()].eq_ignore_ascii_case(name.as_bytes())
                    && matches!(
                        n[name.len()],
                        b' ' | b'\t' | b'\n' | b'\x0c' | b'\r' | b'/' | b'>'
                    )
            });
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

/// An attribute value [`find_attr_value`] found in an opening tag: the byte
/// range of the value relative to the tag, and the quote written around it
/// (`None` for an unquoted value).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AttrValue {
    start: usize,
    end: usize,
    pub quote: Option<char>,
}

impl AttrValue {
    /// The value's bytes, quotes excluded.
    pub fn range(self) -> Range<usize> {
        self.start..self.end
    }

    /// The value's bytes with its quotes. An unquoted value ends at the
    /// whitespace or `>` after it, which this range leaves out.
    pub fn outer(self) -> Range<usize> {
        let q = self.quote.map_or(0, char::len_utf8);
        self.start - q..self.end + q
    }
}

/// Finds the attribute `attr` (case-insensitive name) in the opening tag
/// `tag`, reading its attributes as the browser's tokenizer does: its value
/// lies between the quotes of `name="v"` or `name='v'`, or for an unquoted
/// `name=v` runs up to whitespace or `>`. Spaces around `=` are allowed. Text
/// inside another attribute's value is never read as an attribute, so
/// `alt="a src=x"` holds no `src`. The first attribute of a name wins, as in
/// the browser, and one written without `=` has no value: `None`.
pub fn find_attr_value(tag: &str, attr: &str) -> Option<AttrValue> {
    find_attr(tag, attr).flatten()
}

/// [`find_attr_value`]'s lookup, telling a missing attribute (`None`) from one
/// written without `=` (`Some(None)`).
fn find_attr(tag: &str, attr: &str) -> Option<Option<AttrValue>> {
    attrs(tag)
        .find(|(name, _)| tag[name.clone()].eq_ignore_ascii_case(attr))
        .map(|(_, value)| value)
}

/// The attributes of the opening tag `tag` in order, as [`find_attr_value`]
/// reads them: each name's byte range and its value (`None` for one written
/// without `=`). An unterminated quoted value ends the list.
fn attrs(tag: &str) -> impl Iterator<Item = (Range<usize>, Option<AttrValue>)> + '_ {
    let b = tag.as_bytes();
    let space = move |i: usize| b.get(i).is_some_and(u8::is_ascii_whitespace);
    let name_ends = move |i: usize| i >= b.len() || space(i) || matches!(b[i], b'/' | b'>' | b'=');
    // Past `<` and the tag name.
    let mut i = 1;
    while !name_ends(i) {
        i += 1;
    }
    std::iter::from_fn(move || {
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
        let name = name_start..i;
        while space(i) {
            i += 1;
        }
        if b.get(i) != Some(&b'=') {
            return Some((name, None));
        }
        i += 1;
        while space(i) {
            i += 1;
        }
        let value = match b.get(i) {
            Some(&q @ (b'"' | b'\'')) => {
                let quote = q as char;
                let start = i + 1;
                let end = start + tag[start..].find(quote)?;
                i = end + 1;
                AttrValue {
                    start,
                    end,
                    quote: Some(quote),
                }
            }
            _ => {
                let start = i;
                while i < b.len() && !space(i) && b[i] != b'>' {
                    i += 1;
                }
                AttrValue {
                    start,
                    end: i,
                    quote: None,
                }
            }
        };
        Some((name, Some(value)))
    })
}

/// The attributes of the opening tag `tag` as the browser compares two
/// elements' attributes: lowercased names, the first of a name kept, values
/// with character references decoded, sorted.
fn attr_key(tag: &str) -> Vec<(String, String)> {
    let mut key: Vec<(String, String)> = Vec::new();
    for (name, value) in attrs(tag) {
        let name = tag[name].to_ascii_lowercase();
        if key.iter().any(|(n, _)| *n == name) {
            continue;
        }
        let value = value.map_or_else(String::new, |v| decode_entities(&tag[v.range()]));
        key.push((name, value));
    }
    key.sort();
    key
}

/// `tag` with the attribute value `old` (a [`find_attr_value`] result for
/// `tag`) replaced by `value`, which must already be safe inside a quoted
/// value ([`escape_attr`]). An unquoted value is written in double quotes, so
/// the new one stays one value whatever it holds.
pub fn replace_attr_value(tag: &str, old: AttrValue, value: &str) -> String {
    let quote = if old.quote.is_some() { "" } else { "\"" };
    format!(
        "{}{quote}{value}{quote}{}",
        &tag[..old.start],
        &tag[old.end..]
    )
}

/// What names a card in a list or a page title: its label, else
/// "Canvas post" for a card with no text at all.
pub fn card_title(html: &str) -> String {
    card_label(html).unwrap_or_else(|| "Canvas post".to_string())
}

/// The text of a card's first `<h1>`–`<h3>`, else its first line of visible
/// text (script, style and template contents skipped), whitespace collapsed.
/// `None` for a card with no text at all.
fn card_label(html: &str) -> Option<String> {
    first_heading(html).or_else(|| {
        visible_text(html)
            .lines()
            .map(collapse_whitespace)
            .find(|line| !line.is_empty())
    })
}

/// The visible text of the first `<h1>`–`<h3>` outside a `<template>`,
/// whitespace collapsed. `None` when it is empty or never closed.
pub fn first_heading(html: &str) -> Option<String> {
    let mut scan = tags(html);
    while let Some(Tag { start, end, .. }) = scan.next() {
        if scan.in_template() {
            continue;
        }
        let name = tag_name(&html[start..end]);
        if let Some(level @ ("h1" | "h2" | "h3")) = name.as_deref() {
            let text = visible_text_until(html, &mut scan, level)?;
            let text = collapse_whitespace(&text);
            return (!text.is_empty()).then_some(text);
        }
    }
    None
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

/// `html` with every tag removed, the contents of `<script>`, `<style>`,
/// `<iframe>`, `<noembed>`, `<noframes>` and `<template>` dropped, each block
/// tag starting a new line, and every character reference decoded once
/// ([`decode_entities`]), each run of text between two tags on its own, as
/// the parser reads it. The raw text of `<xmp>` and `<plaintext>` is kept as
/// written, since the parser decodes nothing there.
fn visible_text(html: &str) -> String {
    let (text, _) = read_visible(html, &mut tags(html), None);
    text
}

/// [`visible_text`] of what `scan` reads from where it stopped up to the end
/// tag `</close>` outside a `<template>`; `None` when that never comes.
fn visible_text_until(html: &str, scan: &mut Tags, close: &str) -> Option<String> {
    let (text, closed) = read_visible(html, scan, Some(close));
    closed.then_some(text)
}

/// [`visible_text`] of what `scan` reads from where it stopped, and whether
/// it stopped at the end tag `</close>` outside a `<template>`; with no such
/// tag it runs to the end of `html`.
fn read_visible(html: &str, scan: &mut Tags, close: Option<&str>) -> (String, bool) {
    let mut out = String::new();
    let mut pos = scan.pos;
    // Whether the text after the last tag is inside a `<template>`.
    let mut hidden = scan.in_template();
    while let Some(Tag {
        start,
        end,
        text_end,
        ..
    }) = scan.next()
    {
        if !hidden {
            out.push_str(&decode_entities(&html[pos..start]));
        }
        pos = end;
        let tag = &html[start..end];
        let name = tag_name(&tag.replacen("</", "<", 1));
        if !hidden && close.is_some() && tag.starts_with("</") && name.as_deref() == close {
            return (out, true);
        }
        match name.as_deref() {
            Some("script" | "style" | "iframe" | "noembed" | "noframes") => {
                pos = text_end.unwrap_or(end)
            }
            // Both open and close a block; the opening tag's raw text follows.
            Some("xmp" | "plaintext") => {
                if !hidden {
                    out.push('\n');
                }
                if let Some(text_end) = text_end {
                    if !hidden {
                        out.push_str(&html[end..text_end]);
                    }
                    pos = text_end;
                }
            }
            Some(name) if !hidden && BLOCK_TAGS.contains(&name) => out.push('\n'),
            _ => {}
        }
        hidden = scan.in_template();
    }
    if pos < html.len() && !hidden {
        out.push_str(&decode_entities(&html[pos..]));
    }
    (out, false)
}

/// `s` with every character reference decoded once, as the HTML parser
/// decodes text between tags: any named reference (with or without its
/// semicolon, for the names the spec allows bare) and any decimal or hex
/// numeric one.
pub fn decode_entities(s: &str) -> String {
    htmlize::unescape(s).into_owned()
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
    fn closing_tag_start_needs_the_name_then_space_slash_or_gt() {
        let html = "é<script>a</b>< /script></scriptx></SCRIPT\u{b}></ScRiPt\t>z";
        assert_eq!(
            closing_tag_start(html, 10, "script"),
            html.find("</ScRiPt\t").unwrap()
        );
        for tail in [" ", "\t", "\n", "\x0c", "\r", "/", ">"] {
            let html = format!("x</Style{tail}");
            assert_eq!(closing_tag_start(&html, 0, "style"), 1, "{tail:?}");
        }
        assert_eq!(closing_tag_start(html, 0, "style"), html.len());
        assert_eq!(closing_tag_start("x</script", 0, "script"), 9);
        assert_eq!(closing_tag_start("x</scrip", 0, "script"), 8);
        assert_eq!(closing_tag_start("x<", 1, "script"), 2);
    }

    fn img_count(html: &str) -> usize {
        tags(html)
            .filter(|t| tag_name(&html[t.start..t.end]).as_deref() == Some("img"))
            .count()
    }

    #[test]
    fn a_raw_text_end_tag_with_a_longer_name_stays_text() {
        // Chromium, Playwright's WebKit and Canvas.app's WKWebView build no
        // image for any of these: `</scriptx>` is not `</script>`, nor is a
        // vertical tab or no-break space after the name.
        for el in [
            "iframe", "noembed", "noframes", "script", "style", "textarea", "title", "xmp",
        ] {
            for tail in ["x>", "\u{b}>", "\u{a0}>"] {
                let html = format!("<{el}>x</{el}{tail}<img src=a.png>");
                assert_eq!(img_count(&html), 0, "{html:?}");
            }
            for tail in ["/>", " >", "\t>", "\n>", "\x0c>", "\r>", " a=1>", ">"] {
                let html = format!("<{el}>x</{}{tail}<img src=a.png>", el.to_uppercase());
                assert_eq!(img_count(&html), 1, "{html:?}");
            }
        }
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
        // Each stray end tag walks the open `<g>`s twice, as foreign content
        // and then by the "in body" rules, and WebKit's depth cap holds them
        // to 509 inside a card: 20,000 of them cost about 20 million steps,
        // not the 800 million an uncapped stack would.
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
    fn card_label_costs_about_one_scan_on_a_deep_stack() {
        // Asking whether the scan is inside a template after each tag reads
        // a count, not the 509 open elements: card_label on a capped stack
        // takes about as long as the scan itself. Each side takes its
        // fastest of three runs, so a busy machine slows both.
        let html = format!("<svg>{}<h2>end</h2>", "<g>".repeat(20_000));
        let fastest = |f: &dyn Fn()| {
            (0..3)
                .map(|_| {
                    let t = std::time::Instant::now();
                    f();
                    t.elapsed()
                })
                .min()
                .unwrap()
        };
        let scan = fastest(&|| assert_eq!(tags(&html).count(), 20_003));
        let label = fastest(&|| assert_eq!(card_label(&html).as_deref(), Some("end")));
        // The heading ends the page, so card_label scans it once. A walk of
        // the stack after each tag takes it past six times the scan.
        assert!(label < scan * 3, "card_label {label:?}, scan {scan:?}");
    }

    #[test]
    fn the_template_count_matches_the_open_elements() {
        for html in [
            "<template><div><template><b>x</template></div></template>y",
            "<div><template><table><tr><td>x</template></div>",
            "<template><b><p>x</b>y</p></template>z",
            "<template><a>x<div><a>y</a></div></template>z",
            "<template><form><div>x</form></div></template><form>y</form>",
            "<svg><template><foreignObject><template>x</svg></template>",
            &format!("{}<template>x</div></template>", "<div>".repeat(600)),
            &format!("<template>{}</template>x", "<b><i>".repeat(400)),
            &format!(
                "{}{}",
                "<template><div>".repeat(400),
                "</template>".repeat(400)
            ),
        ] {
            let mut scan = tags(html);
            while scan.next().is_some() {
                let walked = scan.open.iter().filter(|e| e.is_template()).count();
                assert_eq!(scan.open.templates, walked, "{html:.60}");
            }
        }
    }

    #[test]
    fn every_change_to_the_open_elements_keeps_the_template_count() {
        // Each change, on a stack holding HTML and SVG templates, whether or
        // not a tag can make it remove one.
        let element = |id, name: &str, ns| Element {
            id,
            name: name.to_string(),
            ns,
            point: Point::None,
            special: false,
        };
        let mut open = OpenElements::default();
        open.push(element(0, "template", Ns::Html));
        open.push(element(1, "template", Ns::Svg));
        open.insert(1, element(2, "template", Ns::Html));
        open.push(element(3, "div", Ns::Html));
        assert_eq!(open.templates, 2);
        open.remove(1);
        assert_eq!(open.templates, 1);
        open.retain(|e| e.id != 0);
        assert_eq!(open.templates, 0);
        open.push(element(4, "template", Ns::Html));
        open.truncate(1);
        assert_eq!((open.len(), open.templates), (1, 0));
    }

    #[test]
    fn a_long_list_of_formatting_elements_scans_in_one_pass() {
        // Distinct attributes keep every `<b>` listed. Past the depth cap
        // each text run reopens the 300 closed ones, as the browser does, but
        // only the newest stays open, so it pushes one element instead of
        // 300 and checks each entry against the stack in one step, not 509.
        let bs: String = (0..800).map(|i| format!("<b id={i}>")).collect();
        let html = bs + &"<div>x</div>".repeat(4_000);
        let mut scan = tags(&html);
        let mut n = 0;
        while scan.next().is_some() {
            n += 1;
            assert!(scan.open.len() <= MAX_OPEN);
        }
        assert_eq!(n, 8_800);
        // Noah's Ark compares each new entry with every one before it, as
        // the browser does, one hash at a time.
        let bs: String = (0..5_000).map(|i| format!("<b id={i}>t")).collect();
        assert_eq!(tags(&bs).count(), 5_000);
    }

    #[test]
    fn an_end_tag_reaching_a_cell_the_depth_cap_closed_freezes_webkit() {
        // Inside `k` `<div>`s, the cap closes the `<td>` when the next node
        // goes in (at 505 that is the `<svg>`; at 506 and 507 the `<td>`
        // itself closes the `<tr>` first), and the cell mode stays. An end
        // tag for a table part still in table scope then loops in WebKit.
        let deep = |k: usize, tail: &str| format!("{}{tail}", "<div>".repeat(k));
        for (k, tail) in [
            (505, "<table><tr><td><svg></table>"),
            (506, "<table><tr><td><b></table>"),
            (507, "<table><tr><td><svg></table>"),
            (505, "<table><tr><td><svg></tr>"),
            (506, "<table><tr><td><svg></tbody>"),
            // `</td>` with no cell to close is dropped, and the mode stays.
            (505, "<table><tr><td><svg></td></table>"),
        ] {
            let html = deep(k, tail);
            let end = html.rfind("</").unwrap();
            assert_eq!(webkit_freeze(&html), Some(end), "{k} {tail}");
        }
        for (k, tail) in [
            // The cell stays open.
            (504, "<table><tr><td><svg></table>"),
            (505, "<table><tr><td></table>"),
            // The cap closed the table, so `</table>` has nothing to close.
            (508, "<table><tr><td><svg></table>"),
            // The row is closed too, so `</tr>` has nothing to close.
            (506, "<table><tr><td><svg></tr>"),
        ] {
            assert_eq!(webkit_freeze(&deep(k, tail)), None, "{k} {tail}");
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
    fn text_end_marks_only_tags_read_as_raw_text() {
        let html = "<style></style><style/>a{}</style><svg><style></style><style/></svg><b>";
        let ends: Vec<(&str, Option<usize>)> = tags(html)
            .map(|t| (&html[t.start..t.end], t.text_end))
            .collect();
        assert_eq!(
            ends,
            [
                ("<style>", Some(7)),
                ("</style>", None),
                ("<style/>", Some(26)),
                ("</style>", None),
                ("<svg>", None),
                ("<style>", None),
                ("</style>", None),
                ("<style/>", None),
                ("</svg>", None),
                ("<b>", None),
            ]
        );
    }

    #[test]
    fn foreign_marks_start_tags_that_open_svg_or_math_elements() {
        let html = "<link><svg><link/><script></script><foreignObject><link>\
                    </foreignObject><p><link></p><math><mi/></math><b>";
        let foreign: Vec<(&str, bool)> = tags(html)
            .filter(|t| !html[t.start..].starts_with("</"))
            .map(|t| (&html[t.start..t.end], t.foreign))
            .collect();
        assert_eq!(
            foreign,
            [
                ("<link>", false),
                ("<svg>", true),
                ("<link/>", true),
                ("<script>", true),
                ("<foreignObject>", true),
                // An integration point holds HTML; a `<p>` breaks out.
                ("<link>", false),
                ("<p>", false),
                ("<link>", false),
                ("<math>", true),
                ("<mi/>", true),
                ("<b>", false),
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
    fn xmp_iframe_noembed_noframes_and_plaintext_hold_text() {
        let html = "<plaintext><b>x</b></plaintext>";
        let ends: Vec<(&str, Option<usize>)> = tags(html)
            .map(|t| (&html[t.start..t.end], t.text_end))
            .collect();
        assert_eq!(ends, [("<plaintext>", Some(html.len()))]);
        // How many `<b>` elements WebKit and Chromium build from each. In the
        // `<foreignObject>` cases, `</foreignObject>` closes the island only
        // while it is the current node: then `<style/>` closes itself in the
        // `<svg>` and its `<b>` counts; with an HTML element left open,
        // `<style/>` opens HTML raw text and the count is 0.
        for (html, bs) in [
            ("<xmp><b>x</b></xmp>", 0),
            ("<iframe><b>x</b></iframe>", 0),
            ("<noembed><b>x</b></noembed>", 0),
            ("<noframes><b>x</b></noframes>", 0),
            ("<iframe><b>x</b>", 0),
            ("<plaintext><b>x</b></plaintext><b>y</b>", 0),
            ("<xmp></xmp><b>x</b>", 1),
            ("<XMP><b>x</b></XmP><b>y</b>", 1),
            ("<iframe src=a></iframe><b>x</b>", 1),
            ("<template><xmp><b>x</b></xmp></template><b>y</b>", 1),
            // A table foster-parents them, still as raw text.
            ("<table><iframe><b>x</b></iframe></table>", 0),
            ("<table><noframes><b>x</b></noframes></table>", 0),
            ("<table><tr><td><xmp><b>x</b></xmp></td></tr></table>", 0),
            // In SVG they are foreign elements holding markup; at a MathML
            // text integration point they are HTML again.
            ("<svg><iframe><b>x</b></iframe></svg>", 1),
            ("<math><mi><xmp><b>x</b></xmp></mi></math>", 0),
            // Read as markup, the `<div>` would stay open.
            (
                "<svg><foreignObject><iframe><div></iframe></foreignObject><style/><b>x</b></style>",
                1,
            ),
            (
                "<svg><foreignObject><noembed><div></noembed></foreignObject><style/><b>x</b></style>",
                1,
            ),
            (
                "<svg><foreignObject><noframes><div></noframes></foreignObject><style/><b>x</b></style>",
                1,
            ),
            (
                "<svg><foreignObject><xmp><div></xmp></foreignObject><style/><b>x</b></style>",
                1,
            ),
            // `<xmp>` reopens the closed `<strong>` before it opens; nothing
            // reopens it inside an `<iframe>`.
            (
                "<svg><foreignObject><p><strong></p><xmp>t</xmp></foreignObject><style/><b>x</b></style>",
                0,
            ),
            (
                "<svg><foreignObject><p><strong></p><iframe>t</iframe></foreignObject><style/><b>x</b></style>",
                1,
            ),
            // `<xmp>` and `<plaintext>` close an open `<p>`; `<iframe>` does not.
            (
                "<svg><foreignObject><p><xmp>t</xmp></foreignObject><style/><b>x</b></style>",
                1,
            ),
            (
                "<svg><foreignObject><p><iframe>t</iframe></foreignObject><style/><b>x</b></style>",
                0,
            ),
            (
                "<svg><foreignObject><p><plaintext></foreignObject><style/><b>x</b></style>",
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
        // How many `<b>` elements Canvas.app's WebKit builds from each, as
        // Chromium does; Playwright's WebKit build reads a section at an
        // integration point too.
        for (html, bs) in [
            ("<svg><style><![CDATA[a>b]]><b>x</b></style></svg>", 1),
            ("<svg><![CDATA[x>y<b>z</b>", 0),
            ("<svg><g><![CDATA[x>y<b>z</b>]]></g></svg>", 0),
            // `<mglyph>` is MathML even in `<mi>`.
            ("<math><mi><mglyph><![CDATA[x>y<b>z</b>]]></mi></math>", 0),
            // Without an HTML `encoding` it is no integration point.
            (
                "<math><annotation-xml><![CDATA[x>y<b>z</b>]]></annotation-xml></math>",
                0,
            ),
            // A bogus comment, ending at the first `>`, at an integration
            // point and where the current node is HTML.
            (
                "<math><annotation-xml encoding=text/html><![CDATA[x>y<b>z</b>]]></annotation-xml></math>",
                1,
            ),
            (
                "<svg><foreignObject><![CDATA[x>y<b>z</b>]]></foreignObject></svg>",
                1,
            ),
            ("<svg><desc><![CDATA[x>y<b>z</b>]]></desc></svg>", 1),
            ("<math><mi><![CDATA[x>y<b>z</b>]]></mi></math>", 1),
            ("<math><mtext><![CDATA[x>y<b>z</b>]]></mtext></math>", 1),
            ("<math><mo><![CDATA[x>y<b>z</b>]]></mo></math>", 1),
            ("<math><mn><![CDATA[x>y<b>z</b>]]></mn></math>", 1),
            ("<math><ms><![CDATA[x>y<b>z</b>]]></ms></math>", 1),
            ("<svg><title><![CDATA[x>y<b>z</b>]]></title></svg>", 1),
            (
                "<math><annotation-xml encoding='Application/XHTML+XML'><![CDATA[x>y<b>z</b>]]></annotation-xml></math>",
                1,
            ),
            (
                "<math><annotation-xml encoding=text/xml><![CDATA[x>y<b>z</b>]]></annotation-xml></math>",
                0,
            ),
            (
                "<svg><foreignObject><div></div><![CDATA[x>y<b>z</b>]]></foreignObject></svg>",
                1,
            ),
            (
                "<svg><foreignObject><br><![CDATA[x>y<b>z</b>]]></foreignObject></svg>",
                1,
            ),
            (
                "<svg><title><style/>a</style><![CDATA[x>y<b>z</b>]]></title></svg>",
                1,
            ),
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
    }

    #[test]
    fn a_start_tag_closes_what_the_browser_closes_before_it() {
        // Each runs inside `<svg><foreignObject>`, then
        // `</foreignObject><style/><b>x</b></style>`. With nothing HTML left
        // open, `</foreignObject>` closes the island, the SVG `<style/>`
        // closes at once and the `<b>` is a tag; one element left open that
        // WebKit closed keeps the island, and the `<style>` is HTML raw text.
        // The count is the `<b>` elements Canvas.app's WebKit, Playwright's
        // and Chromium build.
        for (inner, bs) in [
            ("<p>a<p>b</p>", 1),
            ("<li>a<li>b</li>", 1),
            ("<li>a<div><li>b</li></div>", 1),
            ("<li>a<span><li>b</li>", 1),
            ("<li><ul><li>b</li></ul>", 0),
            ("<dd>a<dt>b</dt>", 1),
            ("<dt>a<dd>b</dd>", 1),
            ("<option>a<option>b</option>", 1),
            ("<optgroup><option>a<optgroup>b</optgroup>", 0),
            ("<p>a<div>b</div>", 1),
            ("<p>a<h1>b</h1>", 1),
            ("<h1>a<h2>b</h2>", 1),
            ("<button>a<button>b</button>", 1),
            ("<p>a<table></table>", 1),
            ("<p>a<form></form>", 1),
            ("<table><tr><td>a<td>b</td></tr></table>", 1),
            ("<table><tr><td>a<tr><td>b</td></tr></table>", 1),
            ("<table><tr><td>a<th>b</table>", 1),
            ("<table><tbody><tr><td>a<tbody><tr><td>b</table>", 1),
            ("<table><caption>a<tr><td>b</table>", 1),
            ("<table><tr><td><table><tr><td>b</table></table>", 1),
            (
                "<table><tr><td><svg><foreignObject><td>b</td></tr></table>",
                1,
            ),
            ("<table><tr><td><math><mi><td>b</td></tr></table>", 1),
            ("<p><svg><foreignObject><p>a</foreignObject></svg>", 0),
            ("<li><svg><foreignObject><li>a</foreignObject></svg>", 0),
            // A table part outside a table opens nothing.
            ("<td>a", 1),
            ("<tr>a", 1),
            ("<caption>a", 1),
            // The adoption agency.
            ("<em><div></em></div>", 1),
            ("<em><i><div></em></div></i>", 1),
            ("<em><span><div></em></div>", 1),
            ("<em><div><span></em></span></div>", 1),
            ("<em><i><s><u><div></em></div></u></s></i>", 1),
            ("<a><div></a></div>", 1),
            ("<a>x<a>y</a>", 1),
            ("<a><div><a></a><svg></a>", 1),
            ("<em><table><tr><td></em></td></tr></table>", 0),
            // Start tags that leave no HTML element open inside the island.
            ("<image>", 1),
            ("<frame>", 1),
            ("<keygen>", 1),
            ("<basefont>", 1),
            ("<bgsound>", 1),
        ] {
            let html =
                format!("<svg><foreignObject>{inner}</foreignObject><style/><b>x</b></style>");
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
    fn closed_formatting_elements_reopen_before_text() {
        // Each runs inside `<svg><foreignObject>`, then `</foreignObject>`:
        // it closes the island only while the island is the current node, so
        // with an `<em>` reopened inside it, the svg stays open and
        // `<style/>` opens HTML raw text. The count is the `<b>` elements
        // WebKit, Chromium and Canvas.app build alike.
        for (inner, bs) in [
            ("<p><em></p>t", 0),
            ("<p><em></p> ", 0),
            ("<p><em></p>\0", 1),
            ("<p><em></p>", 1),
            // A start tag reopens them too, unless it is a block, a heading,
            // a list item, a table part or what the "in head" rules insert.
            ("<p><em></p><span></span>", 0),
            ("<p><em></p><a></a>", 0),
            ("<p><em></p><img>", 0),
            ("<p><em></p></br>", 0),
            ("<p><em></p><div></div>", 1),
            ("<p><em></p><textarea></textarea>", 1),
            ("<p><em></p></p>", 1),
            // A marker: closing the cell or caption drops what was listed
            // inside it, and nothing before it reopens inside it.
            ("<table><tr><td><p><em></p></td></tr></table>t", 1),
            ("<table><tr><td><p><em></p></tr></table>t", 1),
            ("<table><caption><p><em></p></caption></table>t", 1),
            ("<p><em></p><template>t</template>", 1),
            ("<p><em></p><object></object>t", 0),
            ("<p><em></p><object>t</object>", 0),
            // A fourth identical entry drops the oldest, so three reopen.
            ("<p><em><em><em><em></p>t</em></em></em>", 1),
            ("<p><em id=1><em><em><em></p>t</em></em></em>", 0),
            (
                "<p><em a='&amp;'><em a='&'><em a=&amp;><em a=\"&\"></p>t</em></em></em>",
                1,
            ),
            ("<p><nobr></p><nobr>t</nobr>", 1),
            // The adoption agency's copies.
            ("<strong><p><em></strong>t", 0),
            ("<strong><p><em></strong>t</em>", 0),
            ("<strong><p><em></strong>t</em></p>", 1),
            ("<strong><i><s><u><p></strong>t</u></s></i></p>", 1),
            (
                "<strong><i><s><u><code><p></strong>t</code></u></s></i></p>",
                1,
            ),
            (
                "<strong><i><s><u><code><p></strong></p>t</code></u></s></i>",
                1,
            ),
            (
                "<strong><i><s><u><code><p></strong></p>t</code></u></s></i></strong>",
                1,
            ),
            // Of the elements between, only three keep their place.
            ("<strong><i><s><u><p></strong></p></u></s>", 0),
            ("<strong><i><s><u><code><p></strong></p></code></u></s>", 1),
            ("<a><p>x<a>y</a>t</p>", 1),
            ("<em><p></em></p>t", 1),
            ("<em></em>t", 1),
            ("<p><em></p></em>t", 1),
        ] {
            let html =
                format!("<svg><foreignObject>{inner}</foreignObject><style/><b>x</b></style>");
            let names: Vec<&str> = tags(&html).map(|t| &html[t.start..t.end]).collect();
            assert_eq!(
                names.iter().filter(|n| **n == "<b>").count(),
                bs,
                "{html}: {names:?}"
            );
        }
        for (html, bs) in [
            ("<math><mi><p><em></p>t</mi><style/><b>x</b></style>", 0),
            ("<math><mi><p><em></p></mi><style/><b>x</b></style>", 1),
            // Text inside SVG reopens nothing; the `<svg>` already did.
            (
                "<svg><p><em></p><svg>t</svg></em><style/><b>x</b></style>",
                0,
            ),
            // An `<svg>` start tag reopens the `<em>` it then sits in, so
            // `</em>` closes it, and `<style/>` opens raw text.
            (
                "<p><em></p><table><svg></em><style/><b>x</b></style></table>",
                0,
            ),
            (
                "<p><em></p><table><tr><td><svg></em><style/><b>x</b></style></table>",
                1,
            ),
            (
                "<p><em></p><table><colgroup>t<svg></em><style/><b>x</b></style></table>",
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
    fn a_form_start_tag_is_dropped_until_form_end_tag_clears_the_pointer() {
        // Each runs inside `<svg><foreignObject>`, then `</foreignObject>`,
        // which closes the island only while it is the current node: a form
        // left open keeps it open and `<style/>` opens HTML raw text. The
        // count is the `<b>` elements WebKit, Chromium and Canvas.app build.
        for (inner, bs) in [
            // `</div>` closes the form, but the pointer stays set.
            ("<div><form></div><form>", 1),
            ("<div><form></div></form><form>", 0),
            ("<form></form><form>", 0),
            ("<div><form></div><div></form></div>", 1),
            ("<form id=a><div></form><form>", 0),
            // A form inside a template sets no pointer, and a `</form>`
            // inside one clears none.
            ("<template><form></template><form>", 0),
            ("<div><form></div><template></form></template><form>", 1),
            (
                "<div><form></div><template><span></form></template><form>",
                1,
            ),
            // `</form>` first closes a `<p>` or list item at the top.
            ("<form><p></form>", 1),
            ("<form><li></form>", 1),
            ("<form><span></form>", 0),
            ("<form><p><span></form>", 0),
            // A `</form>` whose form is out of scope still clears the
            // pointer, so the next `</form>` leaves the form open.
            ("<form><marquee></form></marquee></form>", 0),
        ] {
            let html =
                format!("<svg><foreignObject>{inner}</foreignObject><style/><b>x</b></style>");
            let names: Vec<&str> = tags(&html).map(|t| &html[t.start..t.end]).collect();
            assert_eq!(
                names.iter().filter(|n| **n == "<b>").count(),
                bs,
                "{html}: {names:?}"
            );
        }
        // Inside a template, `</form>` closes everything the form holds, so
        // `<style/>` is HTML raw text.
        let html = "<template><form><svg></form><style/><b>x</b></style></template>";
        let names: Vec<&str> = tags(html).map(|t| &html[t.start..t.end]).collect();
        assert!(!names.contains(&"<b>"), "{names:?}");
    }

    #[test]
    fn a_form_inside_a_table_closes_at_once() {
        // A table's rules insert a `<form>` and close it at once, so it holds
        // no place on the stack. Each runs inside `k` `<div>`s, as in
        // `a_node_inserted_at_webkits_depth_cap_closes_the_innermost_element`:
        // the `<svg>` fills the stack only when something is left open
        // before it. The count is the `<b>` elements WebKit and Canvas.app
        // build; Chromium's depth cap differs.
        for (k, inner, bs) in [
            (506, "<table><form>", 1),
            (505, "<table><tbody><form>", 1),
            (506, "<table><tbody><form>", 0),
            (506, "<table><colgroup></colgroup><form>", 1),
            (504, "<table><tbody><tr><form>", 1),
            (505, "<table><tbody><tr><form>", 0),
            // With the pointer set, the form is dropped.
            (506, "<form></form><table><form>", 1),
        ] {
            let html = format!(
                "{}{inner}<svg><!--c--><style><b>x</b></style>",
                "<div>".repeat(k)
            );
            let names: Vec<&str> = tags(&html).map(|t| &html[t.start..t.end]).collect();
            assert_eq!(
                names.iter().filter(|n| **n == "<b>").count(),
                bs,
                "{k} {inner}"
            );
        }
    }

    #[test]
    fn a_template_keeps_the_mode_its_first_start_tag_picks() {
        // A template's first start tag that the "in head" rules don't take
        // picks the mode for the rest of it: a table part's, else the body's,
        // where a table part is dropped. Each `<style/>` shows the mode: a
        // column group drops it, so `</template>` closes the template and the
        // `<b>` after it is a tag; anywhere else it opens raw text. The count
        // is the `<b>` elements WebKit, Chromium and Canvas.app build.
        for (inner, bs) in [
            ("<col>", 1),
            ("<col><col>", 1),
            ("<meta><col>", 1),
            ("<script></script><col>", 1),
            ("<template></template><col>", 1),
            // Text doesn't pick a mode.
            ("t<col>", 1),
            (" <col>", 1),
            // An end tag is dropped.
            ("</div><col>", 1),
            ("<div></div><col>", 0),
            // Closing a nested template goes back to the mode the outer
            // one picked.
            ("<div><template></template><col>", 0),
            ("<caption>", 0),
            ("<colgroup>", 0),
            ("<tr>", 0),
            ("<td>", 0),
        ] {
            let html = format!("<template>{inner}<style/></template><b>x</b>");
            let names: Vec<&str> = tags(&html).map(|t| &html[t.start..t.end]).collect();
            assert_eq!(
                names.iter().filter(|n| **n == "<b>").count(),
                bs,
                "{html}: {names:?}"
            );
        }
        // Inside `<svg><foreignObject>`, a `<td>` the body's rules drop
        // leaves the island the current node, so `</foreignObject>` closes it
        // and `<style/>` is SVG.
        for (inner, bs) in [
            ("<template><div></div>", 1),
            ("<template>", 1),
            ("<template><template></template>", 1),
            ("<template><div><template></template>", 1),
            ("<table><tr><td><template><div></div>", 1),
            ("<template><td>", 0),
        ] {
            let html =
                format!("{inner}<svg><foreignObject><td></foreignObject><style/><b>x</b></style>");
            let names: Vec<&str> = tags(&html).map(|t| &html[t.start..t.end]).collect();
            assert_eq!(
                names.iter().filter(|n| **n == "<b>").count(),
                bs,
                "{html}: {names:?}"
            );
        }
        // With the stack full, a `</p>` or `</br>` the body's rules read
        // would insert an element and close the template; the template's
        // rules drop it, so `</template>` closes the template and `<col>` is
        // dropped.
        for end in ["</p>", "</br>"] {
            let html = format!(
                "{}<template>{end}</template><col><style/><b>x</b></style>",
                "<div>".repeat(508)
            );
            assert!(
                !tags(&html).any(|t| &html[t.start..t.end] == "<b>"),
                "{end}"
            );
        }
    }

    #[test]
    fn a_select_closes_for_a_select_or_input_and_bounds_scope() {
        // The current spec's rules, which WebKit and Chromium follow; the
        // WebKit in Canvas.app still runs the older "in select" mode, which
        // drops most tags inside a select. Each runs inside
        // `<svg><foreignObject>`, then `</foreignObject>`: a select left open
        // keeps the island open and `<style/>` opens HTML raw text.
        for (inner, bs) in [
            ("<select><select>", 1),
            ("<select><input>", 1),
            ("<select><div><select>", 1),
            ("<select><div><input>", 1),
            ("<select><keygen>", 0),
            ("<select><hr>", 0),
            ("<select></select>", 1),
            ("<select><div></select>", 1),
            ("<select><button></select>", 1),
            ("<select><option><optgroup></select>", 1),
            // `<input>` reopens the `<strong>` after closing the select.
            ("<select><p><strong></p><input>", 0),
            // A select bounds the scope of what holds it.
            ("<div><select></div>", 0),
            ("<p><select></p>", 0),
        ] {
            let html =
                format!("<svg><foreignObject>{inner}</foreignObject><style/><b>x</b></style>");
            let names: Vec<&str> = tags(&html).map(|t| &html[t.start..t.end]).collect();
            assert_eq!(
                names.iter().filter(|n| **n == "<b>").count(),
                bs,
                "{html}: {names:?}"
            );
        }
        // Inside a select, `<optgroup>` closes an open option or optgroup,
        // and `<option>` and `<hr>` close an open option and what is open
        // inside it, so `k` `<div>`s leave the `<svg>` short of filling the
        // stack. The count is the `<b>` elements WebKit and Chromium build.
        for (k, inner, bs) in [
            (505, "<select><optgroup><optgroup>", 1),
            (505, "<select><optgroup><option><optgroup>", 1),
            (504, "<select><option><p><option>", 1),
            (506, "<select><option><hr>", 1),
            // `<option>` leaves an optgroup open.
            (505, "<select><optgroup><option>", 0),
        ] {
            let html = format!(
                "{}{inner}<svg><!--c--><style><b>x</b></style>",
                "<div>".repeat(k)
            );
            let names: Vec<&str> = tags(&html).map(|t| &html[t.start..t.end]).collect();
            assert_eq!(
                names.iter().filter(|n| **n == "<b>").count(),
                bs,
                "{k} {inner}"
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
    fn card_label_skips_unrendered_raw_text_and_keeps_xmp_text_as_written() {
        for (html, label) in [
            ("<iframe src=x><p>fallback</p></iframe><p>Real</p>", "Real"),
            ("<noembed>no &amp; embed</noembed><p>Real</p>", "Real"),
            ("<noframes><p>no</p></noframes><p>Real</p>", "Real"),
            ("<xmp><b>&lt;</b></xmp>", "<b>&lt;</b>"),
            ("<plaintext>a &amp; <b>", "a &amp; <b>"),
            ("<template><xmp>x</xmp></template><p>Real</p>", "Real"),
            ("Intro<xmp>code</xmp>", "Intro"),
            ("<xmp>code</xmp>after", "code"),
        ] {
            assert_eq!(card_label(html).as_deref(), Some(label), "{html}");
        }
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
            find_attr_value(tag, attr).map(|v| &tag[v.range()])
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
        let old = find_attr_value(tag, "src").unwrap();
        assert_eq!(
            replace_attr_value(tag, old, "a b"),
            "<img src=\"a b\" alt=x>"
        );
        let tag = "<img src='/a.png'>";
        let old = find_attr_value(tag, "src").unwrap();
        assert_eq!(replace_attr_value(tag, old, "x"), "<img src='x'>");
    }

    #[test]
    fn attr_values_report_their_quote() {
        let tag = "<img src=/a.png alt=x>";
        let v = find_attr_value(tag, "src").unwrap();
        assert_eq!(v.quote, None);
        assert_eq!(&tag[v.outer()], "/a.png");
        // The space after an unquoted value belongs to what follows it.
        assert_eq!(&tag[v.outer().end..], " alt=x>");
        let tag = "<img src = '/a.png' alt=x>";
        let v = find_attr_value(tag, "src").unwrap();
        assert_eq!(v.quote, Some('\''));
        assert_eq!(&tag[v.outer()], "'/a.png'");
        let tag = "<img src=\"\">";
        assert_eq!(find_attr_value(tag, "src").unwrap().quote, Some('"'));
    }

    #[test]
    fn card_label_prefers_the_first_heading() {
        assert_eq!(
            card_label("<p>intro</p><h2>The <b>plan</b>\n now</h2>").as_deref(),
            Some("The plan now")
        );
        // A heading never closed is no heading: the first line stands in.
        assert_eq!(
            card_label("<h1>never closed<p>body").as_deref(),
            Some("never closed")
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
        assert_eq!(
            card_label("<p>&#38; &#x26; &amp &copy x &mdash; &notit; &bogus; &#0;</p>").as_deref(),
            Some("& & & © x — ¬it; &bogus; \u{fffd}")
        );
        // A reference split by a tag is two runs of text, neither decoded.
        assert_eq!(
            card_label("<p>&not<b>in;</b> &am<i></i>p; &#<b>65</b>;</p>").as_deref(),
            Some("¬in; &amp; &#65;")
        );
        assert_eq!(card_label("<p>one</p><p>two</p>").as_deref(), Some("one"));
        assert_eq!(card_label("<img src=x><script>x</script>"), None);
    }

    #[test]
    fn card_title_names_a_card_with_no_text_canvas_post() {
        assert_eq!(card_title("<img src=x><script>x</script>"), "Canvas post");
        assert_eq!(card_title("<h2>Plan</h2>"), "Plan");
    }

    #[test]
    fn card_label_skips_all_of_a_nested_template() {
        let label = |html| card_label(html).unwrap_or_default();
        assert_eq!(
            label("<template>a<template>b</template>HIDDEN</template>shown"),
            "shown"
        );
        // `</template>` closes its template past a `<p>` or `<div>` it holds.
        assert_eq!(
            label("<template><p>a<div>b</template>shown<p>more"),
            "shown"
        );
        // A `</template>` in script text or SVG's own `<template>` ends nothing.
        assert_eq!(
            label("<template><script>'</template>'</script>HIDDEN</template>shown"),
            "shown"
        );
        assert_eq!(
            label("<template><svg><template></template>HIDDEN</svg></template>shown"),
            "shown"
        );
        assert_eq!(label("</template>shown"), "shown");
    }

    #[test]
    fn card_label_skips_a_heading_inside_a_template() {
        let label = |html| card_label(html).unwrap_or_default();
        assert_eq!(
            label("<template><h1>hid</h1></template>Shown line"),
            "Shown line"
        );
        assert_eq!(
            label("<template><div><h2>hid</h2></template><h3>shown</h3>"),
            "shown"
        );
        assert_eq!(card_label("<template><h1>x</h1>"), None);
    }

    #[test]
    fn first_heading_drops_a_template_inside_the_heading() {
        assert_eq!(
            first_heading("<h1>Title<template>hid</template></h1>").as_deref(),
            Some("Title")
        );
        // A `</h1>` inside the template ends nothing.
        assert_eq!(
            first_heading("<h1>A<template></h1></template>B</h1>").as_deref(),
            Some("AB")
        );
        assert_eq!(
            first_heading("<h1>A<script>'</h1>'</script>B</h1>").as_deref(),
            Some("AB")
        );
    }

    #[test]
    fn visible_text_drops_a_template_and_keeps_what_follows() {
        // A block tag inside the template starts no line of its own.
        assert_eq!(visible_text("a<template><div>x</div></template>b"), "ab");
        assert_eq!(
            visible_text("<template><p>a<div>b</template>shown<p>more"),
            "shown\nmore"
        );
        assert_eq!(visible_text("a<template><p>unclosed"), "a");
        // `</template>` closes its template past the table it holds, and the
        // row after it reads as visible again.
        assert_eq!(
            visible_text("<table><tr><template><table><td>x</template><td>y</table>"),
            "\n\n\ny\n"
        );
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
