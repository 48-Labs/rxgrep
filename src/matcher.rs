//! Pattern matching: exact (default), substring, glob, regex and fuzzy
//! variants, plus numeric comparisons ([`NumFilter`]).
//!
//! Matching runs once per scalar in the input, so the common cases avoid
//! allocation: case-sensitive exact/substring use plain `str` operations, and
//! when both pattern and text are ASCII, case-insensitive, glob and fuzzy
//! matching run directly on bytes. Only non-ASCII text with `-i`, `-g` or `-z`
//! takes the Unicode path (lowercase + `char` vector).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Pattern may appear anywhere in the text.
    Substring,
    /// Text must equal the pattern.
    Exact,
    /// Pattern is a glob (`*`, `?`, `\` escape) matched against the whole text.
    Glob,
}

use crate::regex::Regex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tok<T> {
    Star,
    One,
    Lit(T),
}

#[derive(Debug)]
pub struct Matcher {
    /// Lowercased when `ignore_case`.
    pattern: String,
    pattern_chars: Vec<char>,
    glob_chars: Vec<Tok<char>>,
    /// Byte version of the glob, present when the pattern is ASCII.
    glob_bytes: Vec<Tok<u8>>,
    ascii: bool,
    mode: Mode,
    ignore_case: bool,
    /// Maximum number of edits (insert/delete/substitute) tolerated.
    fuzzy: Option<usize>,
    /// A compiled regex and whether it must match the whole text.
    regex: Option<(Regex, bool)>,
}

impl Matcher {
    #[cfg(test)]
    pub fn new(pattern: &str, mode: Mode, ignore_case: bool) -> Self {
        Self::with_fuzzy(pattern, mode, ignore_case, None)
    }

    /// `fuzzy = Some(k)` tolerates up to `k` typos. Not supported for globs.
    pub fn with_fuzzy(pattern: &str, mode: Mode, ignore_case: bool, fuzzy: Option<usize>) -> Self {
        let pattern = if ignore_case { pattern.to_lowercase() } else { pattern.to_string() };
        let ascii = pattern.is_ascii();
        let glob_chars = if mode == Mode::Glob { compile_glob(&pattern) } else { Vec::new() };
        let glob_bytes = if ascii {
            glob_chars
                .iter()
                .map(|t| match *t {
                    Tok::Star => Tok::Star,
                    Tok::One => Tok::One,
                    Tok::Lit(c) => Tok::Lit(c as u8),
                })
                .collect()
        } else {
            Vec::new()
        };
        let pattern_chars = pattern.chars().collect();
        let fuzzy = fuzzy.filter(|&k| k > 0 && mode != Mode::Glob);
        Matcher { pattern, pattern_chars, glob_chars, glob_bytes, ascii, mode, ignore_case, fuzzy, regex: None }
    }

    /// A regular-expression matcher. With `whole`, the regex must match the
    /// entire text; otherwise anywhere in it.
    pub fn regex(pattern: &str, ignore_case: bool, whole: bool) -> Result<Self, String> {
        let re = Regex::new(pattern, ignore_case)?;
        let mut m = Self::with_fuzzy("", Mode::Exact, false, None);
        m.regex = Some((re, whole));
        Ok(m)
    }

    pub fn is_match(&self, text: &str) -> bool {
        if let Some((re, whole)) = &self.regex {
            return re.is_match(text, *whole);
        }
        // Fast paths that work for any UTF-8 without allocating.
        if self.fuzzy.is_none() && !self.ignore_case {
            match self.mode {
                Mode::Exact => return text == self.pattern,
                Mode::Substring => return text.contains(self.pattern.as_str()),
                Mode::Glob => {}
            }
        }
        if self.ascii && text.is_ascii() {
            return self.match_ascii(text.as_bytes());
        }
        self.match_unicode(text)
    }

    /// Byte-level matching; pattern and text are both ASCII.
    fn match_ascii(&self, t: &[u8]) -> bool {
        let p = self.pattern.as_bytes();
        let ic = self.ignore_case;
        let eq = |text: u8, pat: u8| if ic { text.to_ascii_lowercase() == pat } else { text == pat };
        if let Some(k) = self.fuzzy {
            return match self.mode {
                Mode::Exact => levenshtein_within(p, t, k, eq),
                _ => substring_distance(p, t, eq) <= k,
            };
        }
        match self.mode {
            Mode::Exact => t.eq_ignore_ascii_case(p), // only reached with -i
            Mode::Substring => contains_ascii_icase(t, p),
            Mode::Glob => glob_match(&self.glob_bytes, t, eq),
        }
    }

    fn match_unicode(&self, text: &str) -> bool {
        let folded;
        let text = if self.ignore_case {
            folded = text.to_lowercase();
            folded.as_str()
        } else {
            text
        };
        let eq = |a: char, b: char| a == b;
        if let Some(k) = self.fuzzy {
            let t: Vec<char> = text.chars().collect();
            return match self.mode {
                Mode::Exact => levenshtein_within(&self.pattern_chars, &t, k, eq),
                _ => substring_distance(&self.pattern_chars, &t, eq) <= k,
            };
        }
        match self.mode {
            Mode::Substring => text.contains(self.pattern.as_str()),
            Mode::Exact => text == self.pattern,
            Mode::Glob => {
                let chars: Vec<char> = text.chars().collect();
                glob_match(&self.glob_chars, &chars, eq)
            }
        }
    }
}

fn compile_glob(p: &str) -> Vec<Tok<char>> {
    let mut toks = Vec::new();
    let mut chars = p.chars();
    while let Some(c) = chars.next() {
        match c {
            '*' => {
                if toks.last() != Some(&Tok::Star) {
                    toks.push(Tok::Star);
                }
            }
            '?' => toks.push(Tok::One),
            '\\' => toks.push(Tok::Lit(chars.next().unwrap_or('\\'))),
            c => toks.push(Tok::Lit(c)),
        }
    }
    toks
}

/// ASCII case-insensitive substring search; `needle` is already lowercase.
fn contains_ascii_icase(hay: &[u8], needle: &[u8]) -> bool {
    let Some((&first, rest)) = needle.split_first() else {
        return true;
    };
    if hay.len() < needle.len() {
        return false;
    }
    let first_upper = first.to_ascii_uppercase();
    (0..=hay.len() - needle.len())
        .any(|i| (hay[i] == first || hay[i] == first_upper) && hay[i + 1..i + needle.len()].eq_ignore_ascii_case(rest))
}

/// Iterative glob matcher with single-star backtracking. `eq(text, pattern)`.
fn glob_match<T: Copy>(p: &[Tok<T>], t: &[T], eq: impl Fn(T, T) -> bool) -> bool {
    let (mut pi, mut ti) = (0usize, 0usize);
    let mut star: Option<usize> = None;
    let mut mark = 0usize;
    while ti < t.len() {
        match p.get(pi) {
            Some(Tok::One) => {
                pi += 1;
                ti += 1;
            }
            Some(&Tok::Lit(c)) if eq(t[ti], c) => {
                pi += 1;
                ti += 1;
            }
            Some(Tok::Star) => {
                star = Some(pi);
                mark = ti;
                pi += 1;
            }
            _ => match star {
                Some(s) => {
                    pi = s + 1;
                    mark += 1;
                    ti = mark;
                }
                None => return false,
            },
        }
    }
    while matches!(p.get(pi), Some(Tok::Star)) {
        pi += 1;
    }
    pi == p.len()
}

/// Is the edit distance between pattern `p` and text `t` at most `k`?
fn levenshtein_within<T: Copy>(p: &[T], t: &[T], k: usize, eq: impl Fn(T, T) -> bool) -> bool {
    if p.len().abs_diff(t.len()) > k {
        return false;
    }
    let mut prev: Vec<usize> = (0..=t.len()).collect();
    let mut cur = vec![0; t.len() + 1];
    for (i, &pc) in p.iter().enumerate() {
        cur[0] = i + 1;
        let mut row_min = cur[0];
        for (j, &tc) in t.iter().enumerate() {
            let sub = prev[j] + usize::from(!eq(tc, pc));
            cur[j + 1] = sub.min(prev[j + 1] + 1).min(cur[j] + 1);
            row_min = row_min.min(cur[j + 1]);
        }
        if row_min > k {
            return false;
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[t.len()] <= k
}

/// Smallest edit distance between `pat` and any substring of `text`
/// (Sellers' approximate string matching).
fn substring_distance<T: Copy>(pat: &[T], text: &[T], eq: impl Fn(T, T) -> bool) -> usize {
    let m = pat.len();
    let mut col: Vec<usize> = (0..=m).collect();
    let mut best = col[m];
    for &c in text {
        let mut diag = col[0];
        col[0] = 0;
        for i in 1..=m {
            let old = col[i];
            col[i] = (diag + usize::from(!eq(c, pat[i - 1]))).min(old + 1).min(col[i - 1] + 1);
            diag = old;
        }
        best = best.min(col[m]);
        if best == 0 {
            break;
        }
    }
    best
}

/// Numeric comparison on values: `--gt`, `--ge`, `--lt`, `--le`, `--range`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct NumFilter {
    /// Lower bound and whether it is inclusive.
    pub min: Option<(f64, bool)>,
    /// Upper bound and whether it is inclusive.
    pub max: Option<(f64, bool)>,
}

impl NumFilter {
    pub fn is_empty(&self) -> bool {
        self.min.is_none() && self.max.is_none()
    }

    /// Tighten the lower bound.
    pub fn above(&mut self, v: f64, inclusive: bool) {
        if self.min.is_none_or(|(m, _)| v >= m) {
            self.min = Some((v, inclusive && self.min.is_none_or(|(m, inc)| v > m || inc)));
        }
    }

    /// Tighten the upper bound.
    pub fn below(&mut self, v: f64, inclusive: bool) {
        if self.max.is_none_or(|(m, _)| v <= m) {
            self.max = Some((v, inclusive && self.max.is_none_or(|(m, inc)| v < m || inc)));
        }
    }

    /// Parse a number the way values are read (`-1.5`, `+3`, `1e3`).
    pub fn parse_number(s: &str) -> Option<f64> {
        let t = s.trim();
        let t = t.strip_prefix('+').unwrap_or(t);
        if t.is_empty() || !t.bytes().all(|b| b.is_ascii_digit() || matches!(b, b'.' | b'-' | b'e' | b'E' | b'+')) {
            return None;
        }
        t.parse::<f64>().ok().filter(|v| v.is_finite())
    }

    /// Is `text` a number inside the bounds?
    pub fn matches(&self, text: &str) -> bool {
        let Some(v) = Self::parse_number(text) else {
            return false;
        };
        self.min.is_none_or(|(m, inc)| if inc { v >= m } else { v > m })
            && self.max.is_none_or(|(m, inc)| if inc { v <= m } else { v < m })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regex_mode() {
        let whole = Matcher::regex(r"bk\d+", false, true).unwrap();
        assert!(whole.is_match("bk102"));
        assert!(!whole.is_match("id bk102"));
        let search = Matcher::regex(r"bk\d+", false, false).unwrap();
        assert!(search.is_match("id bk102"));
        assert!(Matcher::regex("ALICE", true, true).unwrap().is_match("alice"));
        assert!(Matcher::regex("(", false, true).is_err());
    }

    #[test]
    fn numeric_filter() {
        let mut f = NumFilter::default();
        assert!(f.is_empty());
        f.above(10.0, false);
        f.below(50.0, true);
        assert!(f.matches("39.95") && f.matches(" 50 ") && f.matches("+11"));
        assert!(!f.matches("10") && !f.matches("50.01") && !f.matches("abc") && !f.matches(""));
        assert!(!f.matches("inf") && !f.matches("NaN") && !f.matches("0x20"));
        assert!(f.matches("1.2e1"));
        // tightening keeps the stricter bound
        f.above(5.0, true);
        assert!(!f.matches("8"));
        f.above(20.0, true);
        assert!(f.matches("20") && !f.matches("19.9"));
        f.below(50.0, false);
        assert!(!f.matches("50"));
    }

    #[test]
    fn substring() {
        let m = Matcher::new("ice", Mode::Substring, false);
        assert!(m.is_match("Alice"));
        assert!(!m.is_match("ICE"));
        let mi = Matcher::new("ICE", Mode::Substring, true);
        assert!(mi.is_match("alice"));
        assert!(mi.is_match("ALICE"));
        assert!(mi.is_match("Älice")); // non-ASCII text, Unicode path
        assert!(!mi.is_match("alc"));
        assert!(Matcher::new("", Mode::Substring, true).is_match("anything"));
    }

    #[test]
    fn exact() {
        let m = Matcher::new("alice", Mode::Exact, true);
        assert!(m.is_match("ALICE"));
        assert!(!m.is_match("alice2"));
        assert!(Matcher::new("élan", Mode::Exact, true).is_match("ÉLAN"));
        // Kelvin sign lowercases to ASCII 'k': must go through the Unicode path
        assert!(Matcher::new("k", Mode::Exact, true).is_match("\u{212A}"));
    }

    #[test]
    fn glob() {
        let m = Matcher::new("*@example.com", Mode::Glob, false);
        assert!(m.is_match("bob@example.com"));
        assert!(!m.is_match("bob@example.org"));
        assert!(Matcher::new("a?c*", Mode::Glob, false).is_match("abcdef"));
        assert!(!Matcher::new("a?c", Mode::Glob, false).is_match("ac"));
        assert!(Matcher::new(r"100\*", Mode::Glob, false).is_match("100*"));
        assert!(!Matcher::new(r"100\*", Mode::Glob, false).is_match("1000"));
        assert!(Matcher::new("*a*b*", Mode::Glob, false).is_match("xxaxxbxx"));
        assert!(Matcher::new("*", Mode::Glob, false).is_match(""));
        assert!(Matcher::new("A*Z", Mode::Glob, true).is_match("abcz"));
        // '?' is one character, not one byte
        assert!(Matcher::new("a?c", Mode::Glob, false).is_match("aéc"));
    }

    #[test]
    fn fuzzy_substring() {
        let m = Matcher::with_fuzzy("alice", Mode::Substring, false, Some(1));
        assert!(!m.is_match("user alcie here")); // transposition = 2 edits
        assert!(m.is_match("hello alicx!"));
        assert!(m.is_match("alce"));
        assert!(!m.is_match("bob"));
        assert!(Matcher::with_fuzzy("alice", Mode::Substring, false, Some(2)).is_match("user alcie here"));
        assert!(Matcher::with_fuzzy("ALICE", Mode::Substring, true, Some(1)).is_match("xx alicx"));
    }

    #[test]
    fn fuzzy_exact() {
        let m = Matcher::with_fuzzy("color", Mode::Exact, true, Some(1));
        assert!(m.is_match("Colour"));
        assert!(!m.is_match("my colour"));
        assert!(Matcher::with_fuzzy("café", Mode::Exact, false, Some(1)).is_match("cafe"));
        let eq = |a: char, b: char| a == b;
        assert!(levenshtein_within(&['a'], &[], 1, eq));
        assert!(!levenshtein_within(&['a', 'b', 'c'], &['x', 'y', 'z'], 2, eq));
    }
}
