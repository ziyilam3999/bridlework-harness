#!/usr/bin/env bash
# closed-feed-selftest.sh (ticket 3601, AC-12): hermetic test of closed-feed.py against a mktemp fake tasks
# root and a stub reconcile module. Touches nothing outside its scratch folder; every id and session is synthetic.
set -u
HERE="$(cd "$(dirname "$0")" && pwd)"
S="$(mktemp -d)" || exit 97
[ -n "$S" ] || exit 97
trap 'rm -rf "$S"' EXIT
fail=0
ok() { echo "PASS $1"; }
bad() { echo "FAIL $1"; fail=1; }

# stub reconcile: a closed_stamp with reconcile's own shape rules, and a --json mode with an empty report
cat > "$S/reconcile.py" <<'PY'
import json, re, sys
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
if __name__ == "__main__":
    if "--json" in sys.argv:
        print(json.dumps({"in_progress": 0, "closed_with_evidence": [], "close_candidates": []}))
PY
SESS="s-selftest"
mkdir -p "$S/tasks/$SESS"
rec() { # id status closed-json
  printf '{"id":"%s","status":"%s","metadata":{"closed":%s}}\n' "$1" "$2" "$3" > "$S/tasks/$SESS/$1.json"
}
rec 7001 completed '{"repo":"owner/repo","pr":"88","head":"abc","at":"2026-11-10T10:00:00Z","ledger_rows":3}'   # (i) valid stamp
rec 7002 completed '{"repo":"owner/repo","pr":"abc","at":"2026-11-10T10:00:00Z"}'                              # (ii) bad pr
rec 7003 completed '{"repo":"owner/repo","pr":"90","at":"2026-11-10T10:00:00Z"}'                               # planted, unbound
state_row() { printf '{"group":"%s","status":"proposed","ticket_file":"2026-w45-1.json","proposed_week":"2026-w45","task_id":"%s","task_session":"%s","resolved_pr":"","resolved_date":"","attempts":"0"}\n' "$1" "$2" "$3"; }
{ state_row g-one 7001 "$SESS"; state_row g-two 7002 "$SESS"; state_row g-gone 7999 "$SESS"; } > "$S/state.jsonl"

feed() { python3 -I "$HERE/closed-feed.py" --reconcile "$S/reconcile.py" --tasks-root "$S/tasks" --state "$S/state.jsonl" --session "$SESS" 2>"$S/err"; }
feed > "$S/feed.json"; rc=$?
if [ "$rc" -eq 0 ]; then ok "feed exits 0"; else bad "feed exit $rc: $(cat "$S/err")"; fi
chk() { python3 -I -c "import json,sys;d=json.load(open(sys.argv[1]))['closed_with_evidence'];$1" "$S/feed.json"; }
if chk "r=[x for x in d if x['id']=='7001'];assert len(r)==1 and r[0]['session']=='$SESS' and r[0]['repo']=='owner/repo' and r[0]['pr']=='88' and r[0]['at']=='2026-11-10'"; then ok "(i) valid completed record is in the feed with session set"; else bad "(i) missing"; fi
if chk "assert not [x for x in d if x['id']=='7002']"; then ok "(ii) pr abc rejected by closed_stamp is omitted"; else bad "(ii) leaked"; fi
if chk "assert not [x for x in d if x['id']=='7003']"; then ok "unbound record carrying a valid stamp is absent (only bound ids are opened)"; else bad "unbound leaked"; fi
if chk "assert len(d)==1"; then ok "feed holds exactly the one valid bound row"; else bad "row count"; fi

# arm (a): a reconcile bucket row is tagged with the session and merged without duplicating arm (b)
cat > "$S/reconcile2.py" <<'PY'
import json, sys
if __name__ == "__main__" and "--json" in sys.argv:
    print(json.dumps({"closed_with_evidence": [{"id": "7001", "repo": "owner/repo", "pr": "88", "at": "2026-11-10", "subject": "s"}, {"id": "7500", "repo": "owner/repo", "pr": "91", "at": "2026-11-09", "subject": "t"}, {"id": "bad", "repo": "r", "pr": "1", "at": "2026-11-09"}]}))
def closed_stamp(t):
    return (t.get("metadata") or {}).get("closed")
PY
python3 -I "$HERE/closed-feed.py" --reconcile "$S/reconcile2.py" --tasks-root "$S/tasks" --state "$S/state.jsonl" --session "$SESS" > "$S/feed2.json" 2>"$S/err"
if python3 -I -c "
import json,sys
d=json.load(open(sys.argv[1]))['closed_with_evidence']
ids=sorted(x['id'] for x in d)
assert ids==['7001','7500'], ids
assert all(x['session']==sys.argv[2] for x in d)
" "$S/feed2.json" "$SESS"; then ok "arm (a) rows are tagged with the session, merged once, malformed row dropped"; else bad "arm (a) merge"; fi

# the stamp deleted: the group stays out of the feed
printf '{"id":"7001","status":"completed","metadata":{}}\n' > "$S/tasks/$SESS/7001.json"
feed > "$S/feed3.json"
if python3 -I -c "import json,sys;assert json.load(open(sys.argv[1]))['closed_with_evidence']==[]" "$S/feed3.json"; then ok "a record without metadata.closed is not in the feed"; else bad "stampless record leaked"; fi

# a path-shaped session in the state cannot reach outside the tasks root
mkdir -p "$S/outside"
printf '{"id":"7001","metadata":{"closed":{"repo":"o/r","pr":"1","at":"2026-11-10"}}}\n' > "$S/outside/7001.json"
printf '{"group":"g-x","status":"proposed","ticket_file":"","proposed_week":"2026-w45","task_id":"7001","task_session":"../outside","resolved_pr":"","resolved_date":"","attempts":"0"}\n' > "$S/state.jsonl"
feed > "$S/feed4.json"
if python3 -I -c "import json,sys;assert json.load(open(sys.argv[1]))['closed_with_evidence']==[]" "$S/feed4.json"; then ok "a ../ session never leaves the tasks root"; else bad "path escape"; fi

# a failing reconcile fails the feed closed
printf 'import sys\nsys.exit(3)\n' > "$S/reconcile3.py"
python3 -I "$HERE/closed-feed.py" --reconcile "$S/reconcile3.py" --tasks-root "$S/tasks" --state "$S/state.jsonl" --session "$SESS" > "$S/out5" 2>/dev/null; rc=$?
if [ "$rc" -eq 1 ] && [ ! -s "$S/out5" ]; then ok "a failing reconcile exits 1 with empty stdout"; else bad "reconcile failure rc=$rc"; fi

if [ "$fail" -eq 0 ]; then echo "closed-feed-selftest: PASS"; else echo "closed-feed-selftest: FAIL"; fi
exit "$fail"
