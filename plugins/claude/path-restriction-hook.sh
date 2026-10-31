#!/usr/bin/env bash
# PreToolUse hook: refuse an edit outside the ticket's repo and declared paths (V5).
# Reads the tool call as JSON on stdin. Inputs by environment: HARNESS_BIN, HARNESS_TICKET_FILE,
# HARNESS_REPO_MAP (exactly one <repo_id>=<absolute dir> entry), HARNESS_SCOPE_FILE.
# Exit 0 = allow (stdout and stderr empty). Exit 2 = refuse, one reason line on stderr, nothing on stdout.
# Any failure of the hook itself is converted to exit 2 (only exit 2 blocks in Claude Code).
# The `# G:<name>` tags mark each guard; the mutation proof disables one tag at a time.
set -u

here="${BASH_SOURCE[0]:-$0}"

refuse() {
  printf '%s\n' "$1" >&2
  exit 2
}

# G:py_missing
if ! command -v python3 >/dev/null 2>&1; then refuse "refused: python3 is missing"; fi

PROG=''
read -r -d '' PROG <<'PYEOF'
import json, os, re, subprocess, sys

TOOLS = {"Edit": "file_path", "Write": "file_path", "MultiEdit": "file_path", "NotebookEdit": "notebook_path"}
STDIN_CAP = 1024 * 1024
SCOPE_BYTES = 4096
SCOPE_LINES = 128
PROTECTED = (".git", ".claude")


def refuse(msg):
    sys.stderr.write(msg + "\n")
    sys.exit(2)


def has_control(s):
    return any(ord(c) < 32 or ord(c) == 127 for c in s)


def inside(root, p):
    # strictly below root, component-wise (root + "/" prefix)
    return p.startswith(root + "/")  # root+"/" prefix is the component-wise test


def env_abs(name):
    v = os.environ.get(name, "")
    if v == "" or has_control(v) or not v.startswith("/"):  # G:env_valid
        refuse("refused: " + name + " must be set to an absolute path")
    return v


bin_path = env_abs("HARNESS_BIN")
ticket_path = env_abs("HARNESS_TICKET_FILE")
scope_path = env_abs("HARNESS_SCOPE_FILE")
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
if inside(root, os.path.realpath(scope_path)):  # G:spoof_scope
    refuse("refused: HARNESS_SCOPE_FILE is inside the repo")
if inside(root, os.path.realpath(ticket_path)):  # G:spoof_ticket
    refuse("refused: HARNESS_TICKET_FILE is inside the repo")
if inside(root, os.path.realpath(bin_path)):  # G:spoof_bin
    refuse("refused: HARNESS_BIN is inside the repo")
if inside(root, os.path.realpath(sys.argv[1])):  # G:spoof_hook
    refuse("refused: the hook script is inside the repo")

# Intake: which repo is this ticket about?
try:
    cp = subprocess.run(
        [bin_path, "ticket", "intake", ticket_path, "--repo-map", repo_map],
        stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=20,
    )
except (OSError, subprocess.SubprocessError):
    refuse("refused: the intake command could not run")
if cp.returncode != 0:  # G:intake_exit
    line = cp.stderr.decode("utf-8", "replace").strip().split("\n")[0][:200] if cp.stderr else ""
    if line.startswith("harness: ") and not has_control(line) and root not in line and map_dir not in line:
        refuse("refused: intake: " + line)
    refuse("refused: intake did not accept the ticket")
mi = re.match(rb"^ticket=(\S+) repo_id=([a-z0-9._-]+) head=([0-9a-f]{40}|[0-9a-f]{64})\n\Z", cp.stdout)
if mi is None:  # G:intake_parse
    refuse("refused: intake output is malformed")
ticket_id = mi.group(1).decode("utf-8", "replace")
if mi.group(2).decode() != map_id:  # G:repo_id_cmp
    refuse("refused: intake repo id differs from the repo map")

# Scope file.
try:
    if not os.path.isfile(scope_path):
        refuse("refused: the scope file is missing")
    with open(scope_path, "rb") as f:
        raw = f.read(SCOPE_BYTES + 1)
except OSError:
    refuse("refused: the scope file cannot be read")
if len(raw) > SCOPE_BYTES:  # G:scope_bytes
    refuse("refused: the scope file is too large")
try:
    text = raw.decode("utf-8")
except UnicodeDecodeError:
    refuse("refused: the scope file is not valid UTF-8")
lines = text.split("\n")
if lines and lines[-1] == "":
    lines.pop()
if len(lines) > SCOPE_LINES:  # G:scope_lines
    refuse("refused: the scope file has too many lines")
if not lines or lines[0] != "ticket=" + ticket_id:  # G:ticket_cmp
    refuse("refused: the scope file is not for this ticket")
entries = []
for ln in lines[1:]:
    if ln.strip() == "" or ln.startswith("#"):
        continue
    if has_control(ln):  # G:scope_control
        refuse("refused: the scope file has a bad entry")
    if "\\" in ln or "*" in ln or "?" in ln or "[" in ln or "]" in ln:  # G:scope_glob
        refuse("refused: the scope file has a bad entry")
    folder = ln.endswith("/")
    comps = (ln[:-1] if folder else ln).split("/")
    for c in comps:
        if c == "" or c == "." or c == "..":  # G:scope_entry_comp
            refuse("refused: the scope file has a bad entry")
        if c.lower() in PROTECTED:  # G:scope_entry_protected
            refuse("refused: the scope file has a bad entry")
    entries.append((tuple(comps), folder))

# The tool call.
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
if not isinstance(tool, str) or tool not in TOOLS:  # G:tool_allowlist
    refuse("refused: the tool is not a known edit tool")
tin = call.get("tool_input")
if not isinstance(tin, dict):
    refuse("refused: the tool input is missing")
target = tin.get(TOOLS.get(tool, "file_path"))  # G:path_key
if not isinstance(target, str) or target == "":
    refuse("refused: the edit path is missing")
if has_control(target):  # G:path_control
    refuse("refused: the edit path has a control character")
if not target.startswith("/"):  # G:relative
    refuse("refused: the edit path is not absolute")
if "\\" in target:  # G:backslash
    refuse("refused: the edit path has a backslash")
if "//" in target:  # G:sep_double
    refuse("refused: the edit path is not in canonical form")
if target.endswith("/"):  # G:sep_trailing
    refuse("refused: the edit path names a folder")
parts = target.split("/")[1:]
if "." in parts:  # G:sep_dot
    refuse("refused: the edit path is not in canonical form")
if ".." in parts:  # G:dotdot
    refuse("refused: the edit path has a parent component")

# Resolve once: parent by realpath, then the leaf (a symlink or dangling link is followed).
resolved = os.path.join(os.path.realpath(os.path.dirname(target)), os.path.basename(target))
if os.path.islink(resolved):  # G:leaf_resolve
    resolved = os.path.realpath(resolved)

if not inside(root, resolved):  # G:contain
    refuse("refused: the edit is outside the repo")
rel = resolved[len(root) + 1:].split("/")
low = [c.lower() for c in rel]
if ".git" in low:  # G:git_comp
    refuse("refused: the edit touches a protected .git path")
if ".claude" in low:  # G:claude_comp
    refuse("refused: the edit touches a protected .claude path")

ok = False
for comps, folder in entries:
    n = len(comps)
    if folder:
        if tuple(rel[:n]) == comps and len(rel) > n:  # G:scope_folder
            ok = True
    else:
        if tuple(rel) == comps:  # G:scope_exact
            ok = True
if not ok:  # G:scope_match
    refuse("refused: the edit is outside the declared scope")
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
refuse "refused: internal error in the path hook"
