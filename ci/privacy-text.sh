#!/bin/sh
# CI text job entry. Usage: ci/privacy-text.sh <event json file>
# Scans PR title/body, issue and comment bodies, review bodies, release notes and discussion text.
# Log lines are surface:class-id only. Same fail-closed rules as the gate. Exit 0 clean, 1 on any hit or error.
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd) || exit 1
eng="$here/../core/rust/target/release/harness"
ev="$1"
if [ -z "$ev" ] || [ ! -f "$ev" ]; then
  echo "privacy-text: event file missing" >&2
  exit 1
fi
if [ ! -x "$eng" ]; then
  echo "privacy-text: engine missing; build it first" >&2
  exit 1
fi
if [ -z "${HARNESS_PRIVACY_LIST_SECRET+x}" ]; then
  echo "privacy-text: list secret is not set" >&2
  exit 1
fi
case "$HARNESS_PRIVACY_LIST_SECRET" in
  *[![:space:]]*) ;;
  *) echo "privacy-text: list secret is empty" >&2; exit 1 ;;
esac
tmp=$(mktemp -d) || exit 1
trap 'rm -rf "$tmp"' EXIT
python3 -I - "$ev" "$tmp" <<'PY' || { echo "privacy-text: could not read the event" >&2; exit 1; }
import json, os, sys
ev = json.load(open(sys.argv[1], encoding="utf-8"))
out = sys.argv[2]
FIELDS = {"pull_request": ["title", "body"], "issue": ["title", "body"], "comment": ["body"],
          "review": ["body"], "release": ["name", "body"], "discussion": ["title", "body"]}
found = False
for key, names in FIELDS.items():
    obj = ev.get(key)
    if isinstance(obj, dict):
        found = True
        for n in names:
            v = obj.get(n)
            if isinstance(v, str):
                with open(os.path.join(out, key + "." + n), "w", encoding="utf-8") as fh:
                    fh.write(v)
if not found:
    sys.exit(3)
PY
rc=0
for f in "$tmp"/*; do
  [ -e "$f" ] || continue
  s=$(basename "$f")
  "$eng" privacy scan --text --surface "$s" < "$f" || rc=1
done
if [ "$rc" -ne 0 ]; then
  echo "privacy-text: refused" >&2
fi
exit "$rc"
