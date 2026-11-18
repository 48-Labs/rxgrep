//! A small regular-expression engine (no dependencies).
//!
//! Patterns compile to a Thompson NFA that is run with a Pike VM, so matching
//! takes time linear in the input: no pattern can make it backtrack
//! exponentially. Supported syntax:
//!
//! * literals, `.` (any character except newline), `^`, `$`
//! * classes `[abc]`, `[^a-z]`, and `\d \D \w \W \s \S` (also inside classes)
//! * word boundaries `\b \B`
//! * groups `( )` and `(?: )`, alternation `|`
//! * quantifiers `* + ?`, `{m}`, `{m,}`, `{m,n}` (a trailing `?` is accepted)
//! * escapes `\n \t \r \f \v \0` and `\` before any punctuation

#[derive(Debug, Clone)]
enum ClassItem {
    Range(char, char),
    Digit(bool),
    Word(bool),
    Space(bool),
}

#[derive(Debug, Clone)]
struct Class {
    negated: bool,
    items: Vec<ClassItem>,
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

impl Class {
    fn matches_one(&self, c: char) -> bool {
        self.items.iter().any(|it| match *it {
            ClassItem::Range(a, b) => a <= c && c <= b,
            ClassItem::Digit(pos) => c.is_ascii_digit() == pos,
            ClassItem::Word(pos) => is_word(c) == pos,
            ClassItem::Space(pos) => c.is_whitespace() == pos,
        })
    }

    fn matches(&self, c: char, icase: bool) -> bool {
        let hit = self.matches_one(c)
            || (icase
                && (c.to_lowercase().any(|l| self.matches_one(l)) || c.to_uppercase().any(|u| self.matches_one(u))));
        hit != self.negated
    }
}

#[derive(Debug, Clone)]
enum Node {
    Empty,
    Char(char),
    Any,
    Class(Class),
    Start,
    End,
    WordBoundary(bool),
    Concat(Vec<Node>),
    Alt(Vec<Node>),
    Repeat { node: Box<Node>, min: u32, max: Option<u32> },
}

/// Upper bound on `{m,n}` counts and on the compiled program size.
const MAX_REPEAT: u32 = 1000;
const MAX_PROGRAM: usize = 100_000;

struct Parser<'p> {
    chars: Vec<char>,
    pos: usize,
    src: &'p str,
}

impl Parser<'_> {
    fn err<T>(&self, msg: &str) -> Result<T, String> {
        Err(format!("invalid regex '{}': {msg}", self.src))
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn eat(&mut self, c: char) -> bool {
        if self.peek() == Some(c) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn alt(&mut self) -> Result<Node, String> {
        let mut branches = vec![self.concat()?];
        while self.eat('|') {
            branches.push(self.concat()?);
        }
        Ok(if branches.len() == 1 { branches.pop().unwrap() } else { Node::Alt(branches) })
    }

    fn concat(&mut self) -> Result<Node, String> {
        let mut items = Vec::new();
        while let Some(c) = self.peek() {
            if c == '|' || c == ')' {
                break;
            }
            let atom = self.atom()?;
            items.push(self.quantified(atom)?);
        }
        Ok(match items.len() {
            0 => Node::Empty,
            1 => items.pop().unwrap(),
            _ => Node::Concat(items),
        })
    }

    /// `{m}`, `{m,}`, `{m,n}`; `None` if the text is not a valid quantifier.
    fn braces(&mut self) -> Option<(u32, Option<u32>)> {
        let save = self.pos;
        self.pos += 1; // '{'
        let num = |p: &mut Self| {
            let start = p.pos;
            while p.peek().is_some_and(|c| c.is_ascii_digit()) {
                p.pos += 1;
            }
            p.chars[start..p.pos].iter().collect::<String>().parse::<u32>().ok()
        };
        let parsed = (|| {
            let min = num(self)?;
            let max = if self.eat(',') {
                if self.peek() == Some('}') {
                    None
                } else {
                    Some(num(self)?)
                }
            } else {
                Some(min)
            };
            self.eat('}').then_some((min, max))
        })();
        if parsed.is_none() {
            self.pos = save;
        }
        parsed
    }

    fn quantified(&mut self, mut atom: Node) -> Result<Node, String> {
        loop {
            let (min, max) = match self.peek() {
                Some('*') => {
                    self.pos += 1;
                    (0, None)
                }
                Some('+') => {
                    self.pos += 1;
                    (1, None)
                }
                Some('?') => {
                    self.pos += 1;
                    (0, Some(1))
                }
                Some('{') => match self.braces() {
                    Some(q) => q,
                    None => return Ok(atom),
                },
                _ => return Ok(atom),
            };
            if max.is_some_and(|m| m < min) {
                return self.err("quantifier {m,n} has n < m");
            }
            if min > MAX_REPEAT || max.is_some_and(|m| m > MAX_REPEAT) {
                return self.err("repetition count too large (max 1000)");
            }
            if matches!(atom, Node::Start | Node::End | Node::WordBoundary(_) | Node::Empty) {
                return self.err("nothing to repeat");
            }
            self.eat('?'); // lazy marker: irrelevant for yes/no matching
            atom = Node::Repeat { node: Box::new(atom), min, max };
        }
    }

    fn atom(&mut self) -> Result<Node, String> {
        let c = self.peek().unwrap();
        self.pos += 1;
        match c {
            '(' => {
                if self.eat('?') && !self.eat(':') {
                    return self.err("only (?: ) groups are supported");
                }
                let inner = self.alt()?;
                if !self.eat(')') {
                    return self.err("missing ')'");
                }
                Ok(inner)
            }
            '[' => self.class(),
            '.' => Ok(Node::Any),
            '^' => Ok(Node::Start),
            '$' => Ok(Node::End),
            '*' | '+' | '?' => self.err("nothing to repeat"),
            '\\' => self.escape(false).map(|e| match e {
                Esc::Char(c) => Node::Char(c),
                Esc::Item(it) => Node::Class(Class { negated: false, items: vec![it] }),
                Esc::Boundary(b) => Node::WordBoundary(b),
            }),
            c => Ok(Node::Char(c)),
        }
    }

    fn escape(&mut self, in_class: bool) -> Result<Esc, String> {
        let Some(c) = self.peek() else {
            return self.err("trailing backslash");
        };
        self.pos += 1;
        Ok(match c {
            'd' => Esc::Item(ClassItem::Digit(true)),
            'D' => Esc::Item(ClassItem::Digit(false)),
            'w' => Esc::Item(ClassItem::Word(true)),
            'W' => Esc::Item(ClassItem::Word(false)),
            's' => Esc::Item(ClassItem::Space(true)),
            'S' => Esc::Item(ClassItem::Space(false)),
            'b' if !in_class => Esc::Boundary(true),
            'B' if !in_class => Esc::Boundary(false),
            'n' => Esc::Char('\n'),
            't' => Esc::Char('\t'),
            'r' => Esc::Char('\r'),
            'f' => Esc::Char('\u{c}'),
            'v' => Esc::Char('\u{b}'),
            '0' => Esc::Char('\0'),
            c if c.is_ascii_alphanumeric() => return self.err(&format!("unsupported escape \\{c}")),
            c => Esc::Char(c),
        })
    }

    fn class(&mut self) -> Result<Node, String> {
        let negated = self.eat('^');
        let mut items = Vec::new();
        let mut first = true;
        loop {
            let Some(c) = self.peek() else {
                return self.err("missing ']'");
            };
            if c == ']' && !first {
                self.pos += 1;
                break;
            }
            first = false;
            self.pos += 1;
            let lo = if c == '\\' {
                match self.escape(true)? {
                    Esc::Char(c) => c,
                    Esc::Item(it) => {
                        items.push(it);
                        continue;
                    }
                    Esc::Boundary(_) => unreachable!(),
                }
            } else {
                c
            };
            // a range like a-z (a '-' before ']' is a literal)
            if self.peek() == Some('-') && self.chars.get(self.pos + 1).is_some_and(|&n| n != ']') {
                self.pos += 1;
                let hi = match self.peek() {
                    Some('\\') => {
                        self.pos += 1;
                        match self.escape(true)? {
                            Esc::Char(c) => c,
                            _ => return self.err("invalid class range"),
                        }
                    }
                    Some(h) => {
                        self.pos += 1;
                        h
                    }
                    None => return self.err("missing ']'"),
                };
                if hi < lo {
                    return self.err("class range out of order");
                }
                items.push(ClassItem::Range(lo, hi));
            } else {
                items.push(ClassItem::Range(lo, lo));
            }
        }
        Ok(Node::Class(Class { negated, items }))
    }
}

enum Esc {
    Char(char),
    Item(ClassItem),
    Boundary(bool),
}

#[derive(Debug, Clone)]
enum Inst {
    Char(char),
    Any,
    Class(usize),
    Start,
    End,
    WordBoundary(bool),
    Split(usize, usize),
    Jmp(usize),
    Match,
}

/// A compiled regular expression.
#[derive(Debug, Clone)]
pub struct Regex {
    prog: Vec<Inst>,
    classes: Vec<Class>,
    icase: bool,
}

struct Compiler {
    prog: Vec<Inst>,
    classes: Vec<Class>,
    icase: bool,
}

impl Compiler {
    fn emit(&mut self, i: Inst) -> Result<usize, String> {
        if self.prog.len() >= MAX_PROGRAM {
            return Err("regex is too large".into());
        }
        self.prog.push(i);
        Ok(self.prog.len() - 1)
    }

    fn compile(&mut self, n: &Node) -> Result<(), String> {
        match n {
            Node::Empty => {}
            Node::Char(c) => {
                let c = if self.icase { fold(*c) } else { *c };
                self.emit(Inst::Char(c))?;
            }
            Node::Any => {
                self.emit(Inst::Any)?;
            }
            Node::Class(cl) => {
                self.classes.push(cl.clone());
                self.emit(Inst::Class(self.classes.len() - 1))?;
            }
            Node::Start => {
                self.emit(Inst::Start)?;
            }
            Node::End => {
                self.emit(Inst::End)?;
            }
            Node::WordBoundary(b) => {
                self.emit(Inst::WordBoundary(*b))?;
            }
            Node::Concat(items) => {
                for it in items {
                    self.compile(it)?;
                }
            }
            Node::Alt(branches) => {
                // split L1, next ; L1: a ; jmp end ; next: split L2, ... ; last branch
                let mut jumps = Vec::new();
                for (i, b) in branches.iter().enumerate() {
                    if i + 1 < branches.len() {
                        let split = self.emit(Inst::Split(0, 0))?;
                        self.compile(b)?;
                        jumps.push(self.emit(Inst::Jmp(0))?);
                        let next = self.prog.len();
                        self.prog[split] = Inst::Split(split + 1, next);
                    } else {
                        self.compile(b)?;
                    }
                }
                let end = self.prog.len();
                for j in jumps {
                    self.prog[j] = Inst::Jmp(end);
                }
            }
            Node::Repeat { node, min, max } => {
                for _ in 0..*min {
                    self.compile(node)?;
                }
                match max {
                    None => {
                        // L: split body, out ; body ; jmp L
                        let split = self.emit(Inst::Split(0, 0))?;
                        self.compile(node)?;
                        self.emit(Inst::Jmp(split))?;
                        let out = self.prog.len();
                        self.prog[split] = Inst::Split(split + 1, out);
                    }
                    Some(max) => {
                        // (body (body ...)?)? for the optional copies
                        let mut splits = Vec::new();
                        for _ in *min..*max {
                            splits.push(self.emit(Inst::Split(0, 0))?);
                            self.compile(node)?;
                        }
                        let out = self.prog.len();
                        for s in splits {
                            self.prog[s] = Inst::Split(s + 1, out);
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

fn fold(c: char) -> char {
    let mut l = c.to_lowercase();
    match (l.next(), l.next()) {
        (Some(x), None) => x,
        _ => c,
    }
}

/// Sparse set of program counters (dedupes threads per step).
struct Threads {
    dense: Vec<usize>,
    sparse: Vec<usize>,
}

impl Threads {
    fn new(n: usize) -> Self {
        Threads { dense: Vec::with_capacity(n), sparse: vec![0; n] }
    }
    fn contains(&self, pc: usize) -> bool {
        let i = self.sparse[pc];
        i < self.dense.len() && self.dense[i] == pc
    }
    fn insert(&mut self, pc: usize) {
        self.sparse[pc] = self.dense.len();
        self.dense.push(pc);
    }
    fn clear(&mut self) {
        self.dense.clear();
    }
}

impl Regex {
    /// Compile `pattern`. With `icase`, matching ignores letter case.
    pub fn new(pattern: &str, icase: bool) -> Result<Regex, String> {
        let mut p = Parser { chars: pattern.chars().collect(), pos: 0, src: pattern };
        let ast = p.alt()?;
        if p.pos < p.chars.len() {
            return p.err("unmatched ')'");
        }
        let mut c = Compiler { prog: Vec::new(), classes: Vec::new(), icase };
        c.compile(&ast).map_err(|e| format!("invalid regex '{pattern}': {e}"))?;
        c.emit(Inst::Match)?;
        Ok(Regex { prog: c.prog, classes: c.classes, icase })
    }

    /// Does the regex match? With `whole`, it must match the entire text;
    /// otherwise anywhere in it.
    pub fn is_match(&self, text: &str, whole: bool) -> bool {
        let chars: Vec<char> = text.chars().collect();
        let n = chars.len();
        let mut clist = Threads::new(self.prog.len());
        let mut nlist = Threads::new(self.prog.len());
        let mut stack = Vec::new();
        self.add(&mut clist, &mut stack, 0, 0, &chars);
        for i in 0..=n {
            if clist.dense.is_empty() && (whole || i == n) {
                break;
            }
            for &pc in &clist.dense {
                let ok = match self.prog[pc] {
                    // A thread at the end of the program: a match, if the
                    // match may end here (anywhere, or only at the end with `whole`).
                    Inst::Match if !whole || i == n => return true,
                    Inst::Char(c) => i < n && (if self.icase { fold(chars[i]) } else { chars[i] }) == c,
                    Inst::Any => i < n && chars[i] != '\n',
                    Inst::Class(k) => i < n && self.classes[k].matches(chars[i], self.icase),
                    _ => false,
                };
                if ok {
                    self.add(&mut nlist, &mut stack, pc + 1, i + 1, &chars);
                }
            }
            if !whole && i < n {
                self.add(&mut nlist, &mut stack, 0, i + 1, &chars);
            }
            std::mem::swap(&mut clist, &mut nlist);
            nlist.clear();
        }
        false
    }

    /// Add `pc` and everything reachable through empty transitions at text position `i`.
    fn add(&self, list: &mut Threads, stack: &mut Vec<usize>, pc: usize, i: usize, chars: &[char]) {
        stack.push(pc);
        while let Some(pc) = stack.pop() {
            if list.contains(pc) {
                continue;
            }
            list.insert(pc);
            match self.prog[pc] {
                Inst::Jmp(t) => stack.push(t),
                Inst::Split(a, b) => {
                    stack.push(b);
                    stack.push(a);
                }
                Inst::Start => {
                    if i == 0 {
                        stack.push(pc + 1);
                    }
                }
                Inst::End => {
                    if i == chars.len() {
                        stack.push(pc + 1);
                    }
                }
                Inst::WordBoundary(want) => {
                    let before = i > 0 && is_word(chars[i - 1]);
                    let after = i < chars.len() && is_word(chars[i]);
                    if (before != after) == want {
                        stack.push(pc + 1);
                    }
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(p: &str, t: &str) -> bool {
        Regex::new(p, false).unwrap().is_match(t, false)
    }
    fn whole(p: &str, t: &str) -> bool {
        Regex::new(p, false).unwrap().is_match(t, true)
    }

    #[test]
    fn literals_and_search() {
        assert!(m("ice", "Alice"));
        assert!(!m("ice", "ICE"));
        assert!(Regex::new("ICE", true).unwrap().is_match("alice", false));
        assert!(whole("Alice", "Alice"));
        assert!(!whole("Alice", "Alice2"));
        assert!(m("", "anything"));
    }

    #[test]
    fn classes_and_escapes() {
        assert!(whole(r"bk\d{3}", "bk102"));
        assert!(!whole(r"bk\d{3}", "bk1021"));
        assert!(whole(r"[A-Z][a-z]+", "Alice"));
        assert!(!whole(r"[^a-z]+", "abc"));
        assert!(whole(r"[\w.+-]+@example\.com", "a.b+c@example.com"));
        assert!(!whole(r"[\w.+-]+@example\.com", "a@exampleXcom"));
        assert!(whole(r"\s*x\s*", "  x "));
        assert!(whole("[]a]+", "]a]"));
        assert!(whole("[a-]+", "a-a"));
        assert!(whole(r"\$\d+\.\d\d", "$39.95"));
    }

    #[test]
    fn alternation_groups_quantifiers() {
        assert!(whole("(USD|EUR)", "EUR"));
        assert!(!whole("USD|EUR", "GBP"));
        assert!(whole("(?:ab)+", "ababab"));
        assert!(!whole("(?:ab)+", "ababa"));
        assert!(whole("a{2,3}", "aaa"));
        assert!(!whole("a{2,3}", "aaaa"));
        assert!(whole("a{2,}", "aaaaa"));
        assert!(whole("colou?r", "color"));
        assert!(whole("x{2}", "xx"));
        assert!(whole("a{x", "a{x"), "invalid brace quantifier is a literal");
        assert!(whole(".*?end", "the end"));
    }

    #[test]
    fn anchors_and_boundaries() {
        assert!(m("^Al", "Alice"));
        assert!(!m("^li", "Alice"));
        assert!(m("ce$", "Alice"));
        assert!(m(r"\bRust\b", "Rust in Action"));
        assert!(!m(r"\bRust\b", "Rusty"));
        assert!(m(r"\Bus", "Rust"));
        assert!(!m(".", "\n"));
    }

    #[test]
    fn unicode_and_case() {
        assert!(whole("caf.", "café"));
        assert!(Regex::new("ÉLAN", true).unwrap().is_match("élan", true));
        assert!(Regex::new("[a-z]+", true).unwrap().is_match("ABC", true));
    }

    #[test]
    fn linear_time_on_pathological_patterns() {
        let r = Regex::new("(a*)*b", false).unwrap();
        let text = "a".repeat(5000);
        let t = std::time::Instant::now();
        assert!(!r.is_match(&text, false));
        assert!(t.elapsed().as_secs() < 2, "should not backtrack exponentially");
    }

    #[test]
    fn errors() {
        for bad in ["(", ")", "[a", "*a", r"\q", "a{3,1}", "x{5000}", "(?=x)", "\\"] {
            assert!(Regex::new(bad, false).is_err(), "should reject {bad:?}");
        }
    }
}
