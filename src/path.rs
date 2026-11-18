//! Turning a walk position into a query: an XPath (`/catalog/book[2]/title`),
//! a CSS selector (`catalog > book:nth-of-type(2) > title`), and their
//! reorder-proof forms (`/catalog/book[author='Alice']`, `:has(...)`).

/// One step from the document root to a matched node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step<'a> {
    /// An element. `pos` is its 1-based position among same-named siblings;
    /// `indexed` says whether siblings share its name (so `[pos]` is needed);
    /// `id` is its `id` attribute, when known (CSS selectors start there);
    /// `ns` says the element is in a namespace (prefixed, under a default
    /// `xmlns`, or SVG/MathML inside HTML), so plain XPath name tests would miss it.
    Elem { name: &'a str, pos: usize, indexed: bool, id: Option<&'a str>, ns: bool },
    /// An attribute of the preceding element.
    Attr(&'a str),
    /// A text node of the preceding element. In mixed content (text next to
    /// child elements) it is addressed as `text()[pos]`; otherwise the element
    /// itself stands for its text.
    Text { pos: usize, mixed: bool },
    /// A comment of the preceding element, `comment()[pos]`.
    Comment { pos: usize },
    /// Step into XML embedded in the preceding text: `parse-xml(.)` (XPath 3.1).
    Decode,
}

/// How names are written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Style {
    /// `*[local-name()='book']` instead of `book`: matches regardless of
    /// namespace prefix or default namespace.
    pub local_names: bool,
    /// Write names in lower case (HTML, where tag names are case-insensitive).
    pub lowercase: bool,
}

fn push_case(out: &mut String, s: &str, style: Style) {
    if style.lowercase {
        out.extend(s.chars().map(|c| c.to_ascii_lowercase()));
    } else {
        out.push_str(s);
    }
}

fn push_name(out: &mut String, name: &str, ns: bool, style: Style) {
    if style.local_names || ns {
        let local = name.rsplit(':').next().unwrap_or(name);
        out.push_str("*[local-name()=");
        push_literal(out, local);
        out.push(']');
    } else {
        push_case(out, name, style);
    }
}

fn push_usize(out: &mut String, n: usize) {
    out.push_str(&n.to_string());
}

/// Append an XPath string literal for `s`.
pub fn push_literal(out: &mut String, s: &str) {
    if !s.contains('\'') {
        out.push('\'');
        out.push_str(s);
        out.push('\'');
    } else if !s.contains('"') {
        out.push('"');
        out.push_str(s);
        out.push('"');
    } else {
        // XPath 1.0 has no escapes: concat('it', "'", 's ', '"', 'x')
        out.push_str("concat(");
        for (i, part) in s.split('\'').enumerate() {
            if i > 0 {
                out.push_str(", \"'\", ");
            }
            out.push('\'');
            out.push_str(part);
            out.push('\'');
        }
        out.push(')');
    }
}

/// Render `path` as an absolute XPath. With `generalize`, positions are
/// dropped so one query reaches every same-named sibling.
pub fn format(path: &[Step], style: Style, generalize: bool) -> String {
    let mut s = String::new();
    format_into(&mut s, path, style, generalize);
    s
}

/// Like [`format`], appending to `out`.
pub fn format_into(out: &mut String, path: &[Step], style: Style, generalize: bool) {
    format_steps(out, path, style, generalize, false);
}

/// `owner_ns`: whether the element just before `path` is in a namespace
/// (so a leading attribute keeps its case).
fn format_steps(out: &mut String, path: &[Step], style: Style, generalize: bool, mut owner_ns: bool) {
    // SVG/MathML names inside HTML are case-sensitive (viewBox): keep their case.
    for step in path {
        match *step {
            Step::Elem { name, pos, indexed, ns, .. } => {
                owner_ns = ns;
                let style = if ns { Style { lowercase: false, ..style } } else { style };
                out.push('/');
                push_name(out, name, ns, style);
                if indexed && !generalize {
                    out.push('[');
                    push_usize(out, pos);
                    out.push(']');
                }
            }
            Step::Attr(name) => {
                out.push_str("/@");
                let style = if owner_ns { Style { lowercase: false, ..style } } else { style };
                // only prefixed attributes are in a namespace
                push_name(out, name, name.contains(':'), style);
            }
            Step::Text { mixed: false, .. } => {}
            Step::Text { pos, mixed: true } => {
                out.push_str("/text()");
                if !generalize {
                    out.push('[');
                    push_usize(out, pos);
                    out.push(']');
                }
            }
            Step::Comment { pos } => {
                out.push_str("/comment()");
                if !generalize {
                    out.push('[');
                    push_usize(out, pos);
                    out.push(']');
                }
            }
            Step::Decode => out.push_str("/parse-xml(.)"),
        }
    }
    if out.is_empty() {
        out.push('/');
    }
}

/// Build a query that finds the match by *content* instead of by position:
///
/// `/catalog/book[2]/author = 'Alice'`  →  `/catalog/book[author='Alice']`
///
/// The innermost element that has same-named siblings is filtered by the
/// matched value, so the query keeps working when siblings are reordered.
/// Without such an element the plain path is returned. `name_hit` means an
/// element/attribute name matched; the predicate then tests for existence.
/// `padded` means the node's text had surrounding whitespace that `value`
/// omits; the comparison then goes through `normalize-space()`.
pub fn select_query(path: &[Step], name_hit: bool, value: &str, padded: bool, style: Style) -> String {
    let Some(idx) = path.iter().rposition(|s| matches!(s, Step::Elem { indexed: true, .. })) else {
        return format(path, style, false);
    };
    let mut q = format(&path[..=idx], style, true);
    let idx_ns = matches!(path[idx], Step::Elem { ns: true, .. });
    let mut rel = String::new();
    format_steps(&mut rel, &path[idx + 1..], style, true, idx_ns);
    if rel.is_empty() {
        rel.push('/');
    }
    if rel == "/" {
        rel.clear();
    }
    let rel = rel.strip_prefix('/').unwrap_or(&rel);
    if name_hit {
        if !rel.is_empty() {
            q.push('[');
            q.push_str(rel);
            q.push(']');
        }
    } else if padded {
        // [rel[normalize-space()='v']] — ignores the surrounding whitespace.
        let collapsed = value.split_whitespace().collect::<Vec<_>>().join(" ");
        q.push('[');
        if !rel.is_empty() {
            q.push_str(rel);
            q.push('[');
        }
        q.push_str("normalize-space()=");
        push_literal(&mut q, &collapsed);
        if !rel.is_empty() {
            q.push(']');
        }
        q.push(']');
    } else {
        q.push('[');
        q.push_str(if rel.is_empty() { "." } else { rel });
        q.push('=');
        push_literal(&mut q, value);
        q.push(']');
    }
    q
}

// ---------------------------------------------------------------------------
// CSS selectors
// ---------------------------------------------------------------------------

/// Append `s` as a CSS identifier, escaping what CSS does not allow bare.
fn push_css_ident(out: &mut String, s: &str) {
    for (i, c) in s.chars().enumerate() {
        if c.is_ascii_digit() && i == 0 {
            out.push_str(&format!("\\{:x} ", c as u32));
        } else if c.is_ascii_alphanumeric() || c == '-' || c == '_' || !c.is_ascii() {
            out.push(c);
        } else {
            out.push('\\');
            out.push(c);
        }
    }
}

/// Append `s` as a double-quoted CSS string.
fn push_css_string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\a "),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// CSS type selectors match the local name in any namespace (no default
/// namespace is declared), so a prefix is simply dropped.
fn push_css_name(out: &mut String, name: &str, style: Style) {
    let local = name.rsplit(':').next().unwrap_or(name);
    if style.lowercase {
        push_css_ident(out, &local.to_ascii_lowercase());
    } else {
        push_css_ident(out, local);
    }
}

fn usable_id(id: Option<&str>) -> Option<&str> {
    id.filter(|i| !i.is_empty() && !i.contains(|c: char| c.is_whitespace()))
}

/// Render `path` as a CSS selector. It starts at the innermost element with
/// an `id` (`#main > ul > li:nth-of-type(2)`), like browser DevTools.
/// Attributes become attribute selectors on their element, with
/// `attr_value` (`[id="bk102"]`) or without (`[id]`). Text and comments are
/// represented by their element: CSS cannot select them.
pub fn css(path: &[Step], style: Style, generalize: bool, attr_value: Option<&str>) -> String {
    let mut out = String::new();
    css_into(&mut out, path, style, generalize, attr_value);
    out
}

pub fn css_into(out: &mut String, path: &[Step], style: Style, generalize: bool, attr_value: Option<&str>) {
    // Start at the innermost #id, unless generalizing: then the query must
    // cover every sibling, not one particular element.
    let start = if generalize {
        0
    } else {
        path.iter().rposition(|s| matches!(s, Step::Elem { id, .. } if usable_id(*id).is_some())).unwrap_or(0)
    };
    let start_id = match path.get(start) {
        Some(Step::Elem { id, .. }) if !generalize => usable_id(*id),
        _ => None,
    };
    let mut first = true;
    // SVG/MathML names inside HTML are case-sensitive (viewBox): keep their case.
    let mut owner_ns = path[..start].iter().rev().find_map(|s| match s {
        Step::Elem { ns, .. } => Some(*ns),
        _ => None,
    });
    for (i, step) in path.iter().enumerate().skip(start) {
        match *step {
            Step::Elem { name, pos, indexed, id, ns } => {
                owner_ns = Some(ns);
                let style = if ns { Style { lowercase: false, ..style } } else { style };
                if i == start && start_id.is_some() {
                    out.push('#');
                    push_css_ident(out, id.unwrap_or_default());
                } else {
                    if !first {
                        out.push_str(" > ");
                    }
                    push_css_name(out, name, style);
                    if indexed && !generalize {
                        out.push_str(":nth-of-type(");
                        out.push_str(&pos.to_string());
                        out.push(')');
                    }
                }
                first = false;
            }
            Step::Attr(name) => {
                // #bk102 already says id="bk102"
                if start_id.is_some() && i == start + 1 && name.eq_ignore_ascii_case("id") && attr_value == start_id {
                    continue;
                }
                let style = if owner_ns == Some(true) { Style { lowercase: false, ..style } } else { style };
                out.push('[');
                // an attribute selector without a namespace only matches
                // un-prefixed attributes; *| matches xml:lang and friends
                if name.contains(':') {
                    out.push_str("*|");
                    push_css_ident(out, name.rsplit(':').next().unwrap_or(name));
                } else if style.lowercase {
                    push_css_ident(out, &name.to_ascii_lowercase());
                } else {
                    push_css_ident(out, name);
                }
                if let Some(v) = attr_value {
                    out.push('=');
                    push_css_string(out, v);
                }
                out.push(']');
            }
            Step::Text { .. } | Step::Comment { .. } | Step::Decode => {}
        }
    }
}

/// CSS counterpart of [`select_query`]. CSS can filter by attributes but not
/// by text, so attribute matches become `book[id="bk102"]` or
/// `book:has(> price[currency="USD"])`, name matches `book:has(> title)`,
/// and text matches keep their positional selector.
pub fn css_select(path: &[Step], name_hit: bool, value: &str, style: Style) -> String {
    let attr_value = (!name_hit).then_some(value);
    let Some(idx) = path.iter().rposition(|s| matches!(s, Step::Elem { indexed: true, .. })) else {
        return css(path, style, false, attr_value);
    };
    let last_elem = path.iter().rposition(|s| matches!(s, Step::Elem { .. })).unwrap_or(idx);
    let attr = match path.last() {
        Some(Step::Attr(_)) => Some(path.len() - 1),
        _ => None,
    };
    if attr.is_none() && !(name_hit && last_elem > idx) {
        // a text or comment match, or the repeated element's own name
        return if name_hit { css(&path[..=idx], style, true, None) } else { css(path, style, false, None) };
    }
    let mut q = css(&path[..=idx], style, true, None);
    let filter_end = attr.map_or(path.len(), |a| a + 1);
    if last_elem == idx {
        // attribute on the repeated element itself: book[id="bk102"]
        css_into(&mut q, &path[last_elem + 1..filter_end], style, true, attr_value);
    } else {
        // something inside it: book:has(> price[currency="USD"])
        q.push_str(":has(> ");
        let inner: Vec<Step> = path[idx + 1..filter_end]
            .iter()
            .map(|s| match *s {
                Step::Elem { name, pos, indexed, ns, .. } => Step::Elem { name, pos, indexed, id: None, ns },
                other => other,
            })
            .collect();
        css_into(&mut q, &inner, style, true, attr_value);
        q.push(')');
    }
    q
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(name: &str, pos: usize, indexed: bool) -> Step<'_> {
        Step::Elem { name, pos, indexed, id: None, ns: name.contains(':') }
    }

    const PLAIN: Style = Style { local_names: false, lowercase: false };

    #[test]
    fn plain_paths() {
        let p = [e("catalog", 1, false), e("book", 2, true), e("title", 1, false), Step::Text { pos: 1, mixed: false }];
        assert_eq!(format(&p, PLAIN, false), "/catalog/book[2]/title");
        assert_eq!(format(&p, PLAIN, true), "/catalog/book/title");
        let a = [e("catalog", 1, false), e("book", 1, true), Step::Attr("id")];
        assert_eq!(format(&a, PLAIN, false), "/catalog/book[1]/@id");
        let m = [e("p", 1, false), Step::Text { pos: 2, mixed: true }];
        assert_eq!(format(&m, PLAIN, false), "/p/text()[2]");
        assert_eq!(format(&m, PLAIN, true), "/p/text()");
        let c = [e("a", 1, false), Step::Comment { pos: 1 }];
        assert_eq!(format(&c, PLAIN, false), "/a/comment()[1]");
        let d = [
            e("ev", 1, false),
            e("payload", 1, false),
            Step::Text { pos: 1, mixed: false },
            Step::Decode,
            e("order", 1, false),
        ];
        assert_eq!(format(&d, PLAIN, false), "/ev/payload/parse-xml(.)/order");
    }

    #[test]
    fn local_names() {
        let p = [e("soap:Envelope", 1, false), e("Body", 1, false), Step::Attr("xml:lang")];
        let s = Style { local_names: true, lowercase: false };
        assert_eq!(format(&p, s, false), "/*[local-name()='Envelope']/*[local-name()='Body']/@*[local-name()='lang']");
        // without -N only names in a namespace use local-name()
        assert_eq!(format(&p, PLAIN, false), "/*[local-name()='Envelope']/Body/@*[local-name()='lang']");
    }

    #[test]
    fn literals() {
        let lit = |s: &str| {
            let mut o = String::new();
            push_literal(&mut o, s);
            o
        };
        assert_eq!(lit("Alice"), "'Alice'");
        assert_eq!(lit("O'Neil"), "\"O'Neil\"");
        assert_eq!(lit("it's \"x\""), "concat('it', \"'\", 's \"x\"')");
    }

    #[test]
    fn select_queries() {
        let t = Step::Text { pos: 1, mixed: false };
        let p = [e("catalog", 1, false), e("book", 2, true), e("author", 1, false), t];
        assert_eq!(select_query(&p, false, "Alice", false, PLAIN), "/catalog/book[author='Alice']");
        let a = [e("catalog", 1, false), e("book", 2, true), Step::Attr("id")];
        assert_eq!(select_query(&a, false, "bk102", false, PLAIN), "/catalog/book[@id='bk102']");
        assert_eq!(select_query(&a, true, "id", false, PLAIN), "/catalog/book[@id]");
        let own = [e("list", 1, false), e("item", 3, true), t];
        assert_eq!(select_query(&own, false, "x", false, PLAIN), "/list/item[.='x']");
        let mixed = [e("doc", 1, false), e("p", 2, true), Step::Text { pos: 2, mixed: true }];
        assert_eq!(select_query(&mixed, false, "world", false, PLAIN), "/doc/p[text()='world']");
        let deep = [e("a", 1, false), e("b", 2, true), e("c", 3, true), e("d", 1, false), t];
        assert_eq!(select_query(&deep, false, "v", false, PLAIN), "/a/b/c[d='v']");
        let none = [e("a", 1, false), e("b", 1, false), t];
        assert_eq!(select_query(&none, false, "v", false, PLAIN), "/a/b");
        let name = [e("a", 1, false), e("b", 2, true)];
        assert_eq!(select_query(&name, true, "b", false, PLAIN), "/a/b");
        // text with surrounding whitespace compares through normalize-space()
        assert_eq!(select_query(&mixed, false, "in  2024", true, PLAIN), "/doc/p[text()[normalize-space()='in 2024']]");
        assert_eq!(select_query(&own, false, "x", true, PLAIN), "/list/item[normalize-space()='x']");
    }

    #[test]
    fn css_selectors() {
        let t = Step::Text { pos: 1, mixed: false };
        let p = [e("catalog", 1, false), e("book", 2, true), e("title", 1, false), t];
        assert_eq!(css(&p, PLAIN, false, None), "catalog > book:nth-of-type(2) > title");
        assert_eq!(css(&p, PLAIN, true, None), "catalog > book > title");
        let a = [e("catalog", 1, false), e("book", 2, true), Step::Attr("id")];
        assert_eq!(css(&a, PLAIN, false, Some("bk102")), "catalog > book:nth-of-type(2)[id=\"bk102\"]");
        // start at the innermost element with an id
        let main = Step::Elem { name: "div", pos: 1, indexed: false, id: Some("main"), ns: false };
        let h = [e("html", 1, false), e("body", 1, false), main, e("ul", 1, false), e("li", 3, true), t];
        assert_eq!(css(&h, PLAIN, false, None), "#main > ul > li:nth-of-type(3)");
        // escaping and HTML lower case
        let odd = [e("DIV", 1, false), e("x:y", 1, false), Step::Attr("data-v")];
        let html = Style { local_names: false, lowercase: true };
        assert_eq!(css(&odd, html, false, Some("a\"b")), "div > y[data-v=\"a\\\"b\"]");
        assert_eq!(css(&[e("a", 1, false), Step::Attr("xml:lang")], PLAIN, false, Some("en")), "a[*|lang=\"en\"]");
        let digit = Step::Elem { name: "p", pos: 1, indexed: false, id: Some("1st"), ns: false };
        assert_eq!(css(&[digit], PLAIN, false, None), "#\\31 st");
        let ns = Style { local_names: true, lowercase: false };
        assert_eq!(css(&[e("soap:Envelope", 1, false)], ns, false, None), "Envelope");
    }

    #[test]
    fn foreign_content_keeps_case() {
        let html = Style { local_names: false, lowercase: true };
        let svg = Step::Elem { name: "svg", pos: 1, indexed: false, id: None, ns: true };
        let p = [e("HTML", 1, false), e("BODY", 1, false), svg, Step::Attr("viewBox")];
        assert_eq!(format(&p, html, false), "/html/body/*[local-name()='svg']/@viewBox");
        assert_eq!(css(&p, html, false, Some("0 0 9 9")), "html > body > svg[viewBox=\"0 0 9 9\"]");
        let g = Step::Elem { name: "linearGradient", pos: 1, indexed: false, id: None, ns: true };
        assert_eq!(css(&[e("BODY", 1, false), g], html, false, None), "body > linearGradient");
        let svg2 = Step::Elem { name: "svg", pos: 2, indexed: true, id: None, ns: true };
        let q = [e("HTML", 1, false), e("BODY", 1, false), svg2, Step::Attr("viewBox")];
        assert_eq!(
            select_query(&q, false, "0 0 9 9", false, html),
            "/html/body/*[local-name()='svg'][@viewBox='0 0 9 9']"
        );
    }

    #[test]
    fn css_ids_are_not_repeated_or_generalized() {
        let book = Step::Elem { name: "book", pos: 2, indexed: true, id: Some("bk102"), ns: false };
        let p = [e("catalog", 1, false), book, Step::Attr("id")];
        assert_eq!(css(&p, PLAIN, false, Some("bk102")), "#bk102");
        assert_eq!(css(&p, PLAIN, true, None), "catalog > book[id]");
        let q = [e("catalog", 1, false), book, e("price", 1, false), Step::Attr("currency")];
        assert_eq!(css(&q, PLAIN, false, Some("USD")), "#bk102 > price[currency=\"USD\"]");
        assert_eq!(css_select(&q, false, "USD", PLAIN), "catalog > book:has(> price[currency=\"USD\"])");
    }

    #[test]
    fn css_select_queries() {
        let t = Step::Text { pos: 1, mixed: false };
        let own = [e("catalog", 1, false), e("book", 2, true), Step::Attr("id")];
        assert_eq!(css_select(&own, false, "bk102", PLAIN), "catalog > book[id=\"bk102\"]");
        assert_eq!(css_select(&own, true, "id", PLAIN), "catalog > book[id]");
        let inner = [e("catalog", 1, false), e("book", 2, true), e("price", 1, false), Step::Attr("currency")];
        assert_eq!(css_select(&inner, false, "USD", PLAIN), "catalog > book:has(> price[currency=\"USD\"])");
        let name = [e("catalog", 1, false), e("book", 2, true), e("title", 1, false)];
        assert_eq!(css_select(&name, true, "title", PLAIN), "catalog > book:has(> title)");
        let text = [e("catalog", 1, false), e("book", 2, true), e("title", 1, false), t];
        assert_eq!(css_select(&text, false, "Rust", PLAIN), "catalog > book:nth-of-type(2) > title");
        let none = [e("a", 1, false), Step::Attr("x")];
        assert_eq!(css_select(&none, false, "1", PLAIN), "a[x=\"1\"]");
    }
}
