//! Tag-level HTML scanning shared by the CLI's post-time rewrites and
//! canvasd's export: no parser, just quote-aware tag boundaries and
//! attribute lookups over the byte string.

use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
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
/// `<title>`, `<textarea>`, `<xmp>`, `<iframe>`, `<noembed>`, `<noframes>` or
/// `<noscript>` element holds no tags, so after its opening tag the scan
/// resumes at its closing tag, which the tag's `text_end` holds; after
/// `<plaintext>` the rest of the HTML is its text. The scan reads HTML as a
/// document with scripting on, as a card frame, an artifact and an exported
/// page all run, which is what makes `<noscript>` raw text. In
/// SVG or MathML those elements hold markup like any other, and a
/// `<![CDATA[` section there is one tag through its `]]>`, except at an
/// integration point such as `<foreignObject>`.
pub fn tags(html: &str) -> Tags<'_> {
    scan(html, MAX_OPEN)
}

/// [`tags`] with the depth cap at `max_open`.
fn scan(html: &str, max_open: usize) -> Tags<'_> {
    Tags {
        html,
        max_open,
        pos: 0,
        open: OpenElements::default(),
        formatting: Vec::new(),
        next_id: 0,
        token_ids: 0,
        form: None,
        mode: Mode::Body,
        templates: Vec::new(),
        raw_close: None,
        freeze: None,
        reopened: 0,
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
    /// adoption agency's rounds. Like WebKit's, the stack is capped at
    /// [`MAX_OPEN`] elements, though elements one token opens can go past
    /// it: see [`Tags::insert`].
    open: OpenElements,
    /// The tree builder's list of active formatting elements, oldest first,
    /// `None` for a marker. An entry outlives its element's place on the
    /// stack: text, and most start tags, first reopen every entry since the
    /// last marker whose element was closed, as the browser does.
    formatting: Vec<Option<Formatting>>,
    /// The id the next opened element gets.
    next_id: usize,
    /// The first id opened by the token being read. The browser queues the
    /// nodes a token inserts and attaches them only when the token ends, so
    /// an element with an id from here up has no parent yet: see
    /// [`Tags::insert`].
    token_ids: usize,
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
    /// Where the closing tag of the raw-text element just opened starts. The
    /// tree builder reads that tag in its "text" mode, which closes the
    /// element whatever the insertion mode would do with the tag.
    raw_close: Option<usize>,
    /// Set by the first tag the older WebKit Canvas.app runs reprocesses in
    /// the same mode forever, or that stalls it another way: see [`Freeze`].
    freeze: Option<Freeze>,
    /// How many formatting elements the scan has reopened.
    reopened: usize,
    /// How many elements the stack holds before the depth cap closes one:
    /// [`MAX_OPEN`] for a card, [`MAX_OPEN_PAGE`] for a page.
    max_open: usize,
}

/// A tag that freezes Canvas.app's WebKit, which reprocesses it in the same
/// insertion mode forever, the depth cap having closed what it would close.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Freeze {
    /// An end tag that closes a table, section or row reached the cell mode
    /// with no cell in table scope. The spec has the end tag close the cell
    /// first, and WebKit's "close the cell" finds none. The scan goes on as
    /// a newer WebKit does, in the row mode.
    Cell,
    /// In "in select in table", a table part's start tag, or its end tag in
    /// table scope, closes the select first, and there is none in select
    /// scope. The scan drops the tag.
    Select,
    /// The text or tag before this tag took the elements WebKit has reopened
    /// before text or a start tag past the budget it holds, [`reopen_budget`]
    /// of the bytes read before that text or tag. Each reopening of a run of
    /// closed formatting elements builds a copy of each one, so a page that
    /// keeps closing and reopening a long run builds a tree that grows with
    /// the square of its length, which stalls Canvas.app. From here the scan
    /// reopens only the newest element.
    Reopen(usize),
}

/// How many formatting elements any page may have WebKit reopen. Canvas.app
/// lays out 100,000 copies of a run of 400 `<b>`s in under a second.
const REOPEN_FLOOR: usize = 100_000;

/// The bytes of markup each reopened element past [`REOPEN_FLOOR`] needs
/// before it: `<b>` is the shortest way to write an element out, so the
/// copies never make a tree larger than the page's own length could.
const BYTES_PER_REOPEN: usize = 3;

/// How many formatting elements WebKit may reopen in the first `read` bytes
/// of a page before it counts as [`Freeze::Reopen`]. Canvas.app's time grows
/// with the size of the tree, written out or reopened: a page that misnests
/// five `<b>`s around each of 200,000 paragraphs takes as long as the same
/// tree written out, while 2,500 reopenings of 400 `<b>`s, 34 KB of markup,
/// build that tree's size in 16 s.
fn reopen_budget(read: usize) -> usize {
    REOPEN_FLOOR + read / BYTES_PER_REOPEN
}

/// WebKit's cap on its stack of open elements
/// (`defaultMaximumHTMLParserDOMTreeDepth`), less the `<html>`, `<body>` and
/// wrapper `<div>` that the viewer's card frame and an exported page both put
/// around a card. Two tests read `viewer/app.js` and `canvasd/src/export.rs`
/// and fail when either frame holds a different count open.
const MAX_OPEN: usize = 512 - 3;

/// WebKit's cap less the `<html>` and `<body>` around a whole page, such as
/// an artifact's, which the scan reads as body content: what the page's head
/// holds (a `<meta>`, a `<title>`, a `<style>`, a `<script>`) never nests.
const MAX_OPEN_PAGE: usize = 512 - 2;

/// The stack of open elements, innermost last, with indexes kept as it
/// changes: which ids are open, and for each HTML element name, each SVG or
/// MathML element name and each [`Group`] the positions holding one,
/// ascending. Past the depth cap the stack can hold far more than 509
/// elements (see [`Tags::insert`]), and the tree
/// builder asks for the innermost element of a name or kind after nearly
/// every tag, so each such question reads an index rather than walking the
/// stack. Finding where an open id sits still walks down from the top, as
/// the browser's own search for it does. Reads go through the slice; every
/// change goes through a method here, which keeps the indexes. A change
/// below the top re-indexes the elements above it.
#[derive(Default)]
struct OpenElements {
    elements: Vec<Element>,
    /// Whether the element with each id is open, by id: ids count up from
    /// 0, so this costs a byte per element the scan has opened.
    open_ids: Vec<bool>,
    names: HashMap<String, Vec<usize>>,
    foreign_names: HashMap<String, Vec<usize>>,
    groups: [Vec<usize>; GROUPS],
}

/// The kinds of open element the tree builder looks for by kind rather
/// than by name: each is a bit in [`Element::groups`] and has a list of
/// positions in [`OpenElements`].
#[derive(Clone, Copy)]
enum Group {
    /// An HTML `<h1>` to `<h6>`.
    Heading,
    /// A special element: an end tag for another element never closes past it.
    Special,
    /// A special element that stops a `<li>`, `<dd>` or `<dt>` from closing
    /// a sibling below it: any but `<address>`, `<div>` and `<p>`.
    ListBreak,
    /// An HTML element "reset the insertion mode" reads a mode off.
    Mode,
    /// An element that bounds the scope, one group per [`Scope`].
    ScopeEnd(Scope),
    /// Any HTML element: an end tag inside SVG or MathML looks for its
    /// element only above the innermost one.
    Html,
}

const GROUPS: usize = 9;

// Each group is a bit of `Element::groups`.
const _: () = assert!(GROUPS <= u16::BITS as usize);

impl Group {
    fn index(self) -> usize {
        match self {
            Group::Heading => 0,
            Group::Special => 1,
            Group::ListBreak => 2,
            Group::Mode => 3,
            Group::ScopeEnd(Scope::Default) => 4,
            Group::ScopeEnd(Scope::ListItem) => 5,
            Group::ScopeEnd(Scope::Button) => 6,
            Group::ScopeEnd(Scope::Table) => 7,
            Group::Html => 8,
        }
    }
}

impl std::ops::Deref for OpenElements {
    type Target = [Element];

    fn deref(&self) -> &[Element] {
        &self.elements
    }
}

impl OpenElements {
    fn push(&mut self, element: Element) {
        self.index(self.elements.len(), &element);
        self.elements.push(element);
    }

    fn pop(&mut self) -> Option<Element> {
        let element = self.elements.pop()?;
        self.unindex(&element);
        Some(element)
    }

    fn truncate(&mut self, len: usize) {
        while self.elements.len() > len {
            self.pop();
        }
    }

    /// Moves the element at `lo` up to `hi`, the elements above it down one,
    /// as the adoption agency moves a formatting element above its block when
    /// nothing between them leaves. Only the indexes of elements in
    /// `lo..=hi` change, so it costs as much as that range, not the stack
    /// above it.
    fn lift(&mut self, lo: usize, hi: usize) {
        fn shift(list: &mut [usize], lo: usize, hi: usize) {
            let from = list.partition_point(|&p| p < lo);
            let to = list.partition_point(|&p| p <= hi);
            let range = &mut list[from..to];
            if range.first() == Some(&lo) {
                range.rotate_left(1);
                if let Some(last) = range.last_mut() {
                    *last = hi + 1;
                }
            }
            for p in range {
                *p -= 1;
            }
        }
        let mut names: Vec<(bool, &str)> = self.elements[lo..=hi]
            .iter()
            .map(|e| (e.is_foreign(), e.name.as_str()))
            .collect();
        names.sort_unstable();
        names.dedup();
        for (foreign, name) in names {
            let map = if foreign {
                &mut self.foreign_names
            } else {
                &mut self.names
            };
            if let Some(list) = map.get_mut(name) {
                shift(list, lo, hi);
            }
        }
        let groups = self.elements[lo..=hi].iter().fold(0, |g, e| g | e.groups);
        for (i, list) in self.groups.iter_mut().enumerate() {
            if groups & (1 << i) != 0 {
                shift(list, lo, hi);
            }
        }
        self.elements[lo..=hi].rotate_left(1);
    }

    fn set_id(&mut self, i: usize, id: usize) {
        self.mark(self.elements[i].id, false);
        self.mark(id, true);
        self.elements[i].id = id;
    }

    /// Changes the elements from `i` up, innermost last, in one step: they
    /// leave the indexes, `change` edits them, and they go back.
    fn rebuild_from(&mut self, i: usize, change: impl FnOnce(&mut Vec<Element>)) {
        let mut above = Vec::with_capacity(self.elements.len().saturating_sub(i));
        while self.elements.len() > i {
            above.extend(self.pop());
        }
        above.reverse();
        change(&mut above);
        for element in above {
            self.push(element);
        }
    }

    fn remove(&mut self, i: usize) -> Element {
        let mut removed = None;
        self.rebuild_from(i, |above| removed = Some(above.remove(0)));
        removed.expect("an element at i")
    }

    /// Adds the element about to go in at the top, position `at`, to the
    /// indexes.
    fn index(&mut self, at: usize, element: &Element) {
        self.mark(element.id, true);
        let names = self.names_of(element.is_foreign());
        match names.get_mut(&element.name) {
            Some(list) => list.push(at),
            None => {
                names.insert(element.name.clone(), vec![at]);
            }
        }
        for (i, list) in self.groups.iter_mut().enumerate() {
            if element.groups & (1 << i) != 0 {
                list.push(at);
            }
        }
    }

    /// Removes the element just taken off the top from the indexes, where
    /// its position is the last of each list it is in.
    fn unindex(&mut self, element: &Element) {
        self.mark(element.id, false);
        if let Some(list) = self.names_of(element.is_foreign()).get_mut(&element.name) {
            list.pop();
        }
        for (i, list) in self.groups.iter_mut().enumerate() {
            if element.groups & (1 << i) != 0 {
                list.pop();
            }
        }
    }

    /// The name index for HTML elements, or for SVG and MathML ones.
    fn names_of(&mut self, foreign: bool) -> &mut HashMap<String, Vec<usize>> {
        if foreign {
            &mut self.foreign_names
        } else {
            &mut self.names
        }
    }

    /// Records whether the element `id` is open.
    fn mark(&mut self, id: usize, open: bool) {
        if self.open_ids.len() <= id {
            self.open_ids.resize(id + 1, false);
        }
        self.open_ids[id] = open;
    }

    /// Whether the element `id` is open.
    fn holds(&self, id: usize) -> bool {
        self.open_ids.get(id).copied().unwrap_or(false)
    }

    /// The position of the innermost open HTML element `name`.
    fn last_named(&self, name: &str) -> Option<usize> {
        self.names.get(name)?.last().copied()
    }

    /// The position of the innermost open HTML element `name` below `i`.
    fn last_named_below(&self, name: &str, i: usize) -> Option<usize> {
        let list = self.names.get(name)?;
        list[..list.partition_point(|&p| p < i)].last().copied()
    }

    /// The position of the innermost open SVG or MathML element `name` at
    /// `from` or above.
    fn last_foreign_named_from(&self, name: &str, from: usize) -> Option<usize> {
        self.foreign_names
            .get(name)?
            .last()
            .copied()
            .filter(|&p| p >= from)
    }

    /// The position of the innermost open element of `group`.
    fn last_in(&self, group: Group) -> Option<usize> {
        self.groups[group.index()].last().copied()
    }

    /// The position of the outermost open element of `group` above `i`.
    fn first_in_above(&self, group: Group, i: usize) -> Option<usize> {
        let list = &self.groups[group.index()];
        list.get(list.partition_point(|&p| p <= i)).copied()
    }

    /// Whether the scan is inside an HTML `<template>`.
    fn in_template(&self) -> bool {
        self.last_named("template").is_some()
    }
}

/// One open element. `id` tells it from a copy the adoption agency or a
/// reopening made of it. `groups` holds a bit per [`Group`] it is in, worked
/// out once when it opens.
struct Element {
    id: usize,
    name: String,
    ns: Ns,
    point: Point,
    groups: u16,
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
    fn new(id: usize, name: &str, ns: Ns, point: Point) -> Element {
        let mut element = Element {
            id,
            name: name.to_string(),
            ns,
            point,
            groups: 0,
        };
        let html = ns == Ns::Html;
        let special = match ns {
            Ns::Html => SPECIAL_TAGS.contains(&name),
            _ => element.ends_foreign_scope(),
        };
        let scope = |s| (Group::ScopeEnd(s), element.ends_scope(s));
        let groups = [
            (Group::Heading, html && is_heading(name)),
            (Group::Special, special),
            (
                Group::ListBreak,
                special && !(html && matches!(name, "address" | "div" | "p")),
            ),
            (
                Group::Mode,
                html && (name == "html" || name == "select" || mode_of(name).is_some()),
            ),
            scope(Scope::Default),
            scope(Scope::ListItem),
            scope(Scope::Button),
            scope(Scope::Table),
            (Group::Html, html),
        ];
        for (group, member) in groups {
            element.groups |= u16::from(member) << group.index();
        }
        element
    }

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
    /// The older "in select" mode Canvas.app's WebKit still runs, where the
    /// current spec reads a select's contents as a body: only options,
    /// optgroups, `<hr>`, `<script>` and `<template>` go in, `<select>`,
    /// `<input>`, `<keygen>` and `<textarea>` close the select, and every
    /// other tag is dropped.
    Select,
    /// "In select in table": a select opened in a table mode, which a table
    /// part's start or end tag also closes.
    SelectInTable,
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

    /// An id for the innermost open element, which after a start tag that
    /// opened an element is that element.
    pub fn current(&self) -> Option<usize> {
        self.open.last().map(|e| e.id)
    }

    /// Whether the element [`Tags::current`] named `id` is still open.
    pub fn is_open(&self, id: usize) -> bool {
        self.open.holds(id)
    }

    /// Updates the open elements for the start tag `tag`, and returns where
    /// its text ends when the scan reads it as raw text, and whether it
    /// opened an SVG or MathML element.
    fn start_tag(&mut self, tag: &str, name: &str, end: usize) -> (Option<usize>, bool) {
        // A select holds only HTML, so its mode reads every tag.
        if matches!(self.mode, Mode::Select | Mode::SelectInTable) {
            return self.select_start_tag(tag, name, end);
        }
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
            "iframe" | "noembed" | "noframes" | "noscript" | "script" | "style" | "textarea"
            | "title" | "xmp" => Some(closing_tag_start(self.html, end, name)),
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
        if self.open.holds(newest.id) {
            return;
        }
        let last = self.formatting.len() - 1;
        let mut first = last;
        // Past the budget, finding the first closed entry would cost as
        // much as reopening them all.
        let budget = reopen_budget(self.pos);
        if self.reopened <= budget {
            while let Some(Some(entry)) = first.checked_sub(1).map(|i| &self.formatting[i]) {
                if self.open.holds(entry.id) {
                    break;
                }
                first -= 1;
            }
            self.reopened += last + 1 - first;
        }
        if self.reopened > budget {
            self.freeze_at(Freeze::Reopen(budget));
            first = last;
        }
        // With the stack full, the first copy closes the current element;
        // each later one goes inside the copy before it, which has no parent
        // until the token ends, so the cap closes none of them.
        for i in first..=last {
            let Some(entry) = &self.formatting[i] else {
                return;
            };
            let name = entry.name.clone();
            self.insert("", &name);
            let id = self.push_html(&name);
            if let Some(entry) = &mut self.formatting[i] {
                entry.id = id;
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
        if !self.open.holds(id) {
            return None;
        }
        self.open.iter().rposition(|e| e.id == id)
    }

    /// The index in the list of the element `id`'s entry. An open element's
    /// entry is usually among the newest, so the search starts there.
    fn entry_of(&self, id: usize) -> Option<usize> {
        self.formatting
            .iter()
            .rposition(|f| f.as_ref().is_some_and(|f| f.id == id))
    }

    /// Whether the element `id` has an entry in the list.
    fn listed(&self, id: usize) -> bool {
        self.entry_of(id).is_some()
    }

    /// Opens an element and returns its id.
    fn open_element(&mut self, name: &str, ns: Ns, point: Point) -> usize {
        let id = self.next_id;
        self.next_id += 1;
        self.open.push(Element::new(id, name, ns, point));
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
        if name == "select" {
            self.mode = if matches!(
                self.mode,
                Mode::Table | Mode::TableBody | Mode::Row | Mode::Cell | Mode::Caption
            ) {
                Mode::SelectInTable
            } else {
                Mode::Select
            };
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

    /// Starts the next token: every element open now has its parent.
    fn begin_token(&mut self) {
        self.token_ids = self.next_id;
    }

    /// Applies WebKit's depth cap before the tree builder attaches a node to
    /// the innermost element: with the stack full, that element is closed and
    /// the node goes to its parent instead. Every element counts (void and
    /// self-closed ones too, which push nothing), as does a comment, but not
    /// text or a node foster-parented out of a table. `tag` and `name` are
    /// the start tag being inserted, empty for a comment.
    ///
    /// WebKit closes the innermost element only when it has a parent node,
    /// and it attaches a token's nodes when the token ends, so an element the
    /// same token opened stays open and the stack grows past the cap: the
    /// `<tbody>` a `<tr>` implies holds the row, and each formatting element
    /// reopened before text holds the next.
    fn insert(&mut self, tag: &str, name: &str) {
        if self.open.len() >= self.max_open
            && self.open.last().is_some_and(|e| e.id < self.token_ids)
            && !self.fostered(tag, name)
        {
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
                if !self.close_column_group() {
                    return;
                }
            }
            Mode::Select | Mode::SelectInTable => return,
            _ => {}
        }
        self.reconstruct();
    }

    /// Sets the insertion mode from the open elements, as the tree builder's
    /// "reset the insertion mode appropriately" step does.
    ///
    /// A select is "in select in table" when a table holds it before any
    /// template does.
    fn reset_mode(&mut self) {
        let Some(i) = self.open.last_in(Group::Mode) else {
            self.mode = Mode::Body;
            return;
        };
        self.mode = match self.open[i].name.as_str() {
            "html" => Mode::Body,
            "template" => self.templates.last().copied().unwrap_or(Mode::Template),
            "select" => {
                let below = |name| self.open.last_named_below(name, i);
                match (below("table"), below("template")) {
                    (Some(table), template) if template.is_none_or(|t| t < table) => {
                        Mode::SelectInTable
                    }
                    _ => Mode::Select,
                }
            }
            name => mode_of(name).unwrap_or(Mode::Body),
        };
    }

    /// The position of the innermost open HTML element named in `names`.
    fn last_of(&self, names: &[&str]) -> Option<usize> {
        names.iter().filter_map(|n| self.open.last_named(n)).max()
    }

    /// The position of `found`, when it is in `scope`: no element above it
    /// bounds the scope.
    fn scoped(&self, found: Option<usize>, scope: Scope) -> Option<usize> {
        let found = found?;
        let bound = self.open.last_in(Group::ScopeEnd(scope));
        bound.is_none_or(|b| b <= found).then_some(found)
    }

    /// The index of the innermost open HTML element named in `names`, when
    /// it is in table scope.
    fn in_table_scope(&self, names: &[&str]) -> Option<usize> {
        self.scoped(self.last_of(names), Scope::Table)
    }

    /// Closes elements down to the innermost open HTML element named in
    /// `names` or a `<template>`, as the table modes' "clear the stack back
    /// to a context" steps do. With neither open, that is every element.
    fn clear_to(&mut self, names: &[&str]) {
        let keep = self.last_of(names).max(self.open.last_named("template"));
        self.open.truncate(keep.map_or(0, |i| i + 1));
    }

    /// Closes the column group for a tag or text the column group mode
    /// doesn't take, as WebKit does: it closes the current element whatever
    /// it is, not only a `<colgroup>`, so with the depth cap having closed
    /// the column group it closes the element that held it. A `<template>`
    /// current stays, and the tag or text is dropped; so does nothing open,
    /// which the column group mode never meets, since the cap only closes
    /// elements on a full stack. Returns whether it closed one; the mode is
    /// then the table's.
    fn close_column_group(&mut self) -> bool {
        if self.open.last().is_none_or(Element::is_template) {
            return false;
        }
        self.open.pop();
        self.mode = Mode::Table;
        true
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
                Mode::ColumnGroup if !matches!(name, "col" | "html" | "template") => {
                    if !self.close_column_group() {
                        return false;
                    }
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
                    if name == "col" || !self.close_column_group() || name == "colgroup" {
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
                        None => self.freeze_at(Freeze::Cell),
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

    /// Applies the select modes' rules for the start tag `tag` named `name`
    /// and returns where its text ends when it opens raw text; a select opens
    /// no SVG or MathML element.
    fn select_start_tag(&mut self, tag: &str, name: &str, end: usize) -> (Option<usize>, bool) {
        let closes = match name {
            "select" => {
                self.close_select();
                return (None, false);
            }
            "input" | "keygen" | "textarea" => true,
            "caption" | "table" | "tbody" | "tfoot" | "thead" | "tr" | "td" | "th" => {
                self.mode == Mode::SelectInTable
            }
            _ => false,
        };
        if closes {
            // The tag goes on to the mode the select's end left.
            if self.close_select() {
                return self.start_tag(tag, name, end);
            }
            // WebKit checks for a select before an `<input>`, `<keygen>` or
            // `<textarea>` closes it, but not before a table part does.
            if name != "input" && name != "keygen" && name != "textarea" {
                self.freeze_at(Freeze::Select);
            }
            return (None, false);
        }
        match name {
            "option" | "optgroup" | "hr" => {
                let closes: &[&str] = if name == "option" {
                    &["option"]
                } else {
                    &["option", "optgroup"]
                };
                for close in closes {
                    if self
                        .open
                        .last()
                        .is_some_and(|e| !e.is_foreign() && e.name == *close)
                    {
                        self.open.pop();
                    }
                }
                self.insert(tag, name);
                if name != "hr" {
                    self.push_html(name);
                }
                (None, false)
            }
            "script" | "template" => {
                self.insert(tag, name);
                self.push_html(name);
                let text = (name == "script").then(|| closing_tag_start(self.html, end, name));
                (text, false)
            }
            _ => (None, false),
        }
    }

    /// Applies the select modes' rules for the end tag `</name>`: an
    /// option, optgroup or the select closes when it is the element the
    /// rules name, a table part's end tag in table scope closes the select
    /// and goes on, and every other end tag is dropped.
    fn select_end_tag(&mut self, name: &str) {
        let current = |s: &Self, n: &str| {
            s.open
                .last()
                .is_some_and(|e| !e.is_foreign() && e.name == n)
        };
        match name {
            "option" | "script" if current(self, name) => {
                self.open.pop();
            }
            "optgroup" => {
                let len = self.open.len();
                if current(self, "option")
                    && len >= 2
                    && !self.open[len - 2].is_foreign()
                    && self.open[len - 2].name == "optgroup"
                {
                    self.open.pop();
                }
                if current(self, "optgroup") {
                    self.open.pop();
                }
            }
            "select" => {
                self.close_select();
            }
            "caption" | "table" | "tbody" | "tfoot" | "thead" | "tr" | "td" | "th"
                if self.mode == Mode::SelectInTable && self.in_table_scope(&[name]).is_some() =>
            {
                if self.close_select() {
                    self.html_end_tag(name);
                } else {
                    self.freeze_at(Freeze::Select);
                }
            }
            _ => {}
        }
    }

    /// Records that the current tag freezes WebKit, unless an earlier one did.
    fn freeze_at(&mut self, freeze: Freeze) {
        self.freeze.get_or_insert(freeze);
    }

    /// Closes the select in select scope, where every element but an option
    /// or optgroup ends the scope, and resets the insertion mode. Returns
    /// whether one was open.
    fn close_select(&mut self) -> bool {
        for i in (0..self.open.len()).rev() {
            let e = &self.open[i];
            if e.is_foreign() {
                return false;
            }
            match e.name.as_str() {
                "select" => {
                    self.open.truncate(i);
                    self.reset_mode();
                    return true;
                }
                "option" | "optgroup" => {}
                _ => return false,
            }
        }
        false
    }

    /// Closes what the "in body" rules close for the start tag `<name>`:
    /// an open `<p>` before a block, a list item before its sibling, a
    /// heading before a heading, and the like. Returns whether the tag still
    /// opens an element.
    fn body_start_tag(&mut self, name: &str) -> bool {
        match name {
            "option" | "optgroup"
                if self
                    .open
                    .last()
                    .is_some_and(|e| !e.is_foreign() && e.name == "option") =>
            {
                self.open.pop();
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
                // The innermost sibling closes unless a special element
                // other than an `<address>`, `<div>` or `<p>` is above it.
                if let Some(i) = self.last_of(siblings) {
                    if self.open.last_in(Group::ListBreak).is_none_or(|b| b <= i) {
                        self.open.truncate(i);
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
                    if let Some(j) = id.and_then(|id| self.entry_of(id)) {
                        self.formatting.remove(j);
                    }
                    if let Some(i) = id.and_then(|id| self.open_index(id)) {
                        self.open.remove(i);
                    }
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
            if self
                .open
                .last_in(Group::ScopeEnd(Scope::Default))
                .is_some_and(|b| b > fe)
            {
                return;
            }
            let Some(block) = self.open.first_in_above(Group::Special, fe) else {
                self.open.truncate(fe);
                self.formatting.remove(i);
                return;
            };
            // The copy of the formatting element goes into the list after
            // the copy of the element nearest the block, else in its place.
            let mut after = None;
            let mut gone = HashSet::new();
            let mut copies = Vec::new();
            for (inner, node) in (fe + 1..block).rev().enumerate() {
                let id = self.open[node].id;
                let mut entry = self.entry_of(id);
                if inner >= 3 {
                    if let Some(j) = entry.take() {
                        self.formatting.remove(j);
                    }
                }
                let Some(j) = entry else {
                    gone.insert(id);
                    continue;
                };
                let copy = self.next_id;
                self.next_id += 1;
                copies.push((node, copy));
                if let Some(f) = &mut self.formatting[j] {
                    f.id = copy;
                }
                after.get_or_insert(copy);
            }
            let copy = self.next_id;
            self.next_id += 1;
            let listed = self.entry_of(fe_id);
            if let Some(i) = listed {
                if let Some(mut entry) = self.formatting.remove(i) {
                    entry.id = copy;
                    let at = after.and_then(|a| self.entry_of(a)).map_or(i, |j| j + 1);
                    self.formatting.insert(at, Some(entry));
                }
            }
            // The copies take their ids, the unlisted elements go, and the
            // formatting element's copy goes back in just above the block.
            for (node, id) in copies {
                self.open.set_id(node, id);
            }
            if listed.is_none() {
                self.open.rebuild_from(fe, |above| {
                    above.retain(|e| !gone.contains(&e.id));
                });
                return;
            }
            self.open.set_id(fe, copy);
            if gone.is_empty() {
                self.open.lift(fe, block);
            } else {
                self.open.rebuild_from(fe, |above| {
                    let element = above.remove(0);
                    above.retain(|e| !gone.contains(&e.id));
                    above.insert(block - fe - gone.len(), element);
                });
            }
        }
    }

    /// The index of the innermost open HTML `<template>`.
    fn template_index(&self) -> Option<usize> {
        self.open.last_named("template")
    }

    /// Whether the scan is inside an HTML `<template>`, whose contents the
    /// browser never shows.
    fn in_template(&self) -> bool {
        self.open.in_template()
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
    pub fn cdata_allowed(&self) -> bool {
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
                let from = self.open.last_in(Group::Html).map_or(0, |i| i + 1);
                if let Some(i) = self.open.last_foreign_named_from(name, from) {
                    self.open.truncate(i);
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
        if matches!(self.mode, Mode::Select | Mode::SelectInTable) {
            return self.select_end_tag(name);
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
        if self
            .open
            .last_in(Group::ScopeEnd(Scope::Default))
            .is_some_and(|b| b > i)
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
        if let Some(i) = self.open.last_named(name) {
            if self.open.last_in(Group::Special).is_none_or(|s| s <= i) {
                self.open.truncate(i);
            }
        }
    }

    /// The index of the innermost HTML element `</name>` ends, when it is in
    /// `scope`. A heading's end tag ends any heading.
    fn in_scope(&self, name: &str, scope: Scope) -> Option<usize> {
        let found = if is_heading(name) {
            self.open.last_in(Group::Heading)
        } else {
            self.open.last_named(name)
        };
        self.scoped(found, scope)
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
        self.begin_token();
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
            self.begin_token();
            self.text(text_end);
        }
        self.begin_token();
        let tag = &self.html[start..end];
        let raw_close = self.raw_close.take() == Some(start);
        let (text_end, foreign) = if raw_close {
            self.open.pop();
            (None, false)
        } else if tag.as_bytes()[1].is_ascii_alphabetic() {
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
        self.raw_close = text_end;
        self.pos = text_end.unwrap_or(end);
        Some(Tag {
            start,
            end,
            text_end,
            foreign,
        })
    }
}

/// The offset of the first tag in `html` that freezes Canvas.app's WebKit
/// (see [`Freeze`]): a card holding one must never reach a viewer, and `None`
/// when it holds none.
pub fn webkit_freeze(html: &str) -> Option<usize> {
    first_freeze(tags(html)).map(|(at, _)| at)
}

/// The offset of the first tag that freezes Canvas.app's WebKit, and why.
fn first_freeze(mut scan: Tags<'_>) -> Option<(usize, Freeze)> {
    while let Some(tag) = scan.next() {
        if let Some(freeze) = scan.freeze {
            return Some((tag.start, freeze));
        }
    }
    None
}

/// Why Canvas.app's WebKit freezes on `html` framed as a card or widget, as
/// one line naming the tag [`webkit_freeze`] finds and its offset; the tag's
/// attributes are left out, so the line stays short whatever the tag holds.
/// `None` when it holds none.
pub fn webkit_freeze_reason(html: &str) -> Option<String> {
    freeze_reason(html, tags(html))
}

/// [`webkit_freeze_reason`] for `html` as a whole page in its own frame, such
/// as an artifact's, where the stack holds one element more than in a card.
pub fn webkit_freeze_page_reason(html: &str) -> Option<String> {
    freeze_reason(html, scan(html, MAX_OPEN_PAGE))
}

fn freeze_reason(html: &str, scan: Tags<'_>) -> Option<String> {
    let (at, freeze) = first_freeze(scan)?;
    // Past the `<` or `</`, which the scan only reads as a tag before a letter.
    let slash = if html[at + 1..].starts_with('/') {
        "/"
    } else {
        ""
    };
    let rest = &html[at + 1 + slash.len()..];
    let len = rest
        .find(|c: char| c.is_whitespace() || c == '>' || c == '/')
        .unwrap_or(rest.len());
    let name = rest[..len].to_ascii_lowercase();
    let (closes, nest) = match freeze {
        Freeze::Cell => ("closes a table cell", "the table"),
        Freeze::Select => ("closes a select in a table", "the select"),
        Freeze::Reopen(budget) => {
            return Some(format!(
                "by its <{slash}{name}> at byte {at}, WebKit has reopened more than \
                 {budget} closed formatting elements such as <b>, {REOPEN_FLOOR} plus \
                 one per {BYTES_PER_REOPEN} bytes before them, which stalls Canvas.app; \
                 close each formatting element where it should end"
            ))
        }
    };
    Some(format!(
        "its <{slash}{name}> at byte {at} {closes} nested past WebKit's \
         512-element limit, which freezes Canvas.app; nest {nest} less deeply"
    ))
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
/// `<iframe>`, `<noembed>`, `<noframes>`, `<noscript>` and `<template>`
/// dropped (an SVG or MathML one's child elements' text with it), each block
/// tag starting a new line, and every character reference decoded once
/// ([`decode_entities`]), each run of text between two tags on its own, as
/// the parser reads it. The raw text of `<xmp>` and `<plaintext>` is kept as
/// written, since the parser decodes nothing there.
fn visible_text(html: &str) -> String {
    let mut scan = tags(html);
    let mut text = VisibleText::new(&scan);
    while let Some(tag) = scan.next() {
        text.before(html, &tag);
        text.tag(html, &scan, &tag);
    }
    text.finish(html)
}

/// [`visible_text`] of what `scan` reads from where it stopped up to the end
/// tag `</close>` outside a `<template>`; `None` when that never comes.
fn visible_text_until(html: &str, scan: &mut Tags, close: &str) -> Option<String> {
    let mut text = VisibleText::new(scan);
    while let Some(tag) = scan.next() {
        text.before(html, &tag);
        let raw = &html[tag.start..tag.end];
        if !text.hidden
            && raw.starts_with("</")
            && tag_name(&raw.replacen("</", "<", 1)).as_deref() == Some(close)
        {
            return Some(text.out);
        }
        text.tag(html, scan, &tag);
    }
    None
}

/// The text [`visible_text`] gathers, fed by its caller one tag at a time:
/// [`before`](Self::before) takes the text ahead of a tag,
/// [`tag`](Self::tag) the tag itself, and [`finish`](Self::finish) the text
/// after the last one.
struct VisibleText {
    out: String,
    /// Where the text not yet read starts.
    pos: usize,
    /// Whether the text after the last tag is inside a `<template>`.
    hidden: bool,
    /// The SVG or MathML `<script>`, `<style>` or the like the text after the
    /// last tag is inside: the browser renders none of its text, its child
    /// elements' included.
    muted: Option<usize>,
}

impl VisibleText {
    /// A reader starting where `scan` stopped.
    fn new(scan: &Tags) -> Self {
        Self {
            out: String::new(),
            pos: scan.pos,
            hidden: scan.in_template(),
            muted: None,
        }
    }

    /// Takes the text between the last tag and `tag`.
    fn before(&mut self, html: &str, tag: &Tag) {
        if !self.hidden && self.muted.is_none() {
            self.out
                .push_str(&decode_entities(&html[self.pos..tag.start]));
        }
        self.pos = tag.end;
    }

    /// Takes `tag`, which `scan` has just read, after [`before`](Self::before).
    fn tag(&mut self, html: &str, scan: &Tags, tag: &Tag) {
        let &Tag {
            start,
            end,
            text_end,
            foreign,
        } = tag;
        self.muted = self.muted.filter(|id| scan.is_open(*id));
        let raw = &html[start..end];
        let quiet = self.hidden || self.muted.is_some();
        match tag_name(&raw.replacen("</", "<", 1)).as_deref() {
            Some("script" | "style" | "iframe" | "noembed" | "noframes" | "noscript") => {
                match text_end {
                    Some(text_end) => self.pos = text_end,
                    // Its text follows unless it closed itself.
                    None if foreign && self.muted.is_none() && !raw.ends_with("/>") => {
                        self.muted = scan.current()
                    }
                    None => {}
                }
            }
            // Both open and close a block; the opening tag's raw text follows.
            Some("xmp" | "plaintext") => {
                if !quiet {
                    self.out.push('\n');
                }
                if let Some(text_end) = text_end {
                    if !quiet {
                        self.out.push_str(&html[end..text_end]);
                    }
                    self.pos = text_end;
                }
            }
            Some(name) if !quiet && BLOCK_TAGS.contains(&name) => self.out.push('\n'),
            _ => {}
        }
        self.hidden = scan.in_template();
    }

    /// The text gathered, with whatever follows the last tag.
    fn finish(mut self, html: &str) -> String {
        if self.pos < html.len() && !self.hidden && self.muted.is_none() {
            self.out.push_str(&decode_entities(&html[self.pos..]));
        }
        self.out
    }
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
            "iframe", "noembed", "noframes", "noscript", "script", "style", "textarea", "title",
            "xmp",
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
        // WebKit's depth cap holds the open `<g>`s to 509 inside a card, and
        // each stray end tag reads the indexes rather than walking them.
        let html = format!(
            "<svg>{}{}<h2>end</h2>",
            "<g>".repeat(20_000),
            "</x>".repeat(20_000)
        );
        assert_eq!(tags(&html).count(), 40_003);
        assert_eq!(card_label(&html).as_deref(), Some("end"));
        // A token that opens one element keeps the stack at the cap. One
        // that opens two, as a `<tr>` with the `<tbody>` it implies does,
        // goes one past it each time.
        for (unit, open) in [
            ("<div>", MAX_OPEN),
            ("<span>", MAX_OPEN),
            ("<b>", MAX_OPEN),
            ("<svg><g>", MAX_OPEN),
            ("<table><caption>", MAX_OPEN),
            ("<math><mi>", MAX_OPEN),
            // 509, then one more for each of the 1,873 units past the cap.
            ("<table><tr><td>", 2_382),
        ] {
            let html = unit.repeat(2_000);
            let mut scan = tags(&html);
            while scan.next().is_some() {}
            assert_eq!(scan.open.len(), open, "{unit}");
        }
    }

    /// The elements a frame holds open where it writes the card: its
    /// `<html>` and `<body>`, plus whatever `prefix`, the markup between its
    /// `<body>` and the card, leaves open.
    fn frame_depth(prefix: &str) -> usize {
        let mut scan = tags(prefix);
        while scan.next().is_some() {}
        2 + scan.open.len()
    }

    /// The text from just past the one `<body>` in `source` up to `end`.
    fn after_body<'a>(source: &'a str, end: impl FnOnce(&'a str) -> usize) -> &'a str {
        assert_eq!(
            source.matches("<body>").count(),
            1,
            "one <body> in the frame"
        );
        let rest = &source[source.find("<body>").unwrap() + "<body>".len()..];
        &rest[..end(rest)]
    }

    #[test]
    fn max_open_counts_the_viewer_frame_around_a_card() {
        // buildIframeDoc writes the card at `${html}`; a frame that adds or
        // drops an element around it moves the cap WebKit leaves the card.
        let app = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../viewer/app.js"))
            .unwrap();
        let prefix = after_body(&app, |rest| rest.find("${html}").unwrap());
        assert_eq!(frame_depth(prefix), 512 - MAX_OPEN, "{prefix}");
    }

    #[test]
    fn max_open_counts_the_exported_page_around_a_card() {
        // export.rs writes the card with the next push after the string
        // literal holding `<body>`, which ends at the first quote not escaped.
        let export = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../canvasd/src/export.rs"
        ))
        .unwrap();
        let literal = after_body(&export, |rest| {
            let mut escaped = false;
            rest.find(|c| {
                let end = c == '"' && !escaped;
                escaped = c == '\\' && !escaped;
                end
            })
            .unwrap()
        });
        let after = &export[export.find("<body>").unwrap() + "<body>".len() + literal.len()..];
        let next: String = after.split_whitespace().take(3).collect();
        assert_eq!(
            next, "\",);html.push_str(&body);",
            "the card follows <body>"
        );
        let prefix = literal.replace("\\\"", "\"");
        assert_eq!(frame_depth(&prefix), 512 - MAX_OPEN, "{prefix}");
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
    fn a_stray_end_tag_in_svg_costs_about_a_start_tag() {
        // A `</x>` inside SVG finds the innermost HTML element and any open
        // `x` above it from the indexes, not by walking the 509 open `<g>`s:
        // 20,000 of them scan in about the time 20,000 more `<g>`s do. Each
        // side takes its fastest of three runs, so a busy machine slows both.
        let deep = |tail: &str| format!("<svg>{}{}", "<g>".repeat(20_000), tail.repeat(20_000));
        let fastest = |html: &str| {
            (0..3)
                .map(|_| {
                    let t = std::time::Instant::now();
                    assert_eq!(tags(html).count(), 40_001);
                    t.elapsed()
                })
                .min()
                .unwrap()
        };
        let starts = fastest(&deep("<g>"));
        let ends = fastest(&deep("</x>"));
        // End tags take less time than start tags here; a scan that walked
        // the 509 `<g>`s for each one would take about four times as long.
        assert!(
            ends < starts * 2,
            "end tags {ends:?}, start tags {starts:?}"
        );
    }

    /// Panics unless the stack's indexes are what a walk of its elements
    /// finds.
    fn assert_indexed(open: &OpenElements, context: &str) {
        let ids: HashSet<usize> = open.iter().map(|e| e.id).collect();
        let marked: HashSet<usize> = (0..open.open_ids.len())
            .filter(|&id| open.holds(id))
            .collect();
        assert_eq!(marked, ids, "ids: {context:.60}");
        for (foreign, index) in [(false, &open.names), (true, &open.foreign_names)] {
            let mut names: HashMap<String, Vec<usize>> = HashMap::new();
            for (i, e) in open.iter().enumerate() {
                if e.is_foreign() == foreign {
                    names.entry(e.name.clone()).or_default().push(i);
                }
            }
            let mut indexed = index.clone();
            indexed.retain(|_, list| !list.is_empty());
            assert_eq!(indexed, names, "names (foreign {foreign}): {context:.60}");
        }
        for (g, list) in open.groups.iter().enumerate() {
            let walked: Vec<usize> = (0..open.len())
                .filter(|&i| open[i].groups & (1 << g) != 0)
                .collect();
            assert_eq!(*list, walked, "group {g}: {context:.60}");
        }
    }

    #[test]
    fn the_indexes_match_the_open_elements() {
        for html in [
            "<template><div><template><b>x</template></div></template>y",
            "<div><template><table><tr><td>x</template></div>",
            "<template><b><p>x</b>y</p></template>z",
            "<template><a>x<div><a>y</a></div></template>z",
            "<template><form><div>x</form></div></template><form>y</form>",
            "<svg><template><foreignObject><template>x</svg></template>",
            "<b><p><svg><g><g><a>x</b>y</g></a></svg>z",
            "<svg><g><foreignObject><b><p><svg><g>x</b>y</g></foreignObject></g>z",
            "<math><mi><svg><g>x</mi></g><mtext><b><div>y</b></math>",
            "<b><i><u><s><div>x</b>y</div><a>1<p><a>2</p><h1><h2>z</h3>",
            "<b><div><div><div><div><div><div><div><div><div><div>x</b>y",
            "<ul><li><div><li><dl><dd><dt>x</dl><select><option>a<optgroup>b</select>",
            "<form><div></form><p>x</p><table><caption>y</caption><col><tr><td>z</table>",
            &format!("{}<template>x</div></template>", "<div>".repeat(600)),
            &format!("<template>{}</template>x", "<b><i>".repeat(400)),
            &format!(
                "{}{}",
                "<template><div>".repeat(400),
                "</template>".repeat(400)
            ),
            &format!("{}<b><i><u><span>x<p>y</b>z", "<div>".repeat(520)),
        ] {
            let mut scan = tags(html);
            while scan.next().is_some() {
                assert_indexed(&scan.open, html);
            }
        }
    }

    #[test]
    fn every_change_to_the_open_elements_keeps_the_indexes() {
        let element = |id, name: &str, ns| Element::new(id, name, ns, Point::None);
        let mut open = OpenElements::default();
        open.push(element(0, "template", Ns::Html));
        open.push(element(1, "template", Ns::Svg));
        open.push(element(2, "td", Ns::Html));
        open.push(element(3, "div", Ns::Html));
        open.push(element(4, "b", Ns::Html));
        assert_indexed(&open, "push");
        open.lift(1, 3);
        assert_indexed(&open, "lift");
        assert_eq!(open.remove(3).id, 1);
        assert_indexed(&open, "remove");
        open.rebuild_from(1, |above| {
            above[0].id = 7;
            above.swap(1, 2);
            above.push(element(5, "h2", Ns::Html));
        });
        assert_indexed(&open, "rebuild");
        assert_eq!(open.last_in(Group::Heading), Some(4));
        assert_eq!(open.first_in_above(Group::Special, 0), Some(1));
        open.truncate(1);
        assert_indexed(&open, "truncate");
        assert!(open.in_template() && !open.holds(7));
    }

    #[test]
    fn a_long_list_of_formatting_elements_scans_in_one_pass() {
        // Distinct attributes keep every `<b>` listed. Past the depth cap
        // each `<b>` closes the one before it, and the first text run reopens
        // the 292 closed ones, as the browser does: they open inside one
        // another in one token, so all stay open and the stack holds 800.
        // From then on each text reopens only the `<b>` its `<div>` closed,
        // and checks each entry against the stack in one step, not 800.
        let bs: String = (0..800).map(|i| format!("<b id={i}>")).collect();
        let html = bs + &"<div>x</div>".repeat(4_000);
        let mut scan = tags(&html);
        let mut n = 0;
        while scan.next().is_some() {
            n += 1;
            assert!(scan.open.len() <= 800);
        }
        assert_eq!((n, scan.open.len()), (8_800, 800));
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
        // The reason names the tag alone, however much the tag holds.
        let padded = deep(
            505,
            &format!("<table><tr><td><svg></TABLE\n{}>", "x ".repeat(5_000)),
        );
        assert_eq!(
            webkit_freeze_reason(&padded).as_deref(),
            Some(
                "its </table> at byte 2545 closes a table cell nested past WebKit's \
                 512-element limit, which freezes Canvas.app; nest the table less deeply"
            )
        );
        assert_eq!(
            webkit_freeze_reason(&deep(504, "<table><tr><td><svg></table>")),
            None
        );
    }

    #[test]
    fn a_whole_page_freezes_one_element_deeper_than_a_card() {
        // A page's own frame holds `<html>` and `<body>` but no wrapper
        // `<div>`, so the depth that freezes a card leaves a page one short.
        let head = "<!doctype html><html><head><meta charset=utf-8><title>t</title>\
                    <style>td{}</style><script>let a = '<div>';</script></head><body>";
        let page = |k: usize| {
            format!(
                "{head}{}<table><tr><td><svg></table></body></html>",
                "<div>".repeat(k)
            )
        };
        assert!(webkit_freeze_reason(&page(505)).is_some());
        assert_eq!(webkit_freeze_page_reason(&page(505)), None);
        let html = page(506);
        let at = html.find("</table>").unwrap();
        assert_eq!(
            webkit_freeze_page_reason(&html),
            Some(format!(
                "its </table> at byte {at} closes a table cell nested past WebKit's \
                 512-element limit, which freezes Canvas.app; nest the table less deeply"
            ))
        );
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

    /// The elements open around text at the end of `html`, outermost first,
    /// a run of one name written `name*count`: the chain of parents WebKit
    /// gives that text, less the frame's `<html>`, `<body>` and wrapper.
    fn chain_at_end(html: &str) -> String {
        let mut scan = tags(html);
        while scan.next().is_some() {}
        scan.begin_token();
        scan.text(html.len());
        let mut runs: Vec<(&str, usize)> = Vec::new();
        for e in scan.open.iter() {
            match runs.last_mut() {
                Some((name, n)) if *name == e.name => *n += 1,
                _ => runs.push((&e.name, 1)),
            }
        }
        runs.iter()
            .map(|&(name, n)| match n {
                1 => name.to_string(),
                _ => format!("{name}*{n}"),
            })
            .collect::<Vec<_>>()
            .join(">")
    }

    #[test]
    fn the_depth_cap_closes_no_element_its_own_token_opened() {
        // WebKit attaches a token's nodes when the token ends, and its depth
        // cap closes the innermost element only when that has a parent, so
        // what one token opens stays open past the 509 a card's `<div>`s
        // leave. Each chain is the one WebKit builds around the final text:
        // Canvas.app's for the cases without a table, Playwright's for the
        // tables, since a table past the cap can freeze Canvas.app.
        let deep = |k: usize, tail: &str| format!("{}{tail}", "<div>".repeat(k));
        for (k, tail, chain) in [
            // The first copy reopened before text closes the `<span>`; each
            // later one goes inside the copy before it.
            (509, "<b><i><u><span>Q", "div*508>b>i>u"),
            (509, "<b><i><u><span><p>Q", "div*508>b>i>u"),
            // So does the start tag the copies open before.
            (509, "<b><i><u><span><em>Q", "div*508>b>i>u>em"),
            (509, "<b><i><u><span><svg><g>Q", "div*508>b>i>u>g"),
            (508, "<b><i><u><span><s>Q1<em>Q", "div*508>b>i>u>em"),
            (
                509,
                "<b id=1><b id=2><b id=3><b id=4><span>Q1<div>Q2</div>Q",
                "div*507>b*4",
            ),
            (509, "<b><i><span>Q1</b>Q", "div*508>i"),
            (505, "<a>x<div><a>Q", "div*506>a"),
            // The `<tbody>` a `<tr>` implies keeps the row past the cap, then
            // the cell, which the cap puts beside the closed row.
            (507, "<table><tr><td>Q", "div*507>table>tbody>td"),
            (506, "<table><col><col><tr><td>Q", "div*506>table>tbody>td"),
            (508, "<table><tr><td>Q", "div*508>tbody>td"),
            (507, "<table><td>Q", "div*507>table>tbody>tr>td"),
            (505, "<table><tr><td>Q", "div*505>table>tbody>tr>td"),
        ] {
            assert_eq!(chain_at_end(&deep(k, tail)), chain, "{k} {tail}");
        }
        // With the `<tbody>` open past the cap, an end tag the cell mode
        // reads closes a section or table the cap left in table scope, and
        // finds the cell closed.
        for tail in [
            "<table><tr><td><svg></tbody>",
            "<table><tr></tr><tr><td><svg></table>",
        ] {
            let html = deep(507, tail);
            assert_eq!(webkit_freeze(&html), html.rfind("</"), "{tail}");
        }
    }

    #[test]
    fn reopening_more_elements_than_the_page_could_write_freezes_webkit() {
        // Each text run in a `<div>` reopens the 400 `<b>`s the `</p>`
        // closed, and the `</div>` closes them again: WebKit builds 400
        // copies per 12 bytes, and the 256th run reopens the 102,400th, past
        // 100,000 plus a third of the 6,962 bytes before its text.
        let bs: String = (0..400).map(|i| format!("<b id={i}>")).collect();
        let page = |runs: usize| format!("<p>{bs}</p>{}", "<div>x</div>".repeat(runs));
        assert_eq!(webkit_freeze(&page(255)), None);
        let html = page(300);
        let at = html.len() - 44 * "<div>x</div>".len() - "</div>".len();
        assert_eq!(at - "x".len(), 6_962);
        assert_eq!(webkit_freeze(&html), Some(at));
        assert_eq!(
            webkit_freeze_reason(&html),
            Some(format!(
                "by its </div> at byte {at}, WebKit has reopened more than 102320 \
                 closed formatting elements such as <b>, 100000 plus one per 3 bytes \
                 before them, which stalls Canvas.app; close each formatting element \
                 where it should end"
            ))
        );
    }

    #[test]
    fn past_the_budget_the_scan_reopens_only_the_newest_element() {
        // Each later run grows the budget by 5, so now and then the
        // count falls back under it and the scan counts the 400 closed
        // `<b>`s again, which takes it straight back over: it still reopens
        // only the newest.
        let bs: String = (0..400).map(|i| format!("<b id={i}>")).collect();
        // A `<br>` after each run's text reads the stack with its copies open.
        let html = format!("<p>{bs}</p>{}", "<div>x<br></div>".repeat(1_000));
        let mut scan = tags(&html);
        let mut deepest = 0;
        while let Some(tag) = scan.next() {
            if scan.freeze.is_some() && html[tag.start..].starts_with("<br>") {
                deepest = deepest.max(scan.open.len());
            }
        }
        // The `<div>` and one copy of the newest `<b>`; a `<br>` is void.
        assert_eq!(deepest, 2);
    }

    #[test]
    fn a_long_page_misnesting_a_few_elements_per_paragraph_does_not_freeze() {
        // Each paragraph's text reopens the five elements the first `</p>`
        // closed: 1,000,000 copies over 3.6 MB, fewer elements than the page
        // could write out itself.
        let html = format!(
            "<p><b><i><u><s><em>x</p>{}",
            "<p>a paragraph</p>".repeat(200_000)
        );
        assert_eq!(webkit_freeze(&html), None);
        // The same five reopened every 8 bytes build more than the page's
        // own length could.
        let dense = format!("<p><b><i><u><s><em>x</p>{}", "<p>x</p>".repeat(200_000));
        assert!(webkit_freeze(&dense).is_some());
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
    fn raw_text_elements_and_plaintext_hold_text() {
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
            // A card frame runs scripts, so `<noscript>` is raw text too:
            // DOMParser, with scripting off, would build the `<b>`.
            ("<noscript><b>x</b></noscript>", 0),
            ("<noscript></noscript ><b>x</b>", 1),
            ("<p>Hi<noscript><p>No JS</p></noscript></p>", 0),
            ("<table><tr><td>x</td></tr><noscript><b>x</b></noscript></table>", 0),
            ("<svg><noscript><b>x</b></noscript></svg>", 1),
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
            (
                "<svg><foreignObject><noscript><div></noscript></foreignObject><style/><b>x</b></style>",
                1,
            ),
            ("<math><mi><noscript><b>x</b></noscript></mi></math>", 0),
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
        // The agency leaves a formatting element below a cell or table
        // where it is, and moves one above a block at most eight times. Each chain is the
        // one WebKit builds around the final text.
        for (html, chain) in [
            ("<b><table><tr><td>x</b>", "b>table>tbody>tr>td"),
            ("<b><table></b><tr><td>x", "b>table>tbody>tr>td"),
            (
                "<b><div><div><div><div><div><div><div><div><div><div>x</b>",
                "div*8>b>div*2",
            ),
        ] {
            assert_eq!(chain_at_end(html), chain, "{html}");
        }
        // A row in a template closes nothing below the template.
        assert_eq!(
            card_title("<template><tr><td>hidden</td></tr></template>shown"),
            "shown"
        );
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
            // An SVG end tag looks for its element only above the nearest
            // HTML one: the `</g>` closes nothing, and `</div>` closes the
            // inner svg.
            (
                "<svg><g><foreignObject><div><svg></g></div><style/><b>x</b></style>",
                0,
            ),
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
    fn a_column_group_closes_whatever_the_depth_cap_left_current() {
        // After 507 `<div>`s the cap closes the colgroup when `<col>` goes
        // in, leaving the table current in the column group mode. Anything
        // that mode doesn't take closes the table, as it would the colgroup,
        // and `<style/>` then opens raw text: Canvas.app builds no `<b>`.
        let deep = format!("{}<table><colgroup><col>", "<div>".repeat(507));
        for tail in ["", "x", " ", "</div>", "</colgroup>", "</col>", "<col>"] {
            for (label, html) in [
                ("deep", format!("{deep}{tail}<style/><b>x</b></style>")),
                (
                    "shallow",
                    format!("<table><colgroup><col>{tail}<style/><b>x</b></style>"),
                ),
            ] {
                assert!(
                    !tags(&html).any(|t| &html[t.start..t.end] == "<b>"),
                    "{label} {tail:?}"
                );
            }
        }
        // After 506 `<div>`s and an outer template, the cap closes an inner
        // template when `<col>` goes in, leaving a `<div>` current in the
        // column group mode the inner template picked. Text or an end tag
        // closes that `<div>`, leaving the outer template current; closing a
        // third template then goes back to the column group mode, where
        // `<style/>` is dropped (1 `<b>`). `<html>`, whitespace and `</col>`
        // close nothing, so `<style/>` closes the `<div>` and opens raw text
        // (0), as Canvas.app builds them.
        for (mid, bs) in [
            ("x", 1),
            ("</span>", 1),
            ("<html>", 0),
            (" ", 0),
            ("", 0),
            ("</col>", 0),
        ] {
            let html = format!(
                "{}<template><div><template><col>{mid}<template></template><style/>\
                 </template></template><b>x</b>",
                "<div>".repeat(506)
            );
            let names: Vec<&str> = tags(&html).map(|t| &html[t.start..t.end]).collect();
            assert_eq!(
                names.iter().filter(|n| **n == "<b>").count(),
                bs,
                "{mid:?}: {:?}",
                &names[506..]
            );
        }
    }

    #[test]
    fn a_template_the_depth_cap_closed_keeps_its_mode() {
        // The cap closes the inner template, and its entry in the stack of
        // template modes stays until a `</template>`, so closing a third
        // template inside the outer one goes back to the inner one's mode.
        // `<style/>` shows it: dropped in a column group with a template
        // current (1 `<b>`), raw text in a body (0), as Canvas.app builds
        // them. With 506 `<div>`s the `<col>` leaves a `<div>` current,
        // which `<style/>` closes before opening raw text; with 508 the cap
        // closes the outer template too.
        let shapes = [
            "<template><div><template><col>",
            "<template><col><template><div></div>",
            "<template><div><template><!----><col>",
        ];
        for (depth, bs) in [
            (505, [1, 0, 1]),
            (506, [0, 0, 0]),
            (507, [1, 0, 1]),
            (508, [0, 0, 0]),
        ] {
            for (shape, bs) in shapes.iter().zip(bs) {
                let html = format!(
                    "{}{shape}<template></template><style/></template></template><b>x</b>",
                    "<div>".repeat(depth)
                );
                let names: Vec<&str> = tags(&html).map(|t| &html[t.start..t.end]).collect();
                assert_eq!(
                    names.iter().filter(|n| **n == "<b>").count(),
                    bs,
                    "{depth} {shape}: {:?}",
                    &names[depth..]
                );
            }
        }
    }

    #[test]
    fn a_table_part_reaching_a_select_the_depth_cap_closed_freezes_webkit() {
        // Inside a cell and `k` `<div>`s, the cap closes the select when the
        // `<option>` goes in, and "in select in table" stays. A table part's
        // start tag, or its end tag in table scope, then loops in WebKit.
        let deep = |k: usize, tail: &str| {
            format!(
                "<table><tr><td>{}<select><option>{tail}<img>",
                "<div>".repeat(k)
            )
        };
        for tail in ["<td>", "</td>", "</tr>", "<TR class=x>"] {
            let html = deep(504, tail);
            let at = html.rfind(tail).unwrap();
            assert_eq!(webkit_freeze(&html), Some(at), "{tail}");
        }
        // `</th>` is not in table scope, WebKit checks for a select before
        // an `<input>` closes it, and 503 `<div>`s leave the select open.
        for (k, tail) in [(504, "</th>"), (504, "<input>"), (504, ""), (503, "</td>")] {
            assert_eq!(webkit_freeze(&deep(k, tail)), None, "{k} {tail}");
        }
        assert_eq!(
            webkit_freeze_reason(&deep(504, "<TD id=a>")).as_deref(),
            Some(
                "its <td> at byte 2551 closes a select in a table nested past WebKit's \
                 512-element limit, which freezes Canvas.app; nest the select less deeply"
            )
        );
    }

    #[test]
    fn a_select_runs_the_older_in_select_mode() {
        // Canvas.app's WebKit still runs the older "in select" mode, where
        // Chromium and newer WebKit follow the current spec: inside a select
        // `<style/>` is dropped, so the `<b>` after it is a tag (1), and
        // once the select closes it opens raw text (0).
        for (inner, bs) in [
            ("<select>", 1),
            ("<select><option>", 1),
            ("<select><optgroup><hr>", 1),
            ("<select><div>", 1),
            ("<select><tr>", 1),
            ("<select></select>", 0),
            ("<select><div></select>", 0),
            ("<select><option></select>", 0),
            ("<select><select>", 0),
            ("<select><input>", 0),
            ("<select><keygen>", 0),
            // `<textarea>` and `<script>` are raw text to the end.
            ("<select><textarea>", 0),
            ("<select><script>", 0),
            ("<select><script></script>", 1),
            // A template's contents are read as a body.
            ("<select><template>", 0),
            ("<select><template></template>", 1),
            // In a table, a table part's start tag or its end tag in table
            // scope closes the select too.
            ("<table><tr><td><select><td>", 0),
            ("<table><tr><td><select></td>", 0),
            ("<table><tr><td><select></tr>", 0),
            ("<table><tr><td><select></th>", 1),
            ("<table><select><tr>", 0),
            ("<table><tr><td><select><template></template><td>", 0),
            // A template between the table and the select keeps it apart.
            ("<table><tr><td><template><select><td>", 1),
            (
                "<table><tr><td><template><select><template></template><td>",
                1,
            ),
        ] {
            let html = format!("{inner}<style/><b>x</b></style>");
            let names: Vec<&str> = tags(&html).map(|t| &html[t.start..t.end]).collect();
            assert_eq!(
                names.iter().filter(|n| **n == "<b>").count(),
                bs,
                "{html}: {names:?}"
            );
        }
        // An `<svg>` in a select is dropped, so it opens no SVG.
        let html = "<select><svg><style/><b>x</b></style>";
        assert!(tags(html).all(|t| !t.foreign), "{html}");
        // Text in a select reopens no formatting element: with the stack
        // full, reopening the `<b>` would close the select.
        let html = format!(
            "{}<b><select>x<input><style/><b>x</b></style>",
            "<div>".repeat(508)
        );
        assert!(!tags(&html).any(|t| &html[t.start..t.end] == "<b>" && t.start > 508 * 5 + 3));
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
            ("<noscript><p>Enable JS</p></noscript><p>Real</p>", "Real"),
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
    fn a_heading_reads_its_body_as_visible_text_does() {
        for body in [
            "A<template></h1></template>B",
            "A<svg><script>s<g>t</g></script></svg>B",
            "a &amp;lt; b<p>c &copy x",
            "<xmp>&amp;</xmp>y",
            "A<svg><style/>s</svg>B",
        ] {
            assert_eq!(
                first_heading(&format!("<h1>{body}</h1>")),
                Some(collapse_whitespace(&visible_text(body))),
                "{body}"
            );
        }
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
    fn visible_text_drops_an_svg_or_mathml_script_or_style() {
        assert_eq!(
            visible_text("<svg><style>@import \"m.css?a=1&amp;b=2\";.c{}</style></svg>shown"),
            "shown"
        );
        assert_eq!(
            visible_text("<math><script>s</script><mi>x</mi></math>"),
            "x"
        );
        // Its child elements' text is muted too, until the scan closes it.
        assert_eq!(
            visible_text("<svg><style>a<g>b</g>c</style><text>t</text></svg>"),
            "t"
        );
        // A block tag in SVG is an SVG element and starts no line there.
        assert_eq!(
            visible_text("<svg><style>a<section>b</section></style></svg>c"),
            "c"
        );
        assert_eq!(visible_text("a<svg><style>never closed"), "a");
        // A `<p>` breaks out of the SVG and closes the style on the way.
        assert_eq!(visible_text("<svg><style>a<p>b"), "\nb");
        // One that closed itself mutes nothing.
        assert_eq!(visible_text("<svg><style/><text>t</text></svg>"), "t");
        assert_eq!(visible_text("<svg><noscript>n</noscript></svg>x"), "x");
    }

    #[test]
    fn card_label_skips_an_svg_style_for_the_next_line() {
        assert_eq!(
            card_label("<svg><style>.c{}\n</style></svg><p>Plan</p>").as_deref(),
            Some("Plan")
        );
        assert_eq!(
            first_heading("<h1>A<svg><script>s</script></svg>B</h1>").as_deref(),
            Some("AB")
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
