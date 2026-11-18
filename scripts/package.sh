#!/usr/bin/env bash
# Package a release build of rxgrep into a zip with a SHA-256 checksum.
#
#   usage: scripts/package.sh <target-triple> [version]
#
#   cargo build --release --locked --target aarch64-apple-darwin
#   scripts/package.sh aarch64-apple-darwin
#     -> dist/rxgrep-v0.1.0-aarch64-apple-darwin.zip
#        dist/rxgrep-v0.1.0-aarch64-apple-darwin.zip.sha256
#
# The version defaults to the one in Cargo.toml. Prints the zip path on stdout.
set -euo pipefail

target="${1:?usage: scripts/package.sh <target-triple> [version]}"
cd "$(dirname "$0")/.."

version="${2:-v$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n 1)}"
bin="target/${target}/release/rxgrep"
if [[ ! -x "$bin" ]]; then
    echo "error: $bin not found; run: cargo build --release --locked --target $target" >&2
    exit 1
fi

name="rxgrep-${version}-${target}"
stage="dist/${name}"
rm -rf "$stage" "dist/${name}.zip" "dist/${name}.zip.sha256"
mkdir -p "$stage"

cp "$bin" "$stage/"
for doc in README.md LICENSE LICENSE-*; do
    [[ -f "$doc" ]] && cp "$doc" "$stage/"
done

# -X: no extra file attributes, so archives are reproducible across hosts.
(cd dist && zip -q -r -X "${name}.zip" "${name}" && shasum -a 256 "${name}.zip" > "${name}.zip.sha256")
rm -rf "$stage"

echo "dist/${name}.zip"
