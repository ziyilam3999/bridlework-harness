#!/bin/sh
# MCP server smoke. Usage: ci/mcp-smoke.sh
# Builds core/rust harness and mcp/server, runs the server's unit tests and clippy, then spawns the BUILT server
# over real pipes and checks it like an MCP client would. Prints "ok <id>" / "FAIL <id>" per check, then
# "N ok, M failed". Exit 0 only if every check holds, 1 on a failed check, 2 on a build or setup failure.
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd) || exit 2
root=$(CDPATH= cd -- "$here/.." && pwd) || exit 2
exec python3 -I - "$root" "$@" <<'PY'
import json, os, subprocess, sys, tempfile

ROOT = sys.argv[1]
TIMEOUT = 30
ENV = {k: v for k, v in os.environ.items() if not k.startswith("HARNESS_")}
SRV = os.path.join(ROOT, "mcp/server/target/debug/harness-mcp")
REAL = os.path.join(ROOT, "core/rust/target/debug/harness")


def build(cwd, *cmd):
    p = subprocess.run(cmd, cwd=os.path.join(ROOT, cwd), env=ENV)
    if p.returncode != 0:
        print("FAIL build: " + " ".join(cmd) + " in " + cwd, file=sys.stderr)
        sys.exit(2)


build("core/rust", "cargo", "build", "--locked")
build("mcp/server", "cargo", "build", "--locked")
build("mcp/server", "cargo", "test", "--locked")
build("mcp/server", "cargo", "clippy", "--locked", "--all-targets", "--", "-D", "warnings")

with open(os.path.join(ROOT, "mcp/tools.json"), encoding="utf-8") as fh:
    ROWS = json.load(fh)["tools"]
NAMES = [r["name"] for r in ROWS]

TMP = tempfile.mkdtemp()
STUB = os.path.join(TMP, "stub")
MARK = os.path.join(TMP, "spawned")
with open(STUB, "w", encoding="utf-8") as fh:
    fh.write('#!/bin/sh\n: > "%s"\necho "harness stub"\n' % MARK)
os.chmod(STUB, 0o755)


def req(i, method, params=None):
    return json.dumps({"jsonrpc": "2.0", "id": i, "method": method, "params": params if params is not None else {}})


def call(i, name, args):
    return req(i, "tools/call", {"name": name, "arguments": args})


class Session:
    def __init__(self, harness):
        self.p = subprocess.Popen([SRV, "--harness", harness], stdin=subprocess.PIPE, stdout=subprocess.PIPE, env=ENV)

    def send(self, line):
        self.p.stdin.write((line + "\n").encode())
        self.p.stdin.flush()

    def read(self):
        # One line with a hard timeout; a timeout kills the server and raises.
        import threading
        box = []
        t = threading.Thread(target=lambda: box.append(self.p.stdout.readline()), daemon=True)
        t.start()
        t.join(TIMEOUT)
        if t.is_alive() or not box or not box[0]:
            self.p.kill()
            raise RuntimeError("no answer")
        return json.loads(box[0].decode())

    def close(self):
        try:
            self.p.stdin.close()
        except OSError:
            pass
        try:
            return self.p.wait(TIMEOUT)
        except subprocess.TimeoutExpired:
            self.p.kill()
            return None


def one(harness, lines, n=1):
    """Send all lines to a fresh server, read n answers, close stdin; return (answers, exit code)."""
    s = Session(harness)
    try:
        for ln in lines:
            s.send(ln)
        got = [s.read() for _ in range(n)]
    finally:
        rc = s.close()
    return got, rc


def refused(argv):
    p = subprocess.run([SRV] + argv, stdin=subprocess.DEVNULL, capture_output=True, timeout=TIMEOUT, env=ENV)
    return p.returncode == 2 and p.stdout == b""


def clear_mark():
    if os.path.exists(MARK):
        os.remove(MARK)


results = []


def check(cid, fn):
    try:
        good = bool(fn())
    except Exception as exc:  # any error is a failed check
        print("detail %s: %r" % (cid, exc), file=sys.stderr)
        good = False
    results.append(good)
    print(("ok " if good else "FAIL ") + cid, flush=True)


def c_initialize():
    (a,), _ = one(REAL, [req(1, "initialize", {"protocolVersion": "2025-06-18"})])
    r = a["result"]
    return (r["protocolVersion"] == "2025-06-18" and r["serverInfo"]["name"] == "harness-mcp"
            and isinstance(r["capabilities"]["tools"], dict))


def c_silent():
    note = json.dumps({"jsonrpc": "2.0", "method": "notifications/initialized"})
    (a,), _ = one(REAL, [note, req(2, "ping")])
    return a.get("id") == 2 and a.get("result") == {}


LISTED = []


def c_list_names():
    (a,), _ = one(REAL, [req(3, "tools/list")])
    LISTED[:] = a["result"]["tools"]
    return a["id"] == 3 and [t["name"] for t in LISTED] == NAMES


def c_list_shape():
    if len(LISTED) != len(ROWS):
        return False
    for t, row in zip(LISTED, ROWS):
        if t.get("annotations", {}).get("readOnlyHint") is not True:
            return False
        if t["inputSchema"] != row["inputSchema"] or t["outputSchema"] != row["outputSchema"]:
            return False
        if any(k in t for k in ("argv", "probe", "read_only")):
            return False
    return True


def c_version():
    d = subprocess.run([REAL, "--version"], capture_output=True, text=True, env=ENV)
    (a,), _ = one(REAL, [call(4, "harness_version", {})])
    r = a["result"]
    sc = r["structuredContent"]
    return (sc == {"exit": d.returncode, "stdout": d.stdout, "stderr": d.stderr}
            and sc["stdout"].startswith("harness ") and r["isError"] is False
            and json.loads(r["content"][0]["text"]) == sc)


def c_malformed():
    a, _ = one(REAL, ["{not json", req(5, "tools/list")], n=2)
    return (a[0]["error"]["code"] == -32700 and a[0]["id"] is None and a[1]["id"] == 5
            and [t["name"] for t in a[1]["result"]["tools"]] == NAMES)


def c_unknown_method():
    (a,), _ = one(REAL, [req(6, "zz/nope")])
    return a["error"]["code"] == -32601


def no_spawn(line):
    clear_mark()
    (a,), _ = one(STUB, [line])
    return a["error"]["code"] == -32602 and not os.path.exists(MARK)


def c_stub_spawn():
    clear_mark()
    (a,), _ = one(STUB, [call(10, "harness_version", {})])
    return os.path.exists(MARK) and a["result"]["structuredContent"]["stdout"] == "harness stub\n"


def c_eof():
    s = Session(REAL)
    return s.close() == 0


check("initialize", c_initialize)
check("notification-silent", c_silent)
check("list-names", c_list_names)
check("list-shape", c_list_shape)
check("version-equals-cli", c_version)
check("malformed-then-alive", c_malformed)
check("unknown-method", c_unknown_method)
check("unknown-tool-no-spawn", lambda: no_spawn(call(7, "zz_no_such_tool", {})))
check("unwired-tool-no-spawn", lambda: no_spawn(call(8, "harness_status", {})))
check("bad-arguments-no-spawn", lambda: no_spawn(call(9, "harness_version", {"x": "y"})))
check("stub-spawn-seen", c_stub_spawn)
check("eof-exits-zero", c_eof)
check("relative-harness-refused", lambda: refused(["--harness", "harness"]))
check("missing-harness-refused", lambda: refused(["--harness", "/nonexistent-mcp/harness"]))
check("no-args-refused", lambda: refused([]))

bad = results.count(False)
print("%d ok, %d failed" % (results.count(True), bad))
sys.exit(0 if bad == 0 else 1)
PY
