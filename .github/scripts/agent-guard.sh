#!/usr/bin/env bash
# Mechanical guardrails for writer-agent PRs (branches agent/*).
# Usage: agent-guard.sh <base-sha> <head-sha>
set -euo pipefail

BASE="$1"
HEAD="$2"
MAX_FILES=3
MAX_LINES=299
fail=0

err() { echo "::error::$*"; fail=1; }

files=$(git diff --name-only "$BASE" "$HEAD")
file_count=$(printf '%s\n' "$files" | grep -c . || true)
if [ "$file_count" -gt "$MAX_FILES" ]; then
  err "changed files: $file_count (max $MAX_FILES)"
fi

lines=$(git diff --numstat "$BASE" "$HEAD" | awk '{a+=$1; d+=$2} END {print a+d+0}')
if [ "$lines" -gt "$MAX_LINES" ]; then
  err "changed lines: $lines (max $MAX_LINES)"
fi

forbidden='^(\.github/|AGENTS\.md$|Cargo\.toml$|Cargo\.lock$|crates/[^/]+/Cargo\.toml$|crates/[^/]+/tests/|crates/codi-core/src/(engine|reliability|mcp|improve)\.rs$)'
while IFS= read -r f; do
  [ -z "$f" ] && continue
  if printf '%s\n' "$f" | grep -Eq "$forbidden"; then
    err "forbidden path touched: $f"
  fi
done <<< "$files"

removed_tests=$(git diff -U0 "$BASE" "$HEAD" -- '*.rs' \
  | grep -E '^-[^-]' \
  | grep -E '#\[(tokio::)?test\]|assert' || true)
if [ -n "$removed_tests" ]; then
  err "existing test or assertion lines removed or changed:"
  printf '%s\n' "$removed_tests"
fi

count_tests() {
  git grep -E '#\[(tokio::)?test\]' "$1" -- '*.rs' | wc -l | tr -d ' '
}
base_tests=$(count_tests "$BASE")
head_tests=$(count_tests "$HEAD")
if [ "$head_tests" -lt "$base_tests" ]; then
  err "test count dropped: $base_tests -> $head_tests"
fi

echo "files=$file_count lines=$lines tests=$base_tests->$head_tests"
exit "$fail"
