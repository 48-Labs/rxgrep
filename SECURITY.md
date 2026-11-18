# Security Policy

## Reporting a vulnerability

Please **do not open a public issue** for security problems.

Report them privately through GitHub instead: on the
[Security tab](https://github.com/48-Labs/rxgrep/security) choose
**Report a vulnerability**. Include the rxgrep version (`rxgrep --version`), the
command you ran and, if possible, an input file that reproduces the problem.

We fix confirmed issues in the latest release and credit reporters in the
release notes unless you prefer otherwise.

## Supported versions

Security fixes go into the latest released version. Please update before
reporting.

## Security design

rxgrep only reads its input and writes results; it never modifies files,
executes input or connects to the network.

- **Memory safety**: written in Rust with `#![forbid(unsafe_code)]`; no
  `unsafe` code anywhere.
- **No dependencies**: the XML/HTML parser, regex engine and everything else are
  part of this repository, so there is no third-party code in the binary.
- **Hostile input**: malformed input produces an error, never a crash. The XML/HTML parser is iterative (no recursion), so deeply nested input cannot overflow the stack.
  The regex engine runs in linear time, so no pattern can hang a search. The
  parsers are fuzz-tested with tens of thousands of malformed inputs.
- **Terminal safety**: when output goes to a terminal, control characters and
  bidirectional-override characters from the input (values, keys, file names,
  error messages) are shown as visible escapes like `\x1b`, so a crafted file
  cannot clear the screen, retitle the window or hide text. Output to pipes and
  files is left byte-exact for scripts.
- **Directory search** (`-R`): symbolic links to directories are not followed,
  so link loops cannot hang a search.

## Verifying a release

Every release zip comes with a SHA-256 checksum and a signed build provenance
attestation that ties it to this repository and the commit it was built from:

```sh
shasum -a 256 -c SHA256SUMS --ignore-missing
gh attestation verify rxgrep-vX.Y.Z-aarch64-apple-darwin.zip --repo 48-Labs/rxgrep
```

Release workflows use GitHub Actions pinned to full commit SHAs, run with
least-privilege permissions, and are kept up to date by Dependabot.
