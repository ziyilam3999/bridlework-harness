#!/usr/bin/env python3
"""closed-feed.py (ticket 3601, A3c): build the `--closed` file for `harness friction triage`.

The feed is the UNION of two arms, printed as one JSON object {"closed_with_evidence": [...]}:
  (a) `python3 <reconcile.py> --json --session <sid>` -> its `closed_with_evidence` rows, each tagged
      `session: <sid>` (a task still in_progress that carries a metadata.closed stamp);
  (b) for each state row that is `proposed` and bound, the ONE record
      <tasks-root>/<task_session>/<task_id>.json, whatever its `status`, taken iff reconcile's OWN
      `closed_stamp` (imported by path, never re-implemented) accepts its metadata.
Arm (b) is a lookup by id, not a scanner: it opens only the records the state names. Every path comes from the
arguments (the private runner reads them from private config). A completed task that dropped its metadata finds
nothing in arm (b): the group stays `proposed` and its age grows, visibly; it never produces a false resolve.

Usage: closed-feed.py --reconcile <path> --tasks-root <dir> --state <file> --session <sid>
Exit: 0 the feed on stdout; 1 reconcile or the state failed (nothing on stdout); 2 usage.
Stdlib only.
"""
import importlib.util
import json
import os
import re
import subprocess
import sys

USAGE = "usage: closed-feed.py --reconcile <path> --tasks-root <dir> --state <file> --session <sid>"
TOKEN = re.compile(r"^[a-z0-9][a-z0-9._-]*$")
DIGITS = re.compile(r"^[0-9]{1,15}$")
DATE = re.compile(r"^[0-9]{4}-[0-9]{2}-[0-9]{2}$")
REPO = re.compile(r"^[A-Za-z0-9._/-]+$")
STATE_CAP = 4 * 1024 * 1024
RECORD_CAP = 1024 * 1024


def usage(msg):
    print(USAGE, file=sys.stderr)
    print(msg, file=sys.stderr)
    return 2


def load_reconcile(path):
    spec = importlib.util.spec_from_file_location("reconcile_by_path", path)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def clean(row, session):
    """One well-formed feed row, or None (a malformed row is dropped here so it can never refuse a whole run)."""
    try:
        rid, repo, pr, at = str(row["id"]), str(row["repo"]), str(row["pr"]), str(row.get("at") or "")[:10]
    except (KeyError, TypeError, AttributeError):
        return None
    if not (DIGITS.match(rid) and REPO.match(repo) and DIGITS.match(pr) and DATE.match(at)):
        return None
    return {"id": rid, "repo": repo, "pr": pr, "at": at, "session": session}


def read_state(path):
    """Last row per group wins; a missing state is an empty one. Raises ValueError on a bad or oversized file."""
    try:
        if os.path.getsize(path) > STATE_CAP:
            raise ValueError("state over the cap")
    except FileNotFoundError:
        return {}
    last = {}
    with open(path, encoding="utf-8") as f:
        for n, line in enumerate(f, 1):
            line = line.rstrip("\n")
            if not line:
                continue
            row = json.loads(line)
            if not isinstance(row, dict) or "group" not in row:
                raise ValueError("bad state row %d" % n)
            last[row["group"]] = row
    return last


def arm_a(reconcile_path, session):
    proc = subprocess.run([sys.executable, reconcile_path, "--json", "--session", session],
                          capture_output=True, text=True, timeout=300)
    if proc.returncode != 0:
        raise ValueError("reconcile failed (exit %d)" % proc.returncode)
    rep = json.loads(proc.stdout)
    rows = rep.get("closed_with_evidence", []) if isinstance(rep, dict) else []
    return [r for r in (clean(x, session) for x in rows) if r]


def arm_b(mod, tasks_root, state, taken):
    out = []
    root = os.path.realpath(tasks_root)
    for group in sorted(state):
        s = state[group]
        if s.get("status") != "proposed":
            continue
        tid, sess = str(s.get("task_id") or ""), str(s.get("task_session") or "")
        if not tid or not sess or not DIGITS.match(tid) or not TOKEN.match(sess):
            continue
        if (tid, sess) in taken:
            continue
        rec = os.path.realpath(os.path.join(root, sess, tid + ".json"))
        if os.path.dirname(os.path.dirname(rec)) != root:  # stays inside the tasks root
            continue
        try:
            if os.path.getsize(rec) > RECORD_CAP:
                continue
            with open(rec, encoding="utf-8") as f:
                t = json.load(f)
        except (OSError, ValueError):
            continue
        if not isinstance(t, dict) or str(t.get("id", tid)) != tid:
            continue
        stamp = mod.closed_stamp(t)
        if not stamp:
            continue
        row = clean({"id": tid, "repo": stamp.get("repo"), "pr": stamp.get("pr"), "at": stamp.get("at")}, sess)
        if row:
            out.append(row)
    return out


def main(argv):
    opts = {}
    names = ("--reconcile", "--tasks-root", "--state", "--session")
    i = 0
    while i < len(argv):
        if argv[i] not in names or i + 1 >= len(argv) or argv[i] in opts:
            return usage("bad or repeated option")
        opts[argv[i]] = argv[i + 1]
        i += 2
    if len(opts) != len(names):
        return usage("all four options are required")
    if not TOKEN.match(opts["--session"]):
        return usage("--session must be a plain token")
    try:
        mod = load_reconcile(opts["--reconcile"])
        rows = arm_a(opts["--reconcile"], opts["--session"])
        taken = {(r["id"], r["session"]) for r in rows}
        rows += arm_b(mod, opts["--tasks-root"], read_state(opts["--state"]), taken)
    except (OSError, ValueError, SystemExit, subprocess.SubprocessError, AttributeError, ImportError) as e:
        print("closed-feed: %s" % e, file=sys.stderr)
        return 1
    print(json.dumps({"closed_with_evidence": rows}, indent=1))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
