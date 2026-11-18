<p align="center">
  <img src=".github/logo.svg" alt="Reverse<XML/>Grep — rxgrep" width="560">
</p>

<p align="center"><b>rxgrep</b> · Reverse XML query: give it a string, get back the XPath queries that reach it.</p>

XPath takes a **query** and gives you a **value**. `rxgrep` goes the other way:
give it a **value** and it prints the **XPath** or **CSS selector** for every
place that value appears in XML or HTML, ready to use with `xmllint`, lxml,
Saxon, browser DevTools, Playwright or any XPath/CSS engine.

```console
$ cat books.xml
<catalog>
  <book id="bk101"><title>Rust in Action</title><author>Alice</author></book>
  <book id="bk102"><title>XML &amp; You</title><author>Bob</author><editor>Alice</editor></book>
</catalog>

$ rxgrep Alice books.xml
/catalog/book[1]/author "Alice"
/catalog/book[2]/editor "Alice"

$ rxgrep bk102 books.xml
/catalog/book[2]/@id    "bk102"

$ rxgrep -S -n Alice books.xml
/catalog/book[author='Alice']
/catalog/book[editor='Alice']

$ xmllint --xpath "$(rxgrep -n -m1 Alice books.xml)" books.xml
<author>Alice</author>

$ cat shop.html
<div id=product>
  <h1>Rust in Action</h1>
  <div class=buy><button>Wishlist</button><button>Add to cart</button></div>
</div>

$ rxgrep --html -f css "Add to cart" shop.html
#product > div > button:nth-of-type(2)  "Add to cart"

$ rxgrep --html "Add to cart" shop.html
/html/body/div/div/button[2]    "Add to cart"
```

## Features

- **Every occurrence**: prints a ready-to-run query for each match in text, attributes or comments.
- **XPath or CSS** (`-f css`): `/catalog/book[2]/title` or `#main > ul > li:nth-of-type(2)`.
- **HTML mode** (`--html`): real-world HTML with unclosed tags, `<br>`, unquoted attributes and `<script>`; paths match the browser DOM (implied `<html>`, `<body>`, `<tbody>`).
- **Minimal queries**: positions like `[2]` only where same-named siblings exist; `text()[n]` only in mixed content.
- **Reorder-proof queries** (`-S`): `/catalog/book[author='Alice']` or `book:has(> price[currency="USD"])`.
- **Wildcard queries** (`-G`): `/catalog/book/author` instead of one line per element.
- **Namespaces handled**: elements in a namespace (prefixed, default `xmlns`, SVG in HTML) use `local-name()` automatically, so queries run without namespace bindings; `-N` does it for every step.
- **Embedded XML** (`-X`): searches XML stored inside text, using XPath 3.1 `parse-xml(.)`.
- **Flexible matching**: whole value (default), substring, glob, regex (`-E`), case-insensitive and typo-tolerant (`-z`).
- **Numbers** (`--gt`, `--lt`, `--range`): find values by numeric value.
- **Whole projects** (`-R`): searches directory trees, skipping hidden and dependency folders.
- **Names or values**: values by default, element and attribute names with `-k`.
- **Real XML**: entities, CDATA, comments, DOCTYPE and whitespace handled like XPath's `normalize-space()`.
- **Fast and small**: pure Rust with no dependencies, its own zero-copy XML/HTML parser and regex engine, parallel file search, one static binary.

## Install

Download the zip for your platform from the
[Releases](../../releases) page, unzip it and put `rxgrep` on your `PATH`:

| Platform | File |
|---|---|
| Linux x86_64 | `rxgrep-vX.Y.Z-x86_64-unknown-linux-musl.zip` |
| Linux arm64 | `rxgrep-vX.Y.Z-aarch64-unknown-linux-musl.zip` |
| macOS Apple Silicon | `rxgrep-vX.Y.Z-aarch64-apple-darwin.zip` |
| macOS Intel | `rxgrep-vX.Y.Z-x86_64-apple-darwin.zip` |

```sh
unzip rxgrep-vX.Y.Z-aarch64-apple-darwin.zip
sudo cp rxgrep-vX.Y.Z-aarch64-apple-darwin/rxgrep /usr/local/bin/
```

The Linux binaries are static and run on any distribution. On macOS, if the
system blocks the downloaded binary, run `xattr -d com.apple.quarantine rxgrep`.

**With Cargo** (Rust 1.82 or newer):

```sh
cargo install rxgrep
```

## Usage

```
rxgrep [OPTIONS] <PATTERN> [FILE|DIR...]
rxgrep [OPTIONS] --gt <N> [FILE|DIR...]
```

Reads standard input when no file is given (with `-R`: the current
directory). Run `rxgrep --help` for the full reference.

### Matching

| Option | Description |
|---|---|
| `-x, --exact` | Whole value must equal the pattern (default) |
| `-s, --substring` | Match anywhere inside the value |
| `-g, --glob` | Glob pattern: `*`, `?` |
| `-E, --regex` | Regular expression (`. [] \d \w \s \b ^ $ \| () * + ? {m,n}`); whole value unless `-s` |
| `-i, --ignore-case` | Case-insensitive |
| `-z, --fuzzy <N>` | Allow up to N typos |
| `-e, --regexp <P>` | Add another pattern (repeatable) |
| `-k, --keys` | Search element and attribute names instead of values (`-kv` for both) |
| `-t, --type <T,...>` | Which values: `text`, `attr`, `comment` (default `text,attr`) |
| `-X, --decode` | Also search XML stored inside text |
| `--gt` `--ge` `--lt` `--le <N>` | Value is a number above / below N (replaces PATTERN; narrow with `-e`) |
| `--range <A..B>` | Value is a number from A to B (`10..`, `..50` also work) |
| `-d, --depth <N>` | Search at most N element levels deep (alias `-L, --level`) |
| `--min-depth <N>` | Ignore matches shallower than N |

### Files and HTML

| Option | Description |
|---|---|
| `-R, --recursive` | Search directories recursively |
| `--include <GLOB>` | Only files matching GLOB (default: `.xml .xsd .xsl .svg .xhtml .rss .atom .wsdl .plist .csproj .resx .config` ...; `.html .htm` with `--html`) |
| `--exclude <GLOB>` / `--exclude-dir <GLOB>` | Skip matching files / directories |
| `--hidden` | Also search hidden files and `node_modules`, `target`, ... |
| `--no-messages` | Hide errors about unreadable or invalid files |
| `--html` | Parse as HTML (never fails on messy markup) |

### Output

| Option | Description |
|---|---|
| `-f, --format <F>` | `xpath` (default) or `css` |
| `-S, --select` | Queries that match by content, so they survive reordering |
| `-G, --generalize` | Drop positions, one line per distinct query |
| `-p, --parent` | Parent of the matched node (e.g. the `book` holding an `author`) |
| `-N, --local-name` | `local-name()` for every step, not only namespaced ones |
| `-n, --no-value` | Print only the query |
| `-r, --raw` | Print matched strings without quotes |
| `-D, --delimiter <S>` | Separate fields with S instead of aligned columns |
| `--no-align` | Single tab between fields (for `cut` / `awk`) |
| `-c, --count` | Print the number of matches |
| `-m, --max-count <N>` | Stop after N matches per file |
| `-l, --files-with-matches` | Print only the names of matching files |
| `-q, --quiet` | No output; exit status only |
| `-H, --with-filename` | Prefix lines with the file name |
| `--color <WHEN>` | `auto`, `always` or `never` |

Each line is the XPath followed by the matched value, in aligned columns.
Text is compared like `normalize-space()`: trimmed, with whitespace runs
collapsed. With several files, lines start with the file name. With several
root elements in one input (e.g. one document per line), they start with
the document number.

Exit status: `0` match found, `1` no match, `2` error.

## Examples

```sh
rxgrep -R Alice ./config                # every XML file below ./config
rxgrep -R -E 'bk\d+' .                  # regex over a whole project
rxgrep --gt 40 books.xml                # numbers greater than 40
rxgrep --range 10..50 -t attr data.xml  # attribute values between 10 and 50
rxgrep --html -f css Login page.html    # CSS selector for the "Login" element
rxgrep -R --html -k data-test -f css .  # where data-test attributes are used
rxgrep -s -i alice books.xml            # values containing "alice", any case
rxgrep -k id books.xml                  # attributes and elements named "id"
rxgrep -G -c -n USD books.xml           # wildcard query + count, e.g. /catalog/book/price/@currency  2
rxgrep -N Alice soap.xml                # works with namespaces and default namespaces
rxgrep -X 42 events.xml                 # /events/event/payload/parse-xml(.)/order/id
rxgrep -t comment -s TODO config.xml    # search comments
rxgrep -z 1 recieve app.xml             # finds "receive" despite the typo
rxgrep -d 2 error log.xml               # only look two levels deep
rxgrep -D ' | ' -r Alice books.xml      # /catalog/book[1]/author | Alice
curl -s https://example.com/feed.xml | rxgrep -s Rust
```

## Security

See [SECURITY.md](SECURITY.md) to report a vulnerability privately and to
verify release downloads (`gh attestation verify`).

## Credits

rxgrep is built by **48Labs**.

## License

Released under the [MIT License](LICENSE). The logo is set in Ubuntu Mono
(Ubuntu Font Licence 1.0).
