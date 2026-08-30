#!/bin/sh
# Build a declarative VSIX without requiring Node.js or vsce.
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
output=${1:-"$root/hengshu-language-0.9.0.vsix"}
case "$output" in
  /*) ;;
  *) output=$(pwd)/$output ;;
esac
stage=$(mktemp -d "${TMPDIR:-/tmp}/hengshu-vsix.XXXXXX")
trap 'rm -rf "$stage"' EXIT INT TERM

mkdir -p "$stage/extension/syntaxes" "$stage/extension/schemas"
cp "$root/package.json" "$root/language-configuration.json" "$root/README.md" "$root/.vscodeignore" "$stage/extension/"
cp "$root/syntaxes/hengshu.tmLanguage.json" "$stage/extension/syntaxes/"
cp "$root/schemas/notebook.schema.json" "$stage/extension/schemas/"
cp "$root/schemas/manifest.schema.json" "$stage/extension/schemas/"
cp "$root/vsix/[Content_Types].xml" "$stage/[Content_Types].xml"
cp "$root/vsix/extension.vsixmanifest" "$stage/extension.vsixmanifest"

mkdir -p "$(dirname -- "$output")"
(cd "$stage" && zip -q -r "$output" .)
printf '%s\n' "$output"
