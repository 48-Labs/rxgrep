//! Searching an XML input.
//!
//! Two quick passes over the same text, both on the zero-copy [`Reader`]:
//!
//! 1. [`Layout::scan`] skims the structure (no decoding) and records, per
//!    element, whether same-named siblings exist (so `[n]` is needed) and
//!    whether it has mixed content (so text is addressed as `text()[n]`).
//!    That keeps queries minimal: `/catalog/name`, not `/catalog[1]/name[1]`.
//! 2. [`Searcher::scan`] walks the events, keeps only the current path and
//!    hands every match to a [`Sink`] immediately; it stops as soon as the
//!    sink has enough.

use crate::matcher::{Matcher, NumFilter};
use crate::path::Step;
use crate::xml::{self, Event, PResult, Reader};
use std::borrow::Cow;

/// XPath `normalize-space()`: trim, and collapse whitespace runs to one space.
fn normalize_space(s: &str) -> Cow<'_, str> {
    let t = s.trim();
    let tidy = !t.as_bytes().windows(2).any(|w| w[0].is_ascii_whitespace() && w[1].is_ascii_whitespace())
        && !t.bytes().any(|b| matches!(b, b'\t' | b'\n' | b'\r'));
    if tidy {
        Cow::Borrowed(t)
    } else {
        Cow::Owned(t.split_ascii_whitespace().collect::<Vec<_>>().join(" "))
    }
}

/// Which parts of the document are compared against the pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Element and attribute names, and values.
    All,
    /// Element and attribute names only.
    Names,
    /// Text, attribute values (and comments, if enabled).
    Values,
}

/// Which kinds of values may match.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KindFilter {
    pub text: bool,
    pub attr: bool,
    pub comment: bool,
}

impl KindFilter {
    /// Text and attribute values; comments only on request.
    pub fn default_kinds() -> Self {
        KindFilter { text: true, attr: true, comment: false }
    }

    /// Parse a comma-separated list such as `text,attr`.
    pub fn parse(list: &str) -> Result<Self, String> {
        let mut f = KindFilter { text: false, attr: false, comment: false };
        for t in list.split(',').map(str::trim).filter(|t| !t.is_empty()) {
            match t.to_ascii_lowercase().as_str() {
                "text" | "cdata" => f.text = true,
                "attr" | "attribute" | "attributes" => f.attr = true,
                "comment" | "comments" => f.comment = true,
                "all" | "any" => f = KindFilter { text: true, attr: true, comment: true },
                _ => return Err(format!("unknown type '{t}' (expected text, attr, comment)")),
            }
        }
        if !(f.text || f.attr || f.comment) {
            return Err("--type needs at least one type".into());
        }
        Ok(f)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HitKind {
    /// An element or attribute name matched.
    Name,
    /// A text, attribute or comment value matched.
    Value,
}

/// One match, valid only for the duration of the [`Sink::hit`] call.
pub struct Hit<'h> {
    pub path: &'h [Step<'h>],
    pub kind: HitKind,
    /// The matched text (whitespace-normalised for text and comments; the
    /// name for name hits).
    pub value: &'h str,
    /// The node's real text had surrounding whitespace that `value` omits.
    pub padded: bool,
}

/// Receives matches as they are found.
pub trait Sink {
    /// A new root element (0-based document number) is starting.
    fn doc_start(&mut self, index: usize);
    /// A match. Return `false` to stop scanning.
    fn hit(&mut self, hit: &Hit) -> bool;
}

// ---------------------------------------------------------------------------
// Pass 1: layout
// ---------------------------------------------------------------------------

/// Per-element facts needed to write minimal queries, indexed by element
/// number in document order.
#[derive(Debug, Default)]
pub struct Layout {
    needs_index: Vec<u64>,
    mixed: Vec<u64>,
    /// Number of root elements (documents) seen.
    pub roots: usize,
}

fn set_bit(bits: &mut Vec<u64>, i: usize) {
    if bits.len() <= i / 64 {
        bits.resize(i / 64 + 1, 0);
    }
    bits[i / 64] |= 1 << (i % 64);
}

/// Element names compare case-insensitively in HTML.
fn same_name(a: &str, b: &str, html: bool) -> bool {
    if html {
        a.eq_ignore_ascii_case(b)
    } else {
        a == b
    }
}

fn bit(bits: &[u64], i: usize) -> bool {
    bits.get(i / 64).is_some_and(|w| w & (1 << (i % 64)) != 0)
}

impl Layout {
    /// Skim `src` (as HTML when `html`). On malformed input, facts up to the
    /// error are kept (the search pass reports the error itself).
    pub fn scan(src: &str, html: bool) -> Layout {
        struct Frame<'a> {
            ord: usize,
            /// (child name, count, element number of the first one)
            names: Vec<(&'a str, u32, usize)>,
            text: u32,
            children: u32,
        }
        let mut layout = Layout::default();
        let mut r = Reader::new(src);
        r.decode = false;
        r.html = html;
        let mut stack: Vec<Frame> = Vec::new();
        let mut pool: Vec<Vec<(&str, u32, usize)>> = Vec::new();
        let mut ord = 0usize;
        while let Ok(ev) = r.next() {
            match ev {
                Event::Start { name, .. } => {
                    let me = ord;
                    ord += 1;
                    match stack.last_mut() {
                        Some(parent) => {
                            parent.children += 1;
                            match parent.names.iter_mut().find(|e| same_name(e.0, name, html)) {
                                Some(e) => {
                                    e.1 += 1;
                                    if e.1 == 2 {
                                        set_bit(&mut layout.needs_index, e.2);
                                    }
                                    set_bit(&mut layout.needs_index, me);
                                }
                                None => parent.names.push((name, 1, me)),
                            }
                        }
                        None => layout.roots += 1,
                    }
                    let mut names = pool.pop().unwrap_or_default();
                    names.clear();
                    stack.push(Frame { ord: me, names, text: 0, children: 0 });
                }
                Event::End => {
                    if let Some(f) = stack.pop() {
                        if f.text >= 2 || (f.text >= 1 && f.children >= 1) {
                            set_bit(&mut layout.mixed, f.ord);
                        }
                        pool.push(f.names);
                    }
                }
                Event::Text(t) => {
                    if !t.trim().is_empty() {
                        if let Some(f) = stack.last_mut() {
                            f.text += 1;
                        }
                    }
                }
                Event::Comment(_) => {}
                Event::Eof => break,
            }
        }
        layout
    }

    fn needs_index(&self, ord: usize) -> bool {
        bit(&self.needs_index, ord)
    }

    fn mixed(&self, ord: usize) -> bool {
        bit(&self.mixed, ord)
    }
}

// ---------------------------------------------------------------------------
// Pass 2: search
// ---------------------------------------------------------------------------

/// What a finished (or stopped) scan saw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanSummary {
    /// Root elements started.
    pub docs: usize,
}

pub struct Searcher<'m> {
    /// A value matches when *any* matcher matches (and the numeric filter
    /// holds). With no matchers, only the numeric filter decides.
    pub matchers: &'m [Matcher],
    /// `--gt`, `--lt`, `--range`, ...: values must be numbers in range.
    pub numeric: NumFilter,
    pub scope: Scope,
    pub kinds: KindFilter,
    /// Deepest element level to inspect (root element = 1). `None` = unlimited.
    pub max_depth: Option<usize>,
    /// Shallowest level at which matches are reported.
    pub min_depth: usize,
    /// Look inside text that is itself an XML document.
    pub decode: bool,
    /// Parse the input leniently as HTML.
    pub html: bool,
    /// Record each element's `id` attribute (for CSS selectors).
    pub want_ids: bool,
}

impl Searcher<'_> {
    /// Scan every root element in `src`, reporting matches to `sink`.
    /// `layout` must come from [`Layout::scan`] on the same text; without it
    /// every repeated element is indexed and no text is treated as mixed.
    pub fn scan<S: Sink>(&self, src: &str, layout: Option<&Layout>, sink: &mut S) -> PResult<ScanSummary> {
        let mut w = Walker {
            s: self,
            layout,
            path: Vec::new(),
            base_depth: 0,
            sink,
            stopped: false,
            top_level: true,
            docs: 0,
            html: self.html,
        };
        w.run(src)?;
        Ok(ScanSummary { docs: w.docs })
    }

    /// Text, attribute values and comments: patterns and numeric bounds.
    #[inline]
    fn is_value_match(&self, text: &str) -> bool {
        (self.matchers.is_empty() || self.matchers.iter().any(|m| m.is_match(text)))
            && (self.numeric.is_empty() || self.numeric.matches(text))
    }

    /// Element and attribute names: patterns only.
    #[inline]
    fn is_name_match(&self, name: &str) -> bool {
        !self.matchers.is_empty() && self.matchers.iter().any(|m| m.is_match(name))
    }

    #[inline]
    fn in_range(&self, depth: usize) -> bool {
        depth >= self.min_depth && self.max_depth.is_none_or(|m| depth <= m)
    }

    fn wants_names(&self) -> bool {
        self.scope != Scope::Values
    }

    fn wants_values(&self) -> bool {
        self.scope != Scope::Names
    }
}

struct Frame<'a> {
    ord: usize,
    /// A default namespace (`xmlns="..."`) is in scope.
    default_ns: bool,
    /// Inside `<svg>` or `<math>` in HTML (those elements are in a namespace).
    foreign: bool,
    /// Same-named children seen so far.
    names: Vec<(&'a str, usize)>,
    texts: usize,
    comments: usize,
    path_len: usize,
    depth: usize,
}

struct Walker<'s, 'm, 'a, S> {
    s: &'s Searcher<'m>,
    layout: Option<&'s Layout>,
    path: Vec<Step<'a>>,
    /// Depth of the element that owns this document (for embedded XML).
    base_depth: usize,
    sink: &'s mut S,
    stopped: bool,
    top_level: bool,
    docs: usize,
    /// Parse this document as HTML (embedded documents are always XML).
    html: bool,
}

impl<'a, S: Sink> Walker<'_, '_, 'a, S> {
    fn emit(&mut self, kind: HitKind, value: &str, padded: bool) {
        let hit = Hit { path: &self.path, kind, value, padded };
        if !self.sink.hit(&hit) {
            self.stopped = true;
        }
    }

    fn run(&mut self, src: &'a str) -> PResult<()> {
        let s = self.s;
        let html = self.html;
        let mut r = Reader::new(src);
        r.html = html;
        let mut frames: Vec<Frame<'a>> = Vec::new();
        let mut pool: Vec<Vec<(&'a str, usize)>> = Vec::new();
        let mut ord = 0usize;
        // While > 0 we are inside an element deeper than --depth: just count.
        let mut skipping = 0usize;
        loop {
            if self.stopped {
                return Ok(());
            }
            let depth = self.base_depth + frames.len();
            r.decode = skipping == 0 && s.in_range(depth) && s.wants_values() && (s.kinds.text || s.decode);
            match r.next()? {
                Event::Start { name, attrs } => {
                    let me = ord;
                    ord += 1;
                    if skipping > 0 {
                        skipping += 1;
                        continue;
                    }
                    let depth = depth + 1;
                    if s.max_depth.is_some_and(|m| depth > m) {
                        skipping = 1;
                        continue;
                    }
                    let pos = match frames.last_mut() {
                        Some(parent) => match parent.names.iter_mut().find(|e| same_name(e.0, name, html)) {
                            Some(e) => {
                                e.1 += 1;
                                e.1
                            }
                            None => {
                                parent.names.push((name, 1));
                                1
                            }
                        },
                        None => {
                            if self.top_level {
                                self.sink.doc_start(self.docs);
                            }
                            self.docs += 1;
                            1
                        }
                    };
                    let is_root = frames.is_empty();
                    let indexed = !is_root && self.layout.map_or(pos > 1, |l| l.needs_index(me));
                    let path_len = self.path.len();
                    let id = if s.want_ids {
                        xml::attributes(attrs)
                            .find(|(n, _)| same_name(n, "id", html))
                            .map(|(_, v)| v)
                            .filter(|v| !v.contains('&'))
                    } else {
                        None
                    };
                    let (mut default_ns, mut foreign) =
                        frames.last().map_or((false, false), |f| (f.default_ns, f.foreign));
                    if !html && attrs.contains("xmlns") {
                        if let Some((_, v)) = xml::attributes(attrs).find(|(n, _)| *n == "xmlns") {
                            default_ns = !v.trim().is_empty();
                        }
                    }
                    if html && (name.eq_ignore_ascii_case("svg") || name.eq_ignore_ascii_case("math")) {
                        foreign = true;
                    }
                    let ns = foreign || default_ns || name.contains(':');
                    self.path.push(Step::Elem { name, pos, indexed, id, ns });
                    let mut names = pool.pop().unwrap_or_default();
                    names.clear();
                    frames.push(Frame { ord: me, default_ns, foreign, names, texts: 0, comments: 0, path_len, depth });

                    if s.in_range(depth) {
                        if s.wants_names() && s.is_name_match(name) {
                            self.emit(HitKind::Name, name, false);
                        }
                        self.attributes(attrs);
                    }
                }
                Event::End => {
                    if skipping > 0 {
                        skipping -= 1;
                        continue;
                    }
                    if let Some(f) = frames.pop() {
                        self.path.truncate(f.path_len);
                        pool.push(f.names);
                    }
                }
                Event::Text(t) => {
                    if skipping > 0 {
                        continue;
                    }
                    let Some(f) = frames.last_mut() else { continue };
                    f.texts += 1;
                    let (pos, ord, fdepth) = (f.texts, f.ord, f.depth);
                    if !(r.decode && s.in_range(fdepth)) {
                        continue;
                    }
                    let trimmed = t.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    let mixed = self.layout.is_some_and(|l| l.mixed(ord));
                    self.path.push(Step::Text { pos, mixed });
                    if s.decode && trimmed.starts_with('<') && xml::is_document(trimmed) {
                        self.embedded(trimmed, fdepth)?;
                    } else if s.kinds.text {
                        let value = normalize_space(trimmed);
                        if s.is_value_match(&value) {
                            self.emit(HitKind::Value, &value, *value != *t);
                        }
                    }
                    self.path.pop();
                }
                Event::Comment(c) => {
                    if skipping > 0 {
                        continue;
                    }
                    let Some(f) = frames.last_mut() else { continue };
                    f.comments += 1;
                    let (pos, fdepth) = (f.comments, f.depth);
                    if s.kinds.comment && s.wants_values() && s.in_range(fdepth) {
                        let value = normalize_space(c);
                        if s.is_value_match(&value) {
                            self.path.push(Step::Comment { pos });
                            self.emit(HitKind::Value, &value, *value != *c);
                            self.path.pop();
                        }
                    }
                }
                Event::Eof => return Ok(()),
            }
        }
    }

    fn attributes(&mut self, raw: &'a str) {
        let s = self.s;
        for (name, raw_value) in xml::attributes(raw) {
            if self.stopped {
                return;
            }
            // Namespace declarations are not attributes in XPath.
            if name == "xmlns" || name.starts_with("xmlns:") {
                continue;
            }
            self.path.push(Step::Attr(name));
            if s.wants_names() && s.is_name_match(name) {
                self.emit(HitKind::Name, name, false);
            }
            if !self.stopped && s.wants_values() && s.kinds.attr {
                let value = xml::decode(raw_value, true);
                if s.is_value_match(&value) {
                    self.emit(HitKind::Value, &value, false);
                }
            }
            self.path.pop();
        }
    }

    /// Search an XML document held in a text node. Its root element sits one
    /// level below the element that holds the text.
    fn embedded(&mut self, src: &str, depth: usize) -> PResult<()> {
        let layout = Layout::scan(src, false);
        let mut path: Vec<Step> = self.path.clone();
        path.push(Step::Decode);
        let mut inner = Walker {
            s: self.s,
            layout: Some(&layout),
            path,
            base_depth: depth,
            sink: &mut *self.sink,
            stopped: false,
            top_level: false,
            docs: 0,
            html: false,
        };
        inner.run(src)?;
        self.stopped = inner.stopped;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::matcher::Mode;
    use crate::path::{format, Style};

    struct Collect {
        out: Vec<String>,
        limit: usize,
    }

    impl Sink for Collect {
        fn doc_start(&mut self, _: usize) {}
        fn hit(&mut self, hit: &Hit) -> bool {
            self.out.push(format!("{} = {}", format(hit.path, Style::default(), false), hit.value));
            self.out.len() < self.limit
        }
    }

    fn searcher(m: &[Matcher]) -> Searcher<'_> {
        Searcher {
            matchers: m,
            scope: Scope::Values,
            kinds: KindFilter::default_kinds(),
            max_depth: None,
            min_depth: 0,
            decode: false,
            numeric: NumFilter::default(),
            html: false,
            want_ids: false,
        }
    }

    fn run(s: &Searcher, src: &str) -> Vec<String> {
        let layout = Layout::scan(src, s.html);
        let mut c = Collect { out: Vec::new(), limit: usize::MAX };
        s.scan(src, Some(&layout), &mut c).unwrap();
        c.out
    }

    const DOC: &str = r#"<lib>
  <book id="b1"><title>Rust</title><author>Alice</author></book>
  <book id="b2"><title>XML</title><author>Bob</author><editor>Alice</editor></book>
  <note>By <b>Alice</b> and Bob</note>
  <!-- Alice -->
</lib>"#;

    fn exact(p: &str) -> Vec<Matcher> {
        vec![Matcher::new(p, Mode::Exact, false)]
    }

    #[test]
    fn finds_text_with_minimal_indices() {
        let m = exact("Alice");
        assert_eq!(
            run(&searcher(&m), DOC),
            ["/lib/book[1]/author = Alice", "/lib/book[2]/editor = Alice", "/lib/note/b = Alice"]
        );
    }

    #[test]
    fn attributes_names_and_comments() {
        let m = exact("b2");
        assert_eq!(run(&searcher(&m), DOC), ["/lib/book[2]/@id = b2"]);
        let m = exact("id");
        let names = Searcher { scope: Scope::Names, ..searcher(&m) };
        assert_eq!(run(&names, DOC), ["/lib/book[1]/@id = id", "/lib/book[2]/@id = id"]);
        let m = exact("Alice");
        let comments = Searcher { kinds: KindFilter::parse("comment").unwrap(), ..searcher(&m) };
        assert_eq!(run(&comments, DOC), ["/lib/comment()[1] = Alice"]);
    }

    #[test]
    fn mixed_content_uses_text_index() {
        let m = exact("and Bob");
        assert_eq!(run(&searcher(&m), DOC), ["/lib/note/text()[2] = and Bob"]);
    }

    #[test]
    fn depth_limits() {
        let m = exact("Alice");
        let shallow = Searcher { max_depth: Some(2), ..searcher(&m) };
        assert!(run(&shallow, DOC).is_empty());
        let s3 = Searcher { max_depth: Some(3), min_depth: 3, ..searcher(&m) };
        assert_eq!(run(&s3, DOC).len(), 3);
        let m = exact("b1");
        let s2 = Searcher { max_depth: Some(2), ..searcher(&m) };
        assert_eq!(run(&s2, DOC), ["/lib/book[1]/@id = b1"]);
    }

    #[test]
    fn entities_cdata_and_xmlns() {
        let m = exact("a & <b>");
        let src = r#"<r xmlns="urn:x" xmlns:p="urn:p"><v>a &amp; <![CDATA[<b>]]></v></r>"#;
        // xmlns="urn:x" puts r and v in a namespace
        assert_eq!(run(&searcher(&m), src), ["/*[local-name()='r']/*[local-name()='v'] = a & <b>"]);
        let m = exact("urn:x");
        assert!(run(&searcher(&m), src).is_empty(), "namespace declarations are not attributes");
    }

    #[test]
    fn namespaces_use_local_name() {
        let m = exact("x");
        let src = r#"<feed xmlns="urn:atom"><entry><t>x</t></entry><o xmlns=""><t>x</t></o><p:q xmlns:p="urn:p">x</p:q></feed>"#;
        assert_eq!(
            run(&searcher(&m), src),
            [
                "/*[local-name()='feed']/*[local-name()='entry']/*[local-name()='t'] = x",
                "/*[local-name()='feed']/o/t = x",
                "/*[local-name()='feed']/*[local-name()='q'] = x",
            ]
        );
    }

    #[test]
    fn embedded_xml() {
        let m = exact("Alice");
        let src = "<ev><payload>&lt;order&gt;&lt;c&gt;Alice&lt;/c&gt;&lt;/order&gt;</payload></ev>";
        assert!(run(&searcher(&m), src).is_empty());
        let s = Searcher { decode: true, ..searcher(&m) };
        assert_eq!(run(&s, src), ["/ev/payload/parse-xml(.)/order/c = Alice"]);
    }

    #[test]
    fn stops_early_and_counts_docs() {
        let m = exact("Alice");
        let s = searcher(&m);
        let mut c = Collect { out: Vec::new(), limit: 1 };
        s.scan(DOC, None, &mut c).unwrap();
        assert_eq!(c.out.len(), 1);
        let mut c = Collect { out: Vec::new(), limit: usize::MAX };
        let sum = s.scan("<a>Alice</a>\n<a>x</a>\n<a>Alice</a>", None, &mut c).unwrap();
        assert_eq!(sum.docs, 3);
        assert_eq!(c.out, ["/a = Alice", "/a = Alice"]);
    }

    #[test]
    fn whitespace_is_normalised() {
        let m = exact("Padded Title");
        assert_eq!(run(&searcher(&m), "<a><t>\n   Padded \t\n  Title\n</t></a>"), ["/a/t = Padded Title"]);
        assert!(matches!(normalize_space("  tidy text "), Cow::Borrowed("tidy text")));
        assert_eq!(normalize_space("a\n  b"), "a b");
    }

    #[test]
    fn layout_bits() {
        let l = Layout::scan("<a><b/><c/><b/><d>t<e/></d></a>", false);
        // element numbers: a0 b1 c2 b3 d4 e5
        assert!(l.needs_index(1) && l.needs_index(3));
        assert!(!l.needs_index(2) && !l.needs_index(4));
        assert!(l.mixed(4) && !l.mixed(0));
        assert_eq!(l.roots, 1);
    }

    #[test]
    fn numeric_and_regex_values() {
        let none: Vec<Matcher> = Vec::new();
        let mut numeric = NumFilter::default();
        numeric.above(40.0, false);
        let s = Searcher { numeric, ..searcher(&none) };
        let src = "<c><p>39.95</p><p>45.00</p><p>abc</p><q n='41'/></c>";
        assert_eq!(run(&s, src), ["/c/p[2] = 45.00", "/c/q/@n = 41"]);
        let names = Searcher { scope: Scope::Names, ..s };
        assert!(run(&names, src).is_empty(), "numbers never match names");
        let re = vec![Matcher::regex(r"\d+\.\d\d", false, true).unwrap()];
        assert_eq!(run(&searcher(&re), src), ["/c/p[1] = 39.95", "/c/p[2] = 45.00"]);
    }

    #[test]
    fn html_mode() {
        let m = exact("Two");
        let s = Searcher { html: true, ..searcher(&m) };
        let src = "<!doctype html><html><body><UL id=nav><li>One<li>Two</UL><p>Hi<br>x</body></html>";
        assert_eq!(run(&s, src), ["/html/body/UL/li[2] = Two"]);
        let mut c = Collect { out: Vec::new(), limit: usize::MAX };
        let ids = Searcher { want_ids: true, ..s };
        struct Css(Vec<String>);
        impl Sink for Css {
            fn doc_start(&mut self, _: usize) {}
            fn hit(&mut self, hit: &Hit) -> bool {
                let style = Style { local_names: false, lowercase: true };
                self.0.push(crate::path::css(hit.path, style, false, None));
                true
            }
        }
        let mut css = Css(Vec::new());
        ids.scan(src, Some(&Layout::scan(src, true)), &mut css).unwrap();
        assert_eq!(css.0, ["#nav > li:nth-of-type(2)"]);
        // the same input is not well-formed XML
        assert!(searcher(&m).scan(src, None, &mut c).is_err());
    }
}
