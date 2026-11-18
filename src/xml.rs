//! Zero-copy XML pull parser.
//!
//! [`Reader::next`] returns one [`Event`] at a time. Element names and text
//! without entities are borrowed straight from the input; text, entities and
//! CDATA sections that form one XPath text node are merged into one event.
//! The parser is iterative, so arbitrarily deep documents cannot overflow the
//! stack. Several root elements in one input (e.g. one XML document per line)
//! are read one after another.
//!
//! With [`Reader::html`] set, the reader accepts real-world HTML instead: void
//! elements (`<br>`), implied end tags (`<li>`, `<p>`, `<td>`), unquoted and
//! valueless attributes, raw `<script>`/`<style>` text, case-insensitive tags
//! and stray end tags. HTML input never produces a parse error.

use std::borrow::Cow;
use std::collections::VecDeque;
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub msg: String,
    pub line: usize,
    pub col: usize,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid XML at line {}, column {}: {}", self.line, self.col, self.msg)
    }
}

impl std::error::Error for ParseError {}

pub type PResult<T> = Result<T, ParseError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event<'a> {
    /// An element starts. `attrs` is the raw attribute source (already
    /// validated); iterate it with [`attributes`]. Empty elements (`<a/>`)
    /// are followed by an [`Event::End`] like any other element.
    Start {
        name: &'a str,
        attrs: &'a str,
    },
    /// The innermost open element ends.
    End,
    /// One text node: character data and CDATA sections, entities decoded
    /// (raw source when [`Reader::decode`] is off).
    Text(Cow<'a, str>),
    /// A comment inside an element (top-level comments are skipped).
    Comment(&'a str),
    Eof,
}

const LO: u64 = 0x0101_0101_0101_0101;
const HI: u64 = 0x8080_8080_8080_8080;

/// Index of the first `needle` byte at or after `from` (8 bytes at a time).
#[inline]
fn find_byte(b: &[u8], mut i: usize, needle: u8) -> usize {
    let pat = LO * u64::from(needle);
    while i + 8 <= b.len() {
        let w = u64::from_le_bytes(b[i..i + 8].try_into().unwrap()) ^ pat;
        if w.wrapping_sub(LO) & !w & HI != 0 {
            break;
        }
        i += 8;
    }
    while i < b.len() && b[i] != needle {
        i += 1;
    }
    i
}

#[inline]
fn is_name_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b':' | b'-' | b'.') || b >= 0x80
}

/// HTML elements that never have content or an end tag.
const VOID: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "keygen", "link", "meta", "param", "source", "track",
    "wbr",
];
/// HTML elements whose content is raw text up to their end tag.
const RAW_TEXT: &[&str] = &["script", "style", "xmp", "iframe", "noembed", "noframes"];
/// HTML elements whose content is text with entities, up to their end tag.
const RCDATA: &[&str] = &["textarea", "title"];
/// Start tags that close an open `<p>`.
const CLOSES_P: &[&str] = &[
    "address",
    "article",
    "aside",
    "blockquote",
    "details",
    "dialog",
    "div",
    "dl",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "form",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "header",
    "hgroup",
    "hr",
    "main",
    "menu",
    "nav",
    "ol",
    "p",
    "pre",
    "section",
    "table",
    "ul",
];
/// Elements that belong in `<head>` when they come before any body content.
const HEAD_CONTENT: &[&str] = &["base", "link", "meta", "noscript", "script", "style", "template", "title"];
/// Inline elements an implied `</p>` may close along the way.
const INLINE: &[&str] = &[
    "a", "abbr", "b", "bdi", "bdo", "cite", "code", "data", "dfn", "em", "font", "i", "kbd", "mark", "q", "s", "samp",
    "small", "span", "strong", "sub", "sup", "time", "u", "var",
];

fn is_one_of(name: &str, set: &[&str]) -> bool {
    set.iter().any(|s| s.eq_ignore_ascii_case(name))
}

pub struct Reader<'a> {
    text: &'a str,
    bytes: &'a [u8],
    pos: usize,
    open: Vec<&'a str>,
    /// Events produced ahead of time (end tags implied by HTML rules, the end
    /// of an empty element, raw text).
    queue: VecDeque<Event<'a>>,
    /// Decode entities and CDATA in text events. When off, text events carry
    /// the raw source span (enough to tell whitespace from content).
    pub decode: bool,
    /// Parse leniently as HTML.
    pub html: bool,
    /// HTML: `<html>`, `<head>` and `<body>` exist already (given or implied).
    html_open: bool,
    head_seen: bool,
    body_seen: bool,
}

impl<'a> Reader<'a> {
    pub fn new(src: &'a str) -> Self {
        let text = src.strip_prefix('\u{feff}').unwrap_or(src);
        Reader {
            text,
            bytes: text.as_bytes(),
            pos: 0,
            open: Vec::new(),
            queue: VecDeque::new(),
            decode: true,
            html: false,
            html_open: false,
            head_seen: false,
            body_seen: false,
        }
    }

    #[cold]
    fn err<T>(&self, msg: impl Into<String>) -> PResult<T> {
        let upto = &self.text[..self.pos.min(self.text.len())];
        let line = upto.matches('\n').count() + 1;
        let line_start = upto.rfind('\n').map_or(0, |i| i + 1);
        let col = upto[line_start..].chars().count() + 1;
        Err(ParseError { msg: msg.into(), line, col })
    }

    #[inline]
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    #[inline]
    fn starts_with(&self, s: &str) -> bool {
        self.bytes[self.pos..].starts_with(s.as_bytes())
    }

    fn skip_ws(&mut self) -> bool {
        let start = self.pos;
        while let Some(b' ' | b'\t' | b'\n' | b'\r') = self.peek() {
            self.pos += 1;
        }
        self.pos > start
    }

    /// Advance past the next `end` marker, returning the text before it.
    fn until(&mut self, end: &str, what: &str) -> PResult<&'a str> {
        match self.text[self.pos..].find(end) {
            Some(i) => {
                let s = &self.text[self.pos..self.pos + i];
                self.pos += i + end.len();
                Ok(s)
            }
            None => self.err(format!("unterminated {what}")),
        }
    }

    pub fn next(&mut self) -> PResult<Event<'a>> {
        if let Some(ev) = self.queue.pop_front() {
            return Ok(ev);
        }
        if self.html {
            return Ok(self.next_html());
        }
        loop {
            if self.open.is_empty() {
                // Prolog, between root elements, or after the last one.
                self.skip_ws();
                if self.pos >= self.bytes.len() {
                    return Ok(Event::Eof);
                }
                if self.peek() != Some(b'<') {
                    return self.err("text outside the root element");
                }
                if self.starts_with("<?") {
                    self.pos += 2;
                    self.until("?>", "processing instruction")?;
                } else if self.starts_with("<!--") {
                    self.pos += 4;
                    self.until("-->", "comment")?;
                } else if self.starts_with("<!DOCTYPE") {
                    self.doctype()?;
                } else if self.starts_with("</") {
                    return self.err("closing tag without a matching opening tag");
                } else if self.starts_with("<!") {
                    return self.err("unexpected markup outside the root element");
                } else {
                    return self.start_tag();
                }
                continue;
            }
            return match self.peek() {
                None => {
                    let name = self.open.last().copied().unwrap_or_default();
                    self.err(format!("unexpected end of input: <{name}> is not closed"))
                }
                Some(b'<') if self.starts_with("</") => self.end_tag(),
                Some(b'<') if self.starts_with("<!--") => {
                    self.pos += 4;
                    Ok(Event::Comment(self.until("-->", "comment")?))
                }
                Some(b'<') if self.starts_with("<![CDATA[") => self.text_node(),
                Some(b'<') if self.starts_with("<?") => {
                    self.pos += 2;
                    self.until("?>", "processing instruction")?;
                    continue;
                }
                Some(b'<') if self.starts_with("<!") => self.err("unexpected markup"),
                Some(b'<') => self.start_tag(),
                Some(_) => self.text_node(),
            };
        }
    }

    fn doctype(&mut self) -> PResult<()> {
        self.pos += "<!DOCTYPE".len();
        let mut bracket = 0usize;
        let mut quote: Option<u8> = None;
        while let Some(b) = self.peek() {
            self.pos += 1;
            match (quote, b) {
                (Some(q), _) if b == q => quote = None,
                (Some(_), _) => {}
                (None, b'"' | b'\'') => quote = Some(b),
                (None, b'[') => bracket += 1,
                (None, b']') => bracket = bracket.saturating_sub(1),
                (None, b'>') if bracket == 0 => return Ok(()),
                _ => {}
            }
        }
        self.err("unterminated DOCTYPE")
    }

    fn name(&mut self) -> PResult<&'a str> {
        let start = self.pos;
        while let Some(b) = self.peek() {
            if !is_name_byte(b) {
                break;
            }
            self.pos += 1;
        }
        let name = &self.text[start..self.pos];
        match name.as_bytes().first() {
            None => self.err("expected a name"),
            Some(b) if b.is_ascii_digit() || matches!(b, b'-' | b'.') => {
                self.pos = start;
                self.err(format!("invalid name '{name}'"))
            }
            _ => Ok(name),
        }
    }

    fn start_tag(&mut self) -> PResult<Event<'a>> {
        self.pos += 1;
        let name = self.name()?;
        let attrs_start = self.pos;
        loop {
            let had_ws = self.skip_ws();
            match self.peek() {
                Some(b'>') => {
                    let attrs = &self.text[attrs_start..self.pos];
                    self.pos += 1;
                    self.open.push(name);
                    return Ok(Event::Start { name, attrs });
                }
                Some(b'/') => {
                    if self.bytes.get(self.pos + 1) != Some(&b'>') {
                        return self.err("expected '>' after '/'");
                    }
                    let attrs = &self.text[attrs_start..self.pos];
                    self.pos += 2;
                    self.queue.push_back(Event::End);
                    return Ok(Event::Start { name, attrs });
                }
                None => return self.err(format!("unterminated start tag <{name}>")),
                Some(_) => {
                    if !had_ws {
                        return self.err("expected whitespace before attribute");
                    }
                    self.name()?;
                    self.skip_ws();
                    if self.peek() != Some(b'=') {
                        return self.err("expected '=' after attribute name");
                    }
                    self.pos += 1;
                    self.skip_ws();
                    let quote = match self.peek() {
                        Some(q @ (b'"' | b'\'')) => q,
                        _ => return self.err("attribute value must be quoted"),
                    };
                    self.pos += 1;
                    let end = find_byte(self.bytes, self.pos, quote);
                    if end >= self.bytes.len() {
                        return self.err("unterminated attribute value");
                    }
                    if let Some(i) = self.bytes[self.pos..end].iter().position(|&b| b == b'<') {
                        self.pos += i;
                        return self.err("'<' is not allowed in attribute values");
                    }
                    self.pos = end + 1;
                }
            }
        }
    }

    fn end_tag(&mut self) -> PResult<Event<'a>> {
        let at = self.pos;
        self.pos += 2;
        let name = self.name()?;
        self.skip_ws();
        if self.peek() != Some(b'>') {
            return self.err("expected '>' in closing tag");
        }
        let open = self.open.last().copied().unwrap_or_default();
        if open != name {
            self.pos = at;
            return self.err(format!("mismatched closing tag: expected </{open}>, found </{name}>"));
        }
        self.pos += 1;
        self.open.pop();
        Ok(Event::End)
    }

    /// Advance past the next `>` (or to the end): the HTML "bogus comment".
    fn skip_past_gt(&mut self) {
        self.pos = (find_byte(self.bytes, self.pos, b'>') + 1).min(self.bytes.len());
    }

    /// Queue an end event for every open element above index `keep`.
    fn close_to(&mut self, keep: usize) {
        for _ in keep..self.open.len() {
            self.queue.push_back(Event::End);
        }
        self.open.truncate(keep);
    }

    /// Nearest open element (from the innermost outwards) named one of
    /// `targets`, unless one of `stop` comes first. Returns its index.
    fn find_open(&self, targets: &[&str], stop: &[&str]) -> Option<usize> {
        for (i, name) in self.open.iter().enumerate().rev() {
            if is_one_of(name, targets) {
                return Some(i);
            }
            if is_one_of(name, stop) {
                return None;
            }
        }
        None
    }

    /// End tags implied by starting an element named `name` (HTML rules, simplified).
    fn implicit_close(&mut self, name: &str) {
        let target = if is_one_of(name, &["li"]) {
            self.find_open(&["li"], &["ul", "ol", "menu"])
        } else if is_one_of(name, &["dt", "dd"]) {
            self.find_open(&["dt", "dd"], &["dl"])
        } else if is_one_of(name, &["tr"]) {
            self.find_open(&["tr"], &["table", "thead", "tbody", "tfoot"])
        } else if is_one_of(name, &["td", "th"]) {
            self.find_open(&["td", "th"], &["tr", "table"])
        } else if is_one_of(name, &["thead", "tbody", "tfoot"]) {
            self.find_open(&["thead", "tbody", "tfoot"], &["table"])
        } else if is_one_of(name, &["option"]) {
            self.find_open(&["option"], &["select", "datalist", "optgroup"])
        } else if is_one_of(name, &["optgroup"]) {
            self.find_open(&["optgroup", "option"], &["select"])
        } else if is_one_of(name, CLOSES_P) {
            // close an open <p>, along with inline elements still open inside it
            let mut found = None;
            for (i, open) in self.open.iter().enumerate().rev() {
                if open.eq_ignore_ascii_case("p") {
                    found = Some(i);
                    break;
                }
                if !is_one_of(open, INLINE) {
                    break;
                }
            }
            found
        } else {
            None
        };
        if let Some(i) = target {
            self.close_to(i);
        }
    }

    /// Open an element the HTML parser implies (`<html>`, `<body>`, `<tbody>`, ...).
    fn imply(&mut self, name: &'static str) {
        self.queue.push_back(Event::Start { name, attrs: "" });
        self.open.push(name);
    }

    fn ensure_html(&mut self) {
        if !self.html_open {
            self.html_open = true;
            if self.open.is_empty() {
                self.imply("html");
            }
        }
    }

    /// Make sure body content has a `<body>` to go into, like a browser does.
    fn ensure_body(&mut self) {
        if self.body_seen {
            return;
        }
        self.ensure_html();
        if let Some(i) = self.open.iter().rposition(|o| o.eq_ignore_ascii_case("head")) {
            self.close_to(i);
        }
        self.head_seen = true;
        self.body_seen = true;
        self.imply("body");
    }

    /// Browser tree fix-ups before a start tag: the html/head/body skeleton,
    /// and `<tbody>`/`<tr>` around table rows and cells. Returns `false` when
    /// the tag itself must be dropped (a second `<html>`, `<head>` or `<body>`).
    fn fix_up(&mut self, name: &str) -> bool {
        if name.eq_ignore_ascii_case("html") {
            if self.html_open {
                return false;
            }
            self.html_open = true;
            return true;
        }
        if name.eq_ignore_ascii_case("head") {
            if self.head_seen || self.body_seen {
                return false;
            }
            self.ensure_html();
            self.head_seen = true;
            return true;
        }
        if name.eq_ignore_ascii_case("body") {
            if self.body_seen {
                return false;
            }
            self.ensure_html();
            if let Some(i) = self.open.iter().rposition(|o| o.eq_ignore_ascii_case("head")) {
                self.close_to(i);
            }
            self.head_seen = true;
            self.body_seen = true;
            return true;
        }
        let in_head = self.open.last().is_some_and(|o| o.eq_ignore_ascii_case("head"));
        if is_one_of(name, HEAD_CONTENT) && !self.body_seen && (in_head || !self.head_seen) {
            self.ensure_html();
            if !in_head {
                self.head_seen = true;
                self.imply("head");
            }
            return true;
        }
        self.ensure_body();
        let top = self.open.last().copied().unwrap_or_default();
        if is_one_of(name, &["tr"]) && top.eq_ignore_ascii_case("table") {
            self.imply("tbody");
        } else if is_one_of(name, &["td", "th"]) {
            if top.eq_ignore_ascii_case("table") {
                self.imply("tbody");
                self.imply("tr");
            } else if is_one_of(top, &["tbody", "thead", "tfoot"]) {
                self.imply("tr");
            }
        }
        true
    }

    fn next_html(&mut self) -> Event<'a> {
        loop {
            if let Some(ev) = self.queue.pop_front() {
                return ev;
            }
            if self.pos >= self.bytes.len() {
                if self.open.is_empty() {
                    return Event::Eof;
                }
                self.close_to(0); // unclosed elements end with the input
                continue;
            }
            if self.peek() != Some(b'<') || !self.markup_at(self.pos) {
                let end = self.next_markup(self.pos + 1).min(self.bytes.len());
                let blank = self.text[self.pos..end].trim().is_empty();
                let in_body_part = self.body_seen && !self.open.is_empty();
                if !in_body_part && !self.open.iter().any(|o| is_one_of(o, &["title", "textarea", "script", "style"])) {
                    if blank || (self.body_seen && self.open.is_empty()) {
                        // whitespace around the html/head skeleton, or text with
                        // nowhere left to go: not content
                        self.pos = end;
                        continue;
                    }
                    self.ensure_body();
                    continue; // emit the implied tags first, then come back to the text
                }
                return self.text_html();
            }
            if self.starts_with("<!--") {
                self.pos += 4;
                let body = match self.text[self.pos..].find("-->") {
                    Some(i) => {
                        let c = &self.text[self.pos..self.pos + i];
                        self.pos += i + 3;
                        c
                    }
                    None => {
                        let c = &self.text[self.pos..];
                        self.pos = self.bytes.len();
                        c
                    }
                };
                if !self.open.is_empty() {
                    return Event::Comment(body);
                }
                continue;
            }
            if self.starts_with("<![CDATA[") && !self.open.is_empty() {
                return self.text_html();
            }
            if self.starts_with("<!") || self.starts_with("<?") {
                self.skip_past_gt(); // doctype, processing instruction, bogus comment
                continue;
            }
            if self.starts_with("</") {
                self.pos += 2;
                let start = self.pos;
                while self.peek().is_some_and(|b| !b.is_ascii_whitespace() && b != b'>' && b != b'/') {
                    self.pos += 1;
                }
                let name = &self.text[start..self.pos];
                self.skip_past_gt();
                // Browsers keep <body> and <html> open to the end of the input
                // (content after </body> still lands in the body).
                if name.eq_ignore_ascii_case("body") || name.eq_ignore_ascii_case("html") {
                    continue;
                }
                // close up to the matching open element; ignore stray end tags
                if let Some(i) = self.open.iter().rposition(|o| o.eq_ignore_ascii_case(name)) {
                    self.close_to(i);
                }
                continue;
            }
            return self.start_tag_html();
        }
    }

    /// Does `<` at `i` start markup (rather than being a literal `<`)?
    fn markup_at(&self, i: usize) -> bool {
        matches!(self.bytes.get(i + 1), Some(b) if b.is_ascii_alphabetic() || matches!(b, b'/' | b'!' | b'?'))
    }

    /// Position of the next `<` that starts markup, at or after `from`.
    fn next_markup(&self, mut from: usize) -> usize {
        loop {
            let i = find_byte(self.bytes, from, b'<');
            if i >= self.bytes.len() || self.markup_at(i) {
                return i;
            }
            from = i + 1;
        }
    }

    fn text_html(&mut self) -> Event<'a> {
        let start = self.pos;
        let mut out: Cow<'a, str> = Cow::Borrowed("");
        loop {
            if self.starts_with("<![CDATA[") {
                self.pos += "<![CDATA[".len();
                let end = self.text[self.pos..].find("]]>").map_or(self.bytes.len(), |i| self.pos + i);
                if self.decode {
                    append(&mut out, Cow::Borrowed(&self.text[self.pos..end]));
                }
                self.pos = (end + 3).min(self.bytes.len());
                continue;
            }
            if self.pos >= self.bytes.len() || (self.peek() == Some(b'<') && self.markup_at(self.pos)) {
                break;
            }
            let end = self.next_markup(self.pos + 1).min(self.bytes.len());
            if self.decode {
                append(&mut out, decode(&self.text[self.pos..end], false));
            }
            self.pos = end;
        }
        if !self.decode {
            out = Cow::Borrowed(&self.text[start..self.pos]);
        }
        Event::Text(out)
    }

    fn start_tag_html(&mut self) -> Event<'a> {
        self.pos += 1;
        let name_start = self.pos;
        while self.peek().is_some_and(|b| !b.is_ascii_whitespace() && b != b'>' && b != b'/') {
            self.pos += 1;
        }
        let name = &self.text[name_start..self.pos];
        let attrs_start = self.pos;
        let mut attrs_end = self.bytes.len();
        let mut self_closing = false;
        while let Some(b) = self.peek() {
            match b {
                b'>' => {
                    attrs_end = self.pos;
                    self.pos += 1;
                    break;
                }
                b'/' if self.bytes.get(self.pos + 1) == Some(&b'>') => {
                    attrs_end = self.pos;
                    self.pos += 2;
                    self_closing = true;
                    break;
                }
                b'"' | b'\'' => {
                    self.pos = (find_byte(self.bytes, self.pos + 1, b) + 1).min(self.bytes.len());
                }
                _ => self.pos += 1,
            }
        }
        let attrs = &self.text[attrs_start..attrs_end.min(self.bytes.len())];
        self.implicit_close(name);
        if !self.fix_up(name) {
            return self.queue.pop_front().unwrap_or_else(|| self.next_html());
        }
        self.queue.push_back(Event::Start { name, attrs });
        self.open.push(name);
        if self_closing || is_one_of(name, VOID) {
            self.close_to(self.open.len() - 1);
        } else if is_one_of(name, RAW_TEXT) || is_one_of(name, RCDATA) {
            // content runs up to </name>
            let mut from = self.pos;
            let end = loop {
                let i = find_byte(self.bytes, from, b'<');
                if i >= self.bytes.len() {
                    break self.bytes.len();
                }
                let tag_end = i + 2 + name.len();
                if self.bytes.get(i + 1) == Some(&b'/')
                    && self.bytes.len() >= tag_end
                    && self.bytes[i + 2..tag_end].eq_ignore_ascii_case(name.as_bytes())
                    && self.bytes.get(tag_end).is_none_or(|b| b.is_ascii_whitespace() || matches!(b, b'>' | b'/'))
                {
                    break i;
                }
                from = i + 1;
            };
            let body = &self.text[self.pos..end];
            self.pos = end;
            if !body.is_empty() {
                let text =
                    if self.decode && is_one_of(name, RCDATA) { decode(body, false) } else { Cow::Borrowed(body) };
                self.queue.push_back(Event::Text(text));
            }
        }
        self.queue.pop_front().unwrap_or(Event::Eof)
    }

    /// One text node: character data and CDATA sections up to the next markup.
    fn text_node(&mut self) -> PResult<Event<'a>> {
        let span_start = self.pos;
        let mut out: Cow<'a, str> = Cow::Borrowed("");
        loop {
            if self.starts_with("<![CDATA[") {
                self.pos += "<![CDATA[".len();
                let seg = self.until("]]>", "CDATA section")?;
                if self.decode {
                    append(&mut out, Cow::Borrowed(seg));
                }
            } else if self.pos >= self.bytes.len() || self.peek() == Some(b'<') {
                break;
            } else {
                let end = find_byte(self.bytes, self.pos, b'<');
                let seg = &self.text[self.pos..end];
                self.pos = end;
                if self.decode {
                    append(&mut out, decode(seg, false));
                }
            }
        }
        if !self.decode {
            out = Cow::Borrowed(&self.text[span_start..self.pos]);
        }
        Ok(Event::Text(out))
    }
}

fn append<'a>(out: &mut Cow<'a, str>, seg: Cow<'a, str>) {
    if out.is_empty() {
        *out = seg;
    } else {
        out.to_mut().push_str(&seg);
    }
}

fn resolve_entity(ent: &str) -> Option<char> {
    match ent {
        "lt" => Some('<'),
        "gt" => Some('>'),
        "amp" => Some('&'),
        "apos" => Some('\''),
        "quot" => Some('"'),
        "nbsp" => Some('\u{a0}'),
        "copy" => Some('©'),
        "reg" => Some('®'),
        "trade" => Some('™'),
        "hellip" => Some('…'),
        "mdash" => Some('—'),
        "ndash" => Some('–'),
        "lsquo" => Some('‘'),
        "rsquo" => Some('’'),
        "ldquo" => Some('“'),
        "rdquo" => Some('”'),
        "laquo" => Some('«'),
        "raquo" => Some('»'),
        "bull" => Some('•'),
        "middot" => Some('·'),
        "deg" => Some('°'),
        "times" => Some('×'),
        "divide" => Some('÷'),
        "euro" => Some('€'),
        "pound" => Some('£'),
        "yen" => Some('¥'),
        "cent" => Some('¢'),
        "sect" => Some('§'),
        "para" => Some('¶'),
        "shy" => Some('\u{ad}'),
        _ => {
            let num = ent.strip_prefix('#')?;
            let code = match num.strip_prefix('x').or_else(|| num.strip_prefix('X')) {
                Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                None => num.parse().ok()?,
            };
            char::from_u32(code)
        }
    }
}

/// Decode entity and character references. Unknown entities (e.g. ones
/// declared in a DTD) are kept literally. In attribute values, literal tabs
/// and newlines become spaces (XML attribute-value normalisation).
pub fn decode(seg: &str, attr: bool) -> Cow<'_, str> {
    let special = |b: u8| b == b'&' || (attr && matches!(b, b'\t' | b'\n' | b'\r'));
    let bytes = seg.as_bytes();
    let Some(first) = bytes.iter().position(|&b| special(b)) else {
        return Cow::Borrowed(seg);
    };
    let mut out = String::with_capacity(seg.len());
    out.push_str(&seg[..first]);
    let mut i = first;
    while i < bytes.len() {
        let run = bytes[i..].iter().position(|&b| special(b)).map_or(bytes.len(), |n| i + n);
        out.push_str(&seg[i..run]);
        i = run;
        if i >= bytes.len() {
            break;
        }
        match bytes[i] {
            b'&' => {
                let mut end = (i + 12).min(seg.len());
                while !seg.is_char_boundary(end) {
                    end -= 1;
                }
                let window = &seg[i + 1..end];
                match window.find(';').and_then(|n| resolve_entity(&window[..n]).map(|c| (n, c))) {
                    Some((n, c)) => {
                        out.push(c);
                        i += n + 2;
                    }
                    None => {
                        out.push('&');
                        i += 1;
                    }
                }
            }
            b'\r' if bytes.get(i + 1) == Some(&b'\n') => i += 1, // CRLF counts once
            _ => {
                out.push(' ');
                i += 1;
            }
        }
    }
    Cow::Owned(out)
}

/// Iterate `(name, raw value)` pairs of a start tag's attribute source.
/// Accepts XML syntax and HTML's unquoted (`a=b`) and valueless (`disabled`)
/// attributes; a valueless attribute has the value `""`.
pub fn attributes(raw: &str) -> impl Iterator<Item = (&str, &str)> {
    let mut rest = raw;
    std::iter::from_fn(move || {
        rest = rest.trim_start_matches(|c: char| c.is_ascii_whitespace() || c == '/');
        if rest.is_empty() {
            return None;
        }
        let name_end = rest.find(|c: char| c.is_ascii_whitespace() || c == '=' || c == '/').unwrap_or(rest.len());
        let name = &rest[..name_end];
        let after = rest[name_end..].trim_start_matches(|c: char| c.is_ascii_whitespace());
        let Some(value_src) = after.strip_prefix('=') else {
            rest = after;
            return Some((name, ""));
        };
        let value_src = value_src.trim_start_matches(|c: char| c.is_ascii_whitespace());
        let (value, remainder) = match value_src.chars().next() {
            Some(q @ ('"' | '\'')) => {
                let body = &value_src[1..];
                match body.find(q) {
                    Some(end) => (&body[..end], &body[end + 1..]),
                    None => (body, ""),
                }
            }
            _ => {
                let end = value_src.find(|c: char| c.is_ascii_whitespace()).unwrap_or(value_src.len());
                (&value_src[..end], &value_src[end..])
            }
        };
        rest = remainder;
        Some((name, value))
    })
}

/// Is `s` exactly one well-formed XML document (used by `--decode`)?
pub fn is_document(s: &str) -> bool {
    if !s.trim_start().starts_with('<') {
        return false;
    }
    let mut r = Reader::new(s);
    r.decode = false;
    let (mut roots, mut depth) = (0, 0usize);
    loop {
        match r.next() {
            Ok(Event::Start { .. }) => {
                if depth == 0 {
                    roots += 1;
                    if roots > 1 {
                        return false;
                    }
                }
                depth += 1;
            }
            Ok(Event::End) => depth -= 1,
            Ok(Event::Eof) => return roots == 1,
            Ok(_) => {}
            Err(_) => return false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn events(src: &str) -> PResult<Vec<String>> {
        let mut r = Reader::new(src);
        let mut out = Vec::new();
        loop {
            match r.next()? {
                Event::Start { name, attrs } => out.push(format!("<{name}{attrs}>")),
                Event::End => out.push("</>".into()),
                Event::Text(t) => out.push(format!("T({t})")),
                Event::Comment(c) => out.push(format!("C({c})")),
                Event::Eof => return Ok(out),
            }
        }
    }

    #[test]
    fn basic_events() {
        let src = r#"<?xml version="1.0"?><!DOCTYPE a [<!ENTITY x "y>">]><!--top--><a k="v"><b/>hi<!--c--></a>"#;
        assert_eq!(events(src).unwrap(), [r#"<a k="v">"#, "<b>", "</>", "T(hi)", "C(c)", "</>"]);
    }

    #[test]
    fn text_merges_entities_and_cdata() {
        let ev = events("<a>x &amp; &lt;y&gt; &#65;&#x42; <![CDATA[<raw> & ]]>tail &unknown;</a>").unwrap();
        assert_eq!(ev[1], "T(x & <y> AB <raw> & tail &unknown;)");
        let mut r = Reader::new("<a>plain</a>");
        r.next().unwrap();
        assert!(matches!(r.next().unwrap(), Event::Text(Cow::Borrowed("plain"))));
    }

    #[test]
    fn raw_text_when_not_decoding() {
        let mut r = Reader::new("<a>x &amp; <![CDATA[y]]></a>");
        r.decode = false;
        r.next().unwrap();
        assert_eq!(r.next().unwrap(), Event::Text(Cow::Borrowed("x &amp; <![CDATA[y]]>")));
    }

    #[test]
    fn multiple_roots() {
        assert_eq!(events("<a/>\n<b>1</b>\n").unwrap(), ["<a>", "</>", "<b>", "T(1)", "</>"]);
    }

    #[test]
    fn attributes_iterate_and_normalise() {
        let attrs: Vec<_> = attributes(r#" id="1" name = 'O&apos;Neil'  x="a&#10;b	c""#).collect();
        assert_eq!(attrs, [("id", "1"), ("name", "O&apos;Neil"), ("x", "a&#10;b\tc")]);
        assert_eq!(decode("O&apos;Neil", true), "O'Neil");
        assert_eq!(decode("a&#10;b\tc\r\nd", true), "a\nb c d");
        assert!(matches!(decode("plain", true), Cow::Borrowed(_)));
        // an '&' followed by multi-byte characters must not split one
        assert_eq!(decode("&ééééééé;", false), "&ééééééé;");
        assert_eq!(decode("a&€", false), "a&€");
    }

    #[test]
    fn errors_have_positions() {
        let e = events("<a>\n  <b></c>\n</a>").unwrap_err();
        assert!(e.msg.contains("expected </b>, found </c>"), "{}", e.msg);
        assert_eq!((e.line, e.col), (2, 6));
        for bad in [
            "<a>",
            "<a><b></a>",
            "</a>",
            "text",
            "<a x=1/>",
            "<a x='1' y/>",
            "<a x=\"<\"/>",
            "<a><!-- x </a>",
            "<1a/>",
            "<a/>junk",
        ] {
            assert!(events(bad).is_err(), "should reject {bad:?}");
        }
    }

    #[test]
    fn find_byte_every_offset() {
        for len in 0..40 {
            let s = format!("{}<", "x".repeat(len));
            assert_eq!(find_byte(s.as_bytes(), 0, b'<'), len);
            let u = format!("{}<", "é".repeat(len));
            assert_eq!(find_byte(u.as_bytes(), 0, b'<'), u.len() - 1);
        }
        assert_eq!(find_byte(b"abc", 0, b'<'), 3);
    }

    fn html_events(src: &str) -> Vec<String> {
        let mut r = Reader::new(src);
        r.html = true;
        let mut out = Vec::new();
        loop {
            match r.next().expect("html never fails") {
                Event::Start { name, .. } => out.push(format!("<{name}>")),
                Event::End => out.push("</>".into()),
                Event::Text(t) => out.push(format!("T({t})")),
                Event::Comment(c) => out.push(format!("C({c})")),
                Event::Eof => return out,
            }
        }
    }

    #[test]
    fn html_void_and_implied_end_tags() {
        let body = |inner: &[&str]| -> Vec<String> {
            let mut v: Vec<String> = vec!["<html>".into(), "<body>".into()];
            v.extend(inner.iter().map(|s| s.to_string()));
            v.extend(["</>".to_string(), "</>".to_string()]);
            v
        };
        assert_eq!(
            html_events("<!doctype html><ul><li>a<li>b</ul><p>x<br>y<p>z"),
            body(&[
                "<ul>", "<li>", "T(a)", "</>", "<li>", "T(b)", "</>", "</>", "<p>", "T(x)", "<br>", "</>", "T(y)",
                "</>", "<p>", "T(z)", "</>"
            ])
        );
        // rows get an implied <tbody>, like in a browser
        assert_eq!(
            html_events("<table><tr><td>1<td>2<tr><td>3</table>"),
            body(&[
                "<table>", "<tbody>", "<tr>", "<td>", "T(1)", "</>", "<td>", "T(2)", "</>", "</>", "<tr>", "<td>",
                "T(3)", "</>", "</>", "</>", "</>"
            ])
        );
        // <div> closes an open <p> along with inline elements inside it
        assert_eq!(
            html_events("<p><b>x<div>y</div>"),
            body(&["<p>", "<b>", "T(x)", "</>", "</>", "<div>", "T(y)", "</>"])
        );
        // head content goes into an implied <head>; body content into <body>
        assert_eq!(
            html_events("<title>T</title><meta charset=utf-8><h1>Hi</h1>"),
            [
                "<html>", "<head>", "<title>", "T(T)", "</>", "<meta>", "</>", "</>", "<body>", "<h1>", "T(Hi)", "</>",
                "</>", "</>"
            ]
        );
        // an explicit skeleton is used as is; duplicates are dropped
        assert_eq!(
            html_events("<html><head></head><body><p>x</p><body></body></html>"),
            ["<html>", "<head>", "</>", "<body>", "<p>", "T(x)", "</>", "</>", "</>"]
        );
        // content after </body></html> still belongs to the body (browser rule)
        assert_eq!(
            html_events("<html><body><p>x</p></body></html>tail<p>y"),
            ["<html>", "<body>", "<p>", "T(x)", "</>", "T(tail)", "<p>", "T(y)", "</>", "</>", "</>"]
        );
    }

    #[test]
    fn html_raw_text_stray_tags_and_case() {
        let strip = |v: Vec<String>| -> Vec<String> { v[2..v.len() - 2].to_vec() }; // drop html/body
        assert_eq!(
            strip(html_events("<body><DIV><script>if (a<b) x='</div>';</script></span>t &copy; 1 < 2</div>")),
            ["<DIV>", "<script>", "T(if (a<b) x='</div>';)", "</>", "T(t © 1 < 2)", "</>"],
            "script text runs to </script>; the stray </span> is ignored"
        );
        assert_eq!(strip(html_events("<p>1 < 2 &amp; 3</P>")), ["<p>", "T(1 < 2 & 3)", "</>"]);
        assert_eq!(
            html_events("<title>a &amp; <b></title>"),
            ["<html>", "<head>", "<title>", "T(a & <b>)", "</>", "</>", "</>"]
        );
        assert_eq!(strip(html_events("stray</x><a>t</a>tail")), ["T(stray)", "<a>", "T(t)", "</>", "T(tail)"]);
    }

    #[test]
    fn html_attributes() {
        let attrs: Vec<_> = attributes(r#" href=/x?a=1&b=2 disabled class="c d" data-x='y' /"#).collect();
        assert_eq!(attrs, [("href", "/x?a=1&b=2"), ("disabled", ""), ("class", "c d"), ("data-x", "y")]);
        let mut r = Reader::new("<a href='1>2' title=x>t</a>");
        r.html = true;
        assert_eq!(r.next().unwrap(), Event::Start { name: "html", attrs: "" });
        assert_eq!(r.next().unwrap(), Event::Start { name: "body", attrs: "" });
        assert_eq!(r.next().unwrap(), Event::Start { name: "a", attrs: " href='1>2' title=x" });
    }

    #[test]
    fn document_detection() {
        assert!(is_document("<order><id>1</id></order>"));
        assert!(is_document("  <?xml version='1.0'?><a/> <!-- end -->"));
        assert!(!is_document("<a/><b/>"));
        assert!(!is_document("<a>"));
        assert!(!is_document("hello"));
        assert!(!is_document("<a/> trailing"));
    }
}
