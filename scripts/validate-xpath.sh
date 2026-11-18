#!/usr/bin/env bash
# Check every XPath that rxgrep generates against a real XPath engine:
# xmllint (libxml2). Each plain query must return the matched value; each
# -S (select) query must select at least one node.
#
#   usage: scripts/validate-xpath.sh [rxgrep-binary] [file.xml ...]
#
# Defaults: target/release/rxgrep, examples/sample.xml and tests/fixtures/*.xml.
# Queries using parse-xml(.) (XPath 3.1, from -X) are skipped: libxml2
# implements XPath 1.0.
set -euo pipefail
cd "$(dirname "$0")/.."

bin="${1:-target/release/rxgrep}"
shift || true
files=("$@")
if [[ ${#files[@]} -eq 0 ]]; then
    files=(examples/sample.xml tests/fixtures/*.xml)
fi
command -v xmllint >/dev/null || { echo "error: xmllint not found (install libxml2-utils)" >&2; exit 2; }
[[ -x "$bin" ]] || { echo "error: $bin not found; run: cargo build --release" >&2; exit 2; }

norm() { tr -s ' \n\t' ' ' | sed 's/^ //; s/ $//'; }
rx() { "$bin" --no-align --color=never -r "$@"; }

pass=0
fail=0
for f in "${files[@]}"; do
    # plain queries (values and comments): string value == matched text
    while IFS=$'\t' read -r q v; do
        got=$(xmllint --xpath "normalize-space(string($q))" "$f" 2>&1 || true)
        want=$(printf '%s' "$v" | norm)
        if [[ "$got" == "$want" ]]; then
            pass=$((pass + 1))
        else
            fail=$((fail + 1))
            echo "FAIL  $f  $q  expected [$want] got [$got]"
        fi
    done < <(rx -s "" "$f" | grep -v 'parse-xml' || true; rx -t comment -s "" "$f" || true)

    # -S queries and name matches: must select at least one node
    while IFS=$'\t' read -r q _; do
        n=$(xmllint --xpath "count($q)" "$f" 2>&1 || true)
        if [[ "$n" =~ ^[0-9]+$ ]] && (( n >= 1 )); then
            pass=$((pass + 1))
        else
            fail=$((fail + 1))
            echo "FAIL  $f  $q  selected [$n]"
        fi
    done < <({ rx -S -s "" "$f"; rx -k -s "" "$f"; } | grep -v 'parse-xml' | sort -u || true)
done

echo "xmllint: $pass queries correct, $fail wrong (${#files[@]} files)"
(( fail == 0 ))
