#!/usr/bin/env bash
# Smoke for plugins/claude/pre-completion-hook.sh (Unix).
# Usage: bash plugins/claude/pre-completion-smoke.sh [case-name]
#        bash plugins/claude/pre-completion-smoke.sh --print-registration
#   no argument: every case; exit 0 only if all pass. With a case name: that case only.
#   --print-registration: print the ONE PreToolUse entry the operator appends (writes nothing, needs no engine).
# Needs the built engine: cargo build --locked (in core/rust) first; exit 2 if it is absent.
# The hook reads five operator variables: HARNESS_BIN, HARNESS_TICKET_FILE, HARNESS_REPO_MAP, HARNESS_RESULT_DIR, HARNESS_CAPS_DIR.
# The smoke's HARNESS_CAPS_DIR is the same $CAPS folder its gate() passes as --caps-dir (any other folder is the W3 state).
# SMOKE_HOOK and SMOKE_MANIFEST (test-only) point the smoke at another copy of the hook or manifest, e.g. a mutant in a scratch folder.
# Everything lives under one mktemp folder; a trap removes it even when a case fails.

HERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
ROOT=$(CDPATH= cd -- "$HERE/../.." && pwd)
HOOK_SRC=${SMOKE_HOOK:-$HERE/pre-completion-hook.sh}
MANIFEST=${SMOKE_MANIFEST:-$HERE/pre-completion-manifest.json}
BIN_SRC=$ROOT/core/rust/target/debug/harness

if [ "${1:-}" = "--print-registration" ]; then
  if [ "$#" -ne 1 ]; then echo "usage: pre-completion-smoke.sh --print-registration" >&2; exit 2; fi
  echo 'append this object to the existing `hooks.PreToolUse` array of <build-launch-folder>/.claude/settings.local.json (the file the operator step names)' >&2
  printf '%s\n' \
    'this text supersedes the four-variable steps 3, 4 and 6 of the P1A-16 operator list (print only: nothing is written).' \
    'the session env block takes FIVE variables: HARNESS_BIN, HARNESS_TICKET_FILE, HARNESS_REPO_MAP, HARNESS_RESULT_DIR, HARNESS_CAPS_DIR.' \
    'HARNESS_CAPS_DIR must be the SAME folder every `ticket gate` run in that session passes as --caps-dir, and it must be outside the repo.' \
    'safe order, three steps (the old hook ignores a variable it does not know, so step 1 is harmless):' \
    '  step 1: add HARNESS_CAPS_DIR to the session env block and relaunch the build session.' \
    '  step 2: only then install the new engine AND the new ~/.harness/claude/pre-completion-hook.sh copy together, in one command line;' \
    '          replace each one atomically: write a temp name in the same folder, then rename it over the target (never copy over in place).' \
    '  step 3: run the five-variable confirm and the live probe below.' \
    'confirm, one command: d=$(mktemp -d); c=$(mktemp -d); printf %s '"'"'{"tool_name":"TaskUpdate","tool_input":{"taskId":"1","status":"completed"}}'"'"' | env HARNESS_BIN=<bin> HARNESS_TICKET_FILE=<ticket> HARNESS_REPO_MAP=<id=dir> HARNESS_RESULT_DIR="$d" HARNESS_CAPS_DIR="$c" bash ~/.harness/claude/pre-completion-hook.sh; echo "exit=$?"; rmdir "$d" "$c"' \
    '  it must print exactly one line containing `no result`, then exit=2; any other reason means a typed value is wrong.' \
    'live probe: in a freshly launched build session whose HARNESS_RESULT_DIR and HARNESS_CAPS_DIR are both empty folders, set a throwaway task completed: it must be refused with `no result`; then set it deleted.' \
    'the confirm and the live probe run no gate, so they cannot detect a caps-folder mismatch; detection is the refusal reason `cap record is missing` at the first completion after a gate run.' \
    'a session started before step 1 refuses every completion (naming HARNESS_CAPS_DIR) until it is relaunched with the fifth variable.' >&2
  printf '%s\n' '{"matcher":"TaskUpdate","hooks":[{"type":"command","command":"~/.harness/claude/pre-completion-hook.sh","timeout":120}]}'
  exit 0
fi

if [ ! -x "$BIN_SRC" ]; then
  echo "smoke: the built engine is missing; run cargo build --locked in core/rust first" >&2
  exit 2
fi
ENVBIN=/usr/bin/env
BASHBIN=$(command -v bash)
PY=$(command -v python3) || { echo "smoke: python3 is required" >&2; exit 2; }
REALGIT=$(command -v git) || { echo "smoke: git is required" >&2; exit 2; }

T=$(mktemp -d "${TMPDIR:-/tmp}/p1a16.XXXXXX") || exit 2
TR=$(CDPATH= cd -P -- "$T" && pwd -P)
cleanup() {
  case "$T" in
    */p1a16.*) [ -n "$SMOKE_REPORT" ] && printf '%s\n' "$T" > "$SMOKE_REPORT"; rm -rf -- "$T" ;;
  esac
}
trap cleanup EXIT
trap 'exit 130' INT TERM

AT=$(printf '\100')
UNSET='__UNSET__'
FAILN=0
TOTALFAIL=0
SKIPN=0
SHA40=0123456789abcdef0123456789abcdef01234567
OTHER40=89abcdef0123456789abcdef0123456789abcdef
TICKET_JSON='{"id":"T-1","repo_id":"demo","title":"t","group":"g","size":"S","objective":"o","acceptance_criteria":["a"],"dependencies":[],"risk":"normal","owner_role":"execution_verification","evidence_links":[]}'

fail() { echo "  FAIL: $*" >&2; FAILN=$((FAILN + 1)); }
skip() { echo "  SKIPPED: $*"; SKIPN=$((SKIPN + 1)); }

git_q() { git -C "$1" -c core.hooksPath=/dev/null -c commit.gpgsign=false -c user.name=t -c "user.email=t${AT}scratch" "${@:2}" >/dev/null 2>&1; }

# A scratch repo the real ticket gate can judge: head-only check docs -> p (ci/p.sh exits 0 for kind pass, 1 for kind fail).
# Commit C0 (the base), then C1 (a docs file): the gate runs over C0..C1. .gitignore names two ignorable paths.
mkgaterepo() { # dir kind
  mkdir -p "$1/gates" "$1/ci" "$1/docs"
  git init -q "$1" >/dev/null 2>&1
  printf 'ignored.txt\nignored-dir/\n' > "$1/.gitignore"
  printf '{"docs":["p"]}' > "$1/gates/path-map.json"
  printf '{"id":"p","program":"ci/p.sh","args":[],"timeout_ms":20000,"max_output_bytes":4096,"both_ends":false}' > "$1/gates/p.json"
  if [ "$2" = fail ]; then printf '#!/bin/sh\nexit 1\n' > "$1/ci/p.sh"; else printf '#!/bin/sh\nexit 0\n' > "$1/ci/p.sh"; fi
  chmod +x "$1/ci/p.sh"
  echo r > "$1/README.md"
  git_q "$1" add -A
  git_q "$1" commit -q -m c0
  BASE=$(git -C "$1" rev-parse HEAD)
  echo x > "$1/docs/x.txt"
  git_q "$1" add -A
  git_q "$1" commit -q -m c1
  HEAD1=$(git -C "$1" rev-parse HEAD)
}
commit_more() { # a new commit on top of the head (the old result is now about an older head)
  echo y > "$R/docs/y.txt"
  git_q "$R" add -A
  git_q "$R" commit -q -m c2
  HEAD2=$(git -C "$R" rev-parse HEAD)
}

# One private copy of the engine per run; each world gets a hard link to it (a plain copy where links fail).
GBIN=$TR/engine
cp "$BIN_SRC" "$GBIN" && chmod +x "$GBIN"
link_bin() { ln "$GBIN" "$1" 2>/dev/null || cp "$GBIN" "$1"; }

new_world() { # [pass|fail]
  W=$(mktemp -d "$TR/w.XXXXXX")
  EXT=$W/ext; R=$W/r; O=$W/other; RES=$W/res; CAPS=$W/caps
  mkdir -p "$EXT" "$RES" "$CAPS" "$W/home" "$O"
  mkgaterepo "$R" "${1:-pass}"
  git init -q "$O" >/dev/null 2>&1; echo o > "$O/o.txt"; git_q "$O" add -A; git_q "$O" commit -q -m o
  cp "$HOOK_SRC" "$EXT/hook.sh"; link_bin "$EXT/harness"; chmod +x "$EXT/hook.sh"
  printf '%s\n' "$TICKET_JSON" > "$EXT/ticket.json"
  H_BIN=$EXT/harness; H_TICKET=$EXT/ticket.json; H_MAP="demo=$R"; H_RES=$RES; H_CAPS=$CAPS; H_HOOK=$EXT/hook.sh
  HPATH=; HCWD=; HEXTRA=()
}

# The real gate writes the result (outside the repo). Optional argument: the exit code the gate should return (default 0).
gate() {
  local want=${1:-0} rc
  (cd "$W/home" && "$ENVBIN" -i PATH="$PATH" HOME="$W/home" "$EXT/harness" ticket gate "$EXT/ticket.json" --repo-map "demo=$R" --base "$BASE" --result-dir "$RES" --caps-dir "$CAPS" >/dev/null 2>&1)
  rc=$?
  [ "$rc" -eq "$want" ] || fail "gate run exited $rc, expected $want"
}

# Run the hook with a bounded wait (45 s): a hook that hangs is killed and the run reports 124. Own process group, so stubs die too.
PYRUN='import os, signal, subprocess, sys
cwd, fin, fout, ferr, tmo = sys.argv[1:6]
argv = sys.argv[6:]
p = subprocess.Popen(argv, cwd=cwd, stdin=open(fin, "rb"), stdout=open(fout, "wb"), stderr=open(ferr, "wb"), start_new_session=True)
try:
    rc = p.wait(timeout=float(tmo))
except subprocess.TimeoutExpired:
    try:
        os.killpg(p.pid, signal.SIGKILL)
    except OSError:
        pass
    p.wait()
    sys.exit(124)
sys.exit(rc if rc >= 0 else 125)'

run_hook_file() { # stdin-file
  local a=("PATH=${HPATH:-$PATH}" "HOME=$W/home")
  [ "$H_BIN" != "$UNSET" ] && a+=("HARNESS_BIN=$H_BIN")
  [ "$H_TICKET" != "$UNSET" ] && a+=("HARNESS_TICKET_FILE=$H_TICKET")
  [ "$H_MAP" != "$UNSET" ] && a+=("HARNESS_REPO_MAP=$H_MAP")
  [ "$H_RES" != "$UNSET" ] && a+=("HARNESS_RESULT_DIR=$H_RES")
  [ "$H_CAPS" != "$UNSET" ] && a+=("HARNESS_CAPS_DIR=$H_CAPS")
  [ "${#HEXTRA[@]}" -gt 0 ] && a+=("${HEXTRA[@]}")
  "$PY" -I -c "$PYRUN" "${HCWD:-$W/home}" "$1" "$W/out" "$W/err" 45 "$ENVBIN" -i "${a[@]}" "$BASHBIN" "$H_HOOK"
  RC=$?
}
run_hook() { printf '%s' "$1" > "$W/in"; run_hook_file "$W/in"; }

claim() { # raw JSON value for status
  run_hook "$(printf '{"tool_name":"TaskUpdate","tool_input":{"taskId":"1","status":%s}}' "$1")"
}
done_claim() { claim '"completed"'; }

expect_allow() {
  if [ "$RC" -ne 0 ] || [ -s "$W/out" ] || [ -s "$W/err" ]; then
    fail "$1: expected allow, got rc=$RC err=$(head -c 120 "$W/err" 2>/dev/null)"
  fi
}
expect_refuse() { # label [reason-substring]
  local n
  n=$(wc -l < "$W/err" | tr -d ' ')
  if [ "$RC" -ne 2 ]; then fail "$1: expected exit 2, got rc=$RC"; return; fi
  [ -s "$W/out" ] && fail "$1: stdout not empty"
  [ -s "$W/err" ] || fail "$1: no reason line"
  [ "$n" -eq 1 ] || fail "$1: reason is not exactly one line"
  if command grep -qF -- "$T" "$W/err" || command grep -qF -- "$TR" "$W/err"; then fail "$1: reason leaks a folder"; fi
  if [ -n "$2" ] && ! command grep -qF -- "$2" "$W/err"; then fail "$1: reason does not contain '$2'"; fi
}
expect_no_text() { # label text
  if command grep -qF -- "$2" "$W/err"; then fail "$1: reason echoes '$2'"; fi
}
ok_done() { done_claim; expect_allow "$1"; }
no_done() { done_claim; expect_refuse "$1" "$2"; }

stub() { # name body...   (a stand-in program in the external folder)
  local n=$1; shift
  printf '#!/bin/sh\n%s\n' "$*" > "$EXT/$n"; chmod +x "$EXT/$n"
}
good_line() { echo "ticket=T-1 repo_id=${1:-demo} head=$SHA40 last_gate=pass gate_head=$SHA40 gate_current=yes failure_class=none fix_rounds=unrecorded"; }
pass_json() { # sha -> a canonical hand-written PASS note
  printf '{"status":"pass","gate_id":"ticket-gate","head_sha":"%s","failure_class":null,"message":"m","file":null,"line":null}' "$1"
}

# ---------------------------------------------------------------- AC 1
allow_pass_current() {
  new_world; gate
  ok_done "1 PASS current, clean"
}

# ---------------------------------------------------------------- AC 2
refuse_fail_result() {
  new_world; gate
  ok_done twin-pass
  new_world fail; gate 1
  no_done "2 FAIL result" fail
}

# ---------------------------------------------------------------- AC 3
refuse_stale_result() {
  new_world; gate
  ok_done twin-pass-at-c1
  commit_more
  no_done "3 PASS at C1, head is C2" stale
  gate
  ok_done "3 PASS re-run at C2"
}

# ---------------------------------------------------------------- AC 4
refuse_no_result() {
  new_world; gate
  ok_done twin-with-result
  new_world
  no_done "4 no result file" "no result"
}

# ---------------------------------------------------------------- AC 5
refuse_bad_result_files() {
  new_world; gate
  ok_done twin-real-pass
  local f=$RES/T-1.json big
  pass_json "$HEAD1" > "$f"; ok_done twin-handwritten-pass
  printf '{"status":"pass"' > "$f"; no_done malformed
  { printf '{"status":"pass","gate_id":"ticket-gate","head_sha":"%s","failure_class":null,"message":"' "$HEAD1"; head -c 1048577 /dev/zero | tr '\0' x; printf '","file":null,"line":null}'; } > "$f"; no_done over-1MiB
  { printf '{"status":"pass","gate_id":"ticket-gate","head_sha":"%s","failure_class":null,"message":"a\377b","file":null,"line":null}' "$HEAD1"; } > "$f"; no_done non-utf8
  printf '{"status":"maybe","gate_id":"ticket-gate","head_sha":"%s","failure_class":null,"message":"m","file":null,"line":null}' "$HEAD1" > "$f"; no_done unknown-status
  printf '{"status":"pass","gate_id":"ticket-gate","failure_class":null,"message":"m","file":null,"line":null}' > "$f"; no_done missing-head-sha
  printf '{"status":"pass","status":"pass","gate_id":"ticket-gate","head_sha":"%s","failure_class":null,"message":"m","file":null,"line":null}' "$HEAD1" > "$f"; no_done duplicate-key
  pass_json "$HEAD1" > "$f"; ok_done twin-restored
  # stand-in engines: a valid PASS-current line, then a bad exit
  stub exit0 "echo '$(good_line)'"; H_BIN=$EXT/exit0; ok_done twin-stub-exit-0
  stub exit1good "echo '$(good_line)'; exit 1"; H_BIN=$EXT/exit1good; no_done stub-valid-line-exit-1
  stub exit2good "echo '$(good_line)'; exit 2"; H_BIN=$EXT/exit2good; no_done stub-valid-line-exit-2
  stub killedgood "echo '$(good_line)'; kill -9 \$\$"; H_BIN=$EXT/killedgood; no_done stub-valid-line-killed
}

# ---------------------------------------------------------------- AC 6
refuse_linked_result() {
  new_world; gate
  ok_done twin-regular-file
  ln -s "$RES" "$EXT/reslink"; H_RES=$EXT/reslink; no_done result-dir-symlink
  H_RES=$RES
  mv "$RES/T-1.json" "$EXT/real-pass.json"
  ln -s "$EXT/real-pass.json" "$RES/T-1.json"; no_done result-file-symlink
  mv "$RES/T-1.json" "$W/moved-link"
  mkdir "$RES/T-1.json"; no_done result-target-directory
}

# ---------------------------------------------------------------- AC 7
refuse_case_twin() {
  new_world
  mkdir -p "$W/cs"; : > "$W/cs/lower"
  if [ -e "$W/cs/LOWER" ]; then skip "case row (case-insensitive scratch volume)"; return; fi
  printf '%s\n' "$TICKET_JSON" | sed 's/"T-1"/"Ex-1"/' > "$EXT/ex.json"
  H_TICKET=$EXT/ex.json
  pass_json "$HEAD1" > "$RES/ex-1.json"
  no_done case-twin-result ""
  mv "$RES/ex-1.json" "$W/moved-ex"
  pass_json "$HEAD1" > "$RES/Ex-1.json"
  no_done "exact-case note without a cap record" "cap record is missing"
  printf '{"v":1,"ticket":"Ex-1","repo_id":"demo","fix_rounds":0,"in_flight":false}\n' > "$CAPS/Ex-1.rounds"
  ok_done exact-case-twin
}

# ---------------------------------------------------------------- AC 8
dirty_world() { new_world; gate; }
refuse_dirty_tree() {
  dirty_world
  ok_done "8i fully clean"
  dirty_world; : > "$R/ignored.txt"; mkdir "$R/ignored-dir"; : > "$R/ignored-dir/f"
  ok_done "8h ignored files present"
  dirty_world; echo more >> "$R/docs/x.txt"
  no_done "8a tracked file modified" uncommitted
  dirty_world; echo more >> "$R/docs/x.txt"; git_q "$R" add docs/x.txt
  no_done "8b change staged only" uncommitted
  dirty_world; mv "$R/docs/x.txt" "$W/gone.txt"
  no_done "8c tracked file deleted" uncommitted
  dirty_world; echo n > "$R/new.txt"
  no_done "8d untracked at the top" uncommitted
  dirty_world; mkdir -p "$R/sub/deep"; echo n > "$R/sub/deep/new.txt"
  no_done "8e untracked in a subfolder" uncommitted
  # 8f: untracked only, and a global git config that hides every untracked file leaks in through GIT_CONFIG_GLOBAL
  dirty_world; echo n > "$R/new.txt"
  printf '*\n' > "$EXT/ign"; printf '[core]\n\texcludesFile = %s/ign\n' "$EXT" > "$EXT/gitcfg"
  HEXTRA=("GIT_CONFIG_GLOBAL=$EXT/gitcfg")
  no_done "8f untracked only, GIT_CONFIG_GLOBAL set" uncommitted
  # 8f2: twin, GIT_DIR / GIT_WORK_TREE aimed at a clean other repo while the mapped repo is dirty
  dirty_world; echo n > "$R/new.txt"
  HEXTRA=("GIT_DIR=$O/.git" "GIT_WORK_TREE=$O")
  no_done "8f2 GIT_DIR and GIT_WORK_TREE at another repo"
  # 8g: git status fails (only that call); everything else reaches the real git
  dirty_world
  mkdir -p "$W/stubbin"
  printf '#!/bin/sh\nfor a in "$@"; do [ "$a" = status ] && exit 1; done\nexec %s "$@"\n' "$REALGIT" > "$W/stubbin/git"; chmod +x "$W/stubbin/git"
  HPATH=$W/stubbin:$PATH
  no_done "8g git status fails" "git status failed"
}

# ---------------------------------------------------------------- AC 9
refuse_completion_spellings() {
  new_world fail; gate 1
  local raw
  for raw in '"Completed"' '" completed"' '"completed\n"' '"done"' '"complete"' '"COMPLETED"' 'null' '1' '["completed"]'; do
    claim "$raw"; expect_refuse "9 status $raw" fail
  done
  claim '"completed"'; expect_refuse "9 status completed" fail
  for raw in '"pending"' '"in_progress"' '"deleted"'; do
    claim "$raw"; expect_allow "9 twin status $raw"
  done
  run_hook '{"tool_name":"TaskUpdate","tool_input":{"taskId":"1"}}'; expect_allow "9 twin no status key"
}

# ---------------------------------------------------------------- AC 10
refuse_other_tools() {
  new_world; gate
  ok_done twin-taskupdate
  local t
  for t in Bash Edit Read; do
    run_hook "{\"tool_name\":\"$t\",\"tool_input\":{\"status\":\"completed\"}}"; expect_refuse "10 tool $t" "not TaskUpdate"
  done
  run_hook '{"tool_name":"","tool_input":{"status":"completed"}}'; expect_refuse "10 tool empty" "not TaskUpdate"
  run_hook '{"tool_input":{"status":"completed"}}'; expect_refuse "10 tool absent" "not TaskUpdate"
  run_hook '{"tool_name":"Bash","tool_input":{"command":"ls"}}'; expect_refuse "10 tool Bash no status" "not TaskUpdate"
}

# ---------------------------------------------------------------- AC 11
refuse_bad_stdin() {
  new_world; gate
  ok_done twin-valid-call
  run_hook ''; expect_refuse "11 empty stdin" "not valid JSON"
  run_hook 'this is not json'; expect_refuse "11 not JSON" "not valid JSON"
  run_hook '[1,2]'; expect_refuse "11 JSON array" "not an object"
  local pre suf padn
  pre='{"tool_name":"TaskUpdate","tool_input":{"taskId":"1","status":"completed"},"pad":"'; suf='"}'
  padn=$((1048576 - ${#pre} - ${#suf}))
  { printf '%s' "$pre"; head -c "$padn" /dev/zero | tr '\0' x; printf '%s' "$suf"; } > "$W/big"
  run_hook_file "$W/big"; expect_allow exactly-1MiB
  { printf '%s' "$pre"; head -c "$((padn + 1))" /dev/zero | tr '\0' x; printf '%s' "$suf"; } > "$W/big"
  run_hook_file "$W/big"; expect_refuse over-1MiB "too large"
}

# ---------------------------------------------------------------- AC 12 / 21
extra_keys_do_not_decide() {
  local extra='"cwd":"/elsewhere","head":"0000000000000000000000000000000000000000","ticket_id":"T-9","result":"pass"'
  local call="{\"tool_name\":\"TaskUpdate\",$extra,\"tool_input\":{\"taskId\":\"1\",\"status\":\"completed\",\"metadata\":{\"status\":\"pass\"}}}"
  new_world fail; gate 1
  run_hook "$call"; expect_refuse "12 extra keys over FAIL" fail
  HCWD=$O; run_hook "$call"; expect_refuse "12 extra keys over FAIL, other cwd" fail
  new_world; gate
  run_hook "$call"; expect_allow "12 extra keys over PASS"
  HCWD=$O; run_hook "$call"; expect_allow "12 extra keys over PASS, other cwd"
}
other_task_same_verdict() {
  local other='{"tool_name":"TaskUpdate","tool_input":{"taskId":"99","subject":"another job","status":"completed"}}'
  local named='{"tool_name":"TaskUpdate","tool_input":{"taskId":"1","subject":"T-1 the ticket task","status":"completed"}}'
  new_world fail; gate 1
  run_hook "$other"; expect_refuse "21 other task over FAIL" fail
  run_hook "$named"; expect_refuse "21 twin: ticket-named task over FAIL" fail
  new_world; gate
  run_hook "$other"; expect_allow "21 other task over PASS"
  run_hook "$named"; expect_allow "21 twin: ticket-named task over PASS"
}

# ---------------------------------------------------------------- AC 13
refuse_inputs_inside_repo() {
  new_world; gate
  ok_done twin-outside
  # engine inside the repo, and an outside link pointing inside
  link_bin "$R/harness"; H_BIN=$R/harness; no_done "13 engine inside" HARNESS_BIN
  ln -s "$R/harness" "$EXT/binlink"; H_BIN=$EXT/binlink; no_done "13 engine link into repo" HARNESS_BIN
  H_BIN=$EXT/harness
  # ticket file inside the repo, and a link
  cp "$EXT/ticket.json" "$R/ticket.json"; H_TICKET=$R/ticket.json; no_done "13 ticket inside" HARNESS_TICKET_FILE
  ln -s "$R/ticket.json" "$EXT/ticketlink"; H_TICKET=$EXT/ticketlink; no_done "13 ticket link into repo" HARNESS_TICKET_FILE
  H_TICKET=$EXT/ticket.json
  # the hook itself inside the repo
  cp "$HOOK_SRC" "$R/hook.sh"; H_HOOK=$R/hook.sh; no_done "13 hook inside" hook
  # result dir inside the repo: the real engine refuses first, so a permissive stand-in prints a valid PASS-current line
  new_world
  stub permissive "echo '$(good_line)'"; H_BIN=$EXT/permissive
  ok_done twin-stand-in-outside
  new_world; gate; ok_done twin-after-spoofs
  new_world
  stub permissive "echo '$(good_line)'"; H_BIN=$EXT/permissive
  mkdir "$R/results"; H_RES=$R/results; no_done "13 result dir inside" HARNESS_RESULT_DIR
  ln -s "$R/results" "$EXT/reslink"; H_RES=$EXT/reslink; no_done "13 result dir link into repo" HARNESS_RESULT_DIR
  # caps dir inside the repo (an EMPTY folder, so the tree check cannot refuse for it), and a link into the repo
  H_RES=$RES
  ok_done twin-caps-outside
  mkdir "$R/capsdir"; H_CAPS=$R/capsdir; no_done "13 caps dir inside" HARNESS_CAPS_DIR
  ln -s "$R/capsdir" "$EXT/capslink"; H_CAPS=$EXT/capslink; no_done "13 caps dir link into repo" HARNESS_CAPS_DIR
  H_CAPS=$CAPS
}

# ---------------------------------------------------------------- AC 14
refuse_bad_env() {
  new_world; gate
  ok_done twin-complete-env
  local v bad ctl
  ctl=$(printf '/x\001y')
  for bad in "$UNSET" "" "relative" "$ctl"; do
    H_BIN=$bad; no_done "14 HARNESS_BIN '$bad'" HARNESS_BIN; H_BIN=$EXT/harness
    H_TICKET=$bad; no_done "14 HARNESS_TICKET_FILE '$bad'" HARNESS_TICKET_FILE; H_TICKET=$EXT/ticket.json
    H_RES=$bad; no_done "14 HARNESS_RESULT_DIR '$bad'" HARNESS_RESULT_DIR; H_RES=$RES
    H_CAPS=$bad; no_done "14 HARNESS_CAPS_DIR '$bad'" HARNESS_CAPS_DIR; H_CAPS=$CAPS
  done
  for bad in "$UNSET" "" "demo=relative/dir" "demo" "DEMO=$R" "$(printf 'demo=%s\nother=%s' "$R" "$O")"; do
    H_MAP=$bad; no_done "14 HARNESS_REPO_MAP '$bad'" HARNESS_REPO_MAP
  done
  H_MAP="demo=$R other=$O"; no_done "14 HARNESS_REPO_MAP two entries (space)" ""
  H_MAP="demo=$R"
  ok_done twin-restored
  # a stand-in naming another repo id (the real engine refuses a missing id first)
  stub wrongid "echo '$(good_line other)'"; H_BIN=$EXT/wrongid; no_done "14 stand-in repo_id=other" "repo id"
  stub sameid "echo '$(good_line demo)'"; H_BIN=$EXT/sameid; ok_done twin-stand-in-repo_id-demo
}

# ---------------------------------------------------------------- AC 15
refuse_forged_status() {
  new_world
  local upper
  upper=$(printf '%s' "$SHA40" | tr a-f A-F)
  stub fa "echo 'ticket=T-1 repo_id=demo head=$SHA40 last_gate=pass gate_head=$SHA40 gate_current=no failure_class=none fix_rounds=unrecorded'"
  H_BIN=$EXT/fa; no_done "15a gate_current=no" stale
  stub fb "echo 'ticket=T-1 repo_id=demo head=$SHA40 last_gate=pass gate_head=$OTHER40 gate_current=yes failure_class=none fix_rounds=unrecorded'"
  H_BIN=$EXT/fb; no_done "15b gate_head is another sha" stale
  stub fc "echo '$(good_line)'; echo '$(good_line)'"
  H_BIN=$EXT/fc; no_done "15c two lines" malformed
  stub fd "echo 'ticket=T-1 repo_id=demo head=$SHA40 last_gate=pass last_gate=fail gate_head=$SHA40 gate_current=yes'"
  H_BIN=$EXT/fd; no_done "15d duplicate last_gate" malformed
  stub fe "echo '$(good_line) trailing'"
  H_BIN=$EXT/fe; no_done "15e trailing text" malformed
  stub ff "echo 'ticket=T-1 repo_id=demo head=$upper last_gate=pass gate_head=$SHA40 gate_current=yes'"
  H_BIN=$EXT/ff; no_done "15f uppercase sha" malformed
  stub fg "echo 'ticket=T-1 repo_id=demo head=$SHA40 last_gate=pass gate_head=$SHA40 gate_current=yes failure_class=none fix_rounds=2 caps=ok'"
  H_BIN=$EXT/fg; ok_done "15 extra opaque pairs fix_rounds=2 caps=ok"
  stub fh "echo '$(good_line)'"
  H_BIN=$EXT/fh; ok_done "15 twin: valid line"
  new_world; gate
  ok_done "15 twin: the real status line"
}

# ---------------------------------------------------------------- AC 16
crash_is_exit_2() {
  new_world; gate
  ok_done twin-real-python
  mkdir -p "$W/nopy"; HPATH=$W/nopy
  no_done python3-missing python3
  mkdir -p "$W/stubbin"
  printf '#!/bin/sh\nexit 3\n' > "$W/stubbin/python3"; chmod +x "$W/stubbin/python3"
  HPATH=$W/stubbin:$PATH; no_done python3-stub-exit-3 "internal error"
  printf '#!/bin/sh\nkill -9 $$\n' > "$W/stubbin/python3"
  no_done python3-stub-killed "internal error"
}
engine_failures_are_exit_2() {
  new_world; gate
  ok_done twin-real-engine
  H_BIN=$EXT/nope; no_done engine-missing "could not run"
  printf '#!/bin/sh\nexit 0\n' > "$EXT/noexec"; chmod -x "$EXT/noexec"
  H_BIN=$EXT/noexec; no_done engine-not-executable "could not run"
  stub empty "exit 0"; H_BIN=$EXT/empty; no_done engine-exit-0-empty malformed
  stub exit1 "exit 1"; H_BIN=$EXT/exit1; no_done engine-exit-1 "did not accept"
}
engine_hang_is_exit_2() {
  new_world; gate
  stub hang "sleep 25; echo '$(good_line)'"; H_BIN=$EXT/hang
  no_done engine-hang "in time"
  H_BIN=$EXT/harness; ok_done twin-real-engine
}
git_hang_is_exit_2() {
  new_world; gate
  ok_done twin-real-git
  mkdir -p "$W/stubbin"
  printf '#!/bin/sh\nfor a in "$@"; do [ "$a" = status ] && { sleep 25; exit 0; }; done\nexec %s "$@"\n' "$REALGIT" > "$W/stubbin/git"; chmod +x "$W/stubbin/git"
  HPATH=$W/stubbin:$PATH
  no_done git-status-hang "in time"
}

# ---------------------------------------------------------------- AC 17
reason_text_is_filtered() {
  new_world
  stub echoed "echo 'harness: error: the ticket file cannot be read' >&2; exit 2"; H_BIN=$EXT/echoed
  no_done "17 a harness: line is echoed" "the ticket file cannot be read"
  stub leakdir "echo \"harness: error: $R is odd\" >&2; exit 2"; H_BIN=$EXT/leakdir
  no_done "17 a line with the mapped folder is not echoed" "did not accept"
  expect_no_text "17 folder line" "is odd"
  stub leakcaps "echo \"harness: error: $CAPS is odd\" >&2; exit 2"; H_BIN=$EXT/leakcaps
  no_done "17 a line with the caps folder is not echoed" "did not accept"
  expect_no_text "17 caps line" "is odd"
  stub nopre "echo 'plain leaky words' >&2; exit 2"; H_BIN=$EXT/nopre
  no_done "17 a line without the prefix is not echoed" "did not accept"
  expect_no_text "17 no-prefix line" leaky
  stub ctlline "printf 'harness: bad\\001words\\n' >&2; exit 2"; H_BIN=$EXT/ctlline
  no_done "17 a line with a control character is not echoed" "did not accept"
  expect_no_text "17 control line" words
}

# ---------------------------------------------------------------- AC 22a
refuse_reasons_in_order() {
  new_world
  echo n > "$R/new.txt"
  no_done "22a dirty + no result" "no result"
  new_world fail; gate 1; echo n > "$R/new.txt"
  no_done "22a dirty + FAIL" fail
  new_world; gate; commit_more; echo n > "$R/new.txt"
  no_done "22a dirty + stale PASS" stale
  new_world; gate; echo n > "$R/new.txt"
  no_done "22a dirty + PASS current: the tree reason" uncommitted
  H_BIN=$UNSET; no_done "22a dirty + HARNESS_BIN unset: the env reason" HARNESS_BIN
  H_BIN=$EXT/harness
  new_world; gate
  ok_done "22a twin: clean + PASS current"
}

# ---------------------------------------------------------------- AC 18
manifest_shape() {
  "$PY" -I - "$MANIFEST" "$HERE" <<'PYEOF' || fail "manifest shape"
import json, os, re, sys
m = json.load(open(sys.argv[1]))
assert set(m) == {"hook", "event", "tools", "env", "requires", "exit_allow", "exit_refuse"}, "keys"
h = os.path.join(sys.argv[2], m["hook"])
assert os.path.isfile(h) and os.access(h, os.X_OK), "hook file"
assert m["event"] == "PreToolUse" and m["requires"] == ["python3"], "event/requires"
assert m["tools"] == ["TaskUpdate"], "tools"
assert m["env"] == ["HARNESS_BIN", "HARNESS_TICKET_FILE", "HARNESS_REPO_MAP", "HARNESS_RESULT_DIR", "HARNESS_CAPS_DIR"], "env"
assert m["exit_allow"] == 0 and m["exit_refuse"] == 2, "exits"
assert os.path.basename(m["hook"]) == m["hook"], "hook is a bare name"
def strings(x):
    if isinstance(x, str): yield x
    elif isinstance(x, list):
        for i in x: yield from strings(i)
    elif isinstance(x, dict):
        for i in x.values(): yield from strings(i)
for s in strings(m):
    assert re.fullmatch(r"[A-Za-z0-9_.-]+", s), "string value shape"
PYEOF
}
manifest_matches_script() {
  new_world
  local tools t
  tools=$("$PY" -I -c 'import json,sys; print("\n".join(json.load(open(sys.argv[1]))["tools"]))' "$MANIFEST") || { fail "manifest unreadable"; return; }
  [ -n "$tools" ] || fail "manifest lists no tools"
  for t in $tools; do
    run_hook "{\"tool_name\":\"$t\",\"tool_input\":{\"status\":\"completed\"}}"; expect_refuse "listed $t, no result" "no result"
  done
  gate
  for t in $tools; do
    run_hook "{\"tool_name\":\"$t\",\"tool_input\":{\"status\":\"completed\"}}"; expect_allow "listed $t, PASS current"
  done
  run_hook '{"tool_name":"Read","tool_input":{"status":"completed"}}'; expect_refuse "unlisted Read" "not TaskUpdate"
  run_hook '{"tool_name":"Bash","tool_input":{"status":"completed"}}'; expect_refuse "unlisted Bash" "not TaskUpdate"
}

# ---------------------------------------------------------------- AC 15 (fix_rounds is opaque)
fix_rounds_is_opaque() {
  new_world fail; gate 1
  local line
  line=$(cd "$W/home" && "$EXT/harness" status "$EXT/ticket.json" --repo-map "demo=$R" --result-dir "$RES" --caps-dir "$CAPS" 2>/dev/null)
  case "$line" in *" fix_rounds=1") ;; *) fail "15 the real status line after one FAIL does not end in fix_rounds=1: $line";; esac
  no_done "15 real FAIL, fix_rounds=1" fail
  new_world
  stub fr2 "echo 'ticket=T-1 repo_id=demo head=$SHA40 last_gate=pass gate_head=$SHA40 gate_current=yes failure_class=none fix_rounds=2'"
  H_BIN=$EXT/fr2; ok_done "15 stand-in fix_rounds=2 over PASS-current"
  stub fru "echo 'ticket=T-1 repo_id=demo head=$SHA40 last_gate=pass gate_head=$SHA40 gate_current=yes failure_class=none fix_rounds=unrecorded'"
  H_BIN=$EXT/fru; ok_done "15 stand-in fix_rounds=unrecorded over PASS-current"
  stub frd "echo 'ticket=T-1 repo_id=demo head=$SHA40 last_gate=pass gate_head=$SHA40 gate_current=yes fix_rounds=1 fix_rounds=1'"
  H_BIN=$EXT/frd; no_done "15 twin: duplicate fix_rounds" malformed
}

# ---------------------------------------------------------------- AC 16 (manifest and hook agree)
envcheck() { # manifest hook: every manifest env name is read by the hook and every HARNESS_ name the hook reads is listed
  "$PY" -I -c 'import json, re, sys
m = json.load(open(sys.argv[1]))
reads = set(re.findall(r"(?:env_abs|os\.environ\.get)\(\s*\"(HARNESS_[A-Z_]+)\"", open(sys.argv[2]).read()))
assert set(m["env"]) == reads and len(m["env"]) == len(reads), "env names differ from the names the hook reads"' "$1" "$2"
}
manifest_env_matches_hook() {
  new_world
  envcheck "$MANIFEST" "$HOOK_SRC" || fail "manifest env names do not match the names the hook reads"
  "$PY" -I -c 'import json, sys; m = json.load(open(sys.argv[1])); m["env"] = m["env"][:4]; json.dump(m, open(sys.argv[2], "w"))' "$MANIFEST" "$W/m-four.json"
  envcheck "$W/m-four.json" "$HOOK_SRC" 2>/dev/null && fail "twin: a four-name manifest passed"
  "$PY" -I -c 'import json, sys; m = json.load(open(sys.argv[1])); m["env"] = m["env"] + ["HARNESS_NEVER_READ"]; json.dump(m, open(sys.argv[2], "w"))' "$MANIFEST" "$W/m-six.json"
  envcheck "$W/m-six.json" "$HOOK_SRC" 2>/dev/null && fail "twin: a manifest with a name the hook never reads passed"
  return 0
}

# ---------------------------------------------------------------- AC 18a-18d (the refuse-all windows)
frozen_old_hook() { # the PR-38 hook, frozen: four variables, no --caps-dir
cat <<'FROZEN_PR38_HOOK_EOF'
#!/usr/bin/env bash
# PreToolUse hook on TaskUpdate: refuse a completion claim unless the gate result for the current head is PASS (V5).
# Reads the tool call as JSON on stdin (only tool_name and tool_input.status are used). Inputs by environment: HARNESS_BIN,
# HARNESS_TICKET_FILE, HARNESS_REPO_MAP (exactly one <repo_id>=<absolute dir> entry), HARNESS_RESULT_DIR.
# Exit 0 = allow (stdout and stderr empty). Exit 2 = refuse, one reason line on stderr, nothing on stdout.
# Any failure of the hook itself is converted to exit 2 (only exit 2 blocks in Claude Code).
# The `# G:<name>` tags mark each guard; the mutation proof disables one tag at a time.
set -u

here="${BASH_SOURCE[0]:-$0}"

refuse() {
  printf '%s\n' "$1" >&2
  exit 2
}

if ! command -v python3 >/dev/null 2>&1; then refuse "refused: python3 is missing"; fi # G:py_missing

PROG=''
read -r -d '' PROG <<'PYEOF'
import json, os, re, subprocess, sys

ENGINE_TIMEOUT = 20
GIT_TIMEOUT = 20
STDIN_CAP = 1024 * 1024
STATUS_CAP = 4096
NONCOMPLETING = ("pending", "in_progress", "deleted")
SHA = r"(?:[0-9a-f]{40}|[0-9a-f]{64})"


def refuse(msg):
    sys.stderr.write(msg + "\n")
    sys.exit(2)


def has_control(s):
    return any(ord(c) < 32 or ord(c) == 127 for c in s)


def inside(root, p):
    # the folder itself or strictly below it, component-wise (root + "/" prefix)
    return p == root or p.startswith(root + "/")


# ---- 1. The tool call: only tool_name and tool_input.status are read.
data = sys.stdin.buffer.read(STDIN_CAP + 1)
if len(data) > STDIN_CAP:  # G:stdin_cap
    refuse("refused: the tool call is too large")
try:
    call = json.loads(data.decode("utf-8"))
except (ValueError, UnicodeDecodeError):  # G:stdin_json
    refuse("refused: the tool call is not valid JSON")
if not isinstance(call, dict):
    refuse("refused: the tool call is not an object")
tool = call.get("tool_name")
if tool != "TaskUpdate":  # G:tool_allowlist
    refuse("refused: the tool is not TaskUpdate")
tin = call.get("tool_input")
if not isinstance(tin, dict):
    refuse("refused: the tool input is missing")
# A completion claim is anything but a known non-completing value (or no status key at all).
st = tin.get("status")
claim = ("status" in tin) and not (isinstance(st, str) and st in NONCOMPLETING)  # G:status_allowlist
if not claim:
    sys.exit(0)


# ---- 2. Inputs and environment.
def env_abs(name):
    v = os.environ.get(name, "")
    if v == "" or has_control(v) or not v.startswith("/"):  # G:env_valid
        refuse("refused: " + name + " must be set to an absolute path")
    return v


bin_path = env_abs("HARNESS_BIN")
ticket_path = env_abs("HARNESS_TICKET_FILE")
result_path = env_abs("HARNESS_RESULT_DIR")
repo_map = os.environ.get("HARNESS_REPO_MAP", "")
m = re.match(r"^([a-z0-9._-]+)=(/[^\x00-\x1f\x7f]*)\Z", repo_map)
if m is None:  # G:map_shape
    refuse("refused: HARNESS_REPO_MAP must be exactly one repo_id=absolute-dir entry")
map_id, map_dir = m.group(1), m.group(2)

# The one root: resolved once, used for every later comparison.
root = os.path.realpath(map_dir)
if root == "/":
    refuse("refused: the repo root is not a usable folder")

# Anti-spoof: the guard's own inputs must live outside the repo (resolved once each).
if inside(root, os.path.realpath(result_path)):  # G:spoof_result
    refuse("refused: HARNESS_RESULT_DIR is inside the repo")
if inside(root, os.path.realpath(ticket_path)):  # G:spoof_ticket
    refuse("refused: HARNESS_TICKET_FILE is inside the repo")
if inside(root, os.path.realpath(bin_path)):  # G:spoof_bin
    refuse("refused: HARNESS_BIN is inside the repo")
if inside(root, os.path.realpath(sys.argv[1])):  # G:spoof_hook
    refuse("refused: the hook script is inside the repo")

# ---- 3. The engine call: `harness status` prints one fixed line; the hook adds no result parser.
try:
    cp = subprocess.run(
        [bin_path, "status", ticket_path, "--repo-map", repo_map, "--result-dir", result_path],
        stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE, cwd="/",
        timeout=ENGINE_TIMEOUT,  # G:timeout
    )
except (OSError, subprocess.SubprocessError):
    refuse("refused: the status command could not run in time")
if cp.returncode != 0:  # G:status_exit
    line = cp.stderr.decode("utf-8", "replace").strip().split("\n")[0][:200] if cp.stderr else ""
    shown = not any(d in line for d in (root, map_dir, result_path, ticket_path, bin_path))
    if line.startswith("harness: ") and not has_control(line) and shown:  # G:reason_filter
        refuse("refused: status: " + line)
    refuse("refused: the status command did not accept the ticket")


def parse_status(out):
    if len(out) > STATUS_CAP or not out.endswith(b"\n") or out.count(b"\n") != 1:
        return None
    try:
        line = out[:-1].decode("ascii")
    except UnicodeDecodeError:
        return None
    d = {}
    for tok in line.split(" "):
        mt = re.fullmatch(r"([a-z_]+)=([A-Za-z0-9._-]+)", tok)
        if mt is None or mt.group(1) in d:
            return None
        d[mt.group(1)] = mt.group(2)
    for k in ("ticket", "repo_id", "head", "last_gate", "gate_head", "gate_current"):
        if k not in d:
            return None
    if re.fullmatch(r"[a-z0-9._-]+", d["repo_id"]) is None or re.fullmatch(SHA, d["head"]) is None:
        return None
    if d["last_gate"] == "none":
        if d["gate_head"] != "none" or d["gate_current"] != "none":
            return None
    elif (
        d["last_gate"] not in ("pass", "fail")
        or re.fullmatch(SHA, d["gate_head"]) is None
        or d["gate_current"] not in ("yes", "no")
    ):
        return None
    return d


f = parse_status(cp.stdout)
if f is None:  # G:status_parse
    refuse("refused: the status output is malformed")
if f["repo_id"] != map_id:  # G:repo_id_cmp
    refuse("refused: the status repo id differs from the repo map")


def check_tree():
    # Clean means what the gate means: empty `git status --porcelain=v1 -z --untracked-files=all`, pinned like the gate's own call.
    env = {k: v for k, v in os.environ.items() if not k.startswith("GIT_")}  # G:git_env
    argv = [
        "git", "--no-optional-locks", "--git-dir", os.path.join(root, ".git"), "--work-tree", root,
        "-c", "core.hooksPath=/dev/null", "-c", "core.fsmonitor=false",
        "status", "--porcelain=v1", "-z",
        "--untracked-files=all",  # G:untracked_all
        # G:no_ignored
    ]
    try:
        gp = subprocess.run(
            argv, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, cwd=root, env=env,
            timeout=GIT_TIMEOUT,  # G:git_timeout
        )
    except (OSError, subprocess.SubprocessError):
        refuse("refused: git status could not run in time")
    if gp.returncode != 0:  # G:git_exit
        refuse("refused: git status failed")
    if gp.stdout != b"":  # G:clean_tree
        refuse("refused: the repo has uncommitted or untracked changes")


# ---- 4. The verdict, then (last) the tree.
# G:reason_order
if f["last_gate"] == "none":  # G:none_refused
    refuse("refused: no result: the gate has not run for this ticket")
if f["last_gate"] != "pass":  # G:last_pass
    refuse("refused: the last gate result is fail")
if f["gate_current"] != "yes":  # G:gate_current
    refuse("refused: the last gate result is stale (it is about another commit)")
if f["gate_head"] != f["head"]:  # G:head_eq
    refuse("refused: the last gate result is stale (it is about another commit)")
check_tree()
sys.exit(0)
PYEOF

# G:catch_all  (any python failure other than a clean refusal becomes exit 2)
err=$(python3 -I -c "$PROG" "$here" 2>&1 >/dev/null)
rc=$?
if [ "$rc" -eq 0 ]; then
  exit 0
fi
if [ "$rc" -eq 2 ]; then
  reason=$(printf '%s\n' "$err" | head -n 1 | cut -c1-300)
  if [ -n "$reason" ]; then refuse "$reason"; fi
fi
refuse "refused: internal error in the pre-completion hook" # G:catch_all
FROZEN_PR38_HOOK_EOF
}
w1_old_hook_new_engine() {
  new_world; gate
  ok_done twin-new-hook-five-variables
  frozen_old_hook > "$EXT/old-hook.sh"; chmod +x "$EXT/old-hook.sh"
  if command grep -q 'HARNESS_CAPS_DIR' "$EXT/old-hook.sh"; then fail "18a the frozen copy mentions HARNESS_CAPS_DIR (it is not the old hook)"; fi
  H_HOOK=$EXT/old-hook.sh; H_CAPS=$UNSET
  no_done "18a old hook, new engine" "--caps-dir is required"
  local raw
  for raw in '"pending"' '"in_progress"' '"deleted"'; do
    claim "$raw"; expect_allow "18a non-completing $raw"
  done
}
engine_usage_error_is_refused() {
  new_world
  stub permissive "echo '$(good_line)'"; H_BIN=$EXT/permissive
  ok_done twin-stand-in-exit-0
  stub usageerr "echo 'harness: error: usage: harness status <ticket-file> ...' >&2; exit 2"; H_BIN=$EXT/usageerr
  no_done "18b engine usage error, all five variables set" "status"
}
w2_new_hook_old_env() {
  new_world; gate
  ok_done twin-five-variables
  H_CAPS=$UNSET
  no_done "18c four variables" HARNESS_CAPS_DIR
  local raw
  for raw in '"pending"' '"in_progress"' '"deleted"'; do
    claim "$raw"; expect_allow "18c non-completing $raw"
  done
}
w3_caps_folder_mismatch() {
  new_world; gate
  ok_done twin-same-caps-folder
  mkdir "$W/capsB"; H_CAPS=$W/capsB
  no_done "18d hook caps folder differs from the gate's" "cap record is missing"
  local raw
  for raw in '"pending"' '"in_progress"' '"deleted"'; do
    claim "$raw"; expect_allow "18d non-completing $raw"
  done
}

# ---------------------------------------------------------------- AC 19
print_registration_is_safe() {
  new_world
  local before after gbefore gafter eng git_t
  before=$(ls -A "$TR"); gbefore=$(git -C "$ROOT" status --porcelain 2>/dev/null)
  bash "$0" --print-registration > "$W/reg.out" 2> "$W/reg.err"; [ "$?" -eq 0 ] || fail "print-registration exited non-zero"
  after=$(ls -A "$TR"); gafter=$(git -C "$ROOT" status --porcelain 2>/dev/null)
  [ "$before" = "$after" ] || fail "print-registration changed the scratch folder"
  [ "$gbefore" = "$gafter" ] || fail "print-registration changed the repo"
  command grep -q 'hooks.PreToolUse' "$W/reg.err" || fail "print-registration omits the append sentence"
  local sub
  for sub in HARNESS_BIN HARNESS_TICKET_FILE HARNESS_REPO_MAP HARNESS_RESULT_DIR HARNESS_CAPS_DIR 'FIVE variables' 'step 1' 'step 2' 'step 3' \
    'mktemp -d' 'rmdir' 'live probe' 'SAME folder' 'temp name in the same folder' 'cannot detect' 'cap record is missing' 'supersedes' 'steps 3, 4 and 6'; do
    command grep -qF -- "$sub" "$W/reg.err" || fail "print-registration text omits '$sub'"
  done
  eng=$(command grep -E '^ENGINE_TIMEOUT = [0-9]+$' "$HOOK_SRC" | sed 's/.*= //')
  git_t=$(command grep -E '^GIT_TIMEOUT = [0-9]+$' "$HOOK_SRC" | sed 's/.*= //')
  [ -n "$eng" ] && [ -n "$git_t" ] || { fail "timeout constants not found"; return; }
  "$PY" -I - "$W/reg.out" "$eng" "$git_t" <<'PYEOF' || fail "registration shape"
import json, sys
raw = open(sys.argv[1]).read()
o = json.loads(raw)
assert isinstance(o, dict) and raw.strip().count("\n") == 0, "one object"
assert set(o) == {"matcher", "hooks"} and o["matcher"] == "TaskUpdate", "matcher"
assert "PreToolUse" not in o and isinstance(o["hooks"], list) and len(o["hooks"]) == 1, "not a whole hooks block"
h = o["hooks"][0]
assert h["type"] == "command" and h["command"].startswith("~/.harness/claude/"), "fixed path"
worst = int(sys.argv[2]) + int(sys.argv[3]) + 20
assert isinstance(h["timeout"], int) and h["timeout"] >= 100 and h["timeout"] > worst, "timeout"
PYEOF
}

# ---------------------------------------------------------------- AC 20
smoke_sandbox_safety() {
  case "$TR" in "$(CDPATH= cd -P -- "${TMPDIR:-/tmp}" && pwd -P)"/*) ;; *) fail "scratch folder is not under the system temp folder";; esac
  local before after rep
  before=$(git -C "$ROOT" status --porcelain 2>/dev/null)
  rep=$(mktemp "$TR/rep.XXXXXX")
  SMOKE_REPORT=$rep bash "$0" _selftest_leave >/dev/null 2>&1
  [ "$?" -eq 1 ] || fail "selftest did not fail as planted"
  [ -s "$rep" ] || fail "selftest reported no scratch folder"
  [ ! -e "$(cat "$rep")" ] || fail "scratch folder survived a failing run"
  after=$(git -C "$ROOT" status --porcelain 2>/dev/null)
  [ "$before" = "$after" ] || fail "the working repo changed during the smoke"
}
_selftest_leave() { fail "planted failure"; }

premise_status_merged() {
  local out rc
  out=$("$BIN_SRC" status 2>&1 < /dev/null); rc=$?
  [ "$rc" -eq 2 ] || fail "status with no arguments exited $rc, expected 2"
  case "$out" in *"usage: harness status"*) ;; *) fail "status usage line missing";; esac
  case "$out" in *"--caps-dir"*) ;; *) fail "status usage line does not name --caps-dir";; esac
}

CASES="allow_pass_current refuse_fail_result refuse_stale_result refuse_no_result refuse_bad_result_files refuse_linked_result refuse_case_twin refuse_dirty_tree refuse_completion_spellings refuse_other_tools refuse_bad_stdin extra_keys_do_not_decide other_task_same_verdict refuse_inputs_inside_repo refuse_bad_env refuse_forged_status crash_is_exit_2 engine_failures_are_exit_2 engine_hang_is_exit_2 git_hang_is_exit_2 reason_text_is_filtered refuse_reasons_in_order manifest_shape manifest_matches_script manifest_env_matches_hook fix_rounds_is_opaque w1_old_hook_new_engine engine_usage_error_is_refused w2_new_hook_old_env w3_caps_folder_mismatch print_registration_is_safe smoke_sandbox_safety premise_status_merged"

run_case() {
  FAILN=0
  echo "== $1"
  "$1"
  case "$W" in "$TR"/w.*) rm -rf -- "$W" ;; esac
  W=
  if [ "$FAILN" -eq 0 ]; then echo "PASS $1"; else echo "FAIL $1 ($FAILN)"; TOTALFAIL=$((TOTALFAIL + 1)); fi
}

if [ "$#" -gt 1 ]; then echo "usage: pre-completion-smoke.sh [case-name | --print-registration]" >&2; exit 2; fi
if [ "$#" -eq 1 ]; then
  if [ "$1" = _selftest_leave ]; then run_case _selftest_leave; exit 1; fi
  case " $CASES " in *" $1 "*) ;; *) echo "smoke: unknown case" >&2; exit 2;; esac
  run_case "$1"
  echo "smoke: skipped=$SKIPN failed=$TOTALFAIL"
  [ "$TOTALFAIL" -eq 0 ]; exit $?
fi
for c in $CASES; do run_case "$c"; done
echo "smoke: skipped=$SKIPN failed=$TOTALFAIL"
[ "$TOTALFAIL" -eq 0 ]
