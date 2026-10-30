#!/usr/bin/env bash
# Fixture lint: a generator must not read the deny-list source, env or secret (or any file, env or input at all).
# Usage: ci/lint-fixtures.sh [file ...]   (default: the real generator module)
# Exit 0 clean, 1 at least one refusal (file:line:rule printed, never content), 2 usage or missing file.
set -u
HERE="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
if [ "$#" -eq 0 ]; then
  set -- "$HERE/../core/rust/harness/src/fixtures.rs"
fi
BAD='env::|std::env|option_env|env!|fs::|File::|OpenOptions|overlay::|Command|stdin|HARNESS_PRIVACY|deny-tokens|local/scratch|read_to_string|include_str|include_bytes'
rc=0
for f in "$@"; do
  [ -f "$f" ] || { echo "lint-fixtures: missing file" >&2; exit 2; }
  out="$(awk -v pat="$BAD" '
    { s=$0; sub(/^[ \t]+/, "", s) }
    s ~ /^\/\// { next }
    s ~ pat { printf "%s:%d:reads-input\n", FILENAME, NR }
  ' "$f")" || { echo "lint-fixtures: scan error" >&2; exit 2; }
  if [ -n "$out" ]; then
    printf '%s\n' "$out" | sed 's|.*/||'
    rc=1
  fi
done
exit "$rc"
