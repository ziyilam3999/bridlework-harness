#!/bin/sh
# CI release-text entry. Usage: ci/privacy-release-text.sh <releases json file>
# The file is the GitHub releases API answer (one list, or a list of pages). Each release's name and body is scanned as text
# through ci/privacy-text.sh. Prints releases_scanned=N (a count, never text). Same fail-closed rules as the gate:
# unset or blank list secret, missing, empty or malformed file, an API error object (also one wrapped in a list), or any item
# that is not an object with an integer id or a string tag_name = FAIL.
# Zero releases is a clean pass with releases_scanned=0 (callers that need proof of a real scan check the count is >= 1).
# Exit 0 clean, 1 on any hit or error.
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd) || exit 1
f="$1"
if [ -z "$f" ] || [ ! -f "$f" ]; then
  echo "privacy-release-text: releases file missing" >&2
  exit 1
fi
if [ -z "${HARNESS_PRIVACY_LIST_SECRET+x}" ]; then
  echo "privacy-release-text: list secret is not set" >&2
  exit 1
fi
case "$HARNESS_PRIVACY_LIST_SECRET" in
  *[![:space:]]*) ;;
  *) echo "privacy-release-text: list secret is empty" >&2; exit 1 ;;
esac
tmp=$(mktemp -d) || exit 1
trap 'rm -rf "$tmp"' EXIT
python3 -I - "$f" "$tmp" <<'PY' || { echo "privacy-release-text: could not read the releases" >&2; exit 1; }
import json, os, sys
data = json.load(open(sys.argv[1], encoding="utf-8"))
if not isinstance(data, list):
    sys.exit(3)
items = []
for x in data:
    if isinstance(x, list):
        items.extend(x)
    else:
        items.append(x)
for i, r in enumerate(items):
    if not isinstance(r, dict):
        sys.exit(3)
    # a real release has an integer id or a string tag_name; an API error object ({"message": ...}) has neither
    rid = r.get("id")
    if not ((isinstance(rid, int) and not isinstance(rid, bool)) or isinstance(r.get("tag_name"), str)):
        sys.exit(3)
    rel = {}
    for k in ("name", "body"):
        v = r.get(k)
        if v is None:
            continue
        if not isinstance(v, str):
            sys.exit(3)
        rel[k] = v
    with open(os.path.join(sys.argv[2], "%06d.json" % i), "w", encoding="utf-8") as fh:
        json.dump({"release": rel}, fh)
PY
n=$(ls "$tmp" | wc -l | tr -d ' ')
rc=0
for ev in "$tmp"/*.json; do
  [ -e "$ev" ] || continue
  sh "$here/privacy-text.sh" "$ev" || rc=1
done
echo "releases_scanned=$n" >&2
if [ "$rc" -ne 0 ]; then
  echo "privacy-release-text: refused" >&2
fi
exit "$rc"
