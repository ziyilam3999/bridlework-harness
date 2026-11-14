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


def write_stub(name, body):
    path = os.path.join(TMP, name)
    with open(path, "w", encoding="utf-8") as fh:
        fh.write("#!/bin/sh\n" + body)
    os.chmod(path, 0o755)
    return path


ARGVSTUB = write_stub("argvstub", 'for a in "$@"; do printf "%s\\n" "$a"; done\n')
ENVSTUB = write_stub("envstub", 'env | sed -n "s/^\\(HARNESS_[A-Za-z_]*\\)=.*/\\1/p" | sort >&2\ncat\n')
BIGAT = write_stub("bigstub-at", 'head -c 262144 /dev/zero | tr "\\0" a\nhead -c 262144 /dev/zero | tr "\\0" b >&2\n')
BIGOVER = write_stub("bigstub-over", 'head -c 300000 /dev/zero | tr "\\0" a\nhead -c 300000 /dev/zero | tr "\\0" b >&2\n')

F = os.path.join(TMP, "fx")
os.mkdir(F)
FREPO, FRES, FCAPS, FMD, FLOG = [os.path.join(F, n) for n in ("repo", "res", "caps", "md", "e.jsonl")]
TK = os.path.join(ROOT, "examples/project-template/tickets/ex-1.json")
LIST = os.path.join(ROOT, "mcp/fixtures/probe-list.txt")


def setup(cmd):
    p = subprocess.run(cmd, env=ENV, capture_output=True)
    if p.returncode != 0:
        print("FAIL setup: " + " ".join(cmd), file=sys.stderr)
        sys.exit(2)


setup(["cp", "-R", os.path.join(ROOT, "examples/project-template/project"), FREPO])
setup(["git", "-C", FREPO, "init", "-q"])
setup(["git", "-C", FREPO, "add", "-A"])
setup(["git", "-C", FREPO, "-c", "user.name=smoke", "-c", "user.email=ci", "commit", "-q", "-m", "base"])
for d in (FRES, FCAPS, FMD):
    os.mkdir(d)
setup([REAL, "events", "append", "--log", FLOG, "--kind", "escalation", "--ticket", "ex-1", "--repo", "example",
       "--reason", "r", "--role", "r", "--model", "m", "--ts-ms", "1000"])
setup([REAL, "manifest", "init", "ex-1", "--root", FREPO, "--dir", FMD, "--ts-ms", "1000"])
RM = "example=" + FREPO


def cli(argv, stdin="", env_extra=None):
    p = subprocess.run([REAL] + argv, input=stdin, capture_output=True, text=True, env=dict(ENV, **(env_extra or {})))
    return {"exit": p.returncode, "stdout": p.stdout, "stderr": p.stderr}


def req(i, method, params=None):
    return json.dumps({"jsonrpc": "2.0", "id": i, "method": method, "params": params if params is not None else {}})


def call(i, name, args):
    return req(i, "tools/call", {"name": name, "arguments": args})


class Session:
    def __init__(self, harness, extra=(), env=None):
        self.p = subprocess.Popen([SRV, "--harness", harness] + list(extra), stdin=subprocess.PIPE, stdout=subprocess.PIPE, env=env or ENV)

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


def one(harness, lines, n=1, extra=(), env=None):
    """Send all lines to a fresh server, read n answers, close stdin; return (answers, exit code)."""
    s = Session(harness, extra, env)
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


def no_spawn(line, extra=(), want=None):
    clear_mark()
    (a,), _ = one(STUB, [line], extra=extra)
    return a["error"]["code"] == -32602 and not os.path.exists(MARK) and (want is None or want in a["error"]["message"])


def tool(harness, name, args, extra=(), env=None):
    (a,), _ = one(harness, [call(20, name, args)], extra=extra, env=env)
    return a["result"]


def equals_cli(name, args, argv, code, stdin="", extra=(), env_extra=None):
    r = tool(REAL, name, args, extra)
    want = cli(argv, stdin, env_extra)
    return r["structuredContent"] == want and want["exit"] == code and json.loads(r["content"][0]["text"]) == want


def c_events():
    r = tool(REAL, "harness_events_list", {"log": FLOG})
    return equals_cli("harness_events_list", {"log": FLOG}, ["events", "list", "--log", FLOG], 0) and \
        len(r["structuredContent"]["stdout"].splitlines()) == 2


def c_hit():
    ok = equals_cli("harness_privacy_scan_text", {"text": "hello zzprobe-entry\n"}, ["privacy", "scan", "--text"], 1,
                    "hello zzprobe-entry\n", ["--privacy-list", LIST], {"HARNESS_PRIVACY_LIST": LIST})
    r = tool(REAL, "harness_privacy_scan_text", {"text": "hello zzprobe-entry\n"}, ["--privacy-list", LIST])
    return ok and r["structuredContent"]["stdout"] == "text:deny-token\n"


def c_config():
    r = tool(REAL, "harness_config_check", {"config": "/nonexistent-mcp/c"})
    return equals_cli("harness_config_check", {"config": "/nonexistent-mcp/c"}, ["config", "check", "--config", "/nonexistent-mcp/c"], 2) \
        and "config: file missing or unreadable" in r["structuredContent"]["stderr"]


def c_argv_order():
    lst = ["--privacy-list", LIST]
    a = tool(ARGVSTUB, "harness_status", {"ticket_file": "/t", "repo_map": ["a=/x", "b=/y"], "result_dir": "/r", "caps_dir": "/c"})
    b = tool(ARGVSTUB, "harness_privacy_scan_text", {"text": "x", "surface": "pr.body"}, lst)
    c = tool(ARGVSTUB, "harness_config_check", {"config": "/c", "repo": "r", "budget": "M"})
    return (a["structuredContent"]["stdout"] == "status\n/t\n--repo-map\na=/x\n--repo-map\nb=/y\n--result-dir\n/r\n--caps-dir\n/c\n"
            and b["structuredContent"]["stdout"] == "privacy\nscan\n--text\n--surface\npr.body\n"
            and c["structuredContent"]["stdout"] == "config\ncheck\n--config\n/c\n--repo\nr\n--budget\nM\n")


def c_stdin():
    r = tool(ENVSTUB, "harness_privacy_scan_text", {"text": "line one\nline two\n"}, ["--privacy-list", LIST])
    return r["structuredContent"]["stdout"] == "line one\nline two\n"


def c_dash_text():
    r = tool(ENVSTUB, "harness_privacy_scan_text", {"text": "-x\n"}, ["--privacy-list", LIST])
    return r["isError"] is False and r["structuredContent"]["stdout"] == "-x\n"


def planted_env():
    e = dict(ENV)
    e["HARNESS_PRIVACY_LIST_SECRET"] = "plain-word-placeholder"
    e["HARNESS_CONFIG"] = "/nonexistent-mcp/c"
    return e


def c_env_privacy():
    r = tool(ENVSTUB, "harness_privacy_scan_text", {"text": "x"}, ["--privacy-list", LIST], planted_env())
    return r["structuredContent"]["stderr"] == "HARNESS_PRIVACY_LIST\n"


def c_env_config():
    r = tool(ENVSTUB, "harness_config_check", {}, ["--privacy-list", LIST], planted_env())
    return r["structuredContent"]["stderr"] == ""


def c_cap_at():
    r = tool(BIGAT, "harness_version", {})
    sc = r["structuredContent"]
    return sc["stdout"] == "a" * 262144 and sc["stderr"] == "b" * 262144 and r["isError"] is False


def c_cap_over():
    marker = "\n[harness-mcp: output truncated at 262144 bytes]\n"
    a, _ = one(BIGOVER, [call(21, "harness_version", {}), req(22, "ping")], n=2)
    r = a[0]["result"]
    sc = r["structuredContent"]
    return (sc["stdout"] == "a" * 262144 + marker and sc["stderr"] == "b" * 262144 + marker and r["isError"] is True
            and sc["exit"] == 0 and a[1]["id"] == 22 and a[1]["result"] == {})


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
check("missing-required-no-spawn", lambda: no_spawn(call(8, "harness_status", {})))
check("bad-arguments-no-spawn", lambda: no_spawn(call(9, "harness_version", {"x": "y"})))
check("stub-spawn-seen", c_stub_spawn)
check("eof-exits-zero", c_eof)
check("relative-harness-refused", lambda: refused(["--harness", "harness"]))
check("missing-harness-refused", lambda: refused(["--harness", "/nonexistent-mcp/harness"]))
check("no-args-refused", lambda: refused([]))

LST = ["--privacy-list", LIST]
check("status-equals-cli", lambda: equals_cli(
    "harness_status", {"ticket_file": TK, "repo_map": [RM], "result_dir": FRES, "caps_dir": FCAPS},
    ["status", TK, "--repo-map", RM, "--result-dir", FRES, "--caps-dir", FCAPS], 0))
check("intake-equals-cli", lambda: equals_cli(
    "harness_ticket_intake", {"ticket_file": TK, "repo_map": [RM]}, ["ticket", "intake", TK, "--repo-map", RM], 0))
check("events-list-equals-cli", c_events)
check("evidence-show-equals-cli", lambda: equals_cli(
    "harness_evidence_show", {"ticket": "ex-1", "manifest_dir": FMD}, ["evidence", "show", "ex-1", "--manifest-dir", FMD], 0,
    "", LST, {"HARNESS_PRIVACY_LIST": LIST}))
check("privacy-hit-equals-cli", c_hit)
check("privacy-surface-equals-cli", lambda: equals_cli(
    "harness_privacy_scan_text", {"text": "hello\n", "surface": "pr.body"},
    ["privacy", "scan", "--text", "--surface", "pr.body"], 0, "hello\n", LST, {"HARNESS_PRIVACY_LIST": LIST}))
check("privacy-no-list-equals-cli", lambda: equals_cli(
    "harness_privacy_scan_text", {"text": "hello\n"}, ["privacy", "scan", "--text"], 2, "hello\n"))
check("config-check-equals-cli", c_config)
check("argv-order-exact", c_argv_order)
check("stdin-reaches-child", c_stdin)
check("dash-text-spawns", c_dash_text)
check("env-only-privacy-list", c_env_privacy)
check("env-none-for-config", c_env_config)
check("dash-flag-value-no-spawn", lambda: no_spawn(call(30, "harness_events_list", {"log": "--log"}), want="log starts with '-'"))
check("dash-positional-no-spawn", lambda: no_spawn(
    call(31, "harness_ticket_intake", {"ticket_file": "--testmode", "repo_map": ["a=/x"]}), want="ticket_file starts with '-'"))
check("dash-repo-map-no-spawn", lambda: no_spawn(
    call(32, "harness_ticket_intake", {"ticket_file": "/t", "repo_map": ["-x=/a"]}), want="repo_map starts with '-'"))
check("missing-log-no-spawn", lambda: no_spawn(call(33, "harness_events_list", {}), want="missing log"))
check("relative-log-no-spawn", lambda: no_spawn(call(34, "harness_events_list", {"log": "e.jsonl"}), want="log must be an absolute path"))
check("relative-repo-map-no-spawn", lambda: no_spawn(
    call(35, "harness_ticket_intake", {"ticket_file": "/t", "repo_map": ["a=rel"]}), want="repo_map must be an absolute path"))
check("unknown-argument-no-spawn", lambda: no_spawn(call(36, "harness_config_check", {"zz": "a"}), want="unknown argument zz"))
check("bad-enum-no-spawn", lambda: no_spawn(call(37, "harness_config_check", {"budget": "X"}), want="budget has a bad value"))
check("bad-surface-no-spawn", lambda: no_spawn(
    call(38, "harness_privacy_scan_text", {"text": "x", "surface": "A"}), want="surface has a bad value"))
check("empty-repo-map-no-spawn", lambda: no_spawn(
    call(39, "harness_ticket_intake", {"ticket_file": "/t", "repo_map": []}), want="repo_map has a bad value"))
check("nul-byte-no-spawn", lambda: no_spawn(call(40, "harness_events_list", {"log": "/a\u0000b"}), want="log holds a NUL byte"))
check("text-over-max-no-spawn", lambda: no_spawn(
    call(41, "harness_privacy_scan_text", {"text": "a" * 1048577}), extra=LST, want="text is over 1048576 bytes"))
check("cap-at-limit-untruncated", c_cap_at)
check("cap-over-limit-truncated", c_cap_over)
check("relative-privacy-list-refused", lambda: refused(["--harness", REAL, "--privacy-list", "mcp/fixtures/probe-list.txt"]))
check("missing-privacy-list-refused", lambda: refused(["--privacy-list", "/nonexistent-mcp/l"]))
check("repeated-flag-refused", lambda: refused(["--harness", REAL, "--harness", REAL]))

bad = results.count(False)
print("%d ok, %d failed" % (results.count(True), bad))
sys.exit(0 if bad == 0 else 1)
PY
