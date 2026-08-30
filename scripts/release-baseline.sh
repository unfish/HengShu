#!/usr/bin/env bash
set -euo pipefail

out_dir="${1:-target/release-baseline}"
mkdir -p "$out_dir"
cargo build --release

measure() {
  local name="$1"
  local path="$2"
  local output
  local timing
  timing=$(mktemp)
  TIMEFORMAT='%R'
  { time output=$("$path" 版本 2>&1); } 2>"$timing" || true
  local elapsed_ms
  elapsed_ms=$(awk '{printf "%.0f", $1 * 1000}' "$timing")
  rm -f "$timing"
  local size
  size=$(wc -c < "$path" | tr -d ' ')
  printf '%s\tsize_bytes=%s\tversion_ms=%s\toutput=%s\n' "$name" "$size" "$elapsed_ms" "${output//$'\n'/ }" >> "$out_dir/measurements.tsv"
}

record_size() {
  local name="$1"
  local path="$2"
  local size
  size=$(wc -c < "$path" | tr -d ' ')
  printf '%s\tsize_bytes=%s\n' "$name" "$size" >> "$out_dir/measurements.tsv"
}

: > "$out_dir/measurements.tsv"
measure cli target/release/衡枢
if [[ -x target/release/衡枢GUI ]]; then
  record_size gui target/release/衡枢GUI
fi
printf 'rustc=%s\n' "$(rustc --version)" > "$out_dir/toolchain.txt"
printf 'cargo=%s\n' "$(cargo --version)" >> "$out_dir/toolchain.txt"
printf 'commit=%s\n' "$(git rev-parse HEAD 2>/dev/null || echo unknown)" >> "$out_dir/toolchain.txt"
printf 'baseline_dir=%s\n' "$out_dir"
