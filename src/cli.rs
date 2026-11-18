//! Hand-rolled command-line parsing (no external crates).
//!
//! Supports `-abc` clusters, `-d3` / `-d 3`, `--depth=3` / `--depth 3`, and `--`.

use crate::matcher::{Mode, NumFilter};
use crate::search::{KindFilter, Scope};

pub const HELP: &str = r#"rxgrep - reverse XML query: give it a string, get back the XPath queries that reach it

USAGE:
    rxgrep [OPTIONS] <PATTERN> [FILE|DIR...]
    rxgrep [OPTIONS] -e <PATTERN> [-e <PATTERN>...] [FILE|DIR...]
    rxgrep [OPTIONS] --gt <N> [FILE|DIR...]          (numeric search, no PATTERN)

    rxgrep reads XML from each FILE (or standard input when no FILE is given,
    or FILE is '-'), finds every text node or attribute value equal to
    PATTERN, and prints a ready-to-run XPath query for each occurrence,
    followed by the matched string:

        /catalog/book[1]/author      "Alice"
        /catalog/book[2]/@id         "bk102"

MATCHING:
    -e, --regexp <PATTERN>     Add a pattern (repeatable; a hit on any pattern counts)
    -i, --ignore-case          Case-insensitive matching
    -x, --exact                The whole value must equal PATTERN (the default)
    -s, --substring            Match PATTERN anywhere inside the value instead
    -g, --glob                 PATTERN is a glob: * any run, ? one char, \ escapes.
                               Globs always match the whole value
    -E, --regex                PATTERN is a regular expression: . [] \d \w \s \b
                               ^ $ | () (?:) * + ? {m,n}. Must match the whole
                               value, like the other modes; with -s, any part
    -z, --fuzzy <N>            Tolerate up to N typos (insert/delete/substitute)
                               against the whole string (or any part with -s)
    -k, --keys                 Search element and attribute NAMES instead of
                               values. Names are never matched without this flag
    -v, --values               Search values (the default); combine with -k
                               (-kv) to search names and values together
    -t, --type <TYPES>         Which values to search, comma-separated:
                               text, attr, comment   (default: text,attr)
    -X, --decode               Also search XML stored inside text (escaped or
                               CDATA); queries use XPath 3.1 parse-xml(.)

NUMBERS:
        --gt <N>  --ge <N>     Value is a number greater than (or equal to) N
        --lt <N>  --le <N>     Value is a number less than (or equal to) N
        --range <A..B>         A <= value <= B (either side may be left out: 10..)
                               With a numeric test, every argument is a FILE;
                               add a text pattern with -e to narrow it further:
                               rxgrep --gt 40 -e '*.95' -g prices.xml

    Text and comments are matched like XPath normalize-space(): trimmed, with
    runs of whitespace collapsed to one space. CDATA and entities (&amp; &#65;)
    are decoded first. xmlns declarations are not attributes.

DEPTH (iteration level):
    -d, --depth <N>            Look at most N element levels deep
    -L, --level <N>            Same as --depth
        --min-depth <N>        Ignore matches shallower than N

                 /catalog                 depth 1
                 /catalog/book            depth 2 (with its text and attributes)
                 /catalog/book/title      depth 3

QUERY OUTPUT:
    -f, --format <FMT>         xpath (default) | css
                               css: catalog > book:nth-of-type(2) > title,
                               starting at the nearest #id; attributes become
                               [name="value"]. CSS cannot address text nodes,
                               so text matches give their element
    -S, --select               Emit content-based queries that survive
                               reordering:   /catalog/book[author='Alice']
                               (css: book[id="bk102"], book:has(> price[currency="USD"]))
    -G, --generalize           Drop positions and print each distinct
                               query/string pair once:   /catalog/book/author
    -p, --parent               Print the parent of the matched node instead,
                               e.g. the <book> that holds a matching <author>
    -N, --local-name           Namespace-agnostic queries:
                               /*[local-name()='Envelope']/*[local-name()='Body']

    Positions ([2]) are written only where same-named siblings exist. In
    mixed content (text next to child elements) text is addressed as text()[n].

FILES AND DIRECTORIES:
    -R, --recursive            Search directories recursively (default: the
                               current directory when no FILE is given)
        --include <GLOB>       Only search files whose name matches (repeatable).
                               Default: XML-type files (.xml .xsd .xsl .svg .xhtml
                               .rss .atom .wsdl .plist .csproj .resx .config ...),
                               or .html/.htm with --html
        --exclude <GLOB>       Skip files whose name matches (repeatable)
        --exclude-dir <GLOB>   Skip directories whose name matches (repeatable)
        --hidden               Also search hidden files/folders and node_modules,
                               target, __pycache__
        --no-messages          Do not print errors about unreadable or invalid files
                               (the exit status still reports them)
        --html                 Parse input as HTML: unclosed <p>/<li>/<td>, void
                               tags (<br>), unquoted attributes, <script>/<style>
                               text, any tag case. HTML never fails to parse

DISPLAY:
    -D, --delimiter <STR>      Join fields with exactly STR instead of aligned
                               tab columns. Understands \t \n \r \0 and \\
        --no-align             Single TAB between fields, no padding (for cut/awk)
        --align                With -D, pad columns with spaces before the
                               delimiter so they still line up
    -r, --raw                  Print matched strings without quotes/escapes
    -n, --no-value             Print only the query, without the matched string
    -c, --count                Print the number of matches
                               (with -G / -S: occurrences per query)
    -m, --max-count <N>        Stop after N matches per file
    -l, --files-with-matches   Only print names of files that contain a match
    -H, --with-filename        Prefix every line with the file name
        --no-filename          Never prefix the file name
        --color <WHEN>         auto (default) | always | never
    -q, --quiet                Print nothing; the exit status tells you if a match exists

    -h, --help                 Show this help
    -V, --version              Show the version

INPUT:
    Several root elements in one input (e.g. one XML document per line) are
    searched one after another; matches are then prefixed with the 1-based
    document number.

EXIT STATUS:
    0 a match was found    1 no match    2 an error occurred

EXAMPLES:
    rxgrep Alice books.xml                     /catalog/book[1]/author   "Alice"
    rxgrep -i alice books.xml                  same, any case
    rxgrep -S Alice books.xml                  /catalog/book[author='Alice']
    rxgrep -G -c -s @example.com users.xml     wildcard queries with occurrence counts
    rxgrep -k id books.xml                     attributes/elements named "id"
    rxgrep -N Alice soap.xml                   namespace-agnostic XPath
    rxgrep -X 42 events.xml                    look inside embedded XML payloads
    rxgrep -t comment TODO config.xml          search comments
    rxgrep -d 2 error log.xml                  only look two levels deep
    rxgrep -z 1 recieve app.xml                find "receive" despite the typo
    rxgrep -R Alice ./configs                  every XML file below ./configs
    rxgrep -R --include '*.xml' -E 'bk\d+' .   regex over a whole project
    rxgrep --gt 40 -t text books.xml           numbers above 40
    rxgrep --range 10..50 -k -v price.xml      values between 10 and 50
    rxgrep --html -f css Login page.html       CSS selector for an HTML element
    rxgrep -R --html -f css -k data-test src/  where data-test attributes live
    curl -s https://example.com/feed.xml | rxgrep -s Rust
    xmllint --xpath "$(rxgrep -n -m1 Alice books.xml)" books.xml
"#;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorMode {
    Auto,
    Always,
    Never,
}

/// Query language of the output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    XPath,
    Css,
}

#[derive(Debug)]
pub struct Config {
    pub patterns: Vec<String>,
    pub files: Vec<String>,
    pub ignore_case: bool,
    pub mode: Mode,
    pub fuzzy: Option<usize>,
    pub scope: Scope,
    pub kinds: KindFilter,
    pub decode: bool,
    pub max_depth: Option<usize>,
    pub min_depth: usize,
    pub local_names: bool,
    pub format: Format,
    /// Patterns are regular expressions.
    pub regex: bool,
    pub numeric: NumFilter,
    pub html: bool,
    pub recursive: bool,
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    pub exclude_dir: Vec<String>,
    pub hidden: bool,
    pub no_messages: bool,
    pub select: bool,
    pub generalize: bool,
    pub parent: bool,
    /// Print the matched string after the query.
    pub show_value: bool,
    /// Print matched strings raw (no quoting).
    pub raw: bool,
    /// Separator between output fields.
    pub delimiter: String,
    /// Line the columns up (default unless -D or --no-align).
    pub align: bool,
    pub count: bool,
    pub max_count: Option<usize>,
    pub files_with_matches: bool,
    pub with_filename: Option<bool>,
    pub color: ColorMode,
    pub quiet: bool,
}

#[derive(Debug)]
pub enum Action {
    Run(Box<Config>),
    Help,
    Version,
}

fn number(opt: &str, v: &str) -> Result<f64, String> {
    NumFilter::parse_number(v).ok_or_else(|| format!("option '{opt}' expects a number, got '{v}'"))
}

fn parse_num(opt: &str, v: &str) -> Result<usize, String> {
    v.parse::<usize>().map_err(|_| format!("option '{opt}' expects a non-negative integer, got '{v}'"))
}

/// Interpret `\t`, `\n`, `\r`, `\0` and `\\` so delimiters can be typed easily.
fn unescape(s: &str) -> String {
    let mut out = String::new();
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('t') => out.push('\t'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('0') => out.push('\0'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

fn takes_value(name: &str) -> bool {
    matches!(
        name,
        "-e" | "--regexp"
            | "--pattern"
            | "-z"
            | "--fuzzy"
            | "-t"
            | "--type"
            | "-d"
            | "--depth"
            | "-L"
            | "--level"
            | "--max-depth"
            | "--min-depth"
            | "-m"
            | "--max-count"
            | "--color"
            | "--colour"
            | "-D"
            | "--delimiter"
            | "--delim"
            | "-f"
            | "--format"
            | "--gt"
            | "--ge"
            | "--lt"
            | "--le"
            | "--range"
            | "--include"
            | "--exclude"
            | "--exclude-dir"
    )
}

pub fn parse_args<I: IntoIterator<Item = String>>(args: I) -> Result<Action, String> {
    let mut args = args.into_iter();
    let mut positionals: Vec<String> = Vec::new();
    let mut patterns: Vec<String> = Vec::new();
    let (mut substring, mut glob, mut keys, mut values) = (false, false, false, false);
    let (mut align, mut delimiter_set): (Option<bool>, bool) = (None, false);
    let mut cfg = Config {
        patterns: Vec::new(),
        files: Vec::new(),
        ignore_case: false,
        mode: Mode::Exact,
        fuzzy: None,
        scope: Scope::Values,
        kinds: KindFilter::default_kinds(),
        decode: false,
        max_depth: None,
        min_depth: 0,
        local_names: false,
        format: Format::XPath,
        regex: false,
        numeric: NumFilter::default(),
        html: false,
        recursive: false,
        include: Vec::new(),
        exclude: Vec::new(),
        exclude_dir: Vec::new(),
        hidden: false,
        no_messages: false,
        select: false,
        generalize: false,
        parent: false,
        show_value: true,
        raw: false,
        delimiter: "\t".to_string(),
        align: true,
        count: false,
        max_count: None,
        files_with_matches: false,
        with_filename: None,
        color: ColorMode::Auto,
        quiet: false,
    };

    while let Some(arg) = args.next() {
        if arg == "--" {
            positionals.extend(args.by_ref());
            break;
        }
        // Normalise into (option name, inline value) pairs.
        let mut opts: Vec<(String, Option<String>)> = Vec::new();
        if let Some(long) = arg.strip_prefix("--") {
            match long.split_once('=') {
                Some((n, v)) => opts.push((format!("--{n}"), Some(v.to_string()))),
                None => opts.push((arg.clone(), None)),
            }
        } else if arg.len() > 1 && arg.starts_with('-') {
            let cluster: Vec<char> = arg[1..].chars().collect();
            for (i, c) in cluster.iter().enumerate() {
                let name = format!("-{c}");
                if takes_value(&name) {
                    let rest: String = cluster[i + 1..].iter().collect();
                    opts.push((name, (!rest.is_empty()).then_some(rest)));
                    break;
                }
                opts.push((name, None));
            }
        } else {
            positionals.push(arg);
            continue;
        }

        for (name, inline) in opts {
            let value = if takes_value(&name) {
                match inline {
                    Some(v) => Some(v),
                    None => Some(args.next().ok_or_else(|| format!("option '{name}' requires a value"))?),
                }
            } else {
                if inline.is_some() {
                    return Err(format!("option '{name}' does not take a value"));
                }
                None
            };
            let val = value.as_deref().unwrap_or("");
            match name.as_str() {
                "-h" | "--help" => return Ok(Action::Help),
                "-V" | "--version" => return Ok(Action::Version),
                "-e" | "--regexp" | "--pattern" => patterns.push(val.to_string()),
                "-i" | "--ignore-case" => cfg.ignore_case = true,
                "-x" | "--exact" => substring = false,
                "-s" | "--substring" | "--partial" => substring = true,
                "-g" | "--glob" => glob = true,
                "-z" | "--fuzzy" => cfg.fuzzy = Some(parse_num(&name, val)?),
                "-k" | "--keys" | "--names" => keys = true,
                "-v" | "--values" => values = true,
                "-t" | "--type" => cfg.kinds = KindFilter::parse(val)?,
                "-X" | "--decode" => cfg.decode = true,
                "-d" | "--depth" | "-L" | "--level" | "--max-depth" => cfg.max_depth = Some(parse_num(&name, val)?),
                "--min-depth" => cfg.min_depth = parse_num(&name, val)?,
                "-N" | "--local-name" => cfg.local_names = true,
                "-f" | "--format" => {
                    cfg.format = match val.to_ascii_lowercase().as_str() {
                        "xpath" => Format::XPath,
                        "css" => Format::Css,
                        _ => return Err(format!("unknown format '{val}' (expected xpath or css)")),
                    }
                }
                "-E" | "--regex" | "--extended-regexp" => cfg.regex = true,
                "--gt" => cfg.numeric.above(number(&name, val)?, false),
                "--ge" => cfg.numeric.above(number(&name, val)?, true),
                "--lt" => cfg.numeric.below(number(&name, val)?, false),
                "--le" => cfg.numeric.below(number(&name, val)?, true),
                "--range" => {
                    let (lo, hi) = val
                        .split_once("..")
                        .ok_or_else(|| format!("--range expects A..B (e.g. 10..50), got '{val}'"))?;
                    if lo.trim().is_empty() && hi.trim().is_empty() {
                        return Err("--range needs at least one bound".into());
                    }
                    if !lo.trim().is_empty() {
                        cfg.numeric.above(number(&name, lo)?, true);
                    }
                    if !hi.trim().is_empty() {
                        cfg.numeric.below(number(&name, hi)?, true);
                    }
                }
                "--html" => cfg.html = true,
                "-R" | "--recursive" => cfg.recursive = true,
                "--include" => cfg.include.push(val.to_string()),
                "--exclude" => cfg.exclude.push(val.to_string()),
                "--exclude-dir" => cfg.exclude_dir.push(val.to_string()),
                "--hidden" => cfg.hidden = true,
                "--no-messages" => cfg.no_messages = true,
                "-S" | "--select" => cfg.select = true,
                "-G" | "--generalize" => cfg.generalize = true,
                "-p" | "--parent" => cfg.parent = true,
                "-w" | "--with-value" => cfg.show_value = true,
                "-n" | "--no-value" | "--query-only" => cfg.show_value = false,
                "-r" | "--raw" => cfg.raw = true,
                "-D" | "--delimiter" | "--delim" => {
                    cfg.delimiter = unescape(val);
                    delimiter_set = true;
                }
                "--no-align" => align = Some(false),
                "--align" => align = Some(true),
                "-c" | "--count" => cfg.count = true,
                "-m" | "--max-count" => cfg.max_count = Some(parse_num(&name, val)?),
                "-l" | "--files-with-matches" => cfg.files_with_matches = true,
                "-H" | "--with-filename" => cfg.with_filename = Some(true),
                "--no-filename" => cfg.with_filename = Some(false),
                "--color" | "--colour" => {
                    cfg.color = match val {
                        "auto" => ColorMode::Auto,
                        "always" => ColorMode::Always,
                        "never" => ColorMode::Never,
                        _ => return Err(format!("--color expects auto, always or never, got '{val}'")),
                    }
                }
                "-q" | "--quiet" | "--silent" => cfg.quiet = true,
                _ => return Err(format!("unknown option '{name}'")),
            }
        }
    }

    // A numeric test can stand in for PATTERN: then every argument is a file.
    if patterns.is_empty() && cfg.numeric.is_empty() {
        if positionals.is_empty() {
            return Err("missing PATTERN".into());
        }
        patterns.push(positionals.remove(0));
    }
    cfg.patterns = patterns;
    cfg.files = positionals;
    // Aligned columns by default; an explicit -D means "exactly this separator".
    cfg.align = align.unwrap_or(!delimiter_set);
    cfg.mode = if glob {
        Mode::Glob
    } else if substring {
        Mode::Substring
    } else {
        Mode::Exact
    };
    // Values only by default; names are searched only when -k asks for them.
    cfg.scope = match (keys, values) {
        (true, false) => Scope::Names,
        (true, true) => Scope::All,
        (false, _) => Scope::Values,
    };

    if glob && cfg.fuzzy.is_some() {
        return Err("--fuzzy cannot be combined with --glob".into());
    }
    if cfg.regex && (glob || cfg.fuzzy.is_some()) {
        return Err("--regex cannot be combined with --glob or --fuzzy".into());
    }
    if cfg.format == Format::Css && cfg.decode {
        return Err("--decode needs the xpath format (CSS cannot look inside text)".into());
    }
    if let (Some((lo, _)), Some((hi, _))) = (cfg.numeric.min, cfg.numeric.max) {
        if lo > hi {
            return Err(format!("numeric range is empty: {lo} > {hi}"));
        }
    }
    if cfg.select && cfg.parent {
        return Err("--select and --parent cannot be combined".into());
    }
    if let Some(max) = cfg.max_depth {
        if cfg.min_depth > max {
            return Err(format!("--min-depth ({}) is greater than --depth ({max})", cfg.min_depth));
        }
    }
    Ok(Action::Run(Box::new(cfg)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(args: &[&str]) -> Config {
        match parse_args(args.iter().map(|s| s.to_string())).unwrap() {
            Action::Run(c) => *c,
            other => panic!("expected Run, got {other:?}"),
        }
    }

    #[test]
    fn defaults() {
        let c = cfg(&["Alice", "a.xml", "b.xml"]);
        assert_eq!(c.patterns, ["Alice"]);
        assert_eq!(c.files, ["a.xml", "b.xml"]);
        assert_eq!(c.mode, Mode::Exact);
        assert_eq!(c.scope, Scope::Values);
        assert_eq!(c.kinds, KindFilter::default_kinds());
        assert!(c.show_value && c.align && !c.local_names);
        assert_eq!(c.delimiter, "\t");
    }

    #[test]
    fn clusters_and_values() {
        let c = cfg(&["-ikN", "-d3", "--min-depth=1", "-t", "text,comment", "x"]);
        assert!(c.ignore_case && c.local_names);
        assert_eq!(c.scope, Scope::Names);
        assert_eq!((c.max_depth, c.min_depth), (Some(3), 1));
        assert_eq!(c.kinds, KindFilter { text: true, attr: false, comment: true });
        assert_eq!(cfg(&["-kv", "x"]).scope, Scope::All);
        assert_eq!(cfg(&["-L", "2", "x"]).max_depth, Some(2));
        assert_eq!(cfg(&["-s", "x"]).mode, Mode::Substring);
        assert_eq!(cfg(&["-s", "-g", "x"]).mode, Mode::Glob);
    }

    #[test]
    fn output_flags() {
        let c = cfg(&["-D", " => ", "-r", "-X", "x"]);
        assert_eq!(c.delimiter, " => ");
        assert!(c.raw && c.decode && !c.align);
        assert!(cfg(&["--align", "-D,", "x"]).align);
        assert!(!cfg(&["--no-align", "x"]).align);
        assert!(!cfg(&["-n", "x"]).show_value);
        assert_eq!(cfg(&["-D", "\\t|\\n", "x"]).delimiter, "\t|\n");
    }

    #[test]
    fn patterns_and_double_dash() {
        let c = cfg(&["-e", "a", "-eb", "f.xml"]);
        assert_eq!(c.patterns, ["a", "b"]);
        assert_eq!(c.files, ["f.xml"]);
        let c = cfg(&["--", "-weird-", "-"]);
        assert_eq!(c.patterns, ["-weird-"]);
        assert_eq!(c.files, ["-"]);
    }

    #[test]
    fn new_features() {
        let c = cfg(&["-R", "--include", "*.xml", "--include=*.svg", "--exclude-dir", "gen", "--hidden", "x"]);
        assert!(c.recursive && c.hidden);
        assert_eq!(c.include, ["*.xml", "*.svg"]);
        assert_eq!(c.exclude_dir, ["gen"]);
        let c = cfg(&["--html", "-f", "css", "-E", "a.c", "page.html"]);
        assert!(c.html && c.regex);
        assert_eq!(c.format, Format::Css);
        assert_eq!(c.patterns, ["a.c"]);
        // numeric search needs no PATTERN: all positionals are files
        let c = cfg(&["--gt", "40", "--le", "100", "a.xml", "b.xml"]);
        assert!(c.patterns.is_empty());
        assert_eq!(c.files, ["a.xml", "b.xml"]);
        assert!(c.numeric.matches("41") && c.numeric.matches("100") && !c.numeric.matches("40"));
        let c = cfg(&["--range", "10..", "x.xml"]);
        assert!(c.numeric.matches("10") && c.numeric.matches("1e6") && !c.numeric.matches("9"));
        let c = cfg(&["--range=..5", "-e", "Alice", "x.xml"]);
        assert_eq!(c.patterns, ["Alice"]);
        assert_eq!(c.files, ["x.xml"]);
        assert!(c.numeric.matches("-3"));
    }

    #[test]
    fn help_and_errors() {
        assert!(matches!(parse_args(vec!["-h".to_string()]), Ok(Action::Help)));
        assert!(matches!(parse_args(vec!["--version".to_string()]), Ok(Action::Version)));
        for bad in [
            vec![],
            vec!["--nope", "x"],
            vec!["-d", "abc", "x"],
            vec!["-d"],
            vec!["-g", "-z1", "x"],
            vec!["-S", "-p", "x"],
            vec!["--min-depth", "4", "-d", "2", "x"],
            vec!["-t", "json", "x"],
            vec!["-E", "-g", "x"],
            vec!["-E", "-z", "1", "x"],
            vec!["-f", "json", "x"],
            vec!["-f", "css", "-X", "x"],
            vec!["--gt", "abc", "x"],
            vec!["--range", "5", "x"],
            vec!["--range", "..", "x"],
            vec!["--gt", "10", "--lt", "5", "x"],
            vec!["--count=yes", "x"],
        ] {
            let v: Vec<String> = bad.iter().map(|s| s.to_string()).collect();
            assert!(parse_args(v.clone()).is_err(), "should reject {v:?}");
        }
    }
}
