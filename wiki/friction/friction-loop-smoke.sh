#!/usr/bin/env bash
# friction-loop-smoke.sh (ticket 3601, AC-7): hermetic end-to-end run of wiki/friction/friction-loop.sh over
# scratch files: propose -> bind -> resolve (in-progress bucket and completed record) -> regress -> escalate.
# Everything lives under one mktemp -d: inbox, state, hand-off folder, a git work tree standing in for the private
# wiki, a stub reconcile script and a fake tasks root. Task ids (4242..4245) are numerically distinct from every
# hand-off id (friction-<week>-<n>). The harness binary is HARNESS_BIN, else the debug build in this checkout.
set -u
HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
HARNESS_BIN="${HARNESS_BIN:-$ROOT/core/rust/target/debug/harness}"
if [ ! -x "$HARNESS_BIN" ]; then echo "friction-loop-smoke: build the harness first (cargo build -p harness)" >&2; exit 2; fi
S="$(mktemp -d)" || exit 97
[ -n "$S" ] || exit 97
trap 'rm -rf "$S"' EXIT
fail=0
pass() { echo "PASS $1"; }
bad() { echo "FAIL $1"; fail=1; }

W="$S/wiki"; mkdir -p "$W/raw" "$S/handoff" "$S/tasks/s-smoke"
git init -q "$W"
git -C "$W" config user.name smoke
git -C "$W" config user.email "smoke$(printf '\100')example.invalid"
printf '# Index\n\n' > "$W/index.md"; printf '# Log\n' > "$W/log.md"
sed -e "s#/placeholder/friction-inbox.jsonl#$W/raw/friction-inbox.jsonl#" -e "s#  triage_top: 3#  triage_top: 2#" "$ROOT/config/harness.example.yaml" > "$S/config.yaml"
export HARNESS_PRIVACY_LIST_SECRET="zzdenyzz"
unset HARNESS_CONFIG HARNESS_PRIVACY_LIST
STATE="$W/raw/triage-state.jsonl"

# the stub reconcile: closed_stamp with reconcile's own shape rules, and a --json report read from a fixed file
cat > "$S/reconcile.py" <<'PY'
import json, os, re, sys
def closed_stamp(t):
    md = t.get("metadata")
    if not isinstance(md, dict): return None
    c = md.get("closed")
    if not isinstance(c, dict): return None
    repo, pr = c.get("repo"), c.get("pr")
    if not (isinstance(repo, str) and repo.strip()): return None
    if isinstance(pr, bool): return None
    if isinstance(pr, int):
        if pr < 0: return None
    elif not (isinstance(pr, str) and re.fullmatch(r"[0-9]+", pr)): return None
    return c
if __name__ == "__main__" and "--json" in sys.argv:
    print(open(os.path.join(os.path.dirname(os.path.abspath(__file__)), "report.json")).read())
PY
report() { printf '{"in_progress":0,"closed_with_evidence":[%s],"close_candidates":[]}\n' "$1" > "$S/report.json"; }
report ""

cat > "$S/loop.env" <<EOF
HARNESS_BIN="$HARNESS_BIN"
HARNESS_CONFIG="$S/config.yaml"
WIKI_DIR="$W"
HANDOFF_DIR="$S/handoff"
RECONCILE_PY="$S/reconcile.py"
TASKS_ROOT="$S/tasks"
SESSION_ID="s-smoke"
EOF
loop() { bash "$HERE/friction-loop.sh" --week "$1" --env "$S/loop.env" > "$S/out.txt" 2> "$S/err.txt"; }
h() { "$HARNESS_BIN" "$@"; }
addrow() { # id date breaks impact
  local a=(friction add --id "$1" --date "$2" --ticket t-smoke --where-it-lives memory --text "synthetic lesson $1" --config "$S/config.yaml")
  if [ -n "$3" ]; then a+=(--breaks "$3"); fi
  if [ -n "$4" ]; then a+=(--impact "$4"); fi
  h "${a[@]}" > /dev/null
}
status() { python3 -I -c "
import json,sys
last={}
for l in open(sys.argv[1]):
    r=json.loads(l); last[r['group']]=r
print(last.get(sys.argv[2],{}).get('status','none'))" "$STATE" "$1"; }
field() { python3 -I -c "
import json,sys
last={}
for l in open(sys.argv[1]):
    r=json.loads(l); last[r['group']]=r
print(last[sys.argv[2]][sys.argv[3]])" "$STATE" "$1" "$2"; }
record() { # id with-stamp(yes|no) pr at
  if [ "$2" = yes ]; then
    printf '{"id":"%s","status":"completed","metadata":{"closed":{"repo":"owner/repo","pr":"%s","head":"abc","at":"%s","ledger_rows":3}}}\n' "$1" "$3" "$4" > "$S/tasks/s-smoke/$1.json"
  else
    printf '{"id":"%s","status":"completed","metadata":{}}\n' "$1" > "$S/tasks/s-smoke/$1.json"
  fi
}

addrow g-a 2026-11-02 "" blocker
addrow g-b 2026-11-02 "" high

# ---- propose
loop 2026-w45
if [ "$(status g-a)" = proposed ] && [ "$(status g-b)" = proposed ] && [ -f "$S/handoff/2026-w45-1.json" ] && [ -f "$S/handoff/2026-w45-2.json" ] && grep -q "propose group=g-a file=2026-w45-1.json reason=rank" "$S/out.txt" && [ "$(git -C "$W" rev-list --count HEAD 2>/dev/null)" = 1 ]; then pass propose; else bad "propose: $(cat "$S/err.txt")"; fi

# ---- bind (task ids are numeric and never equal a hand-off id)
h friction bind --state "$STATE" --group g-a --task-id 4242 --task-session s-smoke > /dev/null
h friction bind --state "$STATE" --group g-b --task-id 4243 --task-session s-smoke > /dev/null
if [ "$(field g-a task_id)" = 4242 ] && [ "$(field g-b task_id)" = 4243 ] && [ "$(field g-a ticket_file)" = 2026-w45-1.json ]; then pass bind; else bad bind; fi

# ---- resolve-inprogress (4242 is in the stub reconcile bucket) and resolve-completed (4243 only as a completed record)
report '{"id":"4242","repo":"owner/repo","pr":"101","at":"2026-11-10","subject":"s"}'
record 4243 yes 102 2026-11-10T08:00:00Z
loop 2026-w46
if [ "$(status g-a)" = resolved ] && [ "$(field g-a resolved_pr)" = "owner/repo#101" ]; then pass resolve-inprogress; else bad "resolve-inprogress: $(cat "$S/err.txt")"; fi
if [ "$(status g-b)" = resolved ] && [ "$(field g-b resolved_pr)" = "owner/repo#102" ] && ! grep -q '"id":"4243"' "$S/report.json"; then pass resolve-completed; else bad "resolve-completed: $(cat "$S/err.txt")"; fi

# ---- regress: a new row after the resolved date re-proposes g-a first and clears its binding
addrow g-a2 2026-11-12 g-a ""
report ""
loop 2026-w47
if [ "$(status g-a)" = proposed ] && [ "$(field g-a attempts)" = 1 ] && [ -z "$(field g-a task_id)" ] && grep -q "propose group=g-a file=2026-w47-1.json reason=regressed" "$S/out.txt"; then pass regress; else bad "regress: $(cat "$S/out.txt") $(cat "$S/err.txt")"; fi

# ---- escalate: resolve again, regress again -> escalated, no new hand-off file
h friction bind --state "$STATE" --group g-a --task-id 4244 --task-session s-smoke > /dev/null
report '{"id":"4244","repo":"owner/repo","pr":"103","at":"2026-11-13","subject":"s"}'
loop 2026-w48
resolved_again="$(status g-a)"
addrow g-a3 2026-11-20 g-a ""
report ""
files_before="$(ls "$S/handoff" | wc -l | tr -d ' ')"
loop 2026-w49
files_after="$(ls "$S/handoff" | wc -l | tr -d ' ')"
if [ "$resolved_again" = resolved ] && [ "$(status g-a)" = escalated ] && [ "$files_before" = "$files_after" ] && grep -q "escalate group=g-a attempts=2" "$S/out.txt"; then pass escalate; else bad "escalate: $resolved_again $(cat "$S/out.txt") $(cat "$S/err.txt")"; fi

# ---- the persistent arm's last check: a record whose metadata.closed was deleted never resolves
addrow g-c 2026-11-21 "" blocker
loop 2026-w50
h friction bind --state "$STATE" --group g-c --task-id 4245 --task-session s-smoke > /dev/null
record 4245 no 104 2026-11-22T08:00:00Z
loop 2026-w51
stayed="$(status g-c)"
record 4245 yes 104 2026-11-22T08:00:00Z
loop 2026-w52
if [ "$stayed" = proposed ] && [ "$(status g-c)" = resolved ]; then pass "stamp-deleted-stays-proposed"; else bad "stamp-deleted: $stayed $(status g-c) $(cat "$S/err.txt")"; fi

# ---- the private wiki got one commit per run that changed something, and no row text left the page
if [ "$(git -C "$W" rev-list --count HEAD)" -ge 8 ] && grep -q "synthetic lesson" "$W/triage-2026-w50.md"; then pass "wiki-commits-and-page"; else bad "wiki-commits-and-page"; fi

if [ "$fail" -eq 0 ]; then echo "friction-loop-smoke: PASS"; else echo "friction-loop-smoke: FAIL"; fi
exit "$fail"
