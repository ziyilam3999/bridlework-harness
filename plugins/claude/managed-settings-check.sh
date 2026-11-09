#!/usr/bin/env bash
# Census settings check for the machine-wide managed-settings file (#160h, ticket 165).
# Usage: bash plugins/claude/managed-settings-check.sh <settings-file> [--stat <stat.json>]
#   any other argument count or shape: usage, exit 2.
#   --stat <stat.json> (test seam): {"file_uid":N,"file_mode":"NNN","parent_uid":N,"parent_mode":"NNN"}
#   replaces the live stat calls. No green writability fixture can be built by a non-root
#   test (a chown to root is not available to seats or CI), so the seam exists; the census
#   (#160b) passes the LIVE path, never the seam.
# Exits 0 ONLY if all of: (a) the file parses as JSON; (b) .permissions.deny is an array
# containing every REQUIRED_DENIES entry as an exact string; (c) writability: file owner
# uid 0 with no group/other write, parent folder owner uid 0 with no group/other write.
# Unreadable, missing or invalid JSON is exit 1 with a reason line, never "clean by
# default". Output prints booleans, uids and mode octets only -- never the file's contents.

set -u

# The ONE deny list, verbatim from plugins/claude/managed-settings.template.json.
# Whole tool-SERVER namespaces by prefix, never a tool-name answer key:
#   mcp__github    hosted GitHub connector namespace (default-deny, parent plan item 5)
#   mcp__playwright  browser automation (pinned in live-history allowedTools)
#   mcp__puppeteer   canonical browser MCP server
#   mcp__computer    computer-use desktop control
REQUIRED_DENIES='
mcp__github
mcp__playwright
mcp__puppeteer
mcp__computer
'

usage() {
  echo "usage: managed-settings-check.sh <settings-file> [--stat <stat.json>]" >&2
  exit 2
}

[ "$#" -eq 1 ] || { [ "$#" -eq 3 ] && [ "$2" = "--stat" ] && [ -n "$3" ] || usage; }
FILE=$1
SEAM=""
if [ "$#" -eq 3 ]; then SEAM=$3; fi

reason() { echo "managed-settings-check: FAIL: $*" >&2; exit 1; }

# (a) parse -- fail closed: unreadable, missing or invalid JSON is refusal, not clean.
[ -f "$FILE" ] || reason "settings file is missing: no managed deny is in force"
jq empty "$FILE" 2>/dev/null || reason "settings file does not parse as JSON"

# (b) deny containment -- exact string, every REQUIRED_DENIES entry present.
jq -e '.permissions.deny | type == "array"' "$FILE" >/dev/null 2>&1 \
  || reason ".permissions.deny is missing or is not an array"
for req in $REQUIRED_DENIES; do
  [ -n "$req" ] || continue
  jq -e --arg r "$req" '.permissions.deny | index($r) != null' "$FILE" >/dev/null 2>&1 \
    || reason "required deny entry absent from .permissions.deny: $req"
done
echo "deny_required=all-present"

# (c) writability -- live stat (BSD then GNU spelling) unless the --stat seam is passed.
read_pair() { # path var_uid var_mode
  local out
  if out=$(stat -f '%u %Lp' "$1" 2>/dev/null); then
    printf '%s\n' "$out"
  elif out=$(stat -c '%u %a' "$1" 2>/dev/null); then
    printf '%s\n' "$out"
  else
    reason "cannot stat $1"
  fi
}

if [ -n "$SEAM" ]; then
  jq empty "$SEAM" 2>/dev/null || reason "--stat seam does not parse as JSON"
  FILE_UID=$(jq -r '.file_uid' "$SEAM")
  FILE_MODE=$(jq -r '.file_mode' "$SEAM")
  PARENT_UID=$(jq -r '.parent_uid' "$SEAM")
  PARENT_MODE=$(jq -r '.parent_mode' "$SEAM")
else
  PAIR=$(read_pair "$FILE")
  FILE_UID=${PAIR%% *}
  FILE_MODE=${PAIR#* }
  PARENT=$(CDPATH= cd -- "$(dirname -- "$FILE")" && pwd)
  PAIR=$(read_pair "$PARENT")
  PARENT_UID=${PAIR%% *}
  PARENT_MODE=${PAIR#* }
fi
echo "file_uid=$FILE_UID"
echo "file_mode=$FILE_MODE"
echo "parent_uid=$PARENT_UID"
echo "parent_mode=$PARENT_MODE"

mode_allows_write() { # octal mode -> exit 0 if group or other has the write bit
  local m
  m=$(( 8#$1 ))
  [ $(( m & 8#022 )) -ne 0 ]
}
if mode_allows_write "$FILE_MODE"; then
  reason "settings file is group/other-writable (mode $FILE_MODE)"
fi
if mode_allows_write "$PARENT_MODE"; then
  reason "settings parent folder is group/other-writable (mode $PARENT_MODE)"
fi
[ "$FILE_UID" = "0" ] || reason "settings file owner is not root (uid $FILE_UID)"
[ "$PARENT_UID" = "0" ] || reason "settings parent folder owner is not root (uid $PARENT_UID)"

echo "writability=root-owned,not-group/other-writable"
echo "managed-settings-check: PASS"
exit 0
