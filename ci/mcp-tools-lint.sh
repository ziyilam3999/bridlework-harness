#!/bin/sh
# MCP tools lint. Usage: ci/mcp-tools-lint.sh (--build | --harness <abs-path>) [--list <path>] [--self-test]
# Checks mcp/tools.json: read-only rows only, no deny-list verb, no duplicates, and every row's argv is a verb the real
# harness has (the probe answers as written AND a control with the last argv word swapped for a nonsense word does not).
# Exit 0 clean, 1 at least one refusal (one <row-name>:<rule> line per refusal on stderr), 2 usage, unreadable list,
# or a missing or unbuildable harness. A missing harness never skips.
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd) || exit 2
root=$(CDPATH= cd -- "$here/.." && pwd) || exit 2
exec python3 -I - "$root" "$@" <<'PY'
import copy, json, os, subprocess, sys, tempfile

ROOT = sys.argv[1]
ARGS = sys.argv[2:]
MAX_BYTES = 1 << 20
MAX_ROWS = 200
TIMEOUT = 30
CONTROL_WORD = "zz-no-such-verb"

OUTPUT_SCHEMA = {"type": "object",
                 "properties": {"exit": {"type": "integer"}, "stdout": {"type": "string"}, "stderr": {"type": "string"}},
                 "required": ["exit", "stdout", "stderr"], "additionalProperties": False}
DENY = [["events", "append"], ["config", "write"], ["controls", "write"], ["permission", "write"],
        ["session", "launch"], ["session", "close"], ["session", "record-start"], ["ticket", "finalize"],
        ["process", "start"], ["process", "stop"], ["workspace", "create"], ["workspace", "teardown"],
        ["quarantine", "sweep"], ["quarantine", "delete-listed"], ["manifest", "init"], ["manifest", "add"],
        ["manifest", "remove"], ["friction", "add"], ["friction", "triage"], ["friction", "bind"],
        ["friction", "page"], ["friction", "sweep"]]
ROW_KEYS = {"name", "description", "argv", "read_only", "inputSchema", "outputSchema", "probe"}
PROBE_REQUIRED = {"argv", "exit", "stream", "contains"}
PROBE_KEYS = PROBE_REQUIRED | {"stdin", "env"}


def die(msg, code=2):
    sys.stderr.write("mcp-tools-lint: " + msg + "\n")
    sys.exit(code)


def parse_args(a):
    mode = None; harness = None; lst = os.path.join(ROOT, "mcp", "tools.json"); self_test = False
    i = 0; n_mode = 0
    while i < len(a):
        w = a[i]
        if w == "--build":
            mode = "build"; n_mode += 1; i += 1
        elif w == "--harness":
            if i + 1 >= len(a):
                die("--harness needs a path")
            mode = "harness"; harness = a[i + 1]; n_mode += 1; i += 2
        elif w == "--list":
            if i + 1 >= len(a):
                die("--list needs a path")
            lst = a[i + 1]; i += 2
        elif w == "--self-test":
            self_test = True; i += 1
        else:
            die("usage: ci/mcp-tools-lint.sh (--build | --harness <abs-path>) [--list <path>] [--self-test]")
    if n_mode != 1:
        die("exactly one of --build or --harness <abs-path> is required")
    return mode, harness, lst, self_test


def resolve_harness(mode, harness):
    if mode == "build":
        try:
            r = subprocess.run(["cargo", "build", "--locked"], cwd=os.path.join(ROOT, "core", "rust"))
        except OSError:
            die("cannot run cargo")
        if r.returncode != 0:
            die("cargo build failed")
        harness = os.path.join(ROOT, "core", "rust", "target", "debug", "harness")
    elif not os.path.isabs(harness):
        die("--harness must be an absolute path")
    if not (os.path.isfile(harness) and os.access(harness, os.X_OK)):
        die("harness missing or not executable")
    return harness


def load_list(path):
    try:
        if os.path.getsize(path) > MAX_BYTES:
            die("list too large")
        with open(path, "rb") as f:
            d = json.loads(f.read().decode("utf-8"))
    except SystemExit:
        raise
    except Exception:
        die("list unreadable or not valid JSON")
    return d


def is_strlist(x):
    return isinstance(x, list) and all(isinstance(w, str) for w in x)


def is_int(x):
    return isinstance(x, int) and not isinstance(x, bool)


def row_shape_ok(r):
    if not isinstance(r, dict) or set(r.keys()) != ROW_KEYS:
        return False
    if not (isinstance(r["name"], str) and r["name"] and isinstance(r["description"], str)):
        return False
    if not (is_strlist(r["argv"]) and r["argv"]):
        return False
    if not (isinstance(r["inputSchema"], dict) and isinstance(r["outputSchema"], dict)):
        return False
    p = r["probe"]
    if not isinstance(p, dict) or not (PROBE_REQUIRED <= set(p.keys()) <= PROBE_KEYS):
        return False
    if not (is_strlist(p["argv"]) and is_int(p["exit"]) and p["stream"] in ("stdout", "stderr")
            and isinstance(p["contains"], str)):
        return False
    if "stdin" in p and not isinstance(p["stdin"], str):
        return False
    if "env" in p:
        e = p["env"]
        if not isinstance(e, dict) or not all(isinstance(k, str) and isinstance(v, str) for k, v in e.items()):
            return False
    return True


def run_child(harness, argv, probe):
    env = {k: v for k, v in os.environ.items() if not k.startswith("HARNESS_")}
    env.update(probe.get("env", {}))
    try:
        r = subprocess.run([harness] + argv, cwd=ROOT, env=env, input=probe.get("stdin", "").encode(),
                           stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=TIMEOUT)
    except (subprocess.TimeoutExpired, OSError):
        return None
    return r.returncode, r.stdout.decode("utf-8", "replace"), r.stderr.decode("utf-8", "replace")


def hits(res, probe):
    return res is not None and res[0] == probe["exit"] and probe["contains"] in res[1 if probe["stream"] == "stdout" else 2]


def lint(harness, lst):
    d = load_list(lst)
    bad = []
    if not isinstance(d, dict) or not isinstance(d.get("tools"), list) or len(d["tools"]) > MAX_ROWS:
        return ["(list):row-shape"]
    names = set(); verbs = set()
    for idx, r in enumerate(d["tools"]):
        if not row_shape_ok(r):
            nm = r.get("name") if isinstance(r, dict) and isinstance(r.get("name"), str) else "row%s" % idx
            bad.append(nm + ":row-shape")
            continue
        nm = r["name"]; argv = r["argv"]; p = r["probe"]
        skip_probe = False
        if r["read_only"] is not True:
            bad.append(nm + ":not-read-only")
        if r["outputSchema"] != OUTPUT_SCHEMA:
            bad.append(nm + ":output-schema")
        if nm in names:
            bad.append(nm + ":duplicate-name")
        names.add(nm)
        if tuple(argv) in verbs:
            bad.append(nm + ":duplicate-verb")
        verbs.add(tuple(argv))
        if any(argv[:len(x)] == x for x in DENY):
            bad.append(nm + ":deny-verb"); skip_probe = True
        if any(len(argv) < len(x) and x[:len(argv)] == argv for x in DENY):
            bad.append(nm + ":deny-prefix"); skip_probe = True
        if p["argv"][:len(argv)] != argv:
            bad.append(nm + ":probe-prefix"); skip_probe = True
        if p["contains"] == "" and p["exit"] != 0:
            bad.append(nm + ":probe-empty-contains")
        for v in p.get("env", {}).values():
            if os.path.isabs(v) or ".." in v.replace("\\", "/").split("/"):
                bad.append(nm + ":env-not-relative"); skip_probe = True
                break
        if skip_probe:
            continue
        if not hits(run_child(harness, p["argv"], p), p):
            bad.append(nm + ":probe-miss")
        ctl = list(p["argv"]); ctl[len(argv) - 1] = CONTROL_WORD
        if hits(run_child(harness, ctl, p), p):
            bad.append(nm + ":control-matches")
    return bad


def self_test(harness, lst):
    base = load_list(lst)
    tmp = tempfile.mkdtemp()
    script = os.path.join(ROOT, "ci", "mcp-tools-lint.sh")

    def get(c, name):
        return next(r for r in c["tools"] if r["name"] == name)

    def plant_row(name, argv, probe_argv, contains):
        r = copy.deepcopy(get(base, "harness_status")); r["name"] = name; r["argv"] = argv
        r["probe"] = {"argv": probe_argv, "exit": 2, "stream": "stderr", "contains": contains}
        return r

    def with_row(row):
        c = copy.deepcopy(base); c["tools"].append(row); return c

    def mut(fn):
        c = copy.deepcopy(base); fn(c); return c

    twin = copy.deepcopy(get(base, "harness_status")); twin["name"] = "harness_status_twin"
    vdup = copy.deepcopy(get(base, "harness_version")); vdup["argv"] = ["--version", "x"]
    vdup["probe"]["argv"] = ["--version", "x"]
    cfg = copy.deepcopy(get(base, "harness_status")); cfg["name"] = "harness_config"; cfg["argv"] = ["config"]
    cfg["probe"] = {"argv": ["config"], "exit": 2, "stream": "stderr", "contains": "usage: harness config"}

    def set_env_abs(c):
        get(c, "harness_privacy_scan_text")["probe"]["env"] = {"HARNESS_PRIVACY_LIST": "/nonexistent-mcp/list"}

    def set_ro(c):
        get(c, "harness_version")["read_only"] = False

    def set_secret(c):
        get(c, "harness_privacy_scan_text")["probe"]["env"]["HARNESS_PRIVACY_LIST_SECRET"] = "hello"

    # id, planted list (None = unchanged), extra env, harness override, expected rc, expected line
    checks = [
        ("clean-passes", None, {}, None, 0, None),
        ("planted-events-append", with_row(plant_row("harness_events_append", ["events", "append"], ["events", "append"], "usage: harness events")), {}, None, 1, "harness_events_append:deny-verb"),
        ("planted-duplicate-verb", with_row(twin), {}, None, 1, "harness_status_twin:duplicate-verb"),
        ("planted-zz-unknown-top", with_row(plant_row("harness_zz_unknown", ["zz-unknown"], ["zz-unknown"], "usage: harness")), {}, None, 1, "harness_zz_unknown:control-matches"),
        ("planted-events-zz-unknown", with_row(plant_row("harness_events_zz_unknown", ["events", "zz-unknown"], ["events", "zz-unknown"], "usage: harness events")), {}, None, 1, "harness_events_zz_unknown:control-matches"),
        ("planted-duplicate-name", with_row(vdup), {}, None, 1, "harness_version:duplicate-name"),
        ("planted-deny-prefix", with_row(cfg), {}, None, 1, "harness_config:deny-prefix"),
        ("planted-not-read-only", mut(set_ro), {}, None, 1, "harness_version:not-read-only"),
        ("planted-env-absolute", mut(set_env_abs), {}, None, 1, "harness_privacy_scan_text:env-not-relative"),
        ("secret-env-cleared", None, {"HARNESS_PRIVACY_LIST_SECRET": "hello"}, None, 0, None),
        ("old-list-env-cleared", None, {"HARNESS_PRIVACY_OLD_LIST": "/nonexistent-mcp/old"}, None, 0, None),
        ("planted-secret-in-probe-env", mut(set_secret), {}, None, 1, "harness_privacy_scan_text:probe-miss"),
        ("missing-harness-fails-closed", None, {}, "/nonexistent-mcp/harness", 2, None),
    ]
    ok = 0; failed = 0
    for cid, planted, extra, hover, want_rc, want_line in checks:
        path = lst
        if planted is not None:
            path = os.path.join(tmp, cid + ".json")
            with open(path, "w") as f:
                json.dump(planted, f)
        env = dict(os.environ); env.update(extra)
        try:
            r = subprocess.run(["sh", script, "--harness", hover or harness, "--list", path], cwd=ROOT, env=env,
                               stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=300)
            held = r.returncode == want_rc and (want_line is None or want_line in r.stderr.decode("utf-8", "replace").split("\n"))
        except (subprocess.TimeoutExpired, OSError):
            held = False
        if held:
            ok += 1; print("ok " + cid)
        else:
            failed += 1; print("FAIL " + cid)
    print("%d ok, %d failed" % (ok, failed))
    return 0 if failed == 0 else 1


mode, hpath, lst, st = parse_args(ARGS)
harness = resolve_harness(mode, hpath)
if st:
    sys.exit(self_test(harness, lst))
bad = lint(harness, lst)
for b in bad:
    sys.stderr.write(b + "\n")
sys.exit(1 if bad else 0)
PY
