#!/usr/bin/env bash
# Smoke for plugins/claude/managed-settings-check.sh and managed-settings.template.json (#160h, ticket 165).
# Usage: bash plugins/claude/managed-settings-smoke.sh [case-name]
#        bash plugins/claude/managed-settings-smoke.sh --print-install
#   no argument: every case C1-C10; exit 0 only if all pass. With a case name: that case only.
#   --print-install: print the ONE operator install step in placeholder form (the public repo
#   carries no machine paths; the private items file holds the live path once).
# SMOKE_CHECK and SMOKE_TEMPLATE (test-only) point the smoke at mutant copies in a scratch
# folder, e.g. a REQUIRED_DENIES drift probe. Everything lives under one mktemp folder; a
# trap removes it even when a case fails. Uses `command grep` only; exit non-zero on any FAIL.

set -u

HERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
CHECK=${SMOKE_CHECK:-$HERE/managed-settings-check.sh}
TEMPLATE=${SMOKE_TEMPLATE:-$HERE/managed-settings.template.json}

if [ "${1:-}" = "--print-install" ]; then
  if [ "$#" -ne 1 ]; then echo "usage: managed-settings-smoke.sh --print-install" >&2; exit 2; fi
  echo 'the ONE operator step, placeholder form (run from the public repo checkout; keyboard + admin password):' >&2
  printf '%s\n' \
    '  sudo mkdir -p <managed-settings-dir>' \
    '  sudo install -o root -g wheel -m 644 plugins/claude/managed-settings.template.json <managed-settings-dir>/managed-settings.json' \
    '  bash plugins/claude/managed-settings-check.sh <managed-settings-dir>/managed-settings.json   # must print PASS, exit 0' \
    'then re-run the e0 row-9 fresh-session honour test with the real denies in place, and record the observed result beside row 9 of 160-e0-live-checks.md.' \
    'the live <managed-settings-dir> is recorded once, privately, in the items-needed row of the private repo; the public text carries placeholders only.' >&2
  exit 0
fi

command -v jq >/dev/null 2>&1 || { echo "smoke: jq is required" >&2; exit 2; }

T=$(mktemp -d "${TMPDIR:-/tmp}/160h.XXXXXX") || exit 2
cleanup() {
  case "$T" in
    */160h.*) rm -rf -- "$T" ;;
  esac
}
trap cleanup EXIT
trap 'exit 130' INT TERM

PASSN=0
FAILN=0
run_case() { # id description
  echo "== $1 $2"
}

pass() { echo "  PASS $1"; PASSN=$((PASSN + 1)); }
fail() { echo "  FAIL $1: $2" >&2; FAILN=$((FAILN + 1)); }

write_json() { # path json-text
  printf '%s\n' "$2" > "$1"
}

write_seam() { # path file-uid file-mode parent-uid parent-mode
  write_json "$1" "{\"file_uid\":$2,\"file_mode\":\"$3\",\"parent_uid\":$4,\"parent_mode\":\"$5\"}"
}

# expect_exit <id> <want> <cmd...>: run the checker and compare the exit code.
expect_exit() {
  local id=$1 want=$2 rc=0
  shift 2
  "$@" >/dev/null 2>&1 || rc=$?
  if [ "$rc" -eq "$want" ]; then
    pass "$id (exit $rc)"
  else
    fail "$id" "expected exit $want, got $rc"
  fi
}

check_ok() {   # id fixture seam -> expect 0
  expect_exit "$1" 0 bash "$CHECK" "$2" --stat "$3"
}
check_red() {  # id fixture seam -> expect 1
  expect_exit "$1" 1 bash "$CHECK" "$2" --stat "$3"
}

ROOT_SEAM=$T/seam-root.json
write_seam "$ROOT_SEAM" 0 644 0 755

# C1 template-green: the SHIPPED template + a root stat seam -> exit 0.
run_case C1 template-green
FIX=$T/c1.json
cp "$TEMPLATE" "$FIX"
check_ok C1 "$FIX" "$ROOT_SEAM"

# C2 template-agrees: the template's deny array equals the checker's REQUIRED_DENIES
# set in BOTH directions (jq set equality is symmetric); drift between the two files
# fails here, not at install time.
run_case C2 template-agrees
TMPL_LIST=$(jq -r '.permissions.deny[]' "$TEMPLATE" | sort)
CHK_LIST=$(command grep -E '^mcp__' "$CHECK" | sort)
if jq -n --arg t "$TMPL_LIST" --arg c "$CHK_LIST" \
     '($t | split("\n") | map(select(length > 0)) | sort) == ($c | split("\n") | map(select(length > 0)) | sort)' >/dev/null 2>&1; then
  pass "C2 (deny sets equal in both directions: $(printf '%s\n' "$TMPL_LIST" | tr '\n' ' '))"
else
  fail "C2" "template deny list and checker REQUIRED_DENIES have drifted"
fi

# C3 connector-named-tools-red: only full tool names (the answer-key shape) -> exit 1;
# an exact-string rule can never satisfy a namespace rule.
run_case C3 connector-named-tools-red
FIX=$T/c3.json
write_json "$FIX" '{"permissions":{"deny":["mcp__github__create_issue","mcp__github__create_pull_request","mcp__github__add_comment"]}}'
check_red C3 "$FIX" "$ROOT_SEAM"

# C4 connector-absent-red: browser families present, no mcp__github entry -> exit 1.
run_case C4 connector-absent-red
FIX=$T/c4.json
write_json "$FIX" '{"permissions":{"deny":["mcp__playwright","mcp__puppeteer","mcp__computer"]}}'
check_red C4 "$FIX" "$ROOT_SEAM"

# C5 browser-family-red: template minus ONE browser/desktop entry, each family held out
# in turn (one-at-a-time per direction) -> exit 1 every time.
run_case C5 browser-family-red
for family in mcp__playwright mcp__puppeteer mcp__computer mcp__github; do
  FIX=$T/c5-$(printf '%s' "$family" | tr -d '_').json
  jq --arg f "$family" '.permissions.deny |= (map(select(. != $f)))' "$TEMPLATE" > "$FIX"
  check_red "C5-heldout-$family" "$FIX" "$ROOT_SEAM"
done

# C6 agent-writable-red: the seam lets a non-root test prove every writability refusal arm.
run_case C6 agent-writable-red
FIX=$T/c6.json
cp "$TEMPLATE" "$FIX"
write_seam "$T/c6-file-uid.json" 501 644 0 755
check_red "C6-file-uid-501" "$FIX" "$T/c6-file-uid.json"
write_seam "$T/c6-parent-mode.json" 0 644 0 777
check_red "C6-parent-mode-777" "$FIX" "$T/c6-parent-mode.json"
write_seam "$T/c6-file-mode.json" 0 664 0 755
check_red "C6-file-mode-664" "$FIX" "$T/c6-file-mode.json"
write_seam "$T/c6-parent-uid.json" 0 644 501 755
check_red "C6-parent-uid-501" "$FIX" "$T/c6-parent-uid.json"

# C7 live-stat-red: NO seam -- the checker stats a scratch copy this runner owns
# (uid != 0, mode 600), proving the live stat path reads real ownership, not just the seam.
run_case C7 live-stat-red
FIX=$T/c7.json
cp "$TEMPLATE" "$FIX"
chmod 600 "$FIX"
expect_exit "C7-live-stat-runner-owned" 1 bash "$CHECK" "$FIX"

# C8 absent-state-red: the e0 before-file's other shape -> exit 1, never clean by default.
run_case C8 absent-state-red
FIX=$T/c8.json
write_json "$FIX" '{"state":"absent"}'
check_red C8 "$FIX" "$ROOT_SEAM"

# C9 unparseable-red: truncated JSON and a missing file both refuse.
run_case C9 unparseable-red
FIX=$T/c9.json
printf '{"permissions":{"deny":["mcp__github"' > "$FIX"
check_red "C9-truncated-json" "$FIX" "$ROOT_SEAM"
FIX=$T/c9-does-not-exist.json
rm -f "$FIX"
expect_exit "C9-missing-file" 1 bash "$CHECK" "$FIX"

# C10 usage-red: any other argument shape is exit 2, nothing run.
run_case C10 usage-red
FIX=$T/c10.json
cp "$TEMPLATE" "$FIX"
expect_exit "C10-three-args" 2 bash "$CHECK" "$FIX" a b
expect_exit "C10-bad-flag" 2 bash "$CHECK" "$FIX" --seam "$ROOT_SEAM"
expect_exit "C10-no-args" 2 bash "$CHECK"

echo
echo "smoke: $PASSN passed, $FAILN failed"
if [ "$FAILN" -gt 0 ]; then
  exit 1
fi
echo "managed-settings-smoke: ALL PASS"
exit 0
