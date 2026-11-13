#!/bin/sh
# Smoke for the release job (ticket 3676). Usage: ci/release-smoke.sh <arm>
# Arms: static lint planted clean failclosed realbundle all
# The driver builds scratch git repos in a temp folder and RUNS THE WORKFLOW FILE'S OWN `run:` TEXT, extracted by step name with
# the step's own `env:` values. The job graph (which job waits for which) is READ FROM .github/workflows/release.yml; the driver
# never hard-codes "publish after scan". Checkout, Build engine, Upload and Download are stood in for (scratch repo, a copy of the
# built engine, the dist folder copied across). `gh` is a stub on PATH that records its argv to a marker file.
# The deny list is the dummy sentinel list, never the real secret; a home path is assembled at run time from pieces.
# Prints "ok <id>" / "FAIL <id>" per check, then "N ok, M failed". Exit 0 only if every check of the arm holds, 1 otherwise,
# 2 on a setup error.
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd) || exit 2
exec python3 -I - "$here/.." "$@" <<'PY'
import hashlib, io, json, os, re, shutil, stat, subprocess, sys, tarfile, tempfile

ROOT = os.path.realpath(sys.argv[1])
ARM = sys.argv[2] if len(sys.argv) > 2 else ""
WF = os.path.join(ROOT, ".github", "workflows", "release.yml")
LINT = os.path.join(ROOT, "ci", "workflow-lint.sh")
ENGINE = os.path.join(ROOT, "core", "rust", "target", "release", "harness")
TOKEN = "zzfixturetoken"
LIST = TOKEN + "\n# sentinel-release-smoke-q9\n"
TAG = "v0.1.0"
REPO = "example-owner/example-repo"
MAX_COPY = 5000                      # bound on files copied into a scratch repo (fail closed past it)

fails = 0
oks = 0
TMP = tempfile.mkdtemp(prefix="release-smoke-")


class Setup(Exception):
    pass


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


def setup_die(msg):
    sys.stderr.write("release-smoke: %s\n" % msg)
    shutil.rmtree(TMP, ignore_errors=True)
    sys.exit(2)


# ------------------------------------------------------------------ reading the workflow file
def read_wf(path):
    """{"top": {key: raw value}, "jobs": {id: {"props": {key: value}, "perm": {k: v}, "steps": [step]}}, "text": str}.
    Layout: jobs at 2 spaces, job keys at 4, steps `- ` at 6, step keys at 8."""
    text = open(path, encoding="utf-8").read()
    lines = text.split("\n")
    top, jobs = {}, {}
    cur = None
    ctx = None
    for l in lines:
        if not l.strip() or l.lstrip().startswith("#"):
            continue
        ind = len(l) - len(l.lstrip())
        if ind == 0:
            m = re.match(r"^([A-Za-z0-9_-]+):\s*(.*)$", l)
            if not m:
                raise Setup("unreadable top-level line")
            ctx = m.group(1)
            top[ctx] = m.group(2).strip()
            cur = None
            continue
        if ctx == "permissions":
            m = re.match(r"^\s+([\w-]+):\s*(\S+)$", l)
            if m:
                top.setdefault("_perm", {})[m.group(1)] = m.group(2)
            continue
        if ctx == "on":
            top["_on"] = top.get("_on", "") + l + "\n"
            continue
        if ctx != "jobs":
            continue
        m = re.match(r"^  ([A-Za-z0-9_-]+):\s*$", l)
        if m:
            cur = m.group(1)
            jobs[cur] = {"props": {}, "perm": {}, "blocks": [], "in": None}
            continue
        if cur is None:
            raise Setup("unreadable job line")
        j = jobs[cur]
        m = re.match(r"^    ([A-Za-z0-9_-]+):\s*(.*)$", l)
        if m:
            j["props"][m.group(1)] = m.group(2).strip()
            j["in"] = m.group(1)
            continue
        if j["in"] == "permissions":
            m = re.match(r"^      ([\w-]+):\s*(\S+)$", l)
            if m:
                j["perm"][m.group(1)] = m.group(2)
            continue
        if j["in"] == "steps":
            if re.match(r"^      - ", l):
                j["blocks"].append([l])
            elif j["blocks"]:
                j["blocks"][-1].append(l)
    for j in jobs.values():
        j["steps"] = [parse_step(b) for b in j.pop("blocks")]
    return {"top": top, "jobs": jobs, "text": text}


def parse_step(block):
    norm = ["        " + block[0][8:]] + block[1:]
    st = {"name": None, "uses": None, "run": None, "wd": None, "env": {}, "with": {}, "text": "\n".join(block)}
    i = 0
    while i < len(norm):
        l = norm[i]
        m = re.match(r"^        ([A-Za-z0-9_-]+):\s*(.*)$", l)
        if not m:
            i += 1
            continue
        key, val = m.group(1), m.group(2).strip()
        if key in ("env", "with") and not val:
            i += 1
            while i < len(norm) and (not norm[i].strip() or len(norm[i]) - len(norm[i].lstrip()) > 8):
                mm = re.match(r"^\s+([A-Za-z0-9_-]+):\s*(.*)$", norm[i])
                if mm:
                    st[key][mm.group(1)] = mm.group(2).strip()
                i += 1
            continue
        if key == "run" and re.fullmatch(r"[|>][-+0-9]*", val):
            body = []
            i += 1
            while i < len(norm) and (not norm[i].strip() or len(norm[i]) - len(norm[i].lstrip()) > 8):
                body.append(norm[i])
                i += 1
            ind = min([len(b) - len(b.lstrip()) for b in body if b.strip()] or [0])
            st["run"] = "\n".join(b[ind:] for b in body) + "\n"
            continue
        if key == "run":
            st["run"] = val + "\n"
        elif key == "name":
            st["name"] = val
        elif key == "uses":
            st["uses"] = val
        elif key == "working-directory":
            st["wd"] = val
        i += 1
    return st


def step(wf, job, name):
    for s in wf["jobs"][job]["steps"]:
        if s["name"] == name:
            return s
    raise Setup("step not found: %s/%s" % (job, name))


# ------------------------------------------------------------------ scratch repos and running steps
def base_env(extra=None):
    e = {k: v for k, v in os.environ.items() if not k.startswith("HARNESS_PRIVACY_")}
    e["GIT_CONFIG_GLOBAL"] = os.devnull
    e["GIT_CONFIG_SYSTEM"] = os.devnull
    if extra:
        e.update(extra)
    return e


def git(repo, *a, check_rc=True):
    r = subprocess.run(["git", "-C", repo] + list(a), stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=base_env())
    if check_rc and r.returncode != 0:
        raise Setup("git %s failed" % a[0])
    return r.stdout.decode("utf-8", "replace").strip()


def ensure_engine():
    if not os.path.isfile(ENGINE):
        r = subprocess.run(["cargo", "build", "--release", "--locked"], cwd=os.path.join(ROOT, "core", "rust"),
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        if r.returncode != 0 or not os.path.isfile(ENGINE):
            setup_die("engine build failed")


def make_template():
    """A scratch repo holding the worktree's committed-and-new files (never ignored ones, never the target folder)."""
    out = subprocess.run(["git", "-C", ROOT, "ls-files", "-z", "--cached", "--others", "--exclude-standard"],
                         stdout=subprocess.PIPE, env=base_env())
    files = [f for f in out.stdout.decode("utf-8").split("\0") if f and not f.startswith("core/rust/target/")]
    if out.returncode != 0 or not files or len(files) > MAX_COPY:
        setup_die("cannot list the worktree files")
    dst = os.path.join(TMP, "template")
    for f in files:
        s = os.path.join(ROOT, f)
        if os.path.islink(s) or not os.path.isfile(s):
            continue
        d = os.path.join(dst, f)
        os.makedirs(os.path.dirname(d), exist_ok=True)
        shutil.copy2(s, d)
    for a in (["init", "-q"], ["config", "user.email", "ci"], ["config", "user.name", "smoke"],
              ["config", "core.hooksPath", os.devnull], ["add", "-A"], ["commit", "-q", "-m", "scratch"]):
        git(dst, *a)
    return dst


_template = []
_n = [0]


def scratch(tag=TAG, engine=True):
    if not _template:
        ensure_engine()
        _template.append(make_template())
    _n[0] += 1
    dst = os.path.join(TMP, "repo%d" % _n[0])
    shutil.copytree(_template[0], dst, symlinks=True)
    if engine:
        e = os.path.join(dst, "core", "rust", "target", "release", "harness")
        os.makedirs(os.path.dirname(e), exist_ok=True)
        shutil.copy2(ENGINE, e)
    if tag:
        git(dst, "tag", "-f", tag)
    return dst


def commit(repo, msg="change"):
    git(repo, "add", "-A")
    git(repo, "commit", "-q", "-m", msg)


def stub_bin():
    d = os.path.join(TMP, "stub")
    if not os.path.isdir(d):
        os.makedirs(d)
        p = os.path.join(d, "gh")
        with open(p, "w") as fh:
            fh.write('#!/bin/sh\necho "$*" >> "$GH_MARKER"\nexit 0\n')
        os.chmod(p, 0o755)
    return d


def resolve(v, secret):
    """The value of a step env entry: only the two expressions the workflow uses are modelled; anything else is a setup error."""
    m = re.fullmatch(r"\$\{\{\s*([\w.]+)\s*\}\}", v)
    if not m:
        if "${{" in v:
            raise Setup("unmodelled expression")
        return v
    if m.group(1) == "secrets.PRIVACY_LIST":
        return secret
    if m.group(1) == "github.token":
        return "dummy-token-not-real"
    raise Setup("unmodelled expression")


def run_text(s, cwd, env, secret):
    e = dict(env)
    for k, v in s["env"].items():
        r = resolve(v, secret)
        if r is None:
            e.pop(k, None)
        else:
            e[k] = r
    p = subprocess.run(["bash", "--noprofile", "--norc", "-eo", "pipefail", "-c", s["run"]], cwd=cwd, env=e,
                       stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    return p.returncode, p.stdout.decode("utf-8", "replace"), p.stderr.decode("utf-8", "replace")


def run_graph(wfpath, repo, secret=LIST, ref="refs/tags/" + TAG, hook=None, no_engine=False):
    """Run the workflow's jobs in the order its own `needs:` allows. A job runs only if every job it needs exited 0.
    Returns {"rc": {job: rc}, "ran": [jobs], "out": {job: (stdout, stderr)}, "marker": path, "dist": path}.
    `hook(job, step name, ctx)` may change things just before a step runs."""
    wf = read_wf(wfpath)
    jobs = wf["jobs"]
    need = {}
    for jid, j in jobs.items():
        n = j["props"].get("needs", "")
        need[jid] = [x for x in re.split(r"[\s,\[\]]+", n) if x]
        for x in need[jid]:
            if x not in jobs:
                raise Setup("job %s needs unknown job %s" % (jid, x))
    ctx = {"repo": repo, "rc": {}, "ran": [], "out": {}, "artifacts": {}, "wf": wf}
    ctx["tmp"] = tempfile.mkdtemp(dir=TMP)
    ctx["dist"] = os.path.join(ctx["tmp"], "dist")
    ctx["marker"] = os.path.join(ctx["tmp"], "gh-marker")
    env = base_env({"PATH": stub_bin() + os.pathsep + os.environ.get("PATH", ""), "GH_MARKER": ctx["marker"],
                    "GITHUB_REF": ref, "GITHUB_REF_NAME": ref.rsplit("/", 1)[-1], "GITHUB_REPOSITORY": REPO,
                    "RUNNER_TEMP": ctx["tmp"]})
    pending = list(jobs)
    guard = 0
    while pending:
        guard += 1
        if guard > len(jobs) + 1:
            raise Setup("job graph does not resolve")
        progressed = False
        for jid in list(pending):
            if any(x in pending for x in need[jid]):
                continue
            pending.remove(jid)
            progressed = True
            if any(ctx["rc"].get(x) != 0 for x in need[jid]):
                continue                                   # a needed job failed: this job never starts
            ctx["ran"].append(jid)
            jrc = 0
            outs, errs = "", ""
            pub = os.path.join(ctx["tmp"], "pub-" + jid)
            os.makedirs(pub)
            for s in jobs[jid]["steps"]:
                if hook:
                    hook(jid, s["name"], ctx)
                if s["uses"]:
                    u = s["uses"]
                    if u.startswith("actions/checkout@"):
                        continue
                    if u.startswith("actions/upload-artifact@"):
                        p = s["with"]["path"].replace("${{ runner.temp }}", ctx["tmp"])
                        ctx["artifacts"][s["with"]["name"]] = p
                        if not os.path.isdir(p):
                            jrc = 1
                            break
                        continue
                    if u.startswith("actions/download-artifact@"):
                        src = ctx["artifacts"].get(s["with"]["name"])
                        if not src:
                            jrc = 1
                            break
                        shutil.copytree(src, os.path.join(pub, s["with"]["path"]), symlinks=True)
                        continue
                    raise Setup("unmodelled action")
                if s["run"] is None:
                    raise Setup("step without run or uses")
                if s["name"] == "Build engine":
                    if s["run"].strip() != "cargo build --release --locked" or s["wd"] != "core/rust":
                        raise Setup("Build engine step changed")
                    if no_engine:
                        shutil.rmtree(os.path.join(repo, "core", "rust", "target"), ignore_errors=True)
                    continue                               # stand-in: the engine was copied into the scratch repo
                cwd = repo if jid == "scan" else pub
                if jid == "scan":
                    sec = secret
                else:
                    sec = None
                e = dict(env)
                if secret is None:
                    pass
                rc, so, se = run_text(s, cwd, e, sec if sec is not None else "")
                if s["name"] == "Scan" and secret is None:
                    # unset (not just blank): re-run with the variable removed
                    rc, so, se = run_text_unset(s, cwd, e)
                outs += so
                errs += se
                ctx["out"].setdefault(jid, {})[s["name"]] = (so, se, rc)
                if rc != 0:
                    jrc = rc
                    break
            ctx["rc"][jid] = jrc
            ctx["out"].setdefault(jid, {})["_all"] = (outs, errs, jrc)
        if not progressed:
            raise Setup("job graph does not resolve")
    return ctx


def run_text_unset(s, cwd, env):
    e = dict(env)
    for k in s["env"]:
        e.pop(k, None)
    p = subprocess.run(["bash", "--noprofile", "--norc", "-eo", "pipefail", "-c", s["run"]], cwd=cwd, env=e,
                       stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    return p.returncode, p.stdout.decode("utf-8", "replace"), p.stderr.decode("utf-8", "replace")


def step_in(repo, name, ref="refs/tags/" + TAG, secret=LIST, unset=False, dist=None, wf=None):
    """Run ONE named step of the scan job (the workflow's own text) in a scratch repo; returns (rc, out, err, dist)."""
    w = read_wf(wf or WF)
    s = step(w, "scan", name)
    tmp = tempfile.mkdtemp(dir=TMP)
    dist = dist or os.path.join(tmp, "dist")
    env = base_env({"PATH": stub_bin() + os.pathsep + os.environ.get("PATH", ""), "GH_MARKER": os.path.join(tmp, "gh-marker"),
                    "GITHUB_REF": ref, "GITHUB_REF_NAME": ref.rsplit("/", 1)[-1], "GITHUB_REPOSITORY": REPO, "RUNNER_TEMP": tmp})
    if unset:
        rc, o, e = run_text_unset(s, repo, env)
    else:
        rc, o, e = run_text(s, repo, env, secret)
    return rc, o, e, os.path.join(tmp, "dist"), os.path.join(tmp, "gh-marker")


def publish_in(dist, tmp=None):
    """Run the workflow's own Publish text over a copy of a dist folder (the Download stand-in)."""
    w = read_wf(WF)
    s = step(w, "publish", "Publish")
    d = tempfile.mkdtemp(dir=TMP)
    shutil.copytree(dist, os.path.join(d, "release-dist"), symlinks=True)
    marker = os.path.join(d, "gh-marker")
    env = base_env({"PATH": stub_bin() + os.pathsep + os.environ.get("PATH", ""), "GH_MARKER": marker, "GITHUB_REF": "refs/tags/" + TAG,
                    "GITHUB_REF_NAME": TAG, "GITHUB_REPOSITORY": REPO, "RUNNER_TEMP": d})
    rc, o, e = run_text(s, d, env, "")
    return rc, os.path.exists(marker), marker


def build_dist(repo):
    rc, o, e, dist, _ = step_in(repo, "Build release")
    return rc, dist


# ------------------------------------------------------------------ arms
def arm_static():
    try:
        wf = read_wf(WF)
    except (Setup, OSError) as ex:
        bad("static-readable:%s" % ex)
        return
    text = wf["text"]
    on = wf["top"].get("_on", "")
    check("static-triggers-push-tags-only-semver",
          re.fullmatch(r"\s*push:\s*\n\s*tags:\s*\['v\[0-9\]\+\.\[0-9\]\+\.\[0-9\]\+'\]\s*\n", on) is not None)
    check("static-top-permissions-contents-read-only", wf["top"].get("_perm") == {"contents": "read"})
    jobs = wf["jobs"]
    check("static-job-ids-scan-and-publish", sorted(jobs) == ["publish", "scan"])
    scan, pub = jobs.get("scan"), jobs.get("publish")
    if not scan or not pub:
        return
    check("static-scan-step-order", [s["name"] for s in scan["steps"]] == ["Checkout", "Build engine", "Build release", "Scan", "Upload"])
    allsec = [(jid, s["name"]) for jid, j in jobs.items() for s in j["steps"] if "secrets." in s["text"]]
    check("static-only-scan-step-holds-secret", allsec == [("scan", "Scan")])
    check("static-scan-environment-privacy-gate", scan["props"].get("environment") == "privacy-gate")
    check("static-scan-no-job-permissions", not scan["perm"] and "permissions" not in scan["props"])
    check("static-publish-needs-scan", pub["props"].get("needs") == "scan")
    check("static-publish-one-grant-contents-write", pub["perm"] == {"contents": "write"})
    ptext = "\n".join(s["text"] for s in pub["steps"])
    check("static-publish-no-checkout-no-secret-no-environment",
          "actions/checkout" not in ptext and "secrets." not in ptext and "environment" not in pub["props"])
    dl = [(jid, s["name"]) for jid, j in jobs.items() for s in j["steps"] if s["uses"] and "download-artifact" in s["uses"]]
    check("static-download-artifact-only-in-publish", dl and all(j == "publish" for j, _ in dl))
    check("static-no-expression-in-run", all(s["run"] is None or "${{" not in s["run"] for j in jobs.values() for s in j["steps"]))
    body = [l for l in text.split("\n") if not l.lstrip().startswith("#")]
    check("static-release-yml-never-names-the-engine-binary", not any("target/release" in l for l in body))
    sh = open(os.path.join(ROOT, "ci", "release-build.sh"), encoding="utf-8").read().split("\n")
    verbs = re.compile(r"\b(cp|mv|install|ln|rsync|copy\w*|add|addfile|dist)\b")
    check("static-release-build-never-copies-the-engine",
          not any("target/release" in l and verbs.search(l) for l in sh if not l.lstrip().startswith("#")))
    check("static-publish-run-has-no-checkout-code", "ci/" not in (step(wf, "publish", "Publish")["run"] or ""))


def lint_on(text, fname="release.yml"):
    d = tempfile.mkdtemp(dir=TMP)
    p = os.path.join(d, fname)
    with open(p, "w", encoding="utf-8") as fh:
        fh.write(text)
    r = subprocess.run(["sh", LINT, p], stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=base_env())
    return r.returncode, r.stdout.decode()


def mutate(text, a, b, count=1):
    if text.count(a) != count:
        setup_die("mutation anchor not found exactly %d time(s): %r" % (count, a[:40]))
    return text.replace(a, b)


def arm_lint():
    text = open(WF, encoding="utf-8").read()
    r = subprocess.run(["sh", LINT], stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=base_env())
    check("lint-real-tree-clean", r.returncode == 0)
    rc, out = lint_on(text)
    check("lint-unmutated-copy-exit-0", rc == 0)
    muts = [
        ("write-at-top-level", mutate(text, "permissions:\n  contents: read\n", "permissions:\n  contents: write\n"), "release.yml", "permission-write"),
        ("secret-in-publish", mutate(text, "          GH_TOKEN: ${{ github.token }}\n", "          GH_TOKEN: ${{ github.token }}\n          X_LIST: ${{ secrets.PRIVACY_LIST }}\n"), "release.yml", "release-publish-shape"),
        ("checkout-in-publish", mutate(text, "    steps:\n      - name: Download\n", "    steps:\n      - uses: actions/checkout@v4\n        with:\n          persist-credentials: false\n      - name: Download\n"), "release.yml", "release-publish-shape"),
        ("renamed-copy", text, "other.yml", "permission-write"),
        ("third-job-with-write", text + "  extra:\n    runs-on: ubuntu-latest\n    permissions:\n      contents: write\n    steps:\n      - run: echo hi\n", "release.yml", None),
        ("needs-build", mutate(text, "    needs: scan\n", "    needs: build\n"), "release.yml", "release-publish-shape"),
        ("environment-in-publish", mutate(text, "    needs: scan\n", "    needs: scan\n    environment: privacy-gate\n"), "release.yml", "release-publish-shape"),
        ("issues-write-in-publish", mutate(text, "      contents: write\n", "      contents: write\n      issues: write\n"), "release.yml", None),
    ]
    for name, t, fname, rule in muts:
        rc, out = lint_on(t, fname)
        check("lint-refused:%s" % name, rc == 1 and (rule is None or (":" + rule) in out))


PLANTS = ["a-text", "b-home-path", "c-binary", "d-name"]


def plant(repo, kind):
    word = TOKEN
    d = os.path.join(repo, "plugins", "claude")
    if kind == "a-text":
        open(os.path.join(d, "planted-note.txt"), "w").write("note: " + word + "\n")
        return [word]
    if kind == "b-home-path":
        user = "zzplanteduser"
        path = "/" + "ho" + "me" + "/" + user
        open(os.path.join(d, "planted-note.txt"), "w").write("path " + path + "/x\n")
        return [user, path]
    if kind == "c-binary":
        open(os.path.join(d, "planted.bin"), "wb").write(b"\x00\x01\x02" + word.encode() + b"\x00\xff")
        return [word]
    if kind == "d-name":
        open(os.path.join(d, word + ".sh"), "w").write("#!/bin/sh\necho ok\n")
        return [word]
    raise Setup("unknown plant")


def arm_planted():
    for kind in PLANTS:
        repo = scratch()
        words = plant(repo, kind)
        commit(repo, "plant")
        git(repo, "tag", "-f", TAG)
        try:
            ctx = run_graph(WF, repo)
        except Setup as ex:
            bad("planted-%s:setup %s" % (kind, ex))
            continue
        scan = ctx["out"].get("scan", {})
        so, se, _ = scan.get("Scan", ("", "", None))
        build_rc = scan.get("Build release", ("", "", None))[2]
        check("planted-%s-build-release-ok" % kind, build_rc == 0)
        check("planted-%s-scan-exit-1" % kind, scan.get("Scan", (0, 0, None))[2] == 1)
        check("planted-%s-no-publish-no-marker" % kind, "publish" not in ctx["ran"] and not os.path.exists(ctx["marker"]))
        blob = so + se
        check("planted-%s-output-hides-the-plant" % kind, not any(w in blob for w in words))
    # NR1: the graph is read from the file. A needs value that names no job must stop the driver, not publish.
    repo = scratch()
    text = open(WF, encoding="utf-8").read()
    d = tempfile.mkdtemp(dir=TMP)
    p = os.path.join(d, "release.yml")
    open(p, "w", encoding="utf-8").write(mutate(text, "    needs: scan\n", "    needs: nosuchjob\n"))
    raised = False
    try:
        c = run_graph(p, repo)
    except Setup:
        raised = True
    check("planted-nr1-unknown-needs-is-a-setup-error-and-publish-never-runs", raised)
    # and the graph really decides: with `needs:` removed the file would let publish start without scan
    p2 = os.path.join(tempfile.mkdtemp(dir=TMP), "release.yml")
    open(p2, "w", encoding="utf-8").write(mutate(text, "    needs: scan\n", ""))
    c2 = run_graph(p2, scratch())
    check("planted-nr1-graph-is-read-from-the-file", "publish" in c2["ran"])


def md_members(repo):
    out = git(repo, "ls-tree", "-r", "--name-only", "HEAD", "plugins/claude")
    return sorted("bridlework-harness/" + x for x in out.split("\n") if x)


def arm_clean():
    repo = scratch()
    ctx = run_graph(WF, repo)
    scan = ctx["out"]["scan"]
    so, se, src = scan["Scan"]
    n = len(md_members(repo))
    check("clean-scan-exit-0", src == 0)
    check("clean-scan-counts", "artifacts_scanned=3" in se and ("members_scanned=%d" % n) in se)
    check("clean-publish-ran-after-scan", ctx["ran"] == ["scan", "publish"] and ctx["rc"].get("publish") == 0)
    mk = open(ctx["marker"]).read().split("\n") if os.path.exists(ctx["marker"]) else []
    mk = [x for x in mk if x]
    want = "release create %s --repo %s --verify-tag --title %s --notes-file release-notes.md bridlework-harness-claude-plugin-0.1.0.tar.gz SHA256SUMS" % (TAG, REPO, TAG)
    check("clean-exactly-one-gh-call-with-two-assets", mk == [want])
    rc1, d1 = build_dist(scratch())
    rc2, d2 = build_dist(scratch())
    same = rc1 == 0 and rc2 == 0 and open(os.path.join(d1, "SHA256SUMS")).read() == open(os.path.join(d2, "SHA256SUMS")).read()
    check("clean-build-twice-byte-identical-sha256sums", same)


def arm_realbundle():
    repo = scratch()
    head_tree = subprocess.run(["git", "-C", ROOT, "rev-parse", "HEAD:plugins/claude"], stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=base_env())
    check("realbundle-scratch-bundle-is-the-worktree-HEAD-bundle",
          head_tree.returncode == 0 and head_tree.stdout.decode().strip() == git(repo, "rev-parse", "HEAD:plugins/claude"))
    rc, o, e, dist, _ = step_in(repo, "Build release")
    check("realbundle-build-release-exit-0", rc == 0)
    before = {f: hashlib.sha256(open(os.path.join(dist, f), "rb").read()).hexdigest() for f in os.listdir(dist)} if rc == 0 else {}
    w = read_wf(WF)
    s = step(w, "scan", "Scan")
    env = base_env({"RUNNER_TEMP": os.path.dirname(dist)})
    rc2, o2, e2 = run_text(s, repo, env, LIST)
    check("realbundle-scan-exit-0-on-the-on-disk-files", rc2 == 0)
    after = {f: hashlib.sha256(open(os.path.join(dist, f), "rb").read()).hexdigest() for f in os.listdir(dist)} if rc == 0 else {}
    check("realbundle-scan-did-not-touch-the-files", before == after and len(before) == 3)
    name = "bridlework-harness-claude-plugin-0.1.0.tar.gz"
    sums = dict(reversed(l.split("  ", 1)) for l in open(os.path.join(dist, "SHA256SUMS")).read().split("\n") if l) if rc == 0 else {}
    check("realbundle-tarball-sha256-matches-SHA256SUMS", sums.get(name) == before.get(name) and name in sums)
    with tarfile.open(os.path.join(dist, name), "r:gz") as tf:
        members = sorted(tf.getnames())
    check("realbundle-member-list-equals-git-ls-tree", members == md_members(repo) and len(members) >= 15)


def arm_failclosed():
    def built(**kw):
        repo = scratch(**kw)
        rc, dist = build_dist(repo)
        if rc != 0:
            raise Setup("baseline build failed")
        return repo, dist

    def scan_rc(repo, dist, **kw):
        rc, o, e, _, mk = step_in(repo, "Scan", dist=dist, **kw)
        return rc

    # secret unset / blank
    repo, dist = built()
    sec_env = os.path.join(os.path.dirname(dist))
    check("failclosed-secret-unset", step_in(repo, "Scan", unset=True)[0] == 1)
    check("failclosed-secret-blank", step_in(repo, "Scan", secret="  \n")[0] == 1)
    # engine missing
    repo = scratch(engine=False)
    check("failclosed-engine-missing-at-build-release", step_in(repo, "Build release")[0] == 1)
    repo, dist = built()
    os.remove(os.path.join(repo, "core", "rust", "target", "release", "harness"))
    check("failclosed-engine-missing-at-scan", step_in(repo, "Scan")[0] == 1)

    # dist shapes (run in a scratch repo; each dist is hand-made)
    def dist_with(fn):
        d = os.path.join(tempfile.mkdtemp(dir=TMP), "dist")
        os.makedirs(d)
        fn(d)
        return d

    repo = scratch()
    check("failclosed-empty-dist", step_in(repo, "Scan", dist=None)[0] == 1)           # no dist folder at all
    # the workflow text names $RUNNER_TEMP/dist, so place the odd dist there through RUNNER_TEMP by building into the step's tmp
    def odd(label, fn):
        w = read_wf(WF)
        s = step(w, "scan", "Scan")
        tmp = tempfile.mkdtemp(dir=TMP)
        d = os.path.join(tmp, "dist")
        os.makedirs(d)
        fn(d)
        rc, o, e = run_text(s, repo, base_env({"RUNNER_TEMP": tmp}), LIST)
        check("failclosed-" + label, rc == 1)

    odd("empty-dist-folder", lambda d: None)
    open(os.path.join(TMP, "f.txt"), "w").write("hello\n")
    odd("symlink-in-dist", lambda d: (open(os.path.join(d, "a.txt"), "w").write("ok\n"), os.symlink(os.path.join(TMP, "f.txt"), os.path.join(d, "l.txt"))))
    odd("subfolder-in-dist", lambda d: (open(os.path.join(d, "a.txt"), "w").write("ok\n"), os.makedirs(os.path.join(d, "sub"))))

    def mktar(d, members, name="x.tar.gz"):
        with tarfile.open(os.path.join(d, name), "w:gz") as tf:
            for ti, data in members:
                tf.addfile(ti, io.BytesIO(data) if data is not None else None)

    def ti(name, typ=tarfile.REGTYPE, size=3, link=""):
        t = tarfile.TarInfo(name)
        t.type = typ
        t.size = size if typ == tarfile.REGTYPE else 0
        t.linkname = link
        return t

    odd("tar-dotdot-member", lambda d: mktar(d, [(ti("../x"), b"abc")]))
    odd("tar-absolute-member", lambda d: mktar(d, [(ti("/x"), b"abc")]))
    odd("tar-symlink-member", lambda d: mktar(d, [(ti("l", tarfile.SYMTYPE, link="/etc/hosts"), None)]))
    odd("tar-corrupt", lambda d: open(os.path.join(d, "x.tar.gz"), "wb").write(b"\x1f\x8b\x08\x00not really gzip data" * 4))
    odd("tar-not-gzip-at-all", lambda d: open(os.path.join(d, "x.tar.gz"), "wb").write(b"plain"))

    # tags
    for tag, label in (("v0.1", "tag-v0.1"), ("v0.1.0-rc1", "tag-v0.1.0-rc1"), ("v9.9.9", "tag-v9.9.9-not-cargo-version"), ("v01.1.0", "tag-leading-zero")):
        repo = scratch(tag=tag)
        rc, o, e, d, _ = step_in(repo, "Build release", ref="refs/tags/" + tag)
        check("failclosed-" + label, rc == 1 and not os.path.exists(os.path.join(d, "SHA256SUMS")))
    repo = scratch(tag=None)
    check("failclosed-ref-not-a-tag-at-all", step_in(repo, "Build release", ref="refs/heads/master")[0] == 1)
    check("failclosed-tag-names-no-local-commit", step_in(repo, "Build release")[0] == 1)
    # plugin.json drift
    repo = scratch(tag=None)
    pj = os.path.join(repo, "plugins", "claude", ".claude-plugin", "plugin.json")
    t = open(pj).read()
    open(pj, "w").write(t.replace('"version": "0.1.0"', '"version": "0.1.1"'))
    commit(repo, "drift")
    git(repo, "tag", TAG)
    check("failclosed-plugin-json-version-differs", step_in(repo, "Build release")[0] == 1)
    # dist exists and is not empty at build time
    repo = scratch()
    tmp = tempfile.mkdtemp(dir=TMP)
    os.makedirs(os.path.join(tmp, "dist"))
    open(os.path.join(tmp, "dist", "keep"), "w").write("x")
    w = read_wf(WF)
    rc, o, e = run_text(step(w, "scan", "Build release"), repo, base_env({"RUNNER_TEMP": tmp, "GITHUB_REF": "refs/tags/" + TAG}), LIST)
    check("failclosed-dist-not-empty-at-build", rc == 1)
    # binary-in-dist: pins today's reason for the deferral (the engine binary never scans clean)
    repo, dist = built()
    shutil.copy2(ENGINE, os.path.join(dist, "harness"))
    check("failclosed-binary-in-dist", scan_rc(repo, dist) == 1)

    # publish side
    repo, dist = built()
    rc, marker, _ = publish_in(dist)
    check("failclosed-publish-baseline-passes", rc == 0 and marker)
    d2 = os.path.join(tempfile.mkdtemp(dir=TMP), "dist")
    shutil.copytree(dist, d2)
    with open(os.path.join(d2, "release-notes.md"), "a") as fh:
        fh.write("tampered\n")
    rc, marker, _ = publish_in(d2)
    check("failclosed-publish-sha256sums-mismatch", rc != 0 and not marker)
    d3 = os.path.join(tempfile.mkdtemp(dir=TMP), "dist")
    shutil.copytree(dist, d3)
    open(os.path.join(d3, "extra.txt"), "w").write("x\n")
    rc, marker, _ = publish_in(d3)
    check("failclosed-publish-extra-unlisted-file", rc != 0 and not marker)
    d4 = os.path.join(tempfile.mkdtemp(dir=TMP), "dist")
    shutil.copytree(dist, d4)
    os.remove(os.path.join(d4, "release-notes.md"))
    rc, marker, _ = publish_in(d4)
    check("failclosed-publish-listed-file-missing", rc != 0 and not marker)


ARMS = {"static": [arm_static], "lint": [arm_lint], "planted": [arm_planted], "clean": [arm_clean],
        "failclosed": [arm_failclosed], "realbundle": [arm_realbundle]}
ARMS["all"] = [f for k in ["static", "lint", "planted", "clean", "failclosed", "realbundle"] for f in ARMS[k]]
if ARM not in ARMS:
    sys.stderr.write("usage: release-smoke.sh static|lint|planted|clean|failclosed|realbundle|all\n")
    sys.exit(2)
try:
    for fn in ARMS[ARM]:
        fn()
except (Setup, OSError) as ex:
    setup_die("setup error: %s" % ex)
shutil.rmtree(TMP, ignore_errors=True)
print("%d ok, %d failed" % (oks, fails))
sys.exit(1 if fails else 0)
PY
