#!/bin/sh
# Smoke for the base-only privacy gate (ticket 163). Usage: ci/gate-secret-smoke.sh <arm>
# Arms: exfil exfil-master leak failclosed timeout release logcheck static all
# The driver builds scratch git repos in a temp folder (a base commit plus a PR-shaped head) and RUNS THE WORKFLOW FILE'S OWN
# `run:` TEXT, step by step, extracted by step name with the step's own `env:` values (review P5). It never holds a copy of the
# steps. The deny list used is a dummy sentinel list, never the real secret, and nothing here prints it.
# Prints "ok <id>" / "FAIL <id>" per check. Exit 0 only if every check of the arm holds, 1 otherwise, 2 on a setup error.
# GATE_NEXT_WF (optional): path of the workflow file to test instead of .github/workflows/privacy-gate-next.yml.
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd) || exit 2
exec python3 -I - "$here/.." "$@" <<'PY'
import json, os, re, shutil, signal, subprocess, sys, tempfile

ROOT = os.path.realpath(sys.argv[1])
ARM = sys.argv[2] if len(sys.argv) > 2 else ""
WFDIR = os.path.join(ROOT, ".github", "workflows")
NEXT = os.environ.get("GATE_NEXT_WF") or os.path.join(WFDIR, "privacy-gate-next.yml")
RELEASE = os.path.join(WFDIR, "privacy-release-text.yml")
MASTERGATE = os.path.join(WFDIR, "privacy-gate.yml")
ENGINE = os.path.join(ROOT, "core", "rust", "target", "release", "harness")

TOKEN = "zzfixturetoken"                       # a denied word of the dummy list (plain word, not a real value)
SENTINEL = "sentinel-exfil-marker-q9"          # sits on a comment line of the dummy list; the engine ignores it
LIST = TOKEN + "\n# " + SENTINEL + "\n"
SECRET_WORKFLOWS = ["privacy-gate.yml", "privacy-gate-next.yml", "privacy-text.yml", "privacy-release-text.yml"]

fails = 0
oks = 0


def ok(i):
    global oks
    print("ok   " + i)
    oks += 1


def bad(i):
    global fails
    print("FAIL " + i)
    fails += 1


def check(i, cond):
    ok(i) if cond else bad(i)


# ------------------------------------------------------------------ workflow file reading (by step name)
def parse_jobs(path):
    """{job id: {"props": {key: value}, "steps": [ {"name","run","env","text"} ]}} for a workflow in the repo layout
    (job ids at 2 spaces, job keys at 4, steps `- ` at 6)."""
    lines = open(path, encoding="utf-8").read().split("\n")
    jobs = {}
    in_jobs = False
    cur = None
    in_steps = False
    for l in lines:
        if re.match(r"^jobs:\s*$", l):
            in_jobs = True
            continue
        if in_jobs and l and not l[0].isspace() and not l.startswith("#"):
            in_jobs = False
        if not in_jobs:
            continue
        m = re.match(r"^  ([A-Za-z0-9_-]+):\s*$", l)
        if m:
            cur = m.group(1)
            jobs[cur] = {"props": {}, "blocks": []}
            in_steps = False
            continue
        if cur is None:
            continue
        m = re.match(r"^    ([A-Za-z0-9_-]+):\s*(.*)$", l)
        if m:
            jobs[cur]["props"][m.group(1)] = m.group(2).strip()
            in_steps = m.group(1) == "steps"
            continue
        if in_steps:
            if re.match(r"^      - ", l):
                jobs[cur]["blocks"].append([l])
            elif jobs[cur]["blocks"]:
                jobs[cur]["blocks"][-1].append(l)
    for j in jobs.values():
        j["steps"] = [parse_step(b) for b in j.pop("blocks")]
    return jobs


def parse_step(block):
    norm = ["        " + block[0][8:]] + block[1:]       # `      - key:` -> `        key:` (keys at 8)
    st = {"name": None, "run": None, "env": {}, "text": "\n".join(block)}
    i = 0
    while i < len(norm):
        l = norm[i]
        m = re.match(r"^        ([A-Za-z0-9_-]+):\s*(.*)$", l)
        if not m:
            i += 1
            continue
        key, val = m.group(1), m.group(2).strip()
        if key == "name":
            st["name"] = val.strip("\"'")
        elif key == "run":
            if re.fullmatch(r"[|>][-+0-9]*", val):
                body = []
                j = i + 1
                while j < len(norm) and (not norm[j].strip() or len(norm[j]) - len(norm[j].lstrip()) > 8):
                    body.append(norm[j])
                    j += 1
                ind = min([len(x) - len(x.lstrip()) for x in body if x.strip()] or [0])
                st["run"] = "\n".join(x[ind:] if x.strip() else "" for x in body).rstrip("\n") + "\n"
                i = j
                continue
            st["run"] = val + "\n"
        elif key == "env":
            j = i + 1
            while j < len(norm) and (not norm[j].strip() or len(norm[j]) - len(norm[j].lstrip()) > 8):
                em = re.match(r"^\s+([A-Za-z_][A-Za-z0-9_]*):\s*(.*)$", norm[j])
                if em:
                    st["env"][em.group(1)] = em.group(2).strip().strip("\"'")
                j += 1
            i = j
            continue
        i += 1
    return st


def find_step(path, name, job=None):
    hits = []
    for jid, j in parse_jobs(path).items():
        if job and jid != job:
            continue
        hits += [s for s in j["steps"] if s["name"] == name]
    if len(hits) != 1:
        raise SystemExit("gate-secret-smoke: step %r found %d times in %s" % (name, len(hits), os.path.basename(path)))
    return hits[0]


def expr(v, secret):
    """Substitute the few GitHub expressions a secret step may carry; anything else is a setup error."""
    def one(m):
        e = m.group(1).strip()
        if e == "secrets.PRIVACY_LIST":
            return secret
        if e == "github.token":
            return "dummy-token"
        raise SystemExit("gate-secret-smoke: unexpected expression in a step env")
    return re.sub(r"\$\{\{(.*?)\}\}", one, v)


# ------------------------------------------------------------------ process running
def run_bash(script, cwd, env, timeout=None):
    """Run a `run:` text the way Actions does (bash -eo pipefail script-file). Returns (rc, output, timed_out)."""
    fd, p = tempfile.mkstemp(suffix=".sh", dir=env["RUNNER_TEMP"])
    with os.fdopen(fd, "w") as f:
        f.write(script)
    proc = subprocess.Popen(["bash", "--noprofile", "--norc", "-eo", "pipefail", p], cwd=cwd, env=env,
                            stdout=subprocess.PIPE, stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL, start_new_session=True)
    try:
        out, _ = proc.communicate(timeout=timeout)
        return proc.returncode, out.decode("utf-8", "replace"), False
    except subprocess.TimeoutExpired:
        os.killpg(proc.pid, signal.SIGKILL)
        out, _ = proc.communicate()
        return 124, out.decode("utf-8", "replace"), True


def load_env_file(path, env):
    if os.path.isfile(path):
        for l in open(path, encoding="utf-8").read().split("\n"):
            m = re.match(r"^([A-Za-z_][A-Za-z0-9_]*)=(.*)$", l)
            if m:
                env[m.group(1)] = m.group(2)


def run_job(wf, job, steps, cwd, base_env, secret=LIST, timeouts=None):
    """Run the named steps of one job in order, each step's own run text and env, stopping at the first failure.
    Returns {"rc", "failed", "ran", "out", "timed_out"}."""
    env = dict(base_env)
    res = {"rc": 0, "failed": None, "ran": [], "out": "", "timed_out": False}
    for name in steps:
        st = find_step(wf, name, job)
        e = dict(env)
        for k, v in st["env"].items():
            e[k] = expr(v, secret)
        res["ran"].append(name)
        rc, out, to = run_bash(st["run"], cwd, e, (timeouts or {}).get(name))
        res["out"] += out
        if rc != 0:
            res.update(rc=rc, failed=name, timed_out=to)
            return res
        load_env_file(env["GITHUB_ENV"], env)
    return res


# ------------------------------------------------------------------ fixture repos
def clean_env(tmp):
    runner = os.path.join(tmp, "runner")
    os.makedirs(os.path.join(runner, "tmp"), exist_ok=True)
    env = {"PATH": os.environ.get("PATH", "/usr/bin:/bin"), "HOME": os.path.join(runner, "home"),
           "GIT_CONFIG_GLOBAL": "/dev/null", "GIT_CONFIG_NOSYSTEM": "1", "LC_ALL": "C",
           "RUNNER_TEMP": os.path.join(runner, "tmp"), "GITHUB_ENV": os.path.join(runner, "envfile"),
           "GITHUB_EVENT_PATH": os.path.join(runner, "event.json"), "GITHUB_EVENT_NAME": "pull_request_target",
           "GITHUB_REPOSITORY": "o/r"}
    os.makedirs(env["HOME"], exist_ok=True)
    open(env["GITHUB_ENV"], "w").close()
    return env


def G(cwd, *args, check_rc=True, env=None):
    e = {"PATH": os.environ.get("PATH", "/usr/bin:/bin"), "GIT_CONFIG_GLOBAL": "/dev/null", "GIT_CONFIG_NOSYSTEM": "1",
         "HOME": cwd, "LC_ALL": "C"}
    r = subprocess.run(["git", "-c", "user.name=ci", "-c", "user.email=ci", "-c", "commit.gpgsign=false",
                        "-c", "core.hooksPath=/dev/null", "-c", "protocol.file.allow=always"] + list(args),
                       cwd=cwd, env=e, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL)
    if check_rc and r.returncode != 0:
        raise SystemExit("gate-secret-smoke: fixture git step failed: " + " ".join(args[:2]) + " :: " + r.stdout.decode("utf-8", "replace")[:200])
    return r.stdout.decode("utf-8", "replace").strip()


def write(root, rel, data, mode=None):
    p = os.path.join(root, rel)
    os.makedirs(os.path.dirname(p), exist_ok=True)
    with open(p, "wb") as f:
        f.write(data if isinstance(data, bytes) else data.encode())
    if mode:
        os.chmod(p, mode)


def exfil_script():
    return "#!/bin/sh\nprintf '%s' \"$HARNESS_PRIVACY_LIST_SECRET\" > \"$GATE_MARKER\"\nexit 0\n"


def build(tmp, variant="clean", engine="real", event=None, move_head=False, merge=False):
    """Build origin (bare), a base checkout with the engine, and the event file. Returns a dict of paths."""
    seed = os.path.join(tmp, "seed")
    origin = os.path.join(tmp, "origin.git")
    base = os.path.join(tmp, "base")
    os.makedirs(seed)
    G(seed, "init", "-q", "-b", "master")
    write(seed, "README.md", "clean\n")
    write(seed, ".gitignore", "target/\n")
    shutil.copy(os.path.join(ROOT, "ci", "privacy-gate.sh"), os.path.join(seed, "ci_privacy-gate.tmp"))
    os.makedirs(os.path.join(seed, "ci"))
    os.rename(os.path.join(seed, "ci_privacy-gate.tmp"), os.path.join(seed, "ci", "privacy-gate.sh"))
    G(seed, "add", "-A")
    G(seed, "commit", "-q", "-m", "base")
    if variant == "orphan":
        G(seed, "checkout", "-q", "--orphan", "pr")
        G(seed, "rm", "-rqf", ".")
        write(seed, "other.txt", "orphan\n")
    else:
        G(seed, "checkout", "-q", "-b", "pr")
    if variant == "clean":
        write(seed, "docs/note.txt", "hello\n")
    elif variant == "leak":
        write(seed, "docs/leak.txt", "this line has " + TOKEN + " in it\n")
    elif variant == "binary":
        write(seed, "blob.bin", b"a\x00b")
    elif variant == "oversize":
        write(seed, "big.txt", "a" * (3 * 1024 * 1024))
    elif variant == "exfil":
        write(seed, "ci/privacy-gate.sh", exfil_script(), 0o755)
        write(seed, "core/rust/target/release/harness", exfil_script(), 0o755)
        write(seed, ".gitattributes", "* filter=x diff=x\n")
        write(seed, ".lfsconfig", "[lfs]\n\turl = http://127.0.0.1:9/none\n")
        write(seed, "docs/note.txt", "hello\n")
    G(seed, "add", "-A", "-f")
    G(seed, "commit", "-q", "-m", "pr head")
    head = G(seed, "rev-parse", "HEAD")
    subprocess.run(["git", "init", "-q", "--bare", "-b", "master", origin], check=True,
                   env={"PATH": os.environ["PATH"], "GIT_CONFIG_GLOBAL": "/dev/null"})
    G(seed, "push", "-q", origin, "master:refs/heads/master", head + ":refs/pull/1/head")
    G(tmp, "clone", "-q", origin, base)
    # the engine that the runner's base checkout carries (built from base code; untracked)
    eng_dir = os.path.join(base, "core", "rust", "target", "release")
    os.makedirs(eng_dir, exist_ok=True)
    ep = os.path.join(eng_dir, "harness")
    if engine == "real":
        shutil.copy(ENGINE, ep)
    elif engine == "crash":
        write(base, "core/rust/target/release/harness", "#!/bin/sh\nexit 3\n", 0o755)
    elif engine == "sleep":
        write(base, "core/rust/target/release/harness", "#!/bin/sh\nsleep 60\n", 0o755)
    elif engine == "none":
        pass
    env = clean_env(tmp)
    env["GITHUB_WORKSPACE"] = base
    env["GATE_MARKER"] = os.path.join(tmp, "marker.out")
    # a runner with git-lfs-like drivers installed: any filter or textconv process leaves a mark
    fm = os.path.join(tmp, "filter.mark")
    env["FILTER_MARK"] = fm
    cfg = "touch '%s'; cat" % fm
    G(base, "config", "filter.x.clean", cfg)
    G(base, "config", "filter.x.smudge", cfg)
    G(base, "config", "filter.x.process", "touch '%s'" % fm)
    G(base, "config", "diff.x.textconv", "sh -c \"touch '%s'; cat \\\"\\$1\\\"\" --" % fm)
    ev = {"pull_request": {"number": 1, "head": {"sha": head, "repo": {"full_name": "o/r"}}}, "repository": {"full_name": "o/r"}}
    if event:
        event(ev)
    write(os.path.dirname(env["GITHUB_EVENT_PATH"]), "event.json", json.dumps(ev))
    if move_head:
        write(seed, "later.txt", "later\n")
        G(seed, "add", "-A")
        G(seed, "commit", "-q", "-m", "later")
        G(seed, "push", "-q", "-f", origin, "HEAD:refs/pull/1/head")
    return {"tmp": tmp, "base": base, "env": env, "head": head, "origin": origin, "seed": seed, "marker": env["GATE_MARKER"], "filtermark": fm}


def new_tmp():
    return tempfile.mkdtemp(prefix="gate-secret-smoke.")


STEPS = ["Event read", "Fetch data", "Scan"]


def job_of(wf):
    names = [k for k in parse_jobs(wf)]
    return names[0] if len(names) == 1 else "gate-next"


def mutate(src, pattern, repl, dst):
    text = open(src, encoding="utf-8").read()
    new, n = re.subn(pattern, lambda m: repl, text, flags=re.M)
    if n < 1 or new == text:
        raise SystemExit("gate-secret-smoke: mutant did not apply: " + dst)
    with open(dst, "w", encoding="utf-8") as f:
        f.write(new)
    return dst


def counts(out, key):
    return [int(x) for x in re.findall(r"(?<![A-Za-z0-9_])" + key + r"=([0-9]+)", out)]


# ------------------------------------------------------------------ exfil
def exfil_reasons(wf, tmp):
    """[] when the planted exfil got nothing and the base scan really ran; otherwise the list of reasons (never content)."""
    fx = build(tmp, "exfil")
    r = run_job(wf, job_of(wf), STEPS, fx["base"], fx["env"])
    why = []
    if r["rc"] != 0:
        why.append("job-failed:" + str(r["failed"]))
        if os.environ.get("GATE_SMOKE_DEBUG"):
            sys.stderr.write(r["out"][-1500:].replace(SENTINEL, "<s>"))
    if not counts(r["out"], "files") or max(counts(r["out"], "files")) < 1:
        why.append("base-tree-scan-did-not-run")
    if not counts(r["out"], "scanned_commits") or max(counts(r["out"], "scanned_commits")) < 1:
        why.append("base-range-scan-did-not-run")
    if os.path.exists(fx["marker"]) and SENTINEL in open(fx["marker"], errors="replace").read():
        why.append("sentinel-reached-pr-code")
    if os.path.exists(fx["filtermark"]):
        why.append("filter-process-ran")
    data = os.path.join(fx["env"]["RUNNER_TEMP"], "gate-data")
    if not os.path.isdir(data) or sorted(os.listdir(data)) != [".git"]:
        why.append("pr-file-on-disk-or-no-data-dir")
    if SENTINEL in r["out"] or TOKEN in r["out"]:
        why.append("secret-in-output")
    return why


def arm_exfil():
    tmp = new_tmp()
    try:
        why = exfil_reasons(NEXT, os.path.join(tmp, "a"))
        check("exfil-planted-gets-nothing" + ("" if not why else " (" + ",".join(why) + ")"), not why)
        if os.environ.get("GATE_NEXT_WF"):
            return          # explicit single-file run: no mutants (a mutant run is the caller's RED)
        # mutants of the workflow file itself: a later unsafe edit must turn this check red (review P5)
        m1 = mutate(NEXT, r'^(\s*)sh "\$base/ci/privacy-gate\.sh" --tree \|\| rc=1$',
                    '    git checkout -q -f refs/gate/head\n    sh ./ci/privacy-gate.sh --tree || rc=1', os.path.join(tmp, "m1.yml"))
        check("exfil-mutant-checkout-pr-and-run-it-is-caught", bool(exfil_reasons(m1, os.path.join(tmp, "b"))))
        m2 = mutate(NEXT, r'sh "\$base/ci/privacy-gate\.sh"', 'sh ./ci/privacy-gate.sh', os.path.join(tmp, "m2.yml"))
        check("exfil-mutant-script-from-data-dir-is-caught", bool(exfil_reasons(m2, os.path.join(tmp, "c"))))
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def arm_exfil_master():
    """Positive control for the detector: master's own Scan text, run verbatim on a merge-shaped checkout, DOES leak."""
    tmp = new_tmp()
    try:
        fx = build(tmp, "exfil")
        G(fx["base"], "config", "--unset-all", "filter.x.process")     # the merge itself needs a working checkout
        G(fx["base"], "fetch", "-q", "origin", "+refs/pull/1/head:refs/pr/head")
        G(fx["base"], "merge", "-q", "--no-ff", "-m", "merge", "refs/pr/head")
        st = find_step(MASTERGATE, "Scan")
        e = dict(fx["env"])
        e["GITHUB_EVENT_NAME"] = "pull_request"
        for k, v in st["env"].items():
            e[k] = expr(v, LIST)
        rc, out, _ = run_bash(st["run"], fx["base"], e)
        leaked = os.path.exists(fx["marker"]) and SENTINEL in open(fx["marker"], errors="replace").read()
        check("exfil-detector-sees-master-leak (master Scan hands the secret to PR code)", leaked)
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


# ------------------------------------------------------------------ leak
def leak_ok(wf, tmp):
    why = []
    fx = build(os.path.join(tmp, "l"), "leak")
    r = run_job(wf, job_of(wf), STEPS, fx["base"], fx["env"])
    if r["rc"] == 0:
        why.append("leak-not-blocked")
    if TOKEN in r["out"] or SENTINEL in r["out"]:
        why.append("token-in-output")
    fc = build(os.path.join(tmp, "c"), "clean")
    r2 = run_job(wf, job_of(wf), STEPS, fc["base"], fc["env"])
    if r2["rc"] != 0:
        why.append("clean-blocked:" + str(r2["failed"]))
    if not counts(r2["out"], "files") or max(counts(r2["out"], "files")) < 1:
        why.append("clean-scan-did-not-run")
    return why


def arm_leak():
    tmp = new_tmp()
    try:
        why = leak_ok(NEXT, tmp)
        check("leak-blocked-clean-passes-no-token-in-output" + ("" if not why else " (" + ",".join(why) + ")"), not why)
        if os.environ.get("GATE_NEXT_WF"):
            return
        m = mutate(NEXT, r'^\s*sh "\$base/ci/privacy-gate\.sh" .*$', '  true', os.path.join(tmp, "skip.yml"))
        check("leak-mutant-skipping-the-scan-is-caught", bool(leak_ok(m, os.path.join(tmp, "m"))))
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


# ------------------------------------------------------------------ failclosed
def fc_case(label, variant="clean", engine="real", event=None, move_head=False, secret=LIST, expect=None, shim=None,
            before_scan=None, timeouts=None, not_run=None):
    tmp = new_tmp()
    try:
        fx = build(tmp, variant, engine, event, move_head)
        env = fx["env"]
        if shim:
            sd = os.path.join(tmp, "shim")
            write(sd, "git", shim, 0o755)
            env["PATH"] = sd + os.pathsep + env["PATH"]
        steps = STEPS
        r = {"rc": 0}
        if before_scan:
            first = run_job(NEXT, job_of(NEXT), STEPS[:2], fx["base"], env, secret)
            if first["rc"] != 0:
                bad("failclosed-" + label + " (setup step failed: " + str(first["failed"]) + ")")
                return
            before_scan(fx)
            e2 = dict(env)
            load_env_file(env["GITHUB_ENV"], e2)
            r = run_job(NEXT, job_of(NEXT), ["Scan"], fx["base"], e2, secret, timeouts)
        else:
            r = run_job(NEXT, job_of(NEXT), steps, fx["base"], env, secret, timeouts)
        good = r["rc"] != 0 and (expect is None or r["failed"] == expect) and not (not_run and not_run in r["ran"])
        check("failclosed-" + label + ("" if good else " (rc=%s failed=%s)" % (r["rc"], r["failed"])), good)
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def arm_failclosed():
    fc_case("secret-blank", secret="", expect="Scan")
    fc_case("secret-whitespace", secret="  \n", expect="Scan")
    # secret variable UNSET (the script's own first guard): run the base script directly with the variable removed
    tmp = new_tmp()
    try:
        fx = build(tmp, "clean")
        e = {k: v for k, v in fx["env"].items() if k != "HARNESS_PRIVACY_LIST_SECRET"}
        rc, out, _ = run_bash('cd "$GITHUB_WORKSPACE"\nsh ci/privacy-gate.sh --tree\n', fx["base"], e)
        check("failclosed-secret-unset", rc != 0)
    finally:
        shutil.rmtree(tmp, ignore_errors=True)

    def evdel(path):
        def f(ev):
            d = ev
            for k in path[:-1]:
                d = d[k]
            if path[-1] in d:
                del d[path[-1]]
        return f

    def evset(path, val):
        def f(ev):
            d = ev
            for k in path[:-1]:
                d = d[k]
            d[path[-1]] = val
        return f
    fc_case("pr-fetch-fails", event=evset(["pull_request", "number"], 99), expect="Fetch data")
    fc_case("event-number-missing", event=evdel(["pull_request", "number"]), expect="Event read")
    for lab, val in [("letters", "abc"), ("shell-chars", "1;id"), ("negative", -1), ("bool", True), ("float", 1.5)]:
        fc_case("event-number-" + lab, event=evset(["pull_request", "number"], val), expect="Event read")
    fc_case("event-sha-missing", event=evdel(["pull_request", "head", "sha"]), expect="Event read")
    fc_case("event-sha-short", event=evset(["pull_request", "head", "sha"], "abc123"), expect="Event read")
    fc_case("event-sha-upper", event=evset(["pull_request", "head", "sha"], "A" * 40), expect="Event read")
    fc_case("event-sha-not-hex", event=evset(["pull_request", "head", "sha"], "g" * 40), expect="Event read")
    fc_case("fork-event", event=evset(["pull_request", "head", "repo", "full_name"], "x/y"), expect="Event read", not_run="Scan")
    fc_case("fork-repo-missing", event=evdel(["pull_request", "head", "repo"]), expect="Event read", not_run="Scan")
    fc_case("fetched-head-differs-from-event-head", move_head=True, expect="Fetch data", not_run="Scan")
    fc_case("fetched-head-not-40-hex", expect="Fetch data", not_run="Scan",
            shim="#!/bin/sh\nif [ \"$1\" = rev-parse ] && [ \"$2\" = refs/gate/head ]; then echo abc123; exit 0; fi\nexec " + shutil.which("git") + " \"$@\"\n")
    fc_case("merge-base-empty-orphan-history", variant="orphan", expect="Fetch data", not_run="Scan")
    fc_case("oversize-file-in-head-tree", variant="oversize", expect="Scan")
    fc_case("binary-file-in-head-tree", variant="binary", expect="Scan")
    fc_case("engine-binary-missing", engine="none", expect="Scan")
    fc_case("engine-crash", engine="crash", expect="Scan")

    def rm_data(fx):
        shutil.rmtree(os.path.join(fx["env"]["RUNNER_TEMP"], "gate-data"))
    fc_case("data-directory-missing", before_scan=rm_data, expect="Scan")
    fc_case("scan-killed-by-time-limit", engine="sleep", timeouts={"Scan": 3}, expect="Scan")


# ------------------------------------------------------------------ timeout (static)
def timeout_ok(path, job="gate-next"):
    try:
        jobs = parse_jobs(path)
    except Exception:
        return False
    j = jobs.get(job)
    if not j:
        return False
    v = j["props"].get("timeout-minutes", "")
    if not re.fullmatch(r"[0-9]+", v) or not (1 <= int(v) <= 15):
        return False
    return True


def arm_timeout():
    tmp = new_tmp()
    try:
        check("timeout-base-only-job-has-timeout-minutes-<=15", timeout_ok(NEXT))
        text = open(NEXT, encoding="utf-8").read()
        # mutant 1: the key moved to another (new) job
        m1 = re.sub(r"^    timeout-minutes:.*\n", "", text, flags=re.M)
        m1 += "  other:\n    runs-on: ubuntu-latest\n    timeout-minutes: 5\n    steps:\n      - run: echo hi\n"
        p1 = os.path.join(tmp, "m1.yml")
        open(p1, "w").write(m1)
        check("timeout-mutant-key-moved-to-another-job-is-caught", not timeout_ok(p1))
        m2 = re.sub(r"^(    timeout-minutes:).*$", r"\1 360", text, flags=re.M)
        p2 = os.path.join(tmp, "m2.yml")
        open(p2, "w").write(m2)
        check("timeout-mutant-value-360-is-caught", m2 != text and not timeout_ok(p2))
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


# ------------------------------------------------------------------ release text workflow (review P2)
def release_run(releases_text, secret=LIST, gh_shim=None, fetch=False):
    tmp = new_tmp()
    try:
        env = clean_env(tmp)
        env["GITHUB_WORKSPACE"] = ROOT
        rj = os.path.join(env["RUNNER_TEMP"], "releases.json")
        if releases_text is not None:
            with open(rj, "wb") as f:
                f.write(releases_text if isinstance(releases_text, bytes) else releases_text.encode())
        if gh_shim is not None:
            sd = os.path.join(tmp, "shim")
            write(sd, "gh", gh_shim, 0o755)
            env["PATH"] = sd + os.pathsep + env["PATH"]
        steps = ["Fetch releases", "Scan releases"] if fetch else ["Scan releases"]
        return run_job(RELEASE, "release-text", steps, ROOT, env, secret)
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def arm_release():
    rel = lambda name, body: {"name": name, "body": body, "tag_name": "v1", "draft": False}
    r = release_run(json.dumps([rel("v1", "fine notes"), rel("v2", "also fine")]))
    check("release-clean-passes-and-counts-2", r["rc"] == 0 and 2 in counts(r["out"], "releases_scanned"))
    r = release_run(json.dumps([[rel("v1", "ok")], [rel("v2", "also ok"), rel("v3", "ok")]]))
    check("release-paginated-shape-counts-3", r["rc"] == 0 and 3 in counts(r["out"], "releases_scanned"))
    r = release_run(json.dumps([rel("v1", "ok"), rel("v2", "notes with " + TOKEN + " inside")]))
    check("release-denied-token-in-body-blocked-no-token-in-output",
          r["rc"] != 0 and TOKEN not in r["out"] and SENTINEL not in r["out"])
    r = release_run(json.dumps([rel("title " + TOKEN, "ok")]))
    check("release-denied-token-in-name-blocked-no-token-in-output", r["rc"] != 0 and TOKEN not in r["out"])
    r = release_run(json.dumps([rel(None, None)]))
    check("release-null-name-and-body-is-clean-not-a-crash", r["rc"] == 0 and 1 in counts(r["out"], "releases_scanned"))
    r = release_run("[]")
    check("release-zero-releases-counts-0 (AC 7 needs the count >= 1 as its positive control)",
          r["rc"] == 0 and counts(r["out"], "releases_scanned") == [0])
    check("release-malformed-json-fails", release_run("[{not json")["rc"] != 0)
    check("release-empty-response-fails", release_run("")["rc"] != 0)
    check("release-missing-file-fails", release_run(None)["rc"] != 0)
    check("release-api-error-object-fails", release_run(json.dumps({"message": "API rate limit exceeded"}))["rc"] != 0)
    check("release-wrong-item-type-fails", release_run(json.dumps([1, 2]))["rc"] != 0)
    check("release-secret-blank-fails-even-with-zero-releases", release_run("[]", secret="")["rc"] != 0)
    r = release_run(None, gh_shim="#!/bin/sh\nexit 1\n", fetch=True)
    check("release-fetch-step-fails-when-gh-fails", r["rc"] != 0 and r["failed"] == "Fetch releases")
    r = release_run(None, gh_shim="#!/bin/sh\necho '{\"message\":\"rate limit\"}'\n", fetch=True)
    check("release-rate-limit-body-from-gh-fails-at-scan", r["rc"] != 0 and r["failed"] == "Scan releases")
    r = release_run(None, gh_shim="#!/bin/sh\nexit 0\n", fetch=True)
    check("release-empty-output-from-gh-fails-at-scan", r["rc"] != 0 and r["failed"] == "Scan releases")
    for jid, j in parse_jobs(RELEASE).items():
        for s in j["steps"]:
            check("release-no-expression-in-run:" + str(s["name"]), s["run"] is None or "${{" not in s["run"])


# ------------------------------------------------------------------ log check (review P3)
def arm_logcheck():
    tool = os.path.join(ROOT, "ci", "gate-log-check.sh")
    tmp = new_tmp()
    try:
        def go(label, text, canary, concl, expect_zero):
            p = os.path.join(tmp, label + ".log")
            if text is not None:
                open(p, "w").write(text)
            rc = subprocess.run(["sh", tool, p, canary, concl], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode
            check("logcheck-" + label, (rc == 0) == expect_zero)
        good = "2026 Scan\nfiles=3\nscanned_commits=2\n2026 done\n"
        go("good-log", good, "canarywordq", "success", True)
        go("canary-in-log", good + "canarywordq\n", "canarywordq", "success", False)
        go("echoed-run-line-only-is-vacuous", "sh ci/privacy-gate.sh --tree\nprivacy-gate: refused\n", "canarywordq", "success", False)
        go("failure-conclusion", good, "canarywordq", "failure", False)
        go("empty-conclusion", good, "canarywordq", "", False)
        go("zero-commits-scanned", "files=3\nscanned_commits=0\n", "canarywordq", "success", False)
        go("zero-files-scanned", "files=0\nscanned_commits=2\n", "canarywordq", "success", False)
        go("tree-only-no-range-count", "files=3\n", "canarywordq", "success", False)
        go("empty-canary-arg", good, "", "success", False)
        go("missing-log-file", None, "canarywordq", "success", False)
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


# ------------------------------------------------------------------ static file facts
def arm_static():
    nx = parse_jobs(NEXT)
    text = open(NEXT, encoding="utf-8").read()
    check("static-next-triggers-on-pull_request_target-only", re.search(r"^  pull_request_target:", text, re.M) and not re.search(r"^  (pull_request|workflow_run|push|schedule):", text, re.M))
    check("static-next-types-opened-synchronize-reopened", re.search(r"types:\s*\[opened, synchronize, reopened\]", text) is not None)
    j = nx.get("gate-next")
    check("static-next-job-named-gate-next", j is not None and len(nx) == 1)
    if j:
        names = [s["name"] for s in j["steps"]]
        check("static-next-step-order", names[1:] == ["Event read", "Fetch data", "Build engine", "Scan"])
        co = j["steps"][0]["text"]
        check("static-next-checkout-depth0-nocreds-no-ref", "fetch-depth: 0" in co and "persist-credentials: false" in co and not re.search(r"^\s*(ref|repository):", co, re.M))
        check("static-next-only-scan-holds-secret", [s["name"] for s in j["steps"] if "secrets." in s["text"]] == ["Scan"])
        check("static-next-environment", j["props"].get("environment") == "privacy-gate")
        check("static-next-fetch-without-depth-flag", "--depth" not in find_step(NEXT, "Fetch data")["run"])
    for wfn in SECRET_WORKFLOWS:
        p = os.path.join(WFDIR, wfn)
        check("static-exists:" + wfn, os.path.isfile(p))
        if not os.path.isfile(p):
            continue
        t = open(p, encoding="utf-8").read()
        for jid, jj in parse_jobs(p).items():
            check("static-environment:%s:%s" % (wfn, jid), jj["props"].get("environment") == "privacy-gate")
            for s in jj["steps"]:
                check("static-no-expression-in-run:%s:%s" % (wfn, s["name"]), s["run"] is None or "${{" not in s["run"])
        check("static-no-workflow_run:" + wfn, not re.search(r"workflow_run|download-artifact", t))
    pt = open(os.path.join(WFDIR, "privacy-text.yml"), encoding="utf-8").read()
    check("static-privacy-text-has-no-release-trigger", not re.search(r"^  release:", pt, re.M))
    rt = open(RELEASE, encoding="utf-8").read()
    check("static-release-triggers-schedule-and-dispatch-only", re.search(r"^  schedule:", rt, re.M) and re.search(r"^  workflow_dispatch:", rt, re.M) and not re.search(r"^  (release|pull_request|pull_request_target|push):", rt, re.M))
    check("static-release-permissions-contents-read-only", re.search(r"^permissions:\n  contents: read\n(?!  )", rt, re.M) is not None)
    check("static-release-timeout", timeout_ok(RELEASE, "release-text"))


ARMS = {"exfil": [arm_exfil], "exfil-master": [arm_exfil_master], "leak": [arm_leak], "failclosed": [arm_failclosed],
        "timeout": [arm_timeout], "release": [arm_release], "logcheck": [arm_logcheck], "static": [arm_static]}
ARMS["all"] = [f for k in ["static", "exfil-master", "exfil", "leak", "failclosed", "timeout", "release", "logcheck"] for f in ARMS[k]]

if ARM not in ARMS:
    sys.stderr.write("usage: gate-secret-smoke.sh exfil|exfil-master|leak|failclosed|timeout|release|logcheck|static|all\n")
    sys.exit(2)
if not os.path.isfile(ENGINE) and ARM in ("exfil", "exfil-master", "leak", "failclosed", "release", "all"):
    r = subprocess.run(["cargo", "build", "--release", "--locked"], cwd=os.path.join(ROOT, "core", "rust"),
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    if r.returncode != 0 or not os.path.isfile(ENGINE):
        sys.stderr.write("gate-secret-smoke: engine build failed\n")
        sys.exit(2)
for fn in ARMS[ARM]:
    fn()
print("%d ok, %d failed" % (oks, fails))
sys.exit(1 if fails else 0)
PY
