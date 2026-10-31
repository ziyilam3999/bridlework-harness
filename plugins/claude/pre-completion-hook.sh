#!/usr/bin/env bash
# PreToolUse hook on TaskUpdate: refuse a completion claim unless the gate result for the current head is PASS (V5).
# Reads the tool call as JSON on stdin (only tool_name and tool_input.status are used). Inputs by environment: HARNESS_BIN,
# HARNESS_TICKET_FILE, HARNESS_REPO_MAP (exactly one <repo_id>=<absolute dir> entry), HARNESS_RESULT_DIR, HARNESS_CAPS_DIR
# (the same folder every `ticket gate` run in the session passes as --caps-dir).
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
caps_path = env_abs("HARNESS_CAPS_DIR")
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
if inside(root, os.path.realpath(caps_path)):  # G:spoof_caps
    refuse("refused: HARNESS_CAPS_DIR is inside the repo")
if inside(root, os.path.realpath(ticket_path)):  # G:spoof_ticket
    refuse("refused: HARNESS_TICKET_FILE is inside the repo")
if inside(root, os.path.realpath(bin_path)):  # G:spoof_bin
    refuse("refused: HARNESS_BIN is inside the repo")
if inside(root, os.path.realpath(sys.argv[1])):  # G:spoof_hook
    refuse("refused: the hook script is inside the repo")

# ---- 3. The engine call: `harness status` prints one fixed line; the hook adds no result parser.
try:
    cp = subprocess.run(
        [bin_path, "status", ticket_path, "--repo-map", repo_map, "--result-dir", result_path, "--caps-dir", caps_path],
        stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE, cwd="/",
        timeout=ENGINE_TIMEOUT,  # G:timeout
    )
except (OSError, subprocess.SubprocessError):
    refuse("refused: the status command could not run in time")
if cp.returncode != 0:  # G:status_exit
    line = cp.stderr.decode("utf-8", "replace").strip().split("\n")[0][:200] if cp.stderr else ""
    shown = not any(d in line for d in (root, map_dir, result_path, caps_path, ticket_path, bin_path))
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
