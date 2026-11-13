#!/usr/bin/env bash
# Smoke for plugins/claude/session-start-hook.sh (Unix).
# Usage: bash plugins/claude/session-start-smoke.sh [case-name]
#        bash plugins/claude/session-start-smoke.sh --print-registration
#   no argument: every case; exit 0 only if all pass. With a case name: that case only.
#   --print-registration: print the ONE SessionStart entry the operator adds (writes nothing, needs no engine).
# Needs the built engine: cargo build --locked (in core/rust) first; exit 2 if it is absent.
# The hook reads two operator variables: HARNESS_BIN and HARNESS_EVENT_LOG (both absolute).
# SMOKE_HOOK and SMOKE_MANIFEST (test-only) point the smoke at another copy of the hook or manifest, e.g. a mutant.
# Everything lives under one mktemp folder; a trap removes it even when a case fails. Ids are synthetic.

HERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
ROOT=$(CDPATH= cd -- "$HERE/../.." && pwd)
HOOK_SRC=${SMOKE_HOOK:-$HERE/session-start-hook.sh}
MANIFEST=${SMOKE_MANIFEST:-$HERE/session-start-manifest.json}
BIN_SRC=$ROOT/core/rust/target/debug/harness

if [ "${1:-}" = "--print-registration" ]; then
  if [ "$#" -ne 1 ]; then echo "usage: session-start-smoke.sh --print-registration" >&2; exit 2; fi
  echo 'append this object to the `hooks.SessionStart` array of the operator settings file (print only: nothing is written).' >&2
  echo 'the session env block needs two variables: HARNESS_BIN and HARNESS_EVENT_LOG, both absolute paths.' >&2
  printf '%s\n' '{"matcher":"startup|resume|clear|compact","hooks":[{"type":"command","command":"~/.harness/claude/session-start-hook.sh"}]}'
  exit 0
fi

if [ ! -x "$BIN_SRC" ]; then
  echo "smoke: the built engine is missing; run cargo build --locked in core/rust first" >&2
  exit 2
fi
BASHBIN=$(command -v bash)
PY=$(command -v python3) || { echo "smoke: python3 is required" >&2; exit 2; }

T=$(mktemp -d "${TMPDIR:-/tmp}/p1b21.XXXXXX") || exit 2
TR=$(CDPATH= cd -P -- "$T" && pwd -P)
cleanup() { case "$T" in */p1b21.*) rm -rf -- "$T" ;; esac; }
trap cleanup EXIT
trap 'exit 130' INT TERM

ID=5c1e7a3b-9d24-4f6a-8b0e-2a7d9c4f1e65
FAILN=0
fail() { echo "  FAIL: $*" >&2; FAILN=$((FAILN + 1)); }

# stubs: one that records it ran, one that sleeps past the hook timeout, one that refuses with a path in its message
printf '#!/bin/sh\ntouch "%s/marker"\nexit 0\n' "$TR" > "$TR/stub-marker"
printf '#!/bin/sh\nsleep 24\nexit 0\n' > "$TR/stub-slow"
printf '#!/bin/sh\necho "harness: error: cannot open %s/leak.jsonl" >&2\nexit 2\n' "$TR" > "$TR/stub-leak"
chmod +x "$TR/stub-marker" "$TR/stub-slow" "$TR/stub-leak"

# run the hook: payload, bin, log ("-" = leave that variable unset); sets RC OUT ERR. PATHV overrides PATH when set.
run_hook() {
  local payload=$1 bin=$2 log=$3
  local -a e=(env)
  [ "$bin" = "-" ] && e+=(-u HARNESS_BIN)
  [ "$log" = "-" ] && e+=(-u HARNESS_EVENT_LOG)
  [ "$bin" = "-" ] || e+=("HARNESS_BIN=$bin")
  [ "$log" = "-" ] || e+=("HARNESS_EVENT_LOG=$log")
  [ -n "${PATHV:-}" ] && e+=("PATH=$PATHV")
  printf '%s' "$payload" | "${e[@]}" "$BASHBIN" "$HOOK_SRC" >"$TR/out" 2>"$TR/err"
  RC=$?
  OUT=$(cat "$TR/out"); ERR=$(head -n 1 "$TR/err")
  [ -z "$OUT" ] || fail "stdout was not empty: $OUT"
}
# expect <rc> <first stderr line, or - for empty> <log path or ->: the log must be absent after a refusal
expect() {
  [ "$RC" = "$1" ] || fail "exit $RC, wanted $1 (stderr: $ERR)"
  if [ "$2" = "-" ]; then [ -z "$ERR" ] || fail "stderr not empty: $ERR"; else [ "$ERR" = "$2" ] || fail "stderr '$ERR', wanted '$2'"; fi
  if [ "$1" != 0 ] && [ "$3" != "-" ] && [ -e "$3" ]; then fail "a refusal wrote the log"; fi
}
fresh() { L="$TR/$1.jsonl"; rm -f "$L" "$TR/marker"; }
P() { printf '{"hook_event_name":"SessionStart","source":"startup","session_id":%s}' "$1"; }
NOID_MSG='refused: the hook payload has no session id'

c_planted() {
  fresh planted
  run_hook "$(P "\"$ID\"")" "$BIN_SRC" "$L"; expect 0 - -
  local l; l=$("$BIN_SRC" events list --log "$L")
  [ "$(printf '%s\n' "$l" | wc -l | tr -d ' ')" = 1 ] || fail "not exactly one event line"
  case "$l" in *session_started*"$ID"*hook*|*session_started*hook*"$ID"*) ;; *) fail "event line lacks kind, id or source: $l" ;; esac
  "$BIN_SRC" events verify --log "$L" >/dev/null 2>&1 || fail "events verify failed"
}
c_no_id() {
  fresh noid
  run_hook '{"hook_event_name":"SessionStart","source":"startup"}' "$TR/stub-marker" "$L"; expect 2 "$NOID_MSG" "$L"
  [ ! -e "$TR/marker" ] || fail "the engine ran although the payload had no id"
}
c_empty_id() { fresh e; run_hook "$(P '""')" "$TR/stub-marker" "$L"; expect 2 "$NOID_MSG" "$L"; [ ! -e "$TR/marker" ] || fail "engine ran"; }
c_null_id() { fresh n; run_hook "$(P null)" "$TR/stub-marker" "$L"; expect 2 "$NOID_MSG" "$L"; [ ! -e "$TR/marker" ] || fail "engine ran"; }
c_number_id() { fresh num; run_hook "$(P 12345)" "$TR/stub-marker" "$L"; expect 2 "$NOID_MSG" "$L"; [ ! -e "$TR/marker" ] || fail "engine ran"; }
c_control_id() { fresh ctl; run_hook "$(P '"a\u0001b"')" "$TR/stub-marker" "$L"; expect 2 "$NOID_MSG" "$L"; [ ! -e "$TR/marker" ] || fail "engine ran"; }
c_bad_json() { fresh bj; run_hook '{"session_id": ' "$TR/stub-marker" "$L"; expect 2 'refused: the hook payload is not valid JSON' "$L"; [ ! -e "$TR/marker" ] || fail "engine ran"; }
c_not_an_object() { fresh nao; run_hook "[\"$ID\"]" "$TR/stub-marker" "$L"; expect 2 'refused: the hook payload is not an object' "$L"; }
c_wrong_event() {
  fresh we
  run_hook "{\"hook_event_name\":\"PreToolUse\",\"session_id\":\"$ID\"}" "$TR/stub-marker" "$L"; expect 2 'refused: the hook event is not SessionStart' "$L"
  [ ! -e "$TR/marker" ] || fail "engine ran"
}
c_oversize() {
  fresh big
  local pad; pad=$("$PY" -I -c 'print("A" * (1024 * 1024 + 8))')
  run_hook "{\"hook_event_name\":\"SessionStart\",\"session_id\":\"$ID\",\"pad\":\"$pad\"}" "$BIN_SRC" "$L"; expect 2 'refused: the hook payload is too large' "$L"
}
c_env() {
  fresh env
  local p; p="$(P "\"$ID\"")"
  run_hook "$p" - "$L"; expect 2 'refused: HARNESS_BIN must be set to an absolute path' "$L"
  run_hook "$p" "bin/harness" "$L"; expect 2 'refused: HARNESS_BIN must be set to an absolute path' "$L"
  run_hook "$p" "$BIN_SRC" -; expect 2 'refused: HARNESS_EVENT_LOG must be set to an absolute path' "$L"
  run_hook "$p" "$BIN_SRC" "events.jsonl"; expect 2 'refused: HARNESS_EVENT_LOG must be set to an absolute path' "$L"
  [ ! -e events.jsonl ] || fail "a relative log was written in the working folder"
}
c_engine_refuses() {
  fresh er
  run_hook "$(P '"NOT-A-UUID"')" "$BIN_SRC" "$L"; expect 2 'refused: record: harness: error: session_id must be a full lowercase uuid (8-4-4-4-12 hex; a short id is refused)' "$L"
}
c_engine_leaks_path() {
  fresh leak
  run_hook "$(P "\"$ID\"")" "$TR/stub-leak" "$L"; expect 2 'refused: the record command did not accept the session id' "$L"
}
c_timeout() {
  fresh slow
  run_hook "$(P "\"$ID\"")" "$TR/stub-slow" "$L"; expect 2 'refused: the record command could not run in time' "$L"
}
c_catch_all() {
  fresh ca
  run_hook "$(P '"\ud800"')" "$TR/stub-marker" "$L"; expect 2 'refused: internal error in the session-start hook' "$L"
}
c_py_missing() {
  fresh pm
  PATHV=/nonexistent run_hook "$(P "\"$ID\"")" "$BIN_SRC" "$L"; expect 2 'refused: python3 is missing' "$L"
}
c_manifest() {
  "$PY" -I - "$MANIFEST" "$ROOT/controls/registry.json" <<'PYEOF' || fail "manifest does not match the registry row"
import json, sys
m = json.load(open(sys.argv[1]))
rows = [r for r in json.load(open(sys.argv[2]))["controls"] if r["id"] == "session-start-hook"]
assert len(rows) == 1, "one registry row"
h = rows[0]["hook"]
assert m["event"] == h["event"] == "SessionStart", "event"
assert m["tools"] == h["tools"], "tools"
assert m["env"] == ["HARNESS_BIN", "HARNESS_EVENT_LOG"], "env"
assert m["hook"] == "session-start-hook.sh" and m["exit_allow"] == 0 and m["exit_refuse"] == 2, "shape"
PYEOF
}
c_registration() {
  bash "$0" --print-registration >"$TR/reg.out" 2>"$TR/reg.err" || fail "print-registration exited non-zero"
  "$PY" -I -c 'import json,sys; o=json.load(open(sys.argv[1])); assert o["matcher"]=="startup|resume|clear|compact" and len(o["hooks"])==1' "$TR/reg.out" || fail "registration shape"
  command grep -q 'hooks.SessionStart' "$TR/reg.err" || fail "registration omits the append sentence"
}

# mutants: every `# G:` tag in the hook, disabled, must turn the named case red. An unmapped tag fails the smoke.
declare -a CASE_OF=(
  "py_missing:py_missing" "stdin_cap:oversize" "stdin_json:bad_json" "event_allowlist:wrong_event" "id_present:null_id"
  "env_valid:env" "timeout:timeout" "engine_exit:engine_refuses" "reason_filter:engine_refuses" "catch_all:catch_all"
)
mutate() { # tag outfile
  "$PY" -I - "$HOOK_SRC" "$1" "$2" <<'PYEOF'
import re, sys
src, tag, out = sys.argv[1:4]
done = 0
res = []
for ln in open(src).read().split("\n"):
    if ("# G:" + tag) in ln and not ln.lstrip().startswith("#"):
        if re.match(r"\s*if .*:\s+# G:" + tag + r"$", ln):
            ln = re.sub(r"^(\s*)if .*:(\s+# G:" + tag + ")$", r"\1if False:\2", ln)
        elif re.match(r"\s*except .*:\s+# G:" + tag + r"$", ln):
            ln = re.sub(r"^(\s*)except .*:(\s+# G:" + tag + ")$", r"\1except ZeroDivisionError:\2", ln)
        elif "timeout=ENGINE_TIMEOUT," in ln:
            ln = ln.replace("timeout=ENGINE_TIMEOUT,", "timeout=None,")
        elif ln.startswith("if ! command -v python3"):
            ln = ln.replace("! command -v python3", "! true")
        elif ln.startswith("refuse "):
            ln = "exit 0 # G:" + tag
        else:
            sys.exit(3)
        done += 1
    res.append(ln)
if done == 0:
    sys.exit(4)
open(out, "w").write("\n".join(res))
PYEOF
}
c_mutants() {
  local tags t pair tag cs known
  tags=$(command grep -o '# G:[a-z_]*' "$HOOK_SRC" | sed 's/# G://' | sort -u)
  for t in $tags; do
    known=
    for pair in "${CASE_OF[@]}"; do
      tag=${pair%%:*}; cs=${pair#*:}
      [ "$tag" = "$t" ] || continue
      known=1
      mutate "$t" "$TR/mut-$t.sh" || { fail "cannot disable guard $t"; continue; }
      if SMOKE_HOOK="$TR/mut-$t.sh" bash "$0" "$cs" >/dev/null 2>&1; then fail "guard $t disabled and case $cs stayed green"; fi
    done
    [ -n "$known" ] || fail "guard tag $t has no mutant case"
  done
}

CASES="planted no_id empty_id null_id number_id control_id bad_json not_an_object wrong_event oversize env engine_refuses engine_leaks_path timeout catch_all py_missing manifest registration mutants"
if [ "$#" -gt 1 ]; then echo "usage: session-start-smoke.sh [case-name]" >&2; exit 2; fi
if [ "$#" = 1 ]; then
  case " $CASES " in *" ${1//-/_} "*) CASES=${1//-/_} ;; *) echo "smoke: unknown case $1" >&2; exit 2 ;; esac
fi
for c in $CASES; do
  before=$FAILN
  "c_$c"
  if [ "$FAILN" = "$before" ]; then echo "ok   $c"; else echo "FAIL $c" >&2; fi
done
[ "$FAILN" = 0 ] || { echo "session-start smoke: $FAILN failure(s)" >&2; exit 1; }
echo "session-start smoke: all passed"
