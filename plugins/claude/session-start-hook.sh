#!/usr/bin/env bash
# SessionStart hook: record the interactive session's id from the hook payload as one `session_started` event (P1B-21).
# Reads the hook payload as JSON on stdin (only hook_event_name and session_id are used; transcript_path and cwd are ignored).
# Inputs by environment: HARNESS_BIN (absolute engine path), HARNESS_EVENT_LOG (absolute event log path).
# The hook calls `harness session record-start`; it does not re-implement the id shape rule (the engine owns it).
# Exit 0 = recorded (stdout and stderr empty: SessionStart stdout reaches the model, so it is ALWAYS empty).
# Exit 2 = refused, one reason line on stderr, nothing written. Any failure of the hook itself is converted to exit 2.
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
import json, os, subprocess, sys

ENGINE_TIMEOUT = 20
STDIN_CAP = 1024 * 1024
ID_CAP = 256


def refuse(msg):
    sys.stderr.write(msg + "\n")
    sys.exit(2)


def has_control(s):
    return any(ord(c) < 32 or ord(c) == 127 for c in s)


# ---- 1. The payload: only hook_event_name and session_id are read.
data = sys.stdin.buffer.read(STDIN_CAP + 1)
if len(data) > STDIN_CAP:  # G:stdin_cap
    refuse("refused: the hook payload is too large")
try:
    payload = json.loads(data.decode("utf-8"))
except (ValueError, UnicodeDecodeError):  # G:stdin_json
    refuse("refused: the hook payload is not valid JSON")
if not isinstance(payload, dict):
    refuse("refused: the hook payload is not an object")
if payload.get("hook_event_name") != "SessionStart":  # G:event_allowlist
    refuse("refused: the hook event is not SessionStart")
sid = payload.get("session_id")
if not isinstance(sid, str) or sid == "" or len(sid) > ID_CAP or has_control(sid):  # G:id_present
    refuse("refused: the hook payload has no session id")


# ---- 2. Environment.
def env_abs(name):
    v = os.environ.get(name, "")
    if v == "" or has_control(v) or not v.startswith("/"):  # G:env_valid
        refuse("refused: " + name + " must be set to an absolute path")
    return v


bin_path = env_abs("HARNESS_BIN")
log_path = env_abs("HARNESS_EVENT_LOG")

# ---- 3. The engine call: the engine validates the id and writes the one event; it prints nothing on success.
try:
    cp = subprocess.run(
        [bin_path, "session", "record-start", "--session-id", sid, "--log", log_path],
        stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, cwd="/",
        timeout=ENGINE_TIMEOUT,  # G:timeout
    )
except (OSError, subprocess.SubprocessError):
    refuse("refused: the record command could not run in time")
if cp.returncode != 0:  # G:engine_exit
    line = cp.stderr.decode("utf-8", "replace").strip().split("\n")[0][:200] if cp.stderr else ""
    shown = not any(d in line for d in (bin_path, log_path))
    if line.startswith("harness: ") and not has_control(line) and shown:  # G:reason_filter
        refuse("refused: record: " + line)
    refuse("refused: the record command did not accept the session id")
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
refuse "refused: internal error in the session-start hook" # G:catch_all
