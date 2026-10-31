#!/usr/bin/env bash
# Smoke for plugins/claude/path-restriction-hook.sh (Unix).
# Usage: bash plugins/claude/smoke-path-restriction.sh [case-name]
#   no argument: every case; exit 0 only if all pass. With a case name: that case only.
# Needs the built engine: cargo build --locked (in core/rust) first; exit 2 if it is absent.
# SMOKE_HOOK (test-only) points the smoke at another copy of the hook, e.g. a mutant in a scratch folder.
# Everything lives under one mktemp folder; a trap removes it even when a case fails.

HERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
ROOT=$(CDPATH= cd -- "$HERE/../.." && pwd)
HOOK_SRC=${SMOKE_HOOK:-$HERE/path-restriction-hook.sh}
BIN_SRC=$ROOT/core/rust/target/debug/harness
MANIFEST=$HERE/manifest.json
if [ ! -x "$BIN_SRC" ]; then
  echo "smoke: the built engine is missing; run cargo build --locked in core/rust first" >&2
  exit 2
fi
ENVBIN=/usr/bin/env
BASHBIN=$(command -v bash)
PY=$(command -v python3) || { echo "smoke: python3 is required" >&2; exit 2; }

T=$(mktemp -d "${TMPDIR:-/tmp}/p1a15.XXXXXX") || exit 2
TR=$(CDPATH= cd -P -- "$T" && pwd -P)
cleanup() {
  case "$T" in
    */p1a15.*) [ -n "$SMOKE_REPORT" ] && printf '%s\n' "$T" > "$SMOKE_REPORT"; rm -rf -- "$T" ;;
  esac
}
trap cleanup EXIT
trap 'exit 130' INT TERM

AT=$(printf '\100')
UNSET='__UNSET__'
FAILN=0
TOTALFAIL=0
TICKET_JSON='{"id":"T-1","repo_id":"demo","title":"t","group":"g","size":"S","objective":"o","acceptance_criteria":["a"],"dependencies":[],"risk":"normal","owner_role":"execution_verification","evidence_links":[]}'
SHA40=0123456789abcdef0123456789abcdef01234567

fail() { echo "  FAIL: $*" >&2; FAILN=$((FAILN + 1)); }

git_q() { git -C "$1" -c core.hooksPath=/dev/null -c user.name=t -c "user.email=t${AT}scratch" "${@:2}" >/dev/null 2>&1; }

mkrepo() { # dir
  mkdir -p "$1/src" "$1/docs" "$1/private-dir" "$1/src/real"
  git init -q "$1" >/dev/null 2>&1
  echo x > "$1/src/a.rs"; echo x > "$1/src/real/a.rs"; echo n > "$1/docs/notes.md"
  echo r > "$1/README.md"; echo p > "$1/private-dir/p.txt"
  git_q "$1" add -A
  git_q "$1" commit -q -m init
}

# One private copy of the engine per run; each world gets a hard link to it (a plain copy where links fail).
GBIN=$TR/engine
cp "$BIN_SRC" "$GBIN" && chmod +x "$GBIN"
link_bin() { ln "$GBIN" "$1" 2>/dev/null || cp "$GBIN" "$1"; }

new_world() {
  W=$(mktemp -d "$TR/w.XXXXXX")
  EXT=$W/ext; R=$W/r; O=$W/other
  mkdir -p "$EXT" "$O/src" "$W/home"
  mkrepo "$R"
  echo o > "$O/src/a.rs"; echo o > "$O/target.txt"
  cp "$HOOK_SRC" "$EXT/hook.sh"; link_bin "$EXT/harness"; chmod +x "$EXT/hook.sh"
  printf '%s\n' "$TICKET_JSON" > "$EXT/ticket.json"
  SCOPE_FILE=$EXT/scope.txt
  scope "src/" "docs/notes.md"
  H_BIN=$EXT/harness; H_TICKET=$EXT/ticket.json; H_MAP="demo=$R"; H_SCOPE=$SCOPE_FILE; H_HOOK=$EXT/hook.sh
  HPATH=; HCWD=
}

scope() { printf '%s\n' "ticket=T-1" "$@" > "$SCOPE_FILE"; }

run_hook_file() { # stdin-file
  local a=("PATH=${HPATH:-$PATH}" "HOME=$W/home")
  [ "$H_BIN" != "$UNSET" ] && a+=("HARNESS_BIN=$H_BIN")
  [ "$H_TICKET" != "$UNSET" ] && a+=("HARNESS_TICKET_FILE=$H_TICKET")
  [ "$H_MAP" != "$UNSET" ] && a+=("HARNESS_REPO_MAP=$H_MAP")
  [ "$H_SCOPE" != "$UNSET" ] && a+=("HARNESS_SCOPE_FILE=$H_SCOPE")
  (cd "${HCWD:-$W/home}" && "$ENVBIN" -i "${a[@]}" "$BASHBIN" "$H_HOOK" < "$1" > "$W/out" 2> "$W/err")
  RC=$?
}
run_hook() { printf '%s' "$1" > "$W/in"; run_hook_file "$W/in"; }

keyof() { if [ "$1" = NotebookEdit ]; then echo notebook_path; else echo file_path; fi; }
edit() { run_hook "$(printf '{"tool_name":"%s","tool_input":{"%s":"%s"}}' "$1" "$(keyof "$1")" "$2")"; }

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
ok_edit() { edit "$1" "$2"; expect_allow "${3:-$1 $2}"; }
no_edit() { edit "$1" "$2"; expect_refuse "${3:-$1 $2}" "$4"; }
raw_no() { run_hook "$2"; expect_refuse "$1" "$3"; }

# ---------------------------------------------------------------- AC 1
allow_inside_scope() {
  new_world
  ok_edit Edit "$R/src/a.rs" 1a
  ok_edit Write "$R/src/deep/new.rs" 1b
  ok_edit Edit "$R/docs/notes.md" 1c
  ok_edit Write "$R/src/a.rs" 1d-write
  ok_edit NotebookEdit "$R/src/a.rs" 1d-notebook
  ok_edit MultiEdit "$R/src/a.rs" 1e
  # raw (non-resolved) spelling of the same folder: map and edit path both use it
  local raw; raw=$(CDPATH= cd -- "$R" && pwd)
  H_MAP="demo=$raw"; ok_edit Edit "$raw/src/a.rs" 1f-raw-spelling
}

# ---------------------------------------------------------------- AC 2
refuse_outside_scope() {
  new_world
  ok_edit Edit "$R/src/a.rs" twin-1a
  no_edit Edit "$R/README.md"
  no_edit Edit "$R/docs/other.md"
  mkdir -p "$R/srcx"
  no_edit Edit "$R/srcx/a.rs"
  no_edit Edit "$R/docs/notes.md.bak"
  no_edit Edit "$R/docs/notes.md/x" below-exact-file-entry
  mkdir -p "$W/cs"; : > "$W/cs/lower"
  if [ -e "$W/cs/LOWER" ]; then
    echo "  SKIPPED: case row (case-insensitive scratch volume)"
  else
    no_edit Edit "$R/SRC/a.rs" case-row
  fi
}

# ---------------------------------------------------------------- AC 3
refuse_outside_repo() {
  new_world
  mkdir -p "$W/r-evil/src"
  ok_edit Edit "$R/src/a.rs" twin-1a
  no_edit Edit "$O/src/a.rs" other-folder
  no_edit Edit "$R-evil/src/a.rs" evil-prefix
  # same-length sibling and prefix-sharing sibling whose tail reads as an in-scope path (only the root differs)
  mkdir -p "$W/q/src" "$W/rxsrc"
  echo o > "$W/q/src/a.rs"; echo o > "$W/rxsrc/a.rs"
  no_edit Edit "$W/q/src/a.rs" same-length-sibling
  no_edit Edit "$W/rxsrc/a.rs" prefix-sharing-sibling
  no_edit Edit /etc/hosts
  no_edit Edit "$W" scratch-parent
  no_edit Edit "$R" repo-root
}

# ---------------------------------------------------------------- AC 4
refuse_dotdot() {
  new_world
  ok_edit Edit "$R/src/a.rs" twin-1a
  no_edit Edit "$R/src/../README.md"
  no_edit Edit "$R/src/../../other/src/a.rs"
  no_edit Edit "$R/../x"
  no_edit Edit ".." lone-dotdot
  no_edit Edit "$R/src/../src/a.rs" dotdot-resolves-inside
}

# ---------------------------------------------------------------- AC 5
refuse_symlink_escapes() {
  new_world
  ln -s "$O/src" "$R/src/link-out"
  ln -s "$O/target.txt" "$R/src/leaf-out"
  ln -s "$O/not-yet.txt" "$R/src/dangling"
  ln -s "$R/src/real" "$R/src/inner"
  ln -s "$R/private-dir" "$R/src/to-undeclared"
  no_edit Edit "$R/src/link-out/a.rs"
  no_edit Edit "$R/src/leaf-out"
  no_edit Write "$R/src/dangling"
  ok_edit Edit "$R/src/inner/a.rs" twin-inner
  no_edit Edit "$R/src/to-undeclared/x.rs"
}

# ---------------------------------------------------------------- AC 6
refuse_relative_and_malformed_input() {
  new_world
  ok_edit Edit "$R/src/a.rs" twin-1a
  HCWD=$R; no_edit Edit "src/a.rs" relative-from-root
  HCWD=$R/src; no_edit Edit "a.rs" relative-from-src
  HCWD=
  raw_no empty-string '{"tool_name":"Edit","tool_input":{"file_path":""}}'
  raw_no null-path '{"tool_name":"Edit","tool_input":{"file_path":null}}'
  raw_no number-path '{"tool_name":"Edit","tool_input":{"file_path":5}}'
  raw_no key-missing '{"tool_name":"Edit","tool_input":{}}'
  raw_no nul-path "{\"tool_name\":\"Edit\",\"tool_input\":{\"file_path\":\"$R/src/a.rs\\u0000x\"}}"
  raw_no newline-path "{\"tool_name\":\"Edit\",\"tool_input\":{\"file_path\":\"$R/src/a\\nb.rs\"}}"
  raw_no not-json 'this is not json'
  raw_no empty-stdin ''
  # a valid, in-scope call padded to exactly the cap (allowed) and to one byte over it (refused)
  local pre suf padn
  pre=$(printf '{"tool_name":"Edit","tool_input":{"file_path":"%s/src/a.rs"},"pad":"' "$R"); suf='"}'
  padn=$((1048576 - ${#pre} - ${#suf}))
  { printf '%s' "$pre"; head -c "$padn" /dev/zero | tr '\0' x; printf '%s' "$suf"; } > "$W/big"
  run_hook_file "$W/big"; expect_allow exactly-1MiB
  { printf '%s' "$pre"; head -c "$((padn + 1))" /dev/zero | tr '\0' x; printf '%s' "$suf"; } > "$W/big"
  run_hook_file "$W/big"; expect_refuse over-1MiB
  raw_no tool-missing "{\"tool_input\":{\"file_path\":\"$R/src/a.rs\"}}"
  raw_no tool-number "{\"tool_name\":5,\"tool_input\":{\"file_path\":\"$R/src/a.rs\"}}"
  raw_no tool-read "{\"tool_name\":\"Read\",\"tool_input\":{\"file_path\":\"$R/src/a.rs\"}}"
  raw_no tool-lowercase "{\"tool_name\":\"edit\",\"tool_input\":{\"file_path\":\"$R/src/a.rs\"}}"
  no_edit Edit "$R/src//a.rs" double-slash
  no_edit Edit "$R/src/./a.rs" dot-component
  no_edit Edit "$R/src/" trailing-slash
  raw_no backslash "{\"tool_name\":\"Edit\",\"tool_input\":{\"file_path\":\"$R/src/new\\\\x.rs\"}}"
}

# ---------------------------------------------------------------- AC 7
refuse_protected_names() {
  new_world
  scope "src/"
  ok_edit Edit "$R/src/.gitkeep" twin-gitkeep
  no_edit Edit "$R/.git/config"
  no_edit Edit "$R/src/.GIT/x"
  no_edit Edit "$R/.git"
  no_edit Edit "$R/.claude/settings.json"
  no_edit Edit "$R/src/.Claude/x"
  scope ".git/"; no_edit Edit "$R/src/a.rs" scope-names-git
  scope ".claude/"; no_edit Edit "$R/src/a.rs" scope-names-claude
  # the repo itself lives under a folder named .claude (a real worker's worktree)
  mkdir -p "$W/.claude/worktrees"
  mkrepo "$W/.claude/worktrees/r2"
  local r2=$W/.claude/worktrees/r2
  scope "src/"; H_MAP="demo=$r2"
  ok_edit Edit "$r2/src/a.rs" root-ancestor-twin
  no_edit Edit "$r2/.claude/x" inner-claude
}

# ---------------------------------------------------------------- AC 8
stub() { # name body...
  local n=$1; shift
  printf '#!/bin/sh\n%s\n' "$*" > "$EXT/$n"; chmod +x "$EXT/$n"
}
fail_closed_on_inputs() {
  new_world
  ok_edit Edit "$R/src/a.rs" twin-real
  stub good "echo 'ticket=T-1 repo_id=demo head=$SHA40'"
  H_BIN=$EXT/good; ok_edit Edit "$R/src/a.rs" twin-good-stub
  H_BIN=$UNSET; no_edit Edit "$R/src/a.rs" bin-unset
  H_BIN=$EXT/nope; no_edit Edit "$R/src/a.rs" bin-missing
  printf '#!/bin/sh\nexit 0\n' > "$EXT/noexec"; chmod -x "$EXT/noexec"
  H_BIN=$EXT/noexec; no_edit Edit "$R/src/a.rs" bin-not-executable
  stub exit1 "exit 1"; H_BIN=$EXT/exit1; no_edit Edit "$R/src/a.rs" stub-exit-1
  stub exit2 "exit 2"; H_BIN=$EXT/exit2; no_edit Edit "$R/src/a.rs" stub-exit-2
  stub exit1good "echo 'ticket=T-1 repo_id=demo head=$SHA40'; exit 1"; H_BIN=$EXT/exit1good; no_edit Edit "$R/src/a.rs" stub-exit-1-with-valid-line
  stub exit2good "echo 'ticket=T-1 repo_id=demo head=$SHA40'; exit 2"; H_BIN=$EXT/exit2good; no_edit Edit "$R/src/a.rs" stub-exit-2-with-valid-line
  stub killedgood "echo 'ticket=T-1 repo_id=demo head=$SHA40'; kill -9 \$\$"; H_BIN=$EXT/killedgood; no_edit Edit "$R/src/a.rs" stub-killed-with-valid-line
  # relative input paths would resolve against the working folder, a second root
  H_BIN=harness; HCWD=$EXT; no_edit Edit "$R/src/a.rs" bin-relative
  H_BIN=$EXT/harness; H_TICKET=ticket.json; no_edit Edit "$R/src/a.rs" ticket-relative
  H_TICKET=$EXT/ticket.json; H_SCOPE=scope.txt; no_edit Edit "$R/src/a.rs" scope-relative
  H_SCOPE=$SCOPE_FILE; HCWD=
  stub malformed "echo garbage"; H_BIN=$EXT/malformed; no_edit Edit "$R/src/a.rs" stub-malformed
  stub wrongid "echo 'ticket=T-1 repo_id=other head=$SHA40'"; H_BIN=$EXT/wrongid; no_edit Edit "$R/src/a.rs" stub-wrong-repo-id
  stub twolines "echo 'ticket=T-1 repo_id=demo head=$SHA40'; echo 'ticket=T-1 repo_id=demo head=$SHA40'"
  H_BIN=$EXT/twolines; no_edit Edit "$R/src/a.rs" stub-two-lines
  stub empty "exit 0"; H_BIN=$EXT/empty; no_edit Edit "$R/src/a.rs" stub-empty-stdout
  stub killed 'kill -9 $$'; H_BIN=$EXT/killed; no_edit Edit "$R/src/a.rs" stub-killed
  stub otherticket "echo 'ticket=T-9 repo_id=demo head=$SHA40'"; H_BIN=$EXT/otherticket; no_edit Edit "$R/src/a.rs" stub-other-ticket-id
  H_BIN=$EXT/harness
  H_TICKET=$UNSET; no_edit Edit "$R/src/a.rs" ticket-unset
  printf '%s\n' '{"id":"T-1","title":"t","group":"g","size":"S","objective":"o","acceptance_criteria":["a"],"dependencies":[],"risk":"normal","owner_role":"execution_verification","evidence_links":[]}' > "$EXT/norepo.json"
  H_TICKET=$EXT/norepo.json; no_edit Edit "$R/src/a.rs" ticket-no-repo-id
  printf '%s\n' "$TICKET_JSON" | sed 's/"demo"/"ghost"/' > "$EXT/ghost.json"
  H_TICKET=$EXT/ghost.json; no_edit Edit "$R/src/a.rs" ticket-repo-not-in-map
  H_TICKET=$EXT/ticket.json
  mkdir -p "$W/plain"; H_MAP="demo=$W/plain"; no_edit Edit "$R/src/a.rs" map-plain-folder
  H_MAP=$UNSET; no_edit Edit "$R/src/a.rs" map-unset
  H_MAP=; no_edit Edit "$R/src/a.rs" map-empty
  H_MAP="demo=relative/dir"; no_edit Edit "$R/src/a.rs" map-relative
  H_MAP="demo=$R other=$O"; no_edit Edit "$R/src/a.rs" map-two-entries-space
  H_MAP=$(printf 'demo=%s\nother=%s' "$R" "$O"); no_edit Edit "$R/src/a.rs" map-two-entries-newline
  H_MAP="demo=$R"
  ok_edit Edit "$R/src/a.rs" twin-after
}

# ---------------------------------------------------------------- AC 9
refuse_bad_scope_file() {
  new_world
  local f=$SCOPE_FILE
  bad() { run_hook "$(printf '{"tool_name":"Edit","tool_input":{"file_path":"%s/src/a.rs"}}' "$R")"; expect_refuse "scope: $1"; }
  H_SCOPE=$UNSET; bad unset
  H_SCOPE=$EXT/nope.txt; bad missing
  H_SCOPE=$EXT; bad folder
  H_SCOPE=$f
  : > "$f"; bad empty
  printf '# c\n\n' > "$f"; bad comments-only
  printf 'src/\n' > "$f"; bad first-line-missing
  printf 'ticket=T-9\nsrc/\n' > "$f"; bad other-ticket
  printf 'ticket=T-1\n' > "$f"; bad no-entries
  local e
  for e in '/abs/path' '../up/' 'src/../x' 'src//a' './src/' '.git/' '.claude/' 'src\x' 'src/*' 'src/?' "sr$(printf '\001')c/"; do
    printf 'ticket=T-1\nsrc/\n%s\n' "$e" > "$f"
    bad "entry $e"
  done
  { printf 'ticket=T-1\n'; for i in $(seq 1 200); do printf '# %s\n' "$i"; done; printf 'src/\n'; } > "$f"; bad over-128-lines
  { printf 'ticket=T-1\nsrc/\n'; head -c 4200 /dev/zero | tr '\0' x | sed 's/^/# /'; printf '\n'; } > "$f"; bad over-4096-bytes
  printf 'ticket=T-1\nsrc/\n\377\n' > "$f"; bad invalid-utf8
  printf 'ticket=T-1\nsrc/\ndocs/notes.md\n../bad/\n' > "$f"; bad one-bad-line-at-end
  printf 'ticket=T-1\n# comment\n\nsrc/\n\n# more\ndocs/notes.md\n' > "$f"
  run_hook "$(printf '{"tool_name":"Edit","tool_input":{"file_path":"%s/src/a.rs"}}' "$R")"; expect_allow twin-valid-with-comments
}

# ---------------------------------------------------------------- AC 10
manifest_matches_script() {
  new_world
  local tools t
  tools=$("$PY" -I -c 'import json,sys; print("\n".join(json.load(open(sys.argv[1]))["tools"]))' "$MANIFEST") || { fail "manifest unreadable"; return; }
  [ -n "$tools" ] || fail "manifest lists no tools"
  for t in $tools; do
    ok_edit "$t" "$R/src/a.rs" "listed $t in scope"
    no_edit "$t" "$R/README.md" "listed $t out of scope"
  done
  no_edit Read "$R/src/a.rs" "unlisted Read"
  no_edit Bash "$R/src/a.rs" "unlisted Bash"
}

# ---------------------------------------------------------------- AC 12
crash_is_exit_2() {
  new_world
  ok_edit Edit "$R/src/a.rs" twin-real-python
  mkdir -p "$W/nopy"; HPATH=$W/nopy
  edit Edit "$R/src/a.rs"; expect_refuse python3-missing python3
}
catch_all_decides() {
  new_world
  ok_edit Edit "$R/src/a.rs" twin-real-python
  mkdir -p "$W/stubbin"
  printf '#!/bin/sh\nexit 3\n' > "$W/stubbin/python3"; chmod +x "$W/stubbin/python3"
  HPATH=$W/stubbin:$PATH; edit Edit "$R/src/a.rs"; expect_refuse python3-stub-exit-3 "internal error"
  printf '#!/bin/sh\nkill -9 $$\n' > "$W/stubbin/python3"
  edit Edit "$R/src/a.rs"; expect_refuse python3-stub-killed "internal error"
}

# ---------------------------------------------------------------- AC 13
manifest_shape() {
  "$PY" -I - "$MANIFEST" "$HERE" <<'PYEOF' || fail "manifest shape"
import json, os, re, sys
m = json.load(open(sys.argv[1]))
assert set(m) == {"hook", "event", "tools", "env", "requires", "exit_allow", "exit_refuse"}, "keys"
h = os.path.join(sys.argv[2], m["hook"])
assert os.path.isfile(h) and os.access(h, os.X_OK), "hook file"
assert m["event"] == "PreToolUse" and m["requires"] == ["python3"], "event/requires"
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

# ---------------------------------------------------------------- AC 14 / 15
premise_intake_merged() {
  local out rc
  out=$("$BIN_SRC" ticket intake 2>&1 < /dev/null); rc=$?
  [ "$rc" -eq 2 ] || fail "intake with no arguments exited $rc, expected 2"
  case "$out" in "usage: harness ticket intake"*) ;; *) fail "intake usage line missing";; esac
}
premise_parser() {
  local out
  out=$(printf '{"a":["b"]}' | "$PY" -I -c 'import json,os,sys; d=json.load(sys.stdin); print(d["a"][0], os.path.realpath("."))') || { fail "python3 parse failed"; return; }
  case "$out" in "b /"*) echo "  parser ok on $(uname -s)";; *) fail "python3 parse output unexpected";; esac
}

# ---------------------------------------------------------------- AC 19
refuse_guard_inputs_inside_repo() {
  local pass
  for pass in src docs; do
    new_world
    scope "$pass/"
    local c="$R/src"
    # 19a: scope file inside the repo
    cp "$SCOPE_FILE" "$c/scope.txt"; H_SCOPE=$c/scope.txt
    no_edit Edit "$R/src/a.rs" "19a-$pass" HARNESS_SCOPE_FILE
    H_SCOPE=$SCOPE_FILE
    # 19b: ticket file inside the repo
    cp "$EXT/ticket.json" "$c/ticket.json"; H_TICKET=$c/ticket.json
    no_edit Edit "$R/src/a.rs" "19b-$pass" HARNESS_TICKET_FILE
    H_TICKET=$EXT/ticket.json
    # 19c: the engine inside the repo
    link_bin "$c/harness"; H_BIN=$c/harness
    no_edit Edit "$R/src/a.rs" "19c-$pass" HARNESS_BIN
    H_BIN=$EXT/harness
    # 19d: the hook itself inside the repo
    cp "$HOOK_SRC" "$c/hook.sh"; H_HOOK=$c/hook.sh
    no_edit Edit "$R/src/a.rs" "19d-$pass" hook
    H_HOOK=$EXT/hook.sh
    # 19e: outside path that is a symlink to a file inside the repo
    cp "$SCOPE_FILE" "$c/scope-real.txt"; ln -s "$c/scope-real.txt" "$EXT/scope-link"
    H_SCOPE=$EXT/scope-link
    no_edit Edit "$R/src/a.rs" "19e-$pass" HARNESS_SCOPE_FILE
    # 19f: twin, outside symlink to another outside file
    ln -s "$SCOPE_FILE" "$EXT/scope-link2"; H_SCOPE=$EXT/scope-link2
    if [ "$pass" = src ]; then ok_edit Edit "$R/src/a.rs" 19f-twin; fi
  done
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

CASES="allow_inside_scope refuse_outside_scope refuse_outside_repo refuse_dotdot refuse_symlink_escapes refuse_relative_and_malformed_input refuse_protected_names fail_closed_on_inputs refuse_bad_scope_file manifest_matches_script crash_is_exit_2 catch_all_decides manifest_shape premise_intake_merged premise_parser refuse_guard_inputs_inside_repo smoke_sandbox_safety"

run_case() {
  FAILN=0
  echo "== $1"
  "$1"
  case "$W" in "$TR"/w.*) rm -rf -- "$W" ;; esac
  if [ "$FAILN" -eq 0 ]; then echo "PASS $1"; else echo "FAIL $1 ($FAILN)"; TOTALFAIL=$((TOTALFAIL + 1)); fi
}

if [ "$#" -gt 1 ]; then echo "usage: smoke-path-restriction.sh [case-name]" >&2; exit 2; fi
if [ "$#" -eq 1 ]; then
  if [ "$1" = _selftest_leave ]; then run_case _selftest_leave; exit 1; fi
  case " $CASES " in *" $1 "*) ;; *) echo "smoke: unknown case" >&2; exit 2;; esac
  run_case "$1"
  [ "$TOTALFAIL" -eq 0 ]; exit $?
fi
for c in $CASES; do run_case "$c"; done
[ "$TOTALFAIL" -eq 0 ]
