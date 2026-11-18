//! End-to-end tests that run the compiled `rxgrep` binary.

use std::io::Write;
use std::process::{Command, Stdio};

const SAMPLE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/sample.xml");

/// Run rxgrep (unaligned, no colour) with `args`, optionally feeding `stdin`.
/// Returns (stdout, stderr, exit code).
fn rxgrep(args: &[&str], stdin: Option<&str>) -> (String, String, i32) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_rxgrep"))
        .args(args)
        .arg("--color=never")
        .arg("--no-align")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn rxgrep");
    {
        let mut si = child.stdin.take().unwrap();
        if let Some(s) = stdin {
            si.write_all(s.as_bytes()).unwrap();
        }
    }
    let out = child.wait_with_output().unwrap();
    (String::from_utf8(out.stdout).unwrap(), String::from_utf8(out.stderr).unwrap(), out.status.code().unwrap_or(-1))
}

/// Default (aligned) output.
fn aligned(args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_rxgrep")).args(args).arg("--color=never").output().expect("run rxgrep");
    String::from_utf8(out.stdout).unwrap()
}

fn stdout(args: &[&str]) -> String {
    rxgrep(args, None).0
}

fn lines(s: &str) -> Vec<&str> {
    s.lines().collect()
}

#[test]
fn finds_every_occurrence_with_minimal_xpath() {
    let (out, _, code) = rxgrep(&["Alice", SAMPLE], None);
    assert_eq!(code, 0);
    assert_eq!(
        lines(&out),
        [
            "/catalog/book[1]/author\t\"Alice\"",
            "/catalog/book[2]/editor\t\"Alice\"",
            "/catalog/book[3]/note/b\t\"Alice\"",
        ]
    );
    assert_eq!(rxgrep(&["Ali", SAMPLE], None).2, 1, "whole-string match is the default");
}

#[test]
fn aligned_columns_by_default() {
    assert_eq!(aligned(&["bk102", SAMPLE]), "/catalog/book[2]/@id\t\"bk102\"\n");
    let out = aligned(&["-s", "Acme", SAMPLE]);
    assert_eq!(out, "/catalog/@name\t\t\t\t\"Acme Books\"\n/catalog/*[local-name()='publisher']\t\"Acme Press\"\n");
}

#[test]
fn attributes_entities_cdata_and_mixed_text() {
    assert_eq!(stdout(&["bk102", SAMPLE]), "/catalog/book[2]/@id\t\"bk102\"\n");
    assert_eq!(stdout(&["XML & You", SAMPLE]), "/catalog/book[2]/title\t\"XML & You\"\n");
    assert_eq!(stdout(&["Data <Structures>", SAMPLE]), "/catalog/book[3]/title\t\"Data <Structures>\"\n");
    assert_eq!(stdout(&["in 2024", SAMPLE]), "/catalog/book[3]/note/text()[2]\t\"in 2024\"\n");
    assert_eq!(rxgrep(&["http://purl.org/dc/elements/1.1/", SAMPLE], None).2, 1, "xmlns is not an attribute");
}

#[test]
fn substring_case_fuzzy_glob() {
    assert_eq!(stdout(&["-s", "-i", "-n", "acme", SAMPLE]), "/catalog/@name\n/catalog/*[local-name()='publisher']\n");
    assert_eq!(lines(&stdout(&["-z", "1", "-n", "Alise", SAMPLE])).len(), 3);
    assert_eq!(stdout(&["-g", "*.95", "-n", SAMPLE]), "/catalog/book[1]/price\n");
    assert_eq!(
        lines(&stdout(&["-e", "Bob", "-e", "Carol", "-n", SAMPLE])),
        ["/catalog/book[2]/author", "/catalog/book[3]/author"]
    );
}

#[test]
fn names_only_with_k() {
    assert_eq!(rxgrep(&["title", SAMPLE], None).2, 1, "names are not matched without -k");
    assert_eq!(
        lines(&stdout(&["-k", "id", SAMPLE])),
        ["/catalog/book[1]/@id\t\"id\"", "/catalog/book[2]/@id\t\"id\"", "/catalog/book[3]/@id\t\"id\""]
    );
    assert_eq!(
        stdout(&["-k", "-n", "tag", SAMPLE]),
        "/catalog/book[1]/tags/tag[1]\n/catalog/book[1]/tags/tag[2]\n/catalog/book[2]/tags/tag\n"
    );
}

#[test]
fn types_comments() {
    assert_eq!(rxgrep(&["-s", "TODO", SAMPLE], None).2, 1, "comments are off by default");
    assert_eq!(stdout(&["-t", "comment", "-s", "-n", "TODO", SAMPLE]), "/catalog/book[3]/comment()[1]\n");
    assert_eq!(rxgrep(&["-t", "text", "bk102", SAMPLE], None).2, 1);
}

#[test]
fn select_generalize_parent() {
    assert_eq!(
        lines(&stdout(&["-S", "-n", "Alice", SAMPLE])),
        ["/catalog/book[author='Alice']", "/catalog/book[editor='Alice']", "/catalog/book[note/b='Alice']"]
    );
    assert_eq!(stdout(&["-S", "-n", "bk102", SAMPLE]), "/catalog/book[@id='bk102']\n");
    assert_eq!(stdout(&["-G", "-c", "-n", "USD", SAMPLE]), "/catalog/book/price/@currency\t2\n");
    assert_eq!(
        lines(&stdout(&["-p", "-n", "Alice", SAMPLE])),
        ["/catalog/book[1]", "/catalog/book[2]", "/catalog/book[3]/note"]
    );
}

#[test]
fn local_names_and_decode() {
    assert_eq!(stdout(&["-N", "-n", "Acme Press", SAMPLE]), "/*[local-name()='catalog']/*[local-name()='publisher']\n");
    assert_eq!(lines(&stdout(&["-n", "Alice", SAMPLE])).len(), 3);
    assert_eq!(
        stdout(&["-X", "-n", "-m", "9", "39.95", SAMPLE]),
        "/catalog/book[1]/price\n/catalog/events/event/payload/parse-xml(.)/order/total\n"
    );
}

#[test]
fn depth_limits() {
    assert_eq!(
        stdout(&["-d", "2", "-s", "-n", "Acme", SAMPLE]),
        "/catalog/@name\n/catalog/*[local-name()='publisher']\n"
    );
    assert_eq!(stdout(&["-d", "1", "-s", "-n", "Acme", SAMPLE]), "/catalog/@name\n");
    assert_eq!(rxgrep(&["-d", "2", "Alice", SAMPLE], None).2, 1);
    assert_eq!(stdout(&["--min-depth", "4", "-n", "Alice", SAMPLE]), "/catalog/book[3]/note/b\n");
}

#[test]
fn delimiter_raw_and_query_only() {
    assert_eq!(stdout(&["-D", " | ", "-r", "bk102", SAMPLE]), "/catalog/book[2]/@id | bk102\n");
    assert_eq!(stdout(&["-n", "bk102", SAMPLE]), "/catalog/book[2]/@id\n");
    assert_eq!(stdout(&["-H", "-D", ",", "bk102", SAMPLE]), format!("{SAMPLE},/catalog/book[2]/@id,\"bk102\"\n"));
}

#[test]
fn multiple_documents_and_stdin() {
    let (out, _, code) = rxgrep(&["x"], Some("<a>y</a>\n<a>x</a>\n"));
    assert_eq!((out.as_str(), code), ("2\t/a\t\"x\"\n", 0));
}

#[test]
fn count_quiet_files_with_matches() {
    assert_eq!(stdout(&["-c", "Alice", SAMPLE]), "3\n");
    assert_eq!(rxgrep(&["-q", "Bob", SAMPLE], None), (String::new(), String::new(), 0));
    assert_eq!(rxgrep(&["-q", "zzz", SAMPLE], None).2, 1);
    assert_eq!(stdout(&["-l", "Bob", SAMPLE]), format!("{SAMPLE}\n"));
    assert_eq!(stdout(&["-m", "1", "Alice", SAMPLE]).lines().count(), 1);
}

#[test]
fn errors_exit_2() {
    let (_, err, code) = rxgrep(&["x", "/definitely/missing.xml"], None);
    assert_eq!(code, 2);
    assert!(err.contains("no such file"));
    let (_, err, code) = rxgrep(&["a"], Some("<a>\n<b></c></a>"));
    assert_eq!(code, 2);
    assert!(err.contains("invalid XML at line 2") && err.contains("expected </b>"), "{err}");
    let (_, err, code) = rxgrep(&[], None);
    assert_eq!(code, 2);
    assert!(err.contains("missing PATTERN"));
}

#[test]
fn help_and_version() {
    let (out, _, code) = rxgrep(&["--help"], None);
    assert_eq!(code, 0);
    for needle in [
        "USAGE:",
        "--depth",
        "--select",
        "--local-name",
        "--decode",
        "--keys",
        "--delimiter",
        "--recursive",
        "--html",
        "--format",
        "--regex",
        "--range",
    ] {
        assert!(out.contains(needle), "help is missing {needle}");
    }
    assert!(stdout(&["-V"]).starts_with("rxgrep "));
}

#[test]
fn regex_matching() {
    assert_eq!(
        lines(&stdout(&["-E", "-n", r"bk\d+", SAMPLE])),
        ["/catalog/book[1]/@id", "/catalog/book[2]/@id", "/catalog/book[3]/@id"]
    );
    assert_eq!(rxgrep(&["-E", r"bk\d", SAMPLE], None).2, 1, "a regex must match the whole value by default");
    assert_eq!(stdout(&["-E", "-s", "-n", "^Rust", SAMPLE]), "/catalog/book[1]/title\n");
    assert_eq!(stdout(&["-E", "-i", "-n", "alice|carol", "-m", "9", SAMPLE]).lines().count(), 4);
    let (_, err, code) = rxgrep(&["-E", "(", SAMPLE], None);
    assert_eq!(code, 2);
    assert!(err.contains("invalid regex"));
}

#[test]
fn numeric_matching() {
    assert_eq!(stdout(&["--gt", "40", SAMPLE]), "/catalog/book[3]/price\t\"45.00\"\n");
    assert_eq!(
        lines(&stdout(&["--range", "25..40", "-n", SAMPLE])),
        ["/catalog/book[1]/price", "/catalog/book[2]/price"]
    );
    // a numeric test narrows a text pattern given with -e
    assert_eq!(stdout(&["--lt", "40", "-e", "*.95", "-g", "-n", SAMPLE]), "/catalog/book[1]/price\n");
}

#[test]
fn css_selectors() {
    // the books have ids, so selectors start there, like browser DevTools
    assert_eq!(stdout(&["-f", "css", "bk102", SAMPLE]), "#bk102\t\"bk102\"\n");
    assert_eq!(stdout(&["-f", "css", "-n", "Bob", SAMPLE]), "#bk102 > author\n");
    assert_eq!(stdout(&["-f", "css", "-n", "Acme Books", SAMPLE]), "catalog[name=\"Acme Books\"]\n");
    assert_eq!(stdout(&["-f", "css", "-G", "-c", "-n", "-s", "Al", SAMPLE]), "catalog > book > author\t1\ncatalog > book > editor\t1\ncatalog > book > note > b\t1\ncatalog > events > event > payload\t1\n");
    assert_eq!(stdout(&["-f", "css", "-S", "-n", "USD", SAMPLE]), "catalog > book:has(> price[currency=\"USD\"])\n");
    assert_eq!(stdout(&["-f", "css", "-n", "Acme Press", SAMPLE]), "catalog > publisher\n");
    assert_eq!(rxgrep(&["-f", "css", "-X", "x", SAMPLE], None).2, 2);
}

#[test]
fn html_mode() {
    let page =
        "<!doctype html><title>Shop</title><div id=main><ul><li>One<li>Two</ul><table><tr><td>A<td>B</table></div>";
    assert_eq!(rxgrep(&["-n", "Two"], Some(page)).2, 2, "not well-formed XML");
    assert_eq!(rxgrep(&["--html", "-n", "Two"], Some(page)).0, "/html/body/div/ul/li[2]\n");
    assert_eq!(rxgrep(&["--html", "-f", "css", "-n", "Two"], Some(page)).0, "#main > ul > li:nth-of-type(2)\n");
    // implied <tbody>, like a browser
    assert_eq!(rxgrep(&["--html", "-n", "B"], Some(page)).0, "/html/body/div/table/tbody/tr/td[2]\n");
    assert_eq!(rxgrep(&["--html", "-n", "Shop"], Some(page)).0, "/html/head/title\n");
}

#[test]
fn recursive_search() {
    let root = std::env::temp_dir().join(format!("rxgrep-cli-{}", std::process::id()));
    let write = |p: &str, body: &str| {
        let f = root.join(p);
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(f, body).unwrap();
    };
    write("a.xml", "<r>Alice</r>");
    write("sub/b.svg", "<svg><title>Alice</title></svg>");
    write("sub/c.txt", "<r>Alice</r>");
    write(".hidden/d.xml", "<r>Alice</r>");
    write("page.html", "<p>Alice");
    let dir = root.to_str().unwrap();
    let rel = |out: String| -> Vec<String> {
        out.lines().map(|l| l.replace(dir, "").trim_start_matches('/').to_string()).collect()
    };
    assert_eq!(rel(stdout(&["-R", "-n", "Alice", dir])), ["a.xml\t/r", "sub/b.svg\t/svg/title"]);
    assert_eq!(rel(stdout(&["-R", "-n", "--include", "*.txt", "Alice", dir])), ["sub/c.txt\t/r"]);
    assert_eq!(rel(stdout(&["-R", "-n", "--hidden", "-c", "Alice", dir])).len(), 3);
    assert_eq!(rel(stdout(&["-R", "-n", "--html", "Alice", dir])), ["page.html\t/html/body/p"]);
    let (_, err, code) = rxgrep(&["Alice", dir], None);
    assert_eq!(code, 2);
    assert!(err.contains("use -R"));
    std::fs::remove_dir_all(&root).unwrap();
}
