#!/usr/bin/env bash
# Rust-test lint: a test must not read the deny-list env or path.
# Usage: ci/lint-rust-tests.sh [dir-or-file ...]   (default: the real tests folder)
# Exit 0 clean, 1 at least one refusal (file:line:rule printed, never content), 2 usage or missing path.
set -u
HERE="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
if [ "$#" -eq 0 ]; then
  set -- "$HERE/../core/rust/harness/tests"
fi
BAD='env::var|var_os|option_env|env![(][^)]*HARNESS|overlay::load|deny-tokens|local/scratch|operator-profile'
rc=0
for p in "$@"; do
  [ -e "$p" ] || { echo "lint-rust-tests: missing path" >&2; exit 2; }
  if [ -d "$p" ]; then
    files="$(find "$p" -type f -name '*.rs' | sort)"
  else
    files="$p"
  fi
  for f in $files; do
    out="$(awk -v pat="$BAD" '
      { s=$0; sub(/^[ \t]+/, "", s) }
      s ~ /^\/\// { next }
      s ~ pat { printf "%s:%d:reads-list-source\n", FILENAME, NR }
    ' "$f")" || { echo "lint-rust-tests: scan error" >&2; exit 2; }
    if [ -n "$out" ]; then
      printf '%s\n' "$out" | sed 's|.*/||'
      rc=1
    fi
  done
done
exit "$rc"
