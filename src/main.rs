//! rxgrep — reverse XML query.
//!
//! Give it a string; it answers with the XPath queries that lead to every
//! occurrence of that string in your XML.

#![forbid(unsafe_code)]

mod cli;
mod matcher;
mod path;
mod regex;
mod search;
mod walk;
mod xml;

use cli::{ColorMode, Config, Format};
use matcher::Matcher;
use path::Style;
use search::{Hit, HitKind, Layout, Scope, Searcher, Sink};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{self, BufWriter, IsTerminal, Read, Write};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;

const TAB_WIDTH: usize = 8;

fn main() -> ExitCode {
    let args = std::env::args_os().skip(1).map(|a| a.to_string_lossy().into_owned());
    let cfg = match cli::parse_args(args) {
        Ok(cli::Action::Run(cfg)) => *cfg,
        Ok(cli::Action::Help) => {
            print!("{}", cli::HELP);
            return ExitCode::SUCCESS;
        }
        Ok(cli::Action::Version) => {
            println!("rxgrep {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Err(msg) => {
            eprintln!("rxgrep: {msg}");
            eprintln!("Usage: rxgrep [OPTIONS] <PATTERN> [FILE...]   (try 'rxgrep --help')");
            return ExitCode::from(2);
        }
    };
    ExitCode::from(run(&cfg))
}

// ---------------------------------------------------------------------------
// Driver
// ---------------------------------------------------------------------------

/// Result of searching one input.
struct FileOutcome {
    matches: usize,
    /// Read or parse error, already prefixed with the input name.
    error: Option<String>,
}

#[derive(Default)]
struct Status {
    any_match: bool,
    any_error: bool,
}

impl Status {
    /// Account for one finished input. Returns `false` when the run should stop.
    fn record<W: Write>(&mut self, cfg: &Config, outcome: FileOutcome, out: &mut W) -> io::Result<bool> {
        if let Some(e) = outcome.error {
            if !cfg.no_messages {
                out.flush()?; // keep stdout/stderr ordering sensible
                eprintln!("rxgrep: {}", stderr_safe(&e));
            }
            self.any_error = true;
        }
        if outcome.matches > 0 {
            self.any_match = true;
            if cfg.quiet {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

fn run(cfg: &Config) -> u8 {
    let matchers: Vec<Matcher> = if cfg.regex {
        let whole = cfg.mode == matcher::Mode::Exact;
        match cfg.patterns.iter().map(|p| Matcher::regex(p, cfg.ignore_case, whole)).collect() {
            Ok(m) => m,
            Err(e) => {
                eprintln!("rxgrep: {e}");
                return 2;
            }
        }
    } else {
        cfg.patterns.iter().map(|p| Matcher::with_fuzzy(p, cfg.mode, cfg.ignore_case, cfg.fuzzy)).collect()
    };
    let searcher = Searcher {
        matchers: &matchers,
        numeric: cfg.numeric,
        scope: cfg.scope,
        kinds: cfg.kinds,
        max_depth: cfg.max_depth,
        min_depth: cfg.min_depth,
        decode: cfg.decode,
        html: cfg.html,
        want_ids: cfg.format == Format::Css,
    };

    // Inputs: files as given; with -R, directories are expanded (default ".").
    let mut walk_errors = Vec::new();
    let given: Vec<String> = match (cfg.files.is_empty(), cfg.recursive) {
        (true, true) => vec![".".to_string()],
        (true, false) => vec!["-".to_string()],
        (false, _) => cfg.files.clone(),
    };
    let files: Vec<String> = if cfg.recursive {
        let opts = walk::WalkOptions {
            include: cfg.include.clone(),
            exclude: cfg.exclude.clone(),
            exclude_dir: cfg.exclude_dir.clone(),
            hidden: cfg.hidden,
            default_extensions: if cfg.html { walk::HTML_EXTENSIONS } else { walk::XML_EXTENSIONS },
        };
        walk::expand(&given, &opts, &mut walk_errors)
    } else {
        given
    };
    let inputs: Vec<&str> = files.iter().map(String::as_str).collect();
    let show_name = cfg.with_filename.unwrap_or(inputs.len() > 1 || cfg.recursive);
    let stdout = io::stdout();
    let paint = Paint {
        on: match cfg.color {
            ColorMode::Always => true,
            ColorMode::Never => false,
            ColorMode::Auto => stdout.is_terminal() && std::env::var_os("NO_COLOR").is_none(),
        },
        sanitize: stdout.is_terminal(),
    };
    let mut out = BufWriter::with_capacity(64 * 1024, stdout.lock());
    let mut status = Status::default();
    for e in &walk_errors {
        if !cfg.no_messages {
            eprintln!("rxgrep: {}", stderr_safe(e));
        }
        status.any_error = true;
    }
    if inputs.is_empty() {
        return if status.any_error { 2 } else { 1 };
    }
    let ctx = Ctx { cfg, searcher: &searcher, paint: &paint, show_name };

    let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).min(inputs.len());
    let result = if threads > 1 {
        run_parallel(&ctx, &inputs, threads, &mut out, &mut status)
    } else {
        run_sequential(&ctx, &inputs, &mut out, &mut status)
    }
    .and_then(|()| out.flush());

    match result {
        Ok(()) => {}
        // Reader went away (e.g. `| head`): stop quietly.
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => return if status.any_error { 2 } else { 0 },
        Err(e) => {
            eprintln!("rxgrep: write error: {e}");
            return 2;
        }
    }
    if status.any_error && !(cfg.quiet && status.any_match) {
        2
    } else if status.any_match {
        0
    } else {
        1
    }
}

/// Everything needed to process one input; shared by worker threads.
struct Ctx<'a> {
    cfg: &'a Config,
    searcher: &'a Searcher<'a>,
    paint: &'a Paint,
    show_name: bool,
}

fn run_sequential<W: Write>(ctx: &Ctx, inputs: &[&str], out: &mut W, status: &mut Status) -> io::Result<()> {
    for input in inputs {
        let outcome = process(ctx, input, out)?;
        if !status.record(ctx.cfg, outcome, out)? {
            break;
        }
    }
    Ok(())
}

/// Search several inputs on worker threads. Each worker renders a whole file
/// into a buffer; buffers are written strictly in command-line order.
fn run_parallel<W: Write>(
    ctx: &Ctx,
    inputs: &[&str],
    threads: usize,
    out: &mut W,
    status: &mut Status,
) -> io::Result<()> {
    let next = AtomicUsize::new(0);
    let stop = AtomicBool::new(false);
    let mut written = 0usize;
    let result = std::thread::scope(|scope| {
        let (tx, rx) = mpsc::channel::<(usize, io::Result<(Vec<u8>, FileOutcome)>)>();
        for _ in 0..threads {
            let (tx, next, stop) = (tx.clone(), &next, &stop);
            // A failed spawn just means fewer workers; leftovers run below.
            let _ = std::thread::Builder::new().spawn_scoped(scope, move || {
                while !stop.load(Ordering::Relaxed) {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= inputs.len() {
                        break;
                    }
                    let mut buf = Vec::new();
                    let r = process(ctx, inputs[i], &mut buf).map(|o| (buf, o));
                    if tx.send((i, r)).is_err() {
                        break;
                    }
                }
            });
        }
        drop(tx);
        let mut pending = BTreeMap::new();
        let res: io::Result<bool> = (|| {
            for (i, r) in rx.iter() {
                pending.insert(i, r);
                while let Some(r) = pending.remove(&written) {
                    written += 1;
                    let (buf, outcome) = r?;
                    out.write_all(&buf)?;
                    if !status.record(ctx.cfg, outcome, out)? {
                        return Ok(false);
                    }
                }
            }
            Ok(true)
        })();
        stop.store(true, Ordering::Relaxed);
        res
    })?;
    if result && written < inputs.len() {
        run_sequential(ctx, &inputs[written..], out, status)?;
    }
    Ok(())
}

fn process<W: Write>(ctx: &Ctx, input: &str, out: &mut W) -> io::Result<FileOutcome> {
    let name = if input == "-" { "(standard input)" } else { input };
    match load(input) {
        Ok(src) => report(ctx, name, &src, out),
        Err(e) => Ok(FileOutcome { matches: 0, error: Some(format!("{name}: {e}")) }),
    }
}

fn load(input: &str) -> Result<String, String> {
    let bytes = if input == "-" {
        let mut buf = Vec::new();
        io::stdin().read_to_end(&mut buf).map_err(|e| e.to_string())?;
        buf
    } else {
        std::fs::read(input).map_err(|e| match e.kind() {
            io::ErrorKind::NotFound => "no such file".to_string(),
            io::ErrorKind::PermissionDenied => "permission denied".to_string(),
            _ if std::path::Path::new(input).is_dir() => "is a directory (use -R to search it)".to_string(),
            _ => e.to_string(),
        })?
    };
    String::from_utf8(bytes).map_err(|_| "input is not valid UTF-8".to_string())
}

// ---------------------------------------------------------------------------
// Collecting matches
// ---------------------------------------------------------------------------

/// Unique (query, matched string) pairs in first-seen order, with occurrence counts.
#[derive(Default)]
struct Groups {
    order: Vec<(String, Option<String>, usize)>,
    index: HashMap<(String, Option<String>), usize>,
}

impl Groups {
    fn add(&mut self, query: String, text: Option<String>) {
        let key = (query, text);
        match self.index.get(&key) {
            Some(&i) => self.order[i].2 += 1,
            None => {
                self.index.insert(key.clone(), self.order.len());
                self.order.push((key.0, key.1, 1));
            }
        }
    }
}

/// Append the matched string: quoted with JSON-style escapes (`"Alice"`), so
/// it always stays on one line, unless `raw`.
fn write_matched(out: &mut String, value: &str, raw: bool) {
    if raw {
        out.push_str(value);
        return;
    }
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

fn matched_text(value: &str, raw: bool) -> String {
    let mut s = String::new();
    write_matched(&mut s, value, raw);
    s
}

/// For `--parent`: the parent of the node the plain query addresses, e.g.
/// the `book` holding a matching `author`, or the element owning a matching
/// attribute. The root element has no parent and stays as it is.
fn parent_path<'p>(path: &'p [path::Step<'p>]) -> &'p [path::Step<'p>] {
    let mut end = path.len();
    // Plain text is addressed by its element, so the element is the node.
    if matches!(path.last(), Some(path::Step::Text { mixed: false, .. })) {
        end -= 1;
    }
    if end > 1 {
        end -= 1;
    }
    &path[..end]
}

/// Receives matches from the scanner and turns them into output rows.
struct Collector<'c, 'w, W: Write> {
    cfg: &'c Config,
    style: Style,
    table: Table<'w, W>,
    listing: bool,
    grouped: bool,
    groups: Groups,
    /// With `--parent`, siblings share a query: remember what was printed.
    seen: HashSet<(String, Option<String>)>,
    /// With `-kv`, a name and a value can match with the same text, back to back.
    last: Option<(String, Option<String>)>,
    doc: usize,
    total: usize,
    stop_at: Option<usize>,
    io_error: Option<io::Error>,
}

impl<W: Write> Sink for Collector<'_, '_, W> {
    fn doc_start(&mut self, index: usize) {
        self.doc = index;
        self.seen.clear();
        self.last = None;
    }

    fn hit(&mut self, hit: &Hit) -> bool {
        self.total += 1;
        if self.listing {
            if let Err(e) = self.record(hit) {
                self.io_error = Some(e);
                return false;
            }
        }
        self.stop_at.is_none_or(|m| self.total < m)
    }
}

impl<W: Write> Collector<'_, '_, W> {
    fn record(&mut self, hit: &Hit) -> io::Result<()> {
        let cfg = self.cfg;
        let style = self.style;
        let qpath = if cfg.parent { parent_path(hit.path) } else { hit.path };

        let name_hit = hit.kind == HitKind::Name;
        // CSS shows an attribute match as [name="value"] on its element.
        let attr_value = (!name_hit && matches!(qpath.last(), Some(path::Step::Attr(_)))).then_some(hit.value);
        if self.grouped {
            let query = match (cfg.format, cfg.select) {
                (Format::XPath, true) => path::select_query(qpath, name_hit, hit.value, hit.padded, style),
                (Format::XPath, false) => path::format(qpath, style, true),
                (Format::Css, true) => path::css_select(qpath, name_hit, hit.value, style),
                (Format::Css, false) => path::css(qpath, style, true, None),
            };
            self.groups.add(query, cfg.show_value.then(|| matched_text(hit.value, cfg.raw)));
            return Ok(());
        }

        // Format straight into the table's buffer: no per-row allocations.
        let t = &mut self.table;
        t.begin_row(Some(self.doc));
        match cfg.format {
            Format::XPath => t.cell_with(Field::Query, |s| path::format_into(s, qpath, style, false)),
            Format::Css => t.cell_with(Field::Query, |s| path::css_into(s, qpath, style, false, attr_value)),
        }
        if cfg.show_value {
            t.cell_with(Field::Value, |s| write_matched(s, hit.value, cfg.raw));
        }
        if cfg.parent || cfg.scope == Scope::All {
            let key = t.last_row_key();
            let duplicate = if cfg.parent {
                !self.seen.insert(key)
            } else {
                let dup = self.last.as_ref() == Some(&key);
                self.last = Some(key);
                dup
            };
            if duplicate {
                t.discard_last_row();
                return Ok(());
            }
        }
        t.end_row()
    }
}

/// Search one input and print its results. Parse errors are reported in the
/// outcome after any matches found before the error.
fn report<W: Write>(ctx: &Ctx, name: &str, src: &str, out: &mut W) -> io::Result<FileOutcome> {
    let cfg = ctx.cfg;
    let grouped = cfg.generalize || cfg.select;
    let listing = !(cfg.quiet || cfg.files_with_matches || (cfg.count && !grouped));
    // Queries need the layout pass (positions, mixed content); it also tells
    // us up front whether the input holds several documents.
    let layout = listing.then(|| Layout::scan(src, cfg.html));
    let multi_doc = layout.as_ref().is_some_and(|l| l.roots > 1);
    let stop_at = match cfg.max_count {
        Some(m) => Some(m),
        None if cfg.quiet || cfg.files_with_matches => Some(1),
        None => None,
    };
    let table = Table::new(out, ctx.paint, &cfg.delimiter, cfg.align, ctx.show_name.then_some(name), multi_doc);
    let mut c = Collector {
        cfg,
        style: Style { local_names: cfg.local_names, lowercase: cfg.html },
        table,
        listing,
        grouped,
        groups: Groups::default(),
        seen: HashSet::new(),
        last: None,
        doc: 0,
        total: 0,
        stop_at,
        io_error: None,
    };

    let error = if stop_at == Some(0) {
        None
    } else {
        match ctx.searcher.scan(src, layout.as_ref(), &mut c) {
            Ok(_) => None,
            Err(e) => Some(format!("{name}: {e}")),
        }
    };
    if let Some(e) = c.io_error.take() {
        return Err(e);
    }
    let Collector { mut table, groups, total, .. } = c;

    if cfg.quiet {
        return Ok(FileOutcome { matches: total, error });
    }
    if cfg.files_with_matches {
        if total > 0 {
            // -l always prints the name, even for a single input.
            let mut line = String::new();
            ctx.paint.push(&mut line, Field::File, name);
            line.push('\n');
            table.out.write_all(line.as_bytes())?;
        }
    } else if grouped {
        for (query, text, n) in groups.order {
            table.begin_row(None);
            table.cell(Field::Query, &query);
            if let Some(t) = text {
                table.cell(Field::Value, &t);
            }
            if cfg.count {
                table.cell(Field::Count, &n.to_string());
            }
            table.end_row()?;
        }
    } else if cfg.count {
        table.begin_row(None);
        table.cell(Field::Count, &total.to_string());
        table.end_row()?;
    }
    table.finish(multi_doc)?;
    Ok(FileOutcome { matches: total, error })
}

// ---------------------------------------------------------------------------
// Output
// ---------------------------------------------------------------------------

/// What a column holds (decides its colour).
#[derive(Clone, Copy)]
enum Field {
    File,
    Doc,
    Query,
    Value,
    Count,
}

/// Should this character be shown as an escape on a terminal? Control
/// characters (they can move the cursor, clear the screen or retitle the
/// window) and the bidirectional overrides used in "Trojan Source" tricks.
fn is_unsafe_for_terminal(c: char) -> bool {
    c.is_control() || matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{200e}' | '\u{200f}')
}

/// Text from the input (values, keys, file names, parse errors) made safe to
/// show on a terminal: unsafe characters become visible escapes like `\x1b`.
/// Output to pipes and files is never altered, so scripts get exact bytes.
fn terminal_safe(s: &str) -> std::borrow::Cow<'_, str> {
    if !s.chars().any(is_unsafe_for_terminal) {
        return std::borrow::Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        if is_unsafe_for_terminal(c) {
            if (c as u32) <= 0xff {
                out.push_str(&format!("\\x{:02x}", c as u32));
            } else {
                out.push_str(&format!("\\u{{{:x}}}", c as u32));
            }
        } else {
            out.push(c);
        }
    }
    std::borrow::Cow::Owned(out)
}

/// Like [`terminal_safe`], for messages on stderr.
fn stderr_safe(s: &str) -> std::borrow::Cow<'_, str> {
    if io::stderr().is_terminal() {
        terminal_safe(s)
    } else {
        std::borrow::Cow::Borrowed(s)
    }
}

/// ANSI colouring that becomes a no-op when disabled.
struct Paint {
    on: bool,
    /// Output is a terminal: show unsafe characters as escapes.
    sanitize: bool,
}

impl Paint {
    fn push(&self, line: &mut String, kind: Field, s: &str) {
        let safe;
        let s = if self.sanitize {
            safe = terminal_safe(s);
            &*safe
        } else {
            s
        };
        if !self.on {
            line.push_str(s);
            return;
        }
        let code = match kind {
            Field::File => "35",
            Field::Doc => "32",
            Field::Query => "1;34",
            Field::Value => "33",
            Field::Count => "36",
        };
        line.push_str("\x1b[");
        line.push_str(code);
        line.push('m');
        line.push_str(s);
        line.push_str("\x1b[0m");
    }
}

#[derive(Clone, Copy)]
struct Row {
    doc: Option<usize>,
    first_cell: usize,
}

/// At most: file, doc, query, value, count.
const MAX_FIELDS: usize = 5;

/// Output rows for one input.
///
/// Cell text lives back to back in one `String` (cells only record their end
/// offset), so holding a large result set for alignment costs little more
/// than the text itself. Unaligned rows are written as soon as they end;
/// aligned rows are held until [`Table::finish`] knows every column width.
struct Table<'w, W: Write> {
    out: &'w mut W,
    paint: &'w Paint,
    delim: &'w str,
    align: bool,
    file: Option<&'w str>,
    multi_doc: bool,
    text: String,
    /// (kind, end offset in `text`) per cell.
    cells: Vec<(Field, usize)>,
    rows: Vec<Row>,
    /// Reused output line.
    line: String,
}

impl<'w, W: Write> Table<'w, W> {
    fn new(
        out: &'w mut W,
        paint: &'w Paint,
        delim: &'w str,
        align: bool,
        file: Option<&'w str>,
        multi_doc: bool,
    ) -> Self {
        Table {
            out,
            paint,
            delim,
            align,
            file,
            multi_doc,
            text: String::new(),
            cells: Vec::new(),
            rows: Vec::new(),
            line: String::new(),
        }
    }

    fn begin_row(&mut self, doc: Option<usize>) {
        self.rows.push(Row { doc, first_cell: self.cells.len() });
    }

    fn cell_with(&mut self, kind: Field, fill: impl FnOnce(&mut String)) {
        fill(&mut self.text);
        self.cells.push((kind, self.text.len()));
    }

    fn cell(&mut self, kind: Field, s: &str) {
        self.cell_with(kind, |t| t.push_str(s));
    }

    fn cell_text(&self, i: usize) -> &str {
        let start = if i == 0 { 0 } else { self.cells[i - 1].1 };
        &self.text[start..self.cells[i].1]
    }

    fn cell_range(&self, r: usize) -> std::ops::Range<usize> {
        let end = self.rows.get(r + 1).map_or(self.cells.len(), |n| n.first_cell);
        self.rows[r].first_cell..end
    }

    /// (query, matched string) of the row being built, for de-duplication.
    fn last_row_key(&self) -> (String, Option<String>) {
        let mut cells = self.cell_range(self.rows.len() - 1);
        let query = cells.next().map(|i| self.cell_text(i).to_string()).unwrap_or_default();
        (query, cells.next().map(|i| self.cell_text(i).to_string()))
    }

    fn discard_last_row(&mut self) {
        if let Some(row) = self.rows.pop() {
            let start = if row.first_cell == 0 { 0 } else { self.cells[row.first_cell - 1].1 };
            self.cells.truncate(row.first_cell);
            self.text.truncate(start);
        }
    }

    fn end_row(&mut self) -> io::Result<()> {
        if self.align {
            return Ok(());
        }
        let res = self.write_row(0, &[]);
        self.rows.clear();
        self.cells.clear();
        self.text.clear();
        res
    }

    /// All columns of row `r`, including file name and document number.
    fn fields<'a>(&'a self, r: usize, doc_buf: &'a mut String) -> ([(Field, &'a str); MAX_FIELDS], usize) {
        let mut fields = [(Field::Query, ""); MAX_FIELDS];
        let mut n = 0;
        if let Some(f) = self.file {
            fields[n] = (Field::File, f);
            n += 1;
        }
        if let (true, Some(d)) = (self.multi_doc, self.rows[r].doc) {
            doc_buf.clear();
            doc_buf.push_str(&(d + 1).to_string());
            fields[n] = (Field::Doc, doc_buf.as_str());
            n += 1;
        }
        for i in self.cell_range(r) {
            fields[n] = (self.cells[i].0, self.cell_text(i));
            n += 1;
        }
        (fields, n)
    }

    fn finish(mut self, multi_doc: bool) -> io::Result<()> {
        if !self.align {
            return Ok(());
        }
        self.multi_doc = multi_doc;
        let mut widths = [0usize; MAX_FIELDS];
        let mut doc_buf = String::new();
        for r in 0..self.rows.len() {
            let (fields, n) = self.fields(r, &mut doc_buf);
            for (i, (_, text)) in fields[..n.saturating_sub(1)].iter().enumerate() {
                widths[i] = widths[i].max(text.chars().count());
            }
        }
        for r in 0..self.rows.len() {
            self.write_row(r, &widths)?;
        }
        Ok(())
    }

    /// With `align`, every column except the last is padded so the next column
    /// starts at the same position on every line (query first, matched string
    /// lined up after it): with tabs for the default TAB delimiter, otherwise
    /// with spaces followed by `delim`. Without `align`, fields are joined by `delim`.
    fn write_row(&mut self, r: usize, widths: &[usize]) -> io::Result<()> {
        let mut line = std::mem::take(&mut self.line);
        line.clear();
        let mut doc_buf = String::new();
        let (fields, n) = self.fields(r, &mut doc_buf);
        for (i, &(kind, text)) in fields[..n].iter().enumerate() {
            self.paint.push(&mut line, kind, text);
            if i + 1 == n {
                break;
            }
            if !self.align {
                line.push_str(self.delim);
            } else if self.delim == "\t" {
                // Tabs needed to reach the first tab stop past the widest cell.
                let tabs = widths[i] / TAB_WIDTH + 1 - text.chars().count() / TAB_WIDTH;
                line.extend(std::iter::repeat_n('\t', tabs));
            } else {
                // Custom delimiter: pad with spaces, then the delimiter.
                let pad = widths[i] - text.chars().count();
                line.extend(std::iter::repeat_n(' ', pad));
                line.push_str(self.delim);
            }
        }
        line.push('\n');
        let res = self.out.write_all(line.as_bytes());
        self.line = line;
        res
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_safe_escapes_control_and_bidi() {
        assert!(matches!(terminal_safe("plain é 😀"), std::borrow::Cow::Borrowed(_)));
        assert_eq!(terminal_safe("a\u{1b}[2Jb"), "a\\x1b[2Jb");
        assert_eq!(terminal_safe("t\u{7}\u{9b}x"), "t\\x07\\x9bx");
        assert_eq!(terminal_safe("abc\u{202e}cba"), "abc\\u{202e}cba");
        assert_eq!(terminal_safe("tab\there"), "tab\\x09here");
    }
}
