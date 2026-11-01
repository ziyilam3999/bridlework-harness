#!/usr/bin/env bash
# Smoke for plugins/claude/gh-text-hook.sh (Unix). Plan file name was smoke-gh-text.sh; named text-rule-smoke.sh
# so the persistence guard exempts it (<name>-smoke.sh).
# Usage: bash plugins/claude/text-rule-smoke.sh [case-name]
#   no argument: every case; exit 0 only if all pass. With a case name: that case only.
# Needs the built engine: cargo build --locked (in core/rust) first; exit 2 if it is absent.
# Run it once per scratch volume: TMPDIR=<case-sensitive volume> bash plugins/claude/text-rule-smoke.sh
# Test-only overrides (none can make the hook allow more; each only points the smoke at another copy):
#   SMOKE_HOOK          another copy of the hook (a mutant in a scratch folder).
#   SMOKE_MANIFEST      another manifest file (the manifest check must go red on a bad copy).
#   SMOKE_ENGINE_EXTRA  an extra stand-in engine executable; the hook must refuse a clean call through it.
#   SMOKE_LEAK_GH       1: put a second recording gh on the smoke PATH (the sink guard must go red).
#   SMOKE_REPORT        a file that receives the scratch folder name (used by the hygiene self-test).
#   SMOKE_RUN_PATH      extra PATH folders (e.g. /usr/bin:/bin) for the shell that runs an ALLOWED command, and
#   SMOKE_RUN_SHELL     the shell to run it in (e.g. zsh). Measurement only, used with case ac24_pass_through_r2 so a
#                       leak becomes a recorded stand-in call; the default run uses neither.
# The hook has one more test-only seam, HARNESS_TEST_SWAP_FILE (see the hook header).
# Markers come only from `harness fixtures`; no real regulated string appears in this file or its runs.
# Everything lives under one mktemp folder; a trap removes it even when a case fails.

HERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
ROOT=$(CDPATH= cd -- "$HERE/../.." && pwd)
HOOK_SRC=${SMOKE_HOOK:-$HERE/gh-text-hook.sh}
MANIFEST=${SMOKE_MANIFEST:-$HERE/gh-text-manifest.json}
BIN_SRC=$ROOT/core/rust/target/debug/harness
if [ ! -x "$BIN_SRC" ]; then
  echo "smoke: the built engine is missing; run cargo build --locked in core/rust first" >&2
  exit 2
fi
ENVBIN=/usr/bin/env
BASHBIN=$(command -v bash)
PY=$(command -v python3) || { echo "smoke: python3 is required" >&2; exit 2; }
DEFPATH="$(dirname "$PY"):/usr/bin:/bin"

T=$(mktemp -d "${TMPDIR:-/tmp}/p1a17.XXXXXX") || exit 2
TR=$(CDPATH= cd -P -- "$T" && pwd -P)
cleanup() {
  case "$T" in
    */p1a17.*) [ -n "$SMOKE_REPORT" ] && printf '%s\n' "$T" > "$SMOKE_REPORT"; rm -rf -- "$T" ;;
  esac
}
trap cleanup EXIT
trap 'exit 130' INT TERM

AT=$(printf '\100')
UNSET='__UNSET__'
FAILN=0
TOTALFAIL=0
PUB=fx-owner/fx-repo
PUBU=FX-Owner/FX-Repo

fail() { echo "  FAIL: $*" >&2; FAILN=$((FAILN + 1)); }

# One private copy of the engine per run; each world gets a hard link to it (a plain copy where links fail).
GBIN=$TR/engine
cp "$BIN_SRC" "$GBIN" && chmod +x "$GBIN"
link_bin() { ln "$GBIN" "$1" 2>/dev/null || cp "$GBIN" "$1"; }

# Synthetic material, fetched at run time from the engine's fixtures command.
M_PROFILE=$("$GBIN" fixtures marker deny-profile)
M_EMAIL=$("$GBIN" fixtures marker email)
L_SESSION=$("$GBIN" fixtures line session-link)
L_SESSION_URL=$("$GBIN" fixtures marker session-link)
X_TRAILER=$("$GBIN" fixtures extra session-trailer)
X_SYNTH=$("$GBIN" fixtures extra synthetic)
MARKS=()
for n in $("$GBIN" fixtures names); do MARKS+=("$("$GBIN" fixtures marker "$n")"); done
MARKS+=("$X_SYNTH" "$X_TRAILER" "$L_SESSION_URL")

json_str() {
  local s=$1
  s=${s//\\/\\\\}; s=${s//\"/\\\"}; s=${s//$'\n'/\\n}; s=${s//$'\t'/\\t}; s=${s//$'\r'/\\r}
  printf '%s' "$s"
}

new_world() {
  W=$(mktemp -d "$TR/w.XXXXXX")
  EXT=$W/ext; IN=$W/in; SINK=$W/sink
  mkdir -p "$EXT" "$IN" "$SINK" "$W/home" "$W/tmp"
  cp "$HOOK_SRC" "$EXT/hook.sh"; chmod +x "$EXT/hook.sh"
  link_bin "$EXT/harness"
  "$GBIN" fixtures list > "$EXT/list.txt"
  SINKLOG=$W/sink.log; : > "$SINKLOG"
  LEAKLOG=$W/leak.log; : > "$LEAKLOG"
  printf '#!/bin/sh\n{ printf "CALL"; for a in "$@"; do printf "\\t%%s" "$a"; done; printf "\\n"; } >> "$SINKLOG"\n' > "$SINK/gh"
  chmod +x "$SINK/gh"
  SINKPATH=$SINK
  if [ -n "$SMOKE_LEAK_GH" ]; then
    mkdir -p "$W/leak"
    printf '#!/bin/sh\necho leaked >> "%s"\n' "$LEAKLOG" > "$W/leak/gh"; chmod +x "$W/leak/gh"
    SINKPATH=$SINK:$W/leak
  fi
  H_BIN=$EXT/harness; H_LIST=$EXT/list.txt; H_REPO=$PUB; H_HOOK=$EXT/hook.sh
  HPATH=; HCWD=; XENV=()
  SINKN=0
  chmod 644 "$EXT/list.txt"
}

stub() { # name body...   (a stand-in engine in the world's ext folder)
  local n=$1; shift
  printf '#!/bin/sh\n%s\n' "$*" > "$EXT/$n"; chmod +x "$EXT/$n"
}

run_hook_file() { # stdin-file
  local a=("PATH=${HPATH:-$DEFPATH}" "HOME=$W/home" "TMPDIR=$W/tmp")
  [ "$H_BIN" != "$UNSET" ] && a+=("HARNESS_BIN=$H_BIN")
  [ "$H_LIST" != "$UNSET" ] && a+=("HARNESS_PRIVACY_LIST=$H_LIST")
  [ "$H_REPO" != "$UNSET" ] && a+=("HARNESS_PUBLIC_REPO=$H_REPO")
  [ "${#XENV[@]}" -gt 0 ] && a+=("${XENV[@]}")
  (cd "${HCWD:-$W/home}" && "$ENVBIN" -i "${a[@]}" "$BASHBIN" "$H_HOOK" < "$1" > "$W/out" 2> "$W/err")
  RC=$?
}

sink_guard() {
  local d n=0 IFS=:
  for d in $SINKPATH; do [ -e "$d/gh" ] && n=$((n + 1)); done
  [ "$n" -eq 1 ] || fail "gh is reachable outside the sink ($n copies on the smoke PATH)"
  [ -s "$LEAKLOG" ] && fail "a gh call reached something other than the sink"
}

run_sink() {
  (cd "$W/home" && "$ENVBIN" -i "PATH=$SINKPATH${SMOKE_RUN_PATH:+:$SMOKE_RUN_PATH}" "HOME=$W/home" "SINKLOG=$SINKLOG" "${SMOKE_RUN_SHELL:-$BASHBIN}" -c "$1") > /dev/null 2>&1
  SINKN=$(command grep -c '^CALL' "$SINKLOG" 2>/dev/null); SINKN=${SINKN:-0}
}

drive() { # command string: run the hook, and the dry-run sink only when the hook allowed it
  SINKN=0; : > "$SINKLOG"; : > "$LEAKLOG"
  printf '{"tool_name":"Bash","tool_input":{"command":"%s"}}' "$(json_str "$1")" > "$W/in.json"
  run_hook_file "$W/in.json"
  if [ "$RC" -eq 0 ]; then run_sink "$1"; fi
  sink_guard
}

expect_allow() {
  if [ "$RC" -ne 0 ] || [ -s "$W/out" ] || [ -s "$W/err" ]; then
    fail "$1: expected allow, got rc=$RC err=$(head -c 160 "$W/err" 2>/dev/null)"
  fi
}
expect_refuse() { # label [required substring]
  local n m bad
  if [ "$RC" -ne 2 ]; then fail "$1: expected exit 2, got rc=$RC (stand-in calls: $SINKN)"; return; fi
  [ -s "$W/out" ] && fail "$1: stdout not empty"
  [ -s "$W/err" ] || { fail "$1: no reason line"; return; }
  n=$(wc -l < "$W/err" | tr -d ' ')
  [ "$n" -ge 1 ] && [ "$n" -le 8 ] || fail "$1: reason is $n lines"
  bad=$(command grep -vc '^refused:' "$W/err")
  [ "$bad" -eq 0 ] || fail "$1: a stderr line does not start with refused:"
  for m in "${MARKS[@]}"; do
    if command grep -qF -- "$m" "$W/err"; then fail "$1: stderr carries matched text"; break; fi
  done
  if command grep -qF -- "$T" "$W/err" || command grep -qF -- "$TR" "$W/err"; then fail "$1: reason leaks a folder"; fi
  [ "$SINKN" -eq 0 ] || fail "$1: the sink recorded a call"
  if [ -n "$2" ] && ! command grep -qF -- "$2" "$W/err"; then fail "$1: reason does not contain '$2'"; fi
}
mono() { "$PY" -I -c 'import time; print(time.monotonic())'; }
elapsed() { "$PY" -I -c 'import sys; print(round(float(sys.argv[2]) - float(sys.argv[1]), 1))' "$1" "$2"; }
under_13() { "$PY" -I -c 'import sys; sys.exit(0 if float(sys.argv[2]) - float(sys.argv[1]) < 13.0 else 1)' "$1" "$2"; }
ok() { local f=$FAILN; drive "$2"; expect_allow "$1"; [ "$FAILN" -eq "$f" ] && echo "  case $1"; }
no() { local f=$FAILN; drive "$2"; expect_refuse "$1" "$3"; [ "$FAILN" -eq "$f" ] && echo "  case $1"; }
raw_no() { printf '%s' "$2" > "$W/in.json"; SINKN=0; run_hook_file "$W/in.json"; expect_refuse "$1" "$3"; }

# ---------------------------------------------------------------- AC 1
ac01_clean_and_marker() {
  new_world
  ok clean-create "gh pr create --title T --body B"
  [ "$SINKN" -eq 1 ] || fail "clean-create: the sink recorded $SINKN calls"
  [ "$(command grep '^CALL' "$SINKLOG")" = "$(printf 'CALL\tpr\tcreate\t--title\tT\t--body\tB')" ] || fail "clean-create: sink argv differs"
  no marker-body "gh pr create --title T --body 'x $M_PROFILE'" deny-token
  no marker-title "gh pr create --title '$M_PROFILE' --body B" deny-token
  no marker-email "gh pr comment 1 --body 'x $M_EMAIL'" email
}

# ---------------------------------------------------------------- AC 2
ac02_session_link() {
  new_world
  local nl; nl=$(printf 'fix it\n\n%s' "$X_TRAILER")
  ok plain-link-create "gh pr create --title T --body 'see https://example.invalid/page'"
  ok plain-link-comment "gh pr comment 1 --body 'see https://example.invalid/page'"
  ok plain-link-issue "gh issue comment 1 --body 'see https://example.invalid/page'"
  ok plain-link-release "gh release create v1 --title T --notes 'see https://example.invalid/page'"
  no url-create-body "gh pr create --title T --body '$L_SESSION'" session-link
  no url-create-title "gh pr create --title '$L_SESSION' --body B" session-link
  no url-pr-comment "gh pr comment 1 --body '$L_SESSION'" session-link
  no url-issue-comment "gh issue comment 1 --body '$L_SESSION'" session-link
  no url-release-notes "gh release create v1 --title T --notes '$L_SESSION'" session-link
  no url-release-title "gh release create v1 --title '$L_SESSION' --notes N" session-link
  no trailer-create "gh pr create --title T --body '$nl'" session-link
  no trailer-pr-comment "gh pr comment 1 --body '$nl'" session-link
  no trailer-issue-comment "gh issue comment 1 --body '$nl'" session-link
  no trailer-release "gh release create v1 --title T --notes '$nl'" session-link
}

# ---------------------------------------------------------------- AC 3
ac03_engine_inputs() {
  new_world
  ok twin-real-engine "gh pr comment 1 --body clean"
  H_BIN=$UNSET; no bin-unset "gh pr comment 1 --body clean"
  H_BIN=$EXT/nope; no bin-missing "gh pr comment 1 --body clean"
  printf '#!/bin/sh\nexit 0\n' > "$EXT/noexec"; chmod -x "$EXT/noexec"
  H_BIN=$EXT/noexec; no bin-not-executable "gh pr comment 1 --body clean"
  H_BIN=$EXT; no bin-is-folder "gh pr comment 1 --body clean"
  stub exit2 "exit 2"; H_BIN=$EXT/exit2; no stub-exit-2 "gh pr comment 1 --body clean"
  stub exit3 "exit 3"; H_BIN=$EXT/exit3; no stub-exit-3 "gh pr comment 1 --body clean"
  stub exit1 "exit 1"; H_BIN=$EXT/exit1; no stub-exit-1-no-output "gh pr comment 1 --body clean"
  stub killed 'kill -9 $$'; H_BIN=$EXT/killed; no stub-killed "gh pr comment 1 --body clean"
  stub exit0 "exit 0"; H_BIN=$EXT/exit0; ok stub-exit-0-is-clean "gh pr comment 1 --body clean"
  H_BIN=$EXT/harness; HCWD=$EXT; H_BIN=harness; no bin-relative "gh pr comment 1 --body clean"
  H_BIN=$EXT/harness; HCWD=
  ok twin-after "gh pr comment 1 --body clean"
  if [ -n "$SMOKE_ENGINE_EXTRA" ]; then
    cp "$SMOKE_ENGINE_EXTRA" "$EXT/extra"; chmod +x "$EXT/extra"; H_BIN=$EXT/extra
    no extra-stand-in-fails-closed "gh pr comment 1 --body clean"
  fi
}

# ---------------------------------------------------------------- AC 4
ac04_crash_is_refusal() {
  new_world
  ok twin-clean "gh pr comment 1 --body clean"
  [ -s "$W/out" ] || [ -s "$W/err" ] && fail "twin-clean: output is not empty"
  mkdir -p "$W/nopy"; HPATH=$W/nopy
  no python3-missing "gh pr comment 1 --body clean" python3
  HPATH=
  mkdir -p "$W/stubbin"
  printf '#!/bin/sh\nexit 3\n' > "$W/stubbin/python3"; chmod +x "$W/stubbin/python3"
  HPATH=$W/stubbin:$DEFPATH; no python3-stub-exit-3 "gh pr comment 1 --body clean" "internal error"
  printf '#!/bin/sh\nkill -9 $$\n' > "$W/stubbin/python3"
  no python3-stub-killed "gh pr comment 1 --body clean" "internal error"
  HPATH=
  stub hang "sleep 30"; H_BIN=$EXT/hang
  local t0 t1; t0=$(date +%s)
  no engine-hang "gh pr comment 1 --body clean"
  t1=$(date +%s); [ $((t1 - t0)) -le 8 ] || fail "engine-hang: took $((t1 - t0)) s"
  H_BIN=$EXT/harness
  raw_no not-json 'this is not json'
  raw_no empty-stdin ''
  raw_no json-array '[1]'
  raw_no no-command '{"tool_name":"Bash","tool_input":{}}'
  raw_no empty-command '{"tool_name":"Bash","tool_input":{"command":""}}'
  raw_no number-command '{"tool_name":"Bash","tool_input":{"command":5}}'
  raw_no no-tool-input '{"tool_name":"Bash"}'
  raw_no other-tool '{"tool_name":"Edit","tool_input":{"command":"echo hi"}}'
  raw_no nul-command '{"tool_name":"Bash","tool_input":{"command":"gh pr comment 1 --body a\u0000b"}}'
  local pre suf padn
  pre='{"tool_name":"Bash","tool_input":{"command":"gh pr comment 1 --body clean"},"pad":"'; suf='"}'
  padn=$((1048576 - ${#pre} - ${#suf}))
  { printf '%s' "$pre"; head -c "$padn" /dev/zero | tr '\0' x; printf '%s' "$suf"; } > "$W/big"
  SINKN=0; run_hook_file "$W/big"; expect_allow exactly-1MiB
  { printf '%s' "$pre"; head -c "$((padn + 1))" /dev/zero | tr '\0' x; printf '%s' "$suf"; } > "$W/big"
  run_hook_file "$W/big"; expect_refuse over-1MiB
}

# ---------------------------------------------------------------- AC 5
ac05_heredoc() {
  new_world
  ok twin-plain-body "gh pr comment 1 --body clean"
  no heredoc "gh pr comment 1 --body ok <<EOF
clean
EOF"
  no heredoc-dash "gh pr comment 1 --body ok <<-EOF
	clean
	EOF"
  no herestring "gh pr comment 1 --body ok <<< clean"
  no heredoc-in-subst "gh pr comment 1 --body \"\$(cat <<'EOF'
clean
EOF
)\""
  no heredoc-marker "gh pr comment 1 --body ok <<EOF
$M_PROFILE
EOF"
}

# ---------------------------------------------------------------- AC 6
ac06_expansion() {
  new_world
  ok twin-literal "gh pr comment 1 --body clean"
  ok twin-quoted-tilde "gh pr comment 1 --body '~x'"
  ok twin-quoted-equals "gh pr comment 1 --body '=x'"
  ok twin-quoted-star "gh pr comment 1 --body '*'"
  ok twin-quoted-brace "gh pr comment 1 --body '{a,b}'"
  ok twin-single-dollar "gh pr comment 1 --body '\$HOME \$(echo)'"
  no subst "gh pr comment 1 --body \"\$(echo clean)\""
  no subst-unquoted "gh pr comment 1 --body \$(echo clean)"
  no backtick "gh pr comment 1 --body \`echo clean\`"
  no brace-var "gh pr comment 1 --body \"\${X}\""
  no dollar-var "gh pr comment 1 --body \$X"
  no dollar-var-dq "gh pr comment 1 --body \"\$X\""
  no ansi-c "gh pr comment 1 --body \$'clean'"
  no procsubst "gh pr comment 1 --body <(echo clean)"
  no zsh-procsubst "gh pr comment 1 --body =(echo clean)"
  no brace-expansion "gh pr comment 1 --body {a,b}"
  no lead-tilde "gh pr comment 1 --body ~x"
  no lead-equals "gh pr comment 1 --body =x"
  no glob-star "gh pr comment 1 --body *"
  no glob-question "gh pr comment 1 --body ?"
  no glob-bracket "gh pr comment 1 --body [x]"
  no attached-subst "gh pr comment 1 --body=\$(echo clean)"
  no short-attached-subst "gh pr comment 1 -b\$(echo clean)"
  no title-subst "gh pr create --title \"\$(echo clean)\" --body B"
  no body-file-subst "gh pr comment 1 --body-file \$X"
}

# ---------------------------------------------------------------- AC 7
mkfiles() {
  printf 'clean text\n' > "$IN/clean.txt"; chmod 644 "$IN/clean.txt"
  printf 'note %s\n' "$M_PROFILE" > "$IN/marker.txt"; chmod 644 "$IN/marker.txt"
}
ac07_body_file() {
  new_world; mkfiles
  ok twin-clean-abs "gh pr comment 1 --body-file '$IN/clean.txt'"
  [ "$SINKN" -eq 1 ] || fail "twin-clean-abs: the sink recorded $SINKN calls"
  ok twin-short-flag "gh pr comment 1 -F '$IN/clean.txt'"
  ok twin-attached "gh pr comment 1 --body-file='$IN/clean.txt'"
  ok twin-pr-create "gh pr create --title T --body-file '$IN/clean.txt'"
  ok twin-release-notes "gh release create v1 --title T --notes-file '$IN/clean.txt'"
  ok twin-issue-comment "gh issue comment 1 --body-file '$IN/clean.txt'"
  ok twin-review "gh pr review 1 --body-file '$IN/clean.txt'"
  no marker-file "gh pr comment 1 --body-file '$IN/marker.txt'" deny-token
  no marker-file-create "gh pr create --title T -F '$IN/marker.txt'" deny-token
  no marker-file-release "gh release create v1 --title T --notes-file '$IN/marker.txt'" deny-token
  HCWD=$IN; no relative "gh pr comment 1 --body-file clean.txt"; HCWD=
  no chain-cd "cd '$IN' && gh pr comment 1 --body-file '$IN/clean.txt'"
  no chain-pushd "pushd '$IN' > /dev/null; gh pr comment 1 --body-file '$IN/clean.txt'"
  no chain-popd "popd; gh pr comment 1 --body-file '$IN/clean.txt'"
  ln -s "$IN/clean.txt" "$IN/link.txt"; no symlink "gh pr comment 1 --body-file '$IN/link.txt'"
  mkfifo "$IN/fifo.txt"; no fifo "gh pr comment 1 --body-file '$IN/fifo.txt'"
  no stdin-dash "gh pr comment 1 --body-file -"
  no stdin-dash-short "gh pr comment 1 -F -"
  mkdir -p "$IN/dir.txt"; no directory "gh pr comment 1 --body-file '$IN/dir.txt'"
  head -c 70000 /dev/zero | tr '\0' x > "$IN/big.txt"; chmod 644 "$IN/big.txt"; no over-cap "gh pr comment 1 --body-file '$IN/big.txt'"
  cp "$IN/clean.txt" "$IN/gw.txt"; chmod 664 "$IN/gw.txt"; no group-writable "gh pr comment 1 --body-file '$IN/gw.txt'"
  cp "$IN/clean.txt" "$IN/ww.txt"; chmod 666 "$IN/ww.txt"; no world-writable "gh pr comment 1 --body-file '$IN/ww.txt'"
  no missing "gh pr comment 1 --body-file '$IN/nope.txt'"
  cp "$IN/clean.txt" "$IN/unr.txt"; chmod 000 "$IN/unr.txt"; no unreadable "gh pr comment 1 --body-file '$IN/unr.txt'"
  no merge-clean-file "gh pr merge 1 --body-file '$IN/clean.txt'"
  no merge-clean-file-short "gh pr merge 1 -F '$IN/clean.txt'"
  ok twin-merge-inline-clean "gh pr merge 1 --body clean"
  # a case-sensitive volume hosts two names that differ only by case; a case-insensitive one cannot
  printf 'x\n' > "$IN/cs-a"; printf 'y\n' > "$IN/CS-A"
  if [ "$(cat "$IN/cs-a")" != x ]; then
    echo "  SKIPPED: case row (case-insensitive scratch volume)"
  else
    printf 'clean text\n' > "$IN/cs-a"; printf 'note %s\n' "$M_PROFILE" > "$IN/CS-A"; chmod 644 "$IN/cs-a" "$IN/CS-A"
    ok case-row-lower-clean "gh pr comment 1 --body-file '$IN/cs-a'"
    no case-row-upper-marker "gh pr comment 1 --body-file '$IN/CS-A'" deny-token
  fi
}

# ---------------------------------------------------------------- AC 8
ac08_stat_swap() {
  new_world; mkfiles
  ok twin-unchanged "gh pr comment 1 --body-file '$IN/clean.txt'"
  XENV=("HARNESS_TEST_SWAP_FILE=$IN/other.txt"); printf 'z\n' > "$IN/other.txt"
  ok twin-seam-other-path "gh pr comment 1 --body-file '$IN/clean.txt'"
  XENV=("HARNESS_TEST_SWAP_FILE=$IN/clean.txt")
  no swap-mid-read "gh pr comment 1 --body-file '$IN/clean.txt'" changed
  XENV=("HARNESS_TEST_SWAP_FILE=")
  mkfiles
  ok twin-seam-empty "gh pr comment 1 --body-file '$IN/clean.txt'"
}

# ---------------------------------------------------------------- AC 9
ac09_flags() {
  new_world
  ok twin-pair-clean "gh pr comment 1 --body clean --body clean2"
  ok twin-long-eq "gh pr create --title=T --body=clean"
  ok twin-short "gh pr create -t T -b clean"
  ok twin-short-attached "gh pr create -tT -bclean"
  no marker-long-eq "gh pr comment 1 --body='$M_PROFILE'" deny-token
  no marker-short-attached "gh pr comment 1 -b'$M_PROFILE'" deny-token
  no marker-first "gh pr comment 1 --body '$M_PROFILE' --body clean" deny-token
  no marker-second "gh pr comment 1 --body clean --body '$M_PROFILE'" deny-token
  no marker-title-eq "gh pr create --title='$M_PROFILE' --body B" deny-token
  no editor "gh pr comment 1 --editor"
  no editor-short "gh pr comment 1 -e"
  no editor-create "gh pr create --title T --body B --editor"
  no web "gh pr comment 1 --web"
  no web-short "gh pr create --title T --body B -w"
  no stdin-body-file "gh pr comment 1 --body-file -"
  no lone-dashdash "gh pr comment 1 --body clean --"
  no dashdash-flags-after "gh pr comment 1 -- --body clean"
  no unknown-flag "gh pr comment 1 --body clean --bogus"
  no unknown-flag-value "gh pr comment 1 --body clean --bogus x"
  no unknown-short "gh pr comment 1 --body clean -Z"
  no flag-no-value "gh pr comment 1 --body"
  no flag-no-value-short "gh pr comment 1 -b"
  no bool-bad-value "gh pr create --title T --body B --draft=maybe"
  no short-equals "gh pr comment 1 -b=clean"
  no no-text-comment "gh pr comment 1"
  no no-body-create "gh pr create --title T"
  ok twin-draft-bool "gh pr create --title T --body B --draft --base main"
}

# ---------------------------------------------------------------- AC 10
ac10_api() {
  new_world
  ok twin-bare-get "gh api repos/o/n"
  ok twin-jq "gh api repos/o/n --jq .name"
  no x-post "gh api -X POST repos/o/n"
  no x-patch "gh api -X PATCH repos/o/n"
  no x-put "gh api -X PUT repos/o/n"
  no x-delete "gh api -X DELETE repos/o/n"
  no x-get "gh api -X GET repos/o/n"
  no attached-xpost "gh api -XPOST repos/o/n"
  no method-eq "gh api --method=post repos/o/n"
  no method-long "gh api --method POST repos/o/n"
  no x-lower "gh api -X post repos/o/n"
  no field-f "gh api repos/o/n -f a=b"
  no field-F "gh api repos/o/n -F a=b"
  no field-long "gh api repos/o/n --field a=b"
  no raw-field "gh api repos/o/n --raw-field a=b"
  no input "gh api repos/o/n --input x"
  no graphql "gh api graphql"
  no graphql-slash "gh api /graphql"
  no method-after-endpoint "gh api repos/o/n -X POST"
}

# ---------------------------------------------------------------- AC 11
ac11_program_word() {
  new_world
  local C="pr comment 1 --body clean" M="pr comment 1 --body '$M_PROFILE'"
  ok command-gh "command gh $C"
  ok env-gh "env X=1 gh $C"
  ok path-gh "$SINK/gh $C"
  ok sudo-gh "sudo gh $C"
  ok nohup-gh "nohup gh $C"
  ok time-gh "time gh $C"
  ok exec-gh "exec gh $C"
  ok assign-gh "X=1 gh $C"
  ok twin-plain-echo "echo hello"
  ok twin-ghost-story "gh pr comment 1 --body 'ghost story'"
  ok twin-github-word "echo github.com"
  ok twin-chain-clean "echo hi && gh $C"
  ok twin-semicolon-clean "echo hi; gh $C"
  ok twin-pipe-clean "echo hi | gh $C"
  ok twin-oror-clean "false || gh $C"
  ok twin-amp-clean "echo hi & gh $C"
  ok twin-newline-clean "echo hi
gh $C"
  no chain-and "echo hi && gh $M" deny-token
  no chain-semicolon "echo hi; gh $M" deny-token
  no chain-pipe "echo hi | gh $M" deny-token
  no chain-oror "false || gh $M" deny-token
  no chain-amp "echo hi & gh $M" deny-token
  no chain-newline "echo hi
gh $M" deny-token
  no chain-first "gh $M && echo hi" deny-token
  no sh-c "sh -c \"gh $C\""
  no eval "eval \"gh $C\""
  no xargs "echo 1 | xargs gh pr view"
  no python-oneliner "python3 -c \"print('gh')\""
  no node-oneliner "node -e \"['gh','pr']\""
  no func-def "f() { gh $C; }; f"
  no alias-def "alias g=gh"
  no gh-alias "gh alias set x y"
  no gh-extension "gh extension list"
  no gh-config "gh config set a b"
  no subshell "(gh $C)"
  no brace-group "{ gh $C; }"
  no bang "! gh $C"
  no then "if true; then gh $C; fi"
  no do "for i in 1; do gh $C; done"
  no else "if false; then echo a; else gh $C; fi"
  no backslash-gh "\\gh $C"
  no dquote-gh "\"gh\" $C"
  no squote-gh "'gh' $C"
  no split-gh "g''h $C"
  no echo-gh "echo gh"
  no timeout "timeout 5 gh $C"
  no nice "nice gh $C"
  no stdbuf "stdbuf -o0 gh $C"
  no carrier-bash-c "bash -c 'gh $C'"
  no carrier-eval "eval 'gh $C'"
  no carrier-env-s "env -S 'gh $C'"
  no carrier-python "python3 -c \"x=['gh','pr']\""
  no carrier-comment "echo hi # gh $C"
  no zsh-equals-gh "=gh $C"
  no program-glob "/usr/bin/g? $C" # G:progword_literal
  no program-brace "g{h,h} $C" # G:progword_literal
  no program-glob-star "/usr/bin/g* $C" # G:progword_literal
  no gh-path-arg "echo /usr/bin/gh"
  # quote forms of the program word (ANSI-C, locale, split, escaped, backslash-newline)
  no ansi-quote-gh "g\$'h' $M" # G:progword_literal
  no ansi-quote-empty "g\$''h $M" # G:progword_literal
  no ansi-quote-octal "\$'\\147\\150' $M" # G:progword_literal
  no ansi-locale-quote "g\$\"\"h $M" # G:progword_literal
  no ansi-quote-chain "gh $C; g\$'h' $M" # G:progword_literal
  no bslash-newline-gh "g\\
h $M" # G:progword_literal
  no zsh-alternation-gh "/usr/bin/g(h|x) $M" # G:progword_literal
  # brace forms of the program word
  no brace-range-gh "g{h..h} $M" # G:progword_literal
  no brace-range-lead "{g..g}h $M" # G:progword_literal
  no brace-range-env "env g{h..h} $M" # G:progword_literal
  no brace-range-other-repo "g{h..h} pr comment 1 -R other/x --body '$M_PROFILE'" # G:progword_literal
  no brace-five-siblings "g{h,}{,}{,}{,}{,} $M" # G:progword_literal
  no brace-five-nested "{{{{{g,},},},},}h $M" # G:progword_literal
  no brace-slash-inside "{/usr/bin/g,x}h $M" # G:progword_literal
  # bracket forms of the program word, against the real folder that holds the stand-in
  no bracket-negate "$SINK/g[^a-g] $M" # G:progword_literal
  no bracket-negate-bang "$SINK/g[!a-g] $M" # G:progword_literal
  no bracket-class-alpha "$SINK/g[[:alpha:]] $M" # G:progword_literal
  no bracket-class-lower "$SINK/g[[:lower:]] $M" # G:progword_literal
  no bracket-class-lead "$SINK/[[:alpha:]]h $M" # G:progword_literal
  no bracket-class-both "$SINK/[[:alnum:]][[:alnum:]] $M" # G:progword_literal
  no bracket-negate-both "$SINK/[^a-f][^a-g] $M" # G:progword_literal
  no bracket-negate-class "$SINK/g[^[:upper:]] $M" # G:progword_literal
  no bracket-collating "$SINK/g[[.h.]] $M" # G:progword_literal
  no bracket-equivalence "$SINK/g[[=h=]] $M" # G:progword_literal
  no bracket-bslash-newline "$SINK/g\\
[^a-g] $M" # G:progword_literal
  no bracket-plain "$SINK/g[h] $M" # G:progword_literal
  no question-mark "$SINK/g? $M" # G:progword_literal
  no star-lead "$SINK/*h $M" # G:progword_literal
  # a "(" touching the last path segment is a zsh group, part of the word
  no zsh-group-lead "$SINK/(g)h $M" # G:glued_paren
  no zsh-group-alt-lead "$SINK/(g|x)h $M" # G:glued_paren
  no zsh-group-two "$SINK/(g|x)(h|y) $M" # G:glued_paren
  no zsh-group-bare "$SINK/(g)(h) $M" # G:glued_paren
  # an allowed prefix followed by any option: the option can change which word runs
  no env-dash-i "env -i $SINK/g? $M" # G:prefix_option
  no env-dash-u "env -u HOME $SINK/g? $M" # G:prefix_option
  no env-dashdash "env -- $SINK/g? $M" # G:prefix_option
  no env-dash-i-path "env -i PATH=$SINK g{h..h} $M" # G:prefix_option
  no env-dash-s-attached "env -Sgh $M" # G:prefix_option
  no command-dash-p "command -p $SINK/g? $M" # G:prefix_option
  no exec-dash-a "exec -a x $SINK/g? $M" # G:prefix_option
  no time-dash-p "time -p $SINK/g? $M" # G:prefix_option
  no sudo-dash-n "sudo -n $SINK/g? $M" # G:prefix_option
  no assign-then-command-dash-p "X=1 command -p $SINK/g? $M" # G:prefix_option
  # shell keywords hand the program position to the next word
  no keyword-then "if true; then g{h..h} $M; fi" # G:keyword_skip
  no keyword-do "for i in 1; do g{h..h} $M; done" # G:keyword_skip
  no keyword-else "if false; then echo a; else $SINK/g? $M; fi" # G:keyword_skip
  no keyword-elif "if false; then echo a; elif true; then $SINK/g? $M; fi" # G:keyword_skip
  no keyword-if-condition "if g{h..h} $M; then echo a; fi" # G:keyword_skip
  no keyword-while "while g{h..h} $M; do break; done" # G:keyword_skip
  no keyword-until "until g{h..h} $M; do echo a; done" # G:keyword_skip
  no keyword-bang "! $SINK/g? $M" # G:keyword_skip
  no keyword-brace-group "{ g{h..h} $M; }" # G:keyword_skip
  no keyword-function "function f { $SINK/g? $M; }; f" # G:keyword_skip
  no keyword-case-body "case x in x) $SINK/g? $M;; esac" # G:keyword_skip
  # a program inside a substitution is still a program word
  no sub-dollar-paren "echo \$($SINK/g? $M)" # G:sub_recurse
  no sub-backtick "echo \`g{h..h} $M\`" # G:sub_recurse
  no sub-assign "x=\$(g{h..h} $M)" # G:sub_recurse
  no sub-dquote "echo \"\$(g{h..h} $M)\"" # G:sub_recurse
  no sub-procsubst "cat <(g{h..h} $M)" # G:sub_recurse
  no sub-nested "echo \$(echo \$(echo \$(g{h..h} $M)))" # G:sub_recurse
  no sub-arith-nested "echo \$(( \$(g{h..h} $M) + 1 ))" # G:sub_recurse
  no sub-array "arr=(\$(g{h..h} $M)); echo" # G:sub_recurse
  # a program word made by an expansion cannot be read
  no expansion-at "g\$@h $M" # G:progword_literal
  no expansion-positional "g\$1h $M" # G:progword_literal
  no expansion-empty-backtick "g\`\`h $M" # G:progword_literal
  no expansion-default "\${x:-g}h $M" # G:progword_literal
  no expansion-subst-lead "\$(echo g)h $M" # G:progword_literal
  no expansion-var-whole "\$X $M" # G:progword_literal
  no tilde-program "~/bin/x $M" # G:progword_literal
  no non-ascii-program "g$(printf '\303\251')h $M" # G:progword_literal
  # text the parser cannot read is not text it can clear
  no unparseable-quote "echo 'unterminated" # G:cannot_decide
  no unparseable-dquote "echo \"unterminated" # G:cannot_decide
  no unparseable-paren "echo \$(echo hi" # G:cannot_decide
  no unparseable-backtick "echo \`echo hi" # G:cannot_decide
  no unparseable-backslash "echo hi \\" # G:cannot_decide
  local deep="echo hi" k
  for k in 1 2 3 4 5 6 7 8 9 10; do deep="echo \$($deep)"; done
  no sub-too-deep "$deep" # G:sub_depth
  # twins: patterns, expansions and groups that are NOT program words stay allowed
  ok twin-ls-glob "ls *.md"
  ok twin-ls-dir-glob "ls -d $SINK/z*"
  ok twin-echo-home "echo \$HOME"
  ok twin-echo-quoted "echo \"a b\" > /dev/null"
  ok twin-subst-args "echo \"\$(echo hi)\" \$(echo hi)"
  ok twin-arith-star "echo \$(( 6 * 7 )) \$((6*7))"
  ok twin-test-brackets "[ -f /nonexistent ] || [[ -n x ]] || echo none"
  ok twin-assign-quoted "FOO=\"a b\" ls > /dev/null"
  ok twin-for-glob "for f in *.md; do echo \"\$f\"; done"
  ok twin-brace-group "{ echo a; echo b; }"
  ok twin-subshell "(cd / && ls > /dev/null)"
  ok twin-function-def "f() { echo hi; }; f"
  ok twin-array "arr=(a b c); echo \${arr[1]}"
  ok twin-find-exec "find . -name '*.nothing' -exec ls {} +"
  ok twin-nightly "echo nightly high"
  ok twin-command-v "command -v ls" # G:plain_fast
  ok twin-time-p "time -p ls > /dev/null" # G:plain_fast
  ok twin-case-pattern "case x in *.md) echo md;; *.txt|*.rst) echo t;; esac" # G:case_pattern
  ok twin-heredoc-body "cat <<'EOF' > /dev/null
it's fine * [x] \$(not run) g?
EOF" # G:heredoc_skip
  ok twin-heredoc-in-subst "echo \"\$(cat <<'EOF'
it's fine * [x] g{h..h}
EOF
)\"" # G:heredoc_skip
  ok twin-heredoc-tab-strip "cat <<-EOF > /dev/null
	it's fine *
	EOF" # G:heredoc_skip
  # T-146: one rule decides pass-through words. A word whose value is unknown until the shell expands it (brace, glob,
  # group, expansion, substitution, replacement string) is refused in any command that is not a plain everyday one,
  # wherever the word stands and whatever the command is called. Every case below is a held-out spelling, none a list entry.
  local R="refused: gh text:" G1='g{h,x}' G2='g(h|x)'
  both() { # id, text before the disguised word, text after it: one case per shell spelling (bash brace, zsh group)
    no "$1-bash" "$2$G1 $3" "$R"
    no "$1-zsh" "$2$G2 $3" "$R"
  }
  both pt-nice "nice " "$M"
  both pt-nice-nice "nice nice " "$M"
  both pt-env-nice "env nice " "$M"
  both pt-timeout-dot "timeout .5 " "$M"
  both pt-timeout-hex "timeout 0x5 " "$M"
  both pt-timeout-exp "timeout 1e1 " "$M"
  both pt-timeout-inf "timeout inf " "$M"
  both pt-timeout-kill-after "timeout --kill-after=1 5 " "$M"
  both pt-gtimeout "gtimeout 5 " "$M"
  both pt-relative-dot "cd /usr/bin && ./nice " "$M"
  both pt-relative-dir "cd /usr && bin/nice " "$M"
  both pt-find-quoted-exec "find . -maxdepth 0 \"-exec\" " "$M \;"
  both pt-find-escaped-exec "find . -maxdepth 0 -ex\\ec " "$M \;"
  both pt-find-exec "find . -maxdepth 0 -exec " "$M \;"
  both pt-flock-file "flock $SINK/x.lock " "$M"
  both pt-flock-option "flock -n $SINK/x.lock " "$M"
  both pt-script-file "script /dev/null " "$M"
  both pt-script-option "script -q /dev/null " "$M"
  both pt-sandbox-exec "sandbox-exec -p '(version 1)(allow default)' " "$M"
  both pt-chroot "chroot / " "$M"
  both pt-xargs "echo x | xargs " "$M"
  both pt-nice-option-value "nice -n 1 " "$M"
  both pt-cd-nice "cd '$SINK' && nice " "$M"
  both pt-alias-bash "shopt -s expand_aliases
alias n=nice
n " "$M"
  both pt-function "n() { nice \"\$@\"; }; n " "$M"
  # held-out spellings of my own: a name on no list, a data word before the disguised word, a variable on the same line,
  # a disguised word inside a substitution that is an argument
  both pt-held-invented-wrapper "frobnicate --level 3 " "$M"
  both pt-held-data-positional "lockwrap $SINK/x.lock " "$M"
  no pt-held-same-line-variable "P=g; Q=h; frobnicate \"\$P\$Q\" $M" "$R"
  no pt-held-variable-prefix "P=g Q=h frobnicate \"\$P\$Q\" $M" "$R"
  no pt-held-substitution-argument "frobnicate \"\$(printf g)h\" $M" "$R"
  no pt-held-substitution-bare "nice \$(printf g)h $M" "$R"
  # a replacement string that takes the program name from standard input
  no pt-xargs-J-percent "printf '%sh\\n' g | xargs -J % % $M" "$R"
  no pt-xargs-I-word "printf '%sh\\n' g | xargs -I XX XX $M" "$R"
  no pt-xargs-I-quoted-brace "printf '%sh\\n' g | xargs -I'{}' '{}' $M" "$R"
  no pt-glued-paren "(g)h $M" "$R"
  # a plain everyday command may carry unknown words
  ok pt-echo-home-pipe "echo \"\$HOME\" | wc -c"
  ok pt-rsync-plain "rsync -a src/ dest/"
  ok pt-find-rustfmt "find . -name '*.rs' -exec rustfmt --check {} +"
  ok pt-find-brace-suffix "find . -name '*.tmp' -exec cp {} {}.bak \;"
  ok pt-assign-then-plain "FOO=1 echo \"\$HOME\""
  ok pt-keyword-then-plain "if [ -n \"\$HOME\" ]; then echo \"\$HOME\"; fi"
  ok pt-tilde-plain "rsync -a ~/src dest/"
  ok pt-gh-repeated-word "gh pr create --title same --body same"
  ok pt-xargs-d-quoted "xargs -d '\\n' rm -f < list.txt"
  ok pt-nice-make "nice make"
  ok pt-nice-n-make "nice -n 5 make -j4"
  ok pt-timeout-cargo "timeout 5 cargo test"
  ok pt-timeout-30s "timeout 30s cargo test --locked"
  ok pt-xargs-wc "echo x | xargs wc -l"
  ok pt-xargs-n-P "xargs -n 1 -P 4 echo"
  ok pt-find-exec-rm "find . -name '*.nothing' -exec rm {} \;"
  ok pt-find-exec-bash-n "find src -type f -name '*.sh' -exec bash -n {} \;"
  ok pt-env-assign-nice "env A=1 nice make"
  # refused as the price of the single rule (recorded in the close note): an unknown value in a command that may run it
  no cost-make-variable "make -j4 test \"\$T\"" "$R"
  no cost-xargs-I-brace "xargs -I{} cp {} out/ < list.txt" "$R"
  no cost-timeout-variable "timeout \"\$T\" make" "$R"
  no cost-nice-variable "nice -n \"\$N\" make" "$R"
  # input bound: a command past the cap is refused, not read
  local longword; longword=$(printf '%*s' 70000 '' | tr ' ' a)
  no input-too-long "echo \"\$HOME\" $longword" "$R"
  local manywords; manywords=$(printf ' a%.0s' $(seq 1 4100))
  no input-too-many-words "echo \"\$HOME\"$manywords" "$R"
  ok input-under-the-caps "echo \"\$HOME\" a b c"
}

# ---------------------------------------------------------------- AC 12
ac12_target() {
  new_world
  local u="user${AT}gith.example"
  local B="--body '$M_PROFILE'" C="pr comment 1"
  ok other-repo-marker-passes "gh $C -R other/repo $B"
  ok other-repo-long "gh $C --repo other/repo $B"
  ok other-repo-eq "gh $C --repo=other/repo $B"
  ok other-repo-attached "gh $C -Rother/repo $B"
  no case-variant "gh $C -R $PUBU $B" deny-token
  no host-form "gh $C -R github.com/$PUB $B" deny-token
  no url-git "gh $C -R https://github.com/$PUB.git $B" deny-token
  no repo-eq "gh $C --repo=$PUB $B" deny-token
  no repeated-other-first "gh $C -R other/x -R $PUB $B"
  no repeated-public-first "gh $C -R $PUB -R other/x $B"
  no repeated-both-other "gh $C -R other/x -R other/y $B"
  no pr-url-positional "gh pr comment https://github.com/$PUB/pull/1 -R other/x $B" deny-token
  no env-assign "GH_REPO=$PUB gh $C $B" deny-token
  no env-export "export GH_REPO=$PUB ; gh $C $B" deny-token
  no cd-public "cd '$W' && gh $C $B" deny-token
  no no-repo-flag "gh $C $B" deny-token
  no scp-form "gh $C -R $u:$PUB.git $B" deny-token
  no ssh-form "gh $C -R ssh://$u/$PUB.git $B" deny-token
  no http-form "gh $C -R http://github.com/$PUB $B" deny-token
  no www-form "gh $C -R https://www.github.com/$PUB $B" deny-token
  no trailing-slash-url "gh $C -R https://github.com/$PUB/ $B" deny-token
  no api-host "gh $C -R api.github.com/$PUB $B" deny-token
  no unparseable-space "gh $C -R 'a b/c' --body clean"
  no unparseable-trailing "gh $C -R $PUB/ --body clean"
  no unparseable-one-word "gh $C -R justone --body clean"
  no subst-positional-other-repo "gh pr comment \$(echo) -R other/x --body clean"
  no subst-flag-value-other-repo "gh pr create --title T --body B --base \$(echo) -R other/x"
  no subst-word-other-repo "gh $C -R other/x --body clean \$(echo)"
  no brace-word-other-repo "gh $C -{R,x} other/x --body clean"
  no env-host-other-repo "GH_HOST=h.example gh $C -R other/x $B" deny-token
  no env-export-other-repo "export GH_REPO=other/y ; gh $C -R other/x $B" deny-token
  no env-prefix-other-repo "env GH_TOKEN=t gh $C -R other/x $B" deny-token
  no public-word-in-body "gh $C -R other/x --body '$M_PROFILE see $PUB'" deny-token
  no dotgit-upper "gh $C -R $PUB.GIT $B" deny-token # G:target_normalise
  no dotgit-mixed "gh $C -R $PUB.Git $B" deny-token
  no repo-value-contains-public "gh $C -R $PUB-fork $B" deny-token # G:target_repo
  ok other-repo-clean "gh $C -R other/repo --body clean"
}

# ---------------------------------------------------------------- AC 13
ac13_read_only() {
  new_world
  local e
  for e in "gh pr list" "gh pr view 1" "gh run list" "gh issue view 2" "gh auth status" "gh pr merge 1 --squash" \
           "gh pr checks 1" "gh pr diff 1" "gh release view v1" "gh search issues x" "gh browse" "gh status" "gh --version" "gh api repos/o/n"; do
    ok "read-only $e" "$e"
    [ -s "$W/out" ] || [ -s "$W/err" ] && fail "read-only $e: output is not empty"
  done
  no merge-body-marker "gh pr merge 1 --body '$M_PROFILE'" deny-token
  no merge-subject-marker "gh pr merge 1 --subject '$M_PROFILE'" deny-token
  no merge-author-email "gh pr merge 1 --author-email '$M_EMAIL'" email
  no merge-author-email-short "gh pr merge 1 -A '$M_EMAIL'" email
  no release-tag-marker "gh release create '$M_PROFILE' --title T --notes N" deny-token
  no release-edit-tag-marker "gh release edit v1 --tag '$M_PROFILE'" deny-token
  no release-no-tag "gh release create --title T --notes N"
  ok twin-merge-clean "gh pr merge 1 --squash --body clean --subject S"
  ok twin-release-clean "gh release create v1 --title T --notes N"
  ok twin-close-clean "gh pr close 1 --comment clean"
  no close-marker "gh pr close 1 --comment '$M_PROFILE'" deny-token
  no issue-close-marker "gh issue close 1 -c '$M_PROFILE'" deny-token
  no unknown-subcommand "gh codespace delete"
  no pr-reopen "gh pr reopen 1"
}

# ---------------------------------------------------------------- AC 14
ac14_no_echo() {
  new_world
  local line="x $M_PROFILE y"
  stub echo1 'cat; exit 1'; H_BIN=$EXT/echo1
  no engine-echoes-stdout-exit1 "gh pr comment 1 --body '$line'"
  stub echo2 'cat >&2; exit 0'; H_BIN=$EXT/echo2
  no-or-ok_noisy engine-echoes-stderr-exit0 "gh pr comment 1 --body '$line'"
  stub echo3 'cat; exit 0'; H_BIN=$EXT/echo3
  no engine-echoes-stdout-exit0 "gh pr comment 1 --body '$line'"
  stub echo4 'printf "pr_body:%s\n" "$(cat | tr -d " \n")"; exit 1'; H_BIN=$EXT/echo4
  no engine-echoes-shaped-line "gh pr comment 1 --body '$M_PROFILE'"
  stub echo5 'printf "comment_body:deny-token\n"; exit 1'; H_BIN=$EXT/echo5
  no engine-wrong-surface-name "gh pr comment 1 --body clean"
  H_BIN=$EXT/harness
  no real-marker "gh pr comment 1 --body '$line'" deny-token
  if command grep -rlF -- "$M_PROFILE" "$W/home" "$W/tmp" > /dev/null 2>&1; then fail "a file under the hook HOME or TMPDIR carries the marker"; fi
}
no-or-ok_noisy() { drive "$2"; if [ "$RC" -eq 2 ]; then expect_refuse "$1"; else expect_allow "$1"; fi; }

# ---------------------------------------------------------------- AC 15
ac15_testmode() {
  new_world
  stub rec "printf '%s\n' \"\$*\" >> \"$W/argv.log\"; exec \"$EXT/harness\" \"\$@\""
  H_BIN=$EXT/rec
  ok rec-clean "gh pr comment 1 --body clean"
  no synthetic-is-a-hit "gh pr comment 1 --body '$X_SYNTH'" synthetic-test
  if command grep -q -- '--testmode' "$W/argv.log"; then fail "the engine argv carries --testmode"; fi
  [ -s "$W/argv.log" ] || fail "the recording stand-in saw no call"
  local rc
  printf '%s\n' "$X_SYNTH" | HARNESS_PRIVACY_LIST="$EXT/list.txt" "$GBIN" privacy scan --text --surface s > /dev/null 2>&1; rc=$?
  [ "$rc" -eq 1 ] || fail "direct engine without test mode: expected 1, got $rc"
  printf '%s\n' "$X_SYNTH" | HARNESS_PRIVACY_LIST="$EXT/list.txt" "$GBIN" --testmode synthetic-test privacy scan --text --surface s > /dev/null 2>&1; rc=$?
  [ "$rc" -eq 0 ] || fail "direct engine with test mode: expected 0, got $rc"
}

# ---------------------------------------------------------------- AC 16
manifest_ok() { # file; exit 0 when the shape holds
  "$PY" -I - "$1" "$HERE" <<'PYEOF'
import json, os, re, sys
m = json.load(open(sys.argv[1]))
assert set(m) == {"hook", "event", "tools", "env", "requires", "exit_allow", "exit_refuse"}, "keys"
assert m["event"] == "PreToolUse" and m["tools"] == ["Bash"] and m["requires"] == ["python3"], "event/tools/requires"
assert m["exit_allow"] == 0 and m["exit_refuse"] == 2, "exits"
assert m["env"] == ["HARNESS_BIN", "HARNESS_PRIVACY_LIST", "HARNESS_PUBLIC_REPO"], "env"
h = os.path.join(sys.argv[2], m["hook"])
assert os.path.basename(m["hook"]) == m["hook"] and os.path.isfile(h), "hook file"
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
ac16_manifest() {
  new_world
  manifest_ok "$MANIFEST" 2>/dev/null || fail "the manifest does not match the plan shape"
  # same key set as the merged P1A-15 manifest
  local p15=$HERE/manifest.json
  if [ -f "$p15" ]; then
    local a b
    a=$("$PY" -I -c 'import json,sys; print(sorted(json.load(open(sys.argv[1]))))' "$p15")
    b=$("$PY" -I -c 'import json,sys; print(sorted(json.load(open(sys.argv[1]))))' "$MANIFEST")
    [ "$a" = "$b" ] || fail "the manifest key set differs from the P1A-15 manifest"
  fi
  # planted twins: the check must fail on each bad copy
  local g=$W/m.json bad
  "$PY" -I - "$MANIFEST" "$W" <<'PYEOF'
import json, sys
m = json.load(open(sys.argv[1]))
w = sys.argv[2]
a = dict(m); del a["env"]; json.dump(a, open(w + "/m-missing.json", "w"))
b = dict(m); b["extra"] = "x"; json.dump(b, open(w + "/m-extra.json", "w"))
c = dict(m); c["event"] = "PostToolUse"; json.dump(c, open(w + "/m-event.json", "w"))
d = dict(m); d["tools"] = ["Edit"]; json.dump(d, open(w + "/m-tools.json", "w"))
e = dict(m); e["hook"] = "no-such-hook.sh"; json.dump(e, open(w + "/m-hook.json", "w"))
PYEOF
  for bad in missing extra event tools hook; do
    if manifest_ok "$W/m-$bad.json" 2>/dev/null; then fail "the manifest check accepted a bad copy ($bad)"; fi
  done
}

# ---------------------------------------------------------------- AC 18
ac18_hygiene() {
  new_world
  sink_guard
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
  # the sink is the only gh the smoke can reach; a clean call lands there exactly once
  ok sink-gets-the-call "gh pr comment 1 --body clean"
  [ "$SINKN" -eq 1 ] || fail "the sink did not get the call"
  [ "$(PATH=$SINKPATH command -v gh)" = "$SINK/gh" ] || fail "gh on the smoke PATH is not the sink"
}
_selftest_leave() { fail "planted failure"; }

# ---------------------------------------------------------------- AC 19
ac19_public_repo_env() {
  new_world
  local c e
  for e in "$UNSET" "" "noslash" "a/b/c" "a b/c" "/x" "x/" "-/-/-"; do
    H_REPO=$e
    for c in "gh pr comment 1 --body clean" "gh pr create --title T --body B" "gh pr comment 1 -R other/x --body clean" \
             "gh pr comment 1 --body \$(echo)" "gh pr merge 1 --body clean"; do
      no "repo-env [$e] $c" "$c"
    done
    ok "repo-env [$e] read-only" "gh pr view 1"
    ok "repo-env [$e] merge-no-text" "gh pr merge 1 --squash"
    ok "repo-env [$e] no gh" "echo hello"
  done
  H_REPO=$PUB
  ok twin-valid "gh pr comment 1 --body clean"
  H_REPO=other/other; ok twin-other-valid "gh pr comment 1 --body clean"
}

# ---------------------------------------------------------------- AC 20
ac20_fields() {
  new_world
  local i c
  c="gh pr comment 1"; for i in 1 2 3 4 5 6 7 8; do c="$c --body f$i"; done
  ok eight-fields "$c"
  c="$c --body f9"; no nine-fields "$c" "too many"
  c="gh pr create"; for i in 1 2 3 4; do c="$c --title t$i --body b$i"; done
  ok eight-fields-mixed "$c"
  stub hang "sleep 30"; H_BIN=$EXT/hang
  local t0 t1
  t0=$(mono); no hang-refused "gh pr comment 1 --body clean"; t1=$(mono)
  under_13 "$t0" "$t1" || fail "hanging engine: refused after $(elapsed "$t0" "$t1") s"
  H_BIN=$EXT/harness
  [ "$("$PY" -I -c 'import json,sys; print("timeout" in json.load(open(sys.argv[1])))' "$MANIFEST")" = False ] || fail "the manifest has a timeout key"
}

ac20_deadline() {
  new_world
  local i c t0 t1
  stub slow "sleep 2; exit 0"; H_BIN=$EXT/slow
  c="gh pr comment 1"; for i in 1 2 3 4 5 6 7 8; do c="$c --body s$i"; done
  t0=$(mono); no deadline-refused "$c"; t1=$(mono)
  under_13 "$t0" "$t1" || fail "slow engine: refused after $(elapsed "$t0" "$t1") s"
  H_BIN=$EXT/harness
  ok twin-after "gh pr comment 1 --body clean"
}

# ---------------------------------------------------------------- AC 21
ac21_deny_list() {
  new_world
  local c='gh pr comment 1 --body clean' m="gh pr comment 1 --body '$M_PROFILE'"
  ok twin-valid "$c"
  H_LIST=$UNSET; no list-unset "$c"
  H_LIST=; no list-empty "$c"
  cp "$EXT/list.txt" "$EXT/list.rel"; chmod 644 "$EXT/list.rel"
  H_LIST=list.rel; HCWD=$EXT; no list-relative "$c"; HCWD=
  H_LIST=$EXT/nope.txt; no list-missing "$c"
  cp "$EXT/list.txt" "$EXT/unr.txt"; chmod 000 "$EXT/unr.txt"
  H_LIST=$EXT/unr.txt; no list-unreadable "$c"
  H_LIST=$EXT; no list-is-folder "$c"
  H_LIST=$EXT/list.txt
  ok read-only-without-list "gh pr view 1"
  H_LIST=$UNSET; ok read-only-no-list "gh pr view 1"; H_LIST=$EXT/list.txt
  XENV=("HARNESS_PRIVACY_LIST_SECRET=" "HARNESS_PRIVACY_OLD_LIST=$EXT/nope.txt")
  ok scrubbed-env-clean "$c"
  no scrubbed-env-marker "$m" deny-token
  cp "$EXT/list.txt" "$EXT/old.txt"; XENV=("HARNESS_PRIVACY_OLD_LIST=$EXT/old.txt" "HARNESS_PRIVACY_LIST_SECRET=zzother")
  ok scrubbed-env-secret-clean "$c"
  no scrubbed-env-secret-marker "$m" deny-token
  XENV=()
  # the smoke builds its list with the engine's fixtures command
  [ -s "$EXT/list.txt" ] || fail "the synthetic list is empty"
}

# ---------------------------------------------------------------- AC 22
ac22_content_shapes() {
  new_world
  ok twin-clean-create "gh pr create --title T --body B"
  no gist-create "gh gist create f.txt"
  no gist-edit "gh gist edit abc"
  no repo-create "gh repo create x"
  no repo-edit "gh repo edit --description clean"
  no repo-edit-visibility "gh repo edit --visibility public"
  no release-asset "gh release create v1 f.txt --title T --notes N"
  no release-upload "gh release upload v1 f.txt"
  no fill "gh pr create --title T --body B --fill"
  no fill-short "gh pr create --title T --body B -f"
  no fill-first "gh pr create --title T --body B --fill-first"
  no fill-verbose "gh pr create --title T --body B --fill-verbose"
  no template "gh pr create --title T --body B --template x.md"
  no template-short "gh pr create --title T --body B -T x.md"
  no template-issue "gh issue create --title T --body B -T x.md"
  no recover "gh pr create --title T --body B --recover x"
  no recover-issue "gh issue create --title T --body B --recover x"
  no notes-from-tag "gh release create v1 --title T --notes N --notes-from-tag"
  no generate-notes "gh release create v1 --title T --notes N --generate-notes"
  no notes-start-tag "gh release create v1 --title T --notes N --notes-start-tag v0"
  ok twin-release-clean "gh release create v1 --title T --notes N"
}

# ---------------------------------------------------------------- AC 23
ac23_anchors() {
  new_world
  local c='gh pr comment 1 --body clean'
  ok twin-outside "$c"
  mkdir -p "$W/repo" "$W/wt" "$W/cleanparent"
  git init -q "$W/repo" > /dev/null 2>&1
  : > "$W/wt/.git"
  # engine
  link_bin "$W/repo/harness"; H_BIN=$W/repo/harness; no engine-in-repo "$c"
  link_bin "$W/repo/.git/harness"; H_BIN=$W/repo/.git/harness; no engine-in-dotgit "$c"
  link_bin "$W/wt/harness"; H_BIN=$W/wt/harness; no engine-in-worktree-file "$c"
  mkdir -p "$W/repo/sub/deeper"; link_bin "$W/repo/sub/deeper/harness"; H_BIN=$W/repo/sub/deeper/harness; no engine-deep-in-repo "$c"
  ln -s "$W/repo/harness" "$W/cleanparent/harness-link"; H_BIN=$W/cleanparent/harness-link; no engine-symlink-into-repo "$c"
  link_bin "$W/cleanparent/harness"; H_BIN=$W/cleanparent/harness; ok engine-outside-any-repo "$c"
  H_BIN=$EXT/harness
  # list
  cp "$EXT/list.txt" "$W/repo/list.txt"; chmod 644 "$W/repo/list.txt"; H_LIST=$W/repo/list.txt; no list-in-repo "$c"
  cp "$EXT/list.txt" "$W/repo/.git/list.txt"; chmod 644 "$W/repo/.git/list.txt"; H_LIST=$W/repo/.git/list.txt; no list-in-dotgit "$c"
  cp "$EXT/list.txt" "$W/wt/list.txt"; chmod 644 "$W/wt/list.txt"; H_LIST=$W/wt/list.txt; no list-in-worktree-file "$c"
  cp "$EXT/list.txt" "$W/cleanparent/list.txt"; chmod 644 "$W/cleanparent/list.txt"; H_LIST=$W/cleanparent/list.txt; ok list-outside-any-repo "$c"
  H_LIST=$EXT/list.txt
  # hook
  cp "$HOOK_SRC" "$W/repo/hook.sh"; H_HOOK=$W/repo/hook.sh; no hook-in-repo "$c"
  cp "$HOOK_SRC" "$W/wt/hook.sh"; H_HOOK=$W/wt/hook.sh; no hook-in-worktree-file "$c"
  ln -s "$EXT/hook.sh" "$W/repo/hook-link.sh"; H_HOOK=$W/repo/hook-link.sh; ok hook-symlink-resolves-outside "$c"
  cp "$HOOK_SRC" "$W/cleanparent/hook.sh"; H_HOOK=$W/cleanparent/hook.sh; ok hook-outside-any-repo "$c"
  H_HOOK=$EXT/hook.sh
  # a .git component that no real repo backs (on a case-sensitive volume only the component rule can see it)
  mkdir -p "$W/plain/.GIT"; link_bin "$W/plain/.GIT/harness"; H_BIN=$W/plain/.GIT/harness; no engine-under-dotgit-component "$c"
  H_BIN=$EXT/harness
  ok twin-after "$c"
}

# ---------------------------------------------------------------- AC 24 (T-146 fix round 2)
# The plain-command trust is not an answer key: git, make, cargo and rg run arguments, a line can rebind a PLAIN name,
# and a PLAIN command can build the program a later word runs. Every case is a held-out spelling.
ac24_pass_through_r2() {
  new_world
  local R="refused: gh text:" M="pr comment 1 --body '$M_PROFILE'" M2="pr comment 1 -b '$M_PROFILE'" NL=$'\n' G1='g{h,x}' G2='g(h|x)'
  # M2 has no long option: a long option such as --body already makes a plain command untrusted (G:long_option), so the
  # cases that pin the name-rebinding and file-building guards use the short flag to leave that guard out of the way.
  both() { no "$1-bash" "$2$G1 $3" "$R"; no "$1-zsh" "$2$G2 $3" "$R"; }
  # group 1: a command that runs an argument as a program (reviewer shapes N01-N03, N13, N14, N25, N34, N35)
  no r2-N01-git-alias-var "X=h; git -c \"alias.y=!g\$X $M\" y" "$R"
  no r2-N02-git-alias-subst "git -c \"alias.y=!\$(printf g)h $M\" y" "$R"
  both r2-N03-git-alias-loop "for p in " "; do git -c \"alias.y=!\$p $M\" y; done"
  no r2-N14-git-alias-printf-v "printf -v P 'g%s' h; git -c \"alias.y=!\$P $M\" y" "$R"
  no r2-N34-git-alias-export "export P=g; git -c \"alias.y=!\${P}h $M\" y" "$R"
  no r2-N35-git-alias-read "read P <<< gh2; git -c \"alias.y=!\${P%2} $M\" y" "$R"
  no r2-N13-make-cc "touch a.c && X=h && make CC=g\$X \"CFLAGS=$M\" a" "$R"
  no r2-N25-rg-pre "printf x > f && X=h && rg --pre g\$X x f" "$R"
  # per-command argument rules with literal values (no unknown word, so only the argument rule can refuse them)
  no r2-git-c-literal "git -c alias.y='!true' y" "$R"
  no r2-git-config-alias "git config alias.y \"true\"" "$R"
  no r2-git-bisect-run "git bisect run \"true\"" "$R"
  no r2-git-rebase-exec "git rebase --exec \"true\" HEAD" "$R"
  no r2-git-upload-pack "git ls-remote --upload-pack=\"true\" origin" "$R"
  no r2-make-command-variable "make CC=\"true\" a" "$R"
  no r2-make-script-from-pipe "printf 'a:\\n\\ttrue\\n' | make -f - a" "$R"
  no r2-cargo-config-runner "cargo --config 'target.x.runner=\"true\"' test" "$R"
  no r2-rg-pre-literal "rg --pre \"true\" x f" "$R"
  no r2-runner-env-literal "GIT_PAGER=\"true\" git log" "$R"
  no r2-env-hook-var-dynamic "X=h; export GIT_PAGER=g\$X; git log" "$R"
  # group 2: a line that rebinds a PLAIN name (N07-N10, and held-out spellings of my own)
  no r2-N07-alias-plain-name "shopt -s expand_aliases${NL}alias cat=nice${NL}cat $G1 $M2" "$R"
  no r2-N08-hash-p "hash -p /usr/bin/nice cat; cat $G1 $M2" "$R"
  no r2-N09-hash-assign "hash cat=/usr/bin/nice; cat $G2 $M2" "$R"
  no r2-N10-path-symlink "ln -s /usr/bin/nice cat && PATH=.:\$PATH cat $G1 $M2" "$R"
  both r2-held-builtin-alias "shopt -s expand_aliases${NL}builtin alias cat=nice${NL}cat " "$M2"
  both r2-held-command-alias "shopt -s expand_aliases${NL}command alias cat=nice${NL}cat " "$M2"
  no r2-held-export-path-by-name "ln -s /usr/bin/nice cat; P=PA; Q=TH; export \"\$P\$Q=.:\$PATH\"; cat $G1 $M2" "$R"
  no r2-held-function-name-plain "cat() { true; }; cat \"\$f\"" "$R"
  no r2-held-function-keyword "function cat { true; }; cat \"\$f\"" "$R"
  # group 3: a PLAIN command builds a program from an unknown word, or a path a command wrote is run (N11, N12)
  no r2-N11-ln-glob "ln -s $SINK/g? x && ./x $M2" "$R"
  no r2-N12-cp-glob "cp $SINK/g? x2 && ./x2 $M2" "$R"
  no r2-held-cat-redirect-build "cat $SINK/g? > x3 && chmod +x x3 && ./x3 $M2" "$R"
  no r2-held-tee-build "cat $SINK/g? | tee x4 > /dev/null; chmod +x x4; ./x4 $M2" "$R"
  no r2-held-install-build "install $SINK/g? x5 && ./x5 $M2" "$R"
  no r2-held-run-path-in-other-dir "mkdir d; cat $SINK/g? > d/x; chmod +x d/x; cd d; ./x $M2" "$R"
  no r2-held-written-then-sh "printf 'true' > x.sh; sh x.sh" "$R"
  no r2-cost-cp-glob "cp *.md out/" "$R"
  no r2-cost-redirect-unknown "echo \"\$HOME\" > \"\$out\"" "$R"
  # group 4: a placeholder glued into the program word of find (N04, N05) and of other tools
  no r2-N04-find-glued-after "mkdir h && find h -prune -exec g{} $M \\;" "$R"
  no r2-N05-find-glued-before "mkdir g && find g -prune -exec {}h $M \\;" "$R"
  no r2-held-find-ok "mkdir h && find h -prune -ok g{} $M \\;" "$R"
  no r2-held-find-execdir-bare "find . -maxdepth 0 -execdir {} $M \\;" "$R"
  no r2-held-parallel-glued "printf 'h\\n' | parallel g{} $M" "$R"
  no r2-held-fd-glued "fd -x g{} $M" "$R"
  no r2-held-parallel-bare-placeholder "printf '%sh\\n' g | parallel {} $M" "$R"
  no r2-held-parallel-quoted-placeholder "printf '%sh\\n' g | parallel '{}' $M" "$R"
  no r2-held-parallel-I-word "printf '%sh\\n' g | parallel -I XX XX $M" "$R"
  no r2-held-find-placeholder-class "find . -type f -exec cp {} {}\$X \\;" "$R"
  # group 5: the replacement string glued into an option (N06)
  no r2-N06-xargs-J-glued "printf '%sh\\n' g | xargs -J% % $M" "$R"
  no r2-held-xargs-I-glued "printf '%sh\\n' g | xargs -I% g% $M" "$R"
  no r2-held-xargs-I-word-glued "printf '%sh\\n' g | xargs -IXX XX $M" "$R"
  no r2-held-xargs-J-spaced-glued "printf 'h\\n' | xargs -J XX gXX $M" "$R"
  no r2-held-xargs-replace-long "printf 'h\\n' | xargs --replace=XX gXX $M" "$R"
  # a PLAIN command with an option that names a program to run (not git, make, cargo or rg)
  no r2-held-sort-compress-program "seq 1 5 | sort --compress-program=$SINK/g? > /dev/null" "$R"
  no r2-held-grep-pre "grep --pre=$SINK/g? x f" "$R"
  # must not fire
  ok r2-ok-git-status "git status"
  ok r2-ok-git-log-head "git log --oneline | head -5"
  ok r2-ok-git-commit "git commit -m \"msg\""
  ok r2-ok-make-j "make -j4 test"
  ok r2-ok-cargo-lib "cargo test --lib"
  ok r2-ok-cat-wc "cat file | wc -l"
  ok r2-ok-ls-tilde "ls ~/notes"
  ok r2-ok-grep-glob "grep -rn TODO src/*.rs"
  ok r2-ok-find-rustfmt "find . -name '*.rs' -exec rustfmt --check {} +"
  ok r2-ok-find-cp-suffix "find . -name '*.tmp' -exec cp {} {}.bak \\;"
  ok r2-ok-grep-include "grep -rn --include=*.rs TODO src/"
  ok r2-ok-env-lang-make "LC_ALL=C make -j4 test"
  ok r2-ok-xargs-n "printf 'a\\nb\\n' | xargs -n 1 echo"
  ok r2-ok-cp-literal "cp a.txt b.txt && wc -l b.txt"
  # input bound is unchanged (cases input-too-long and input-too-many-words in ac11_program_word)
}

CASES="ac01_clean_and_marker ac02_session_link ac03_engine_inputs ac04_crash_is_refusal ac05_heredoc ac06_expansion ac07_body_file ac08_stat_swap ac09_flags ac10_api ac11_program_word ac12_target ac13_read_only ac14_no_echo ac15_testmode ac16_manifest ac18_hygiene ac19_public_repo_env ac20_fields ac20_deadline ac21_deny_list ac22_content_shapes ac23_anchors ac24_pass_through_r2"

run_case() {
  FAILN=0
  echo "== $1"
  "$1"
  case "$W" in "$TR"/w.*) chmod -R u+rw "$W" 2>/dev/null; rm -rf -- "$W" ;; esac
  if [ "$FAILN" -eq 0 ]; then echo "PASS $1"; else echo "FAIL $1 ($FAILN)"; TOTALFAIL=$((TOTALFAIL + 1)); fi
}

if [ "$#" -gt 1 ]; then echo "usage: text-rule-smoke.sh [case-name]" >&2; exit 2; fi
if [ "$#" -eq 1 ]; then
  if [ "$1" = _selftest_leave ]; then run_case _selftest_leave; exit 1; fi
  case " $CASES " in *" $1 "*) ;; *) echo "smoke: unknown case" >&2; exit 2;; esac
  run_case "$1"
  [ "$TOTALFAIL" -eq 0 ]; exit $?
fi
for c in $CASES; do run_case "$c"; done
[ "$TOTALFAIL" -eq 0 ]
