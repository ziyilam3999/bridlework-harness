#!/bin/sh
# Smoke for docs/install.md (ticket 3679). Usage: ci/install-guide-smoke.sh <arm>
# Arms: static run planted all (all = static, run, planted)
# static : the guide's shape (markers, ordered ids, Expected lines, one literal triple, no stray download hosts) and the README link.
# run    : builds a scratch repo from the worktree's tracked files, tags v0.1.0, builds the engine and runs ci/release-build.sh into
#          dist-0.1.0, bumps the scratch copy to 0.1.1, tags, rebuilds and makes dist-0.1.1 (plus git archive sources). Then it runs
#          the guide's own blocks in ONE `sh` (env -i, fresh empty HOME) and asserts each block's Expected: line. Only the right-hand
#          side of the BASE=, SRC_URL= and TRIPLE= lines is replaced (file:// dist paths; the triple read from the built dist).
# planted: four plants, each must turn its own step red: tampered plugin tarball (verify), a HARNESS_ overlay value (overlay),
#          a deleted marker (static), an upgrade version with no dist (upgrade-download).
# Fixture git identity and every planted value are built at run time; a home path is never typed.
# Prints "ok <id>" / "FAIL <id>" per check, then "N ok, M failed". Exit 0 only if every check holds, 1 otherwise, 2 on setup error.
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd) || exit 2
exec python3 -I - "$here/.." "$@" <<'PY'
import glob, os, re, shutil, subprocess, sys, tempfile

ROOT = os.path.realpath(sys.argv[1])
ARM = sys.argv[2] if len(sys.argv) > 2 else ""
ARMS = ("static", "run", "planted", "all")
MAX_COPY = 5000
MAX_GUIDE = 200000
IDS = ("prereqs vars download verify install overlay plugin setup intake workspace-create hook-before gate-wrong hook-wrong "
       "gate-right hook-right status upgrade-download upgrade-verify upgrade-install upgrade-switch upgrade-status").split()
DEFAULT_TRIPLE = "x86_64-unknown-linux-gnu"
OLD, NEW = "0.1.0", "0.1.1"
oks = 0
fails = 0
S = None


class Setup(Exception):
    pass


def ok(i):
    global oks
    print("ok   " + i)
    oks += 1


def bad(i, why=""):
    global fails
    print("FAIL " + i)
    if why:
        sys.stdout.write("     " + why.replace("\n", "\n     ")[:1500] + "\n")
    fails += 1


def check(i, cond, why=""):
    ok(i) if cond else bad(i, why)


def rd(p):
    with open(os.path.join(ROOT, p), encoding="utf-8") as fh:
        data = fh.read(MAX_GUIDE + 1)
    if len(data) > MAX_GUIDE:
        raise Setup(p + " is over the size bound")
    return data


# ------------------------------------------------------------------ guide parsing
FENCE_RE = re.compile(r"^```sh\n(.*?)^```\n", re.M | re.S)
STEP_RE = re.compile(r"^<!-- step: ([a-z-]+) -->\n```sh\n(.*?)^```\nExpected: ([^\n]*)\n", re.M | re.S)


def parse(text):
    return STEP_RE.findall(text), len(FENCE_RE.findall(text))


def static_checks(text, readme):
    """Returns a list of (check id, passed, why)."""
    res = []
    steps, nfences = parse(text)
    ids = [s[0] for s in steps]
    res.append(("static-every-sh-block-has-marker-and-expected", nfences == len(steps), "sh blocks=%d marked+expected=%d" % (nfences, len(steps))))
    res.append(("static-step-ids-in-order", ids == IDS, "got %s" % ids))
    blocks = dict((s[0], s[1]) for s in steps)
    res.append(("static-vars-has-literal-triple", ("TRIPLE=" + DEFAULT_TRIPLE + "\n") in blocks.get("vars", "")))
    others = [i for i, b in blocks.items() if i != "vars" and DEFAULT_TRIPLE in b]
    res.append(("static-triple-only-in-vars", not others, "also in %s" % others))
    res.append(("static-plugin-exports-five", all(("export " + v + "=") in blocks.get("plugin", "") for v in
               ("HARNESS_BIN", "HARNESS_TICKET_FILE", "HARNESS_REPO_MAP", "HARNESS_RESULT_DIR", "HARNESS_CAPS_DIR"))))
    res.append(("static-expected-nonempty", all(s[2].strip() for s in steps)))
    badnet = []
    for i, b, _ in steps:
        for line in b.split("\n"):
            if re.search(r"(^|[;&|(]\s*)(curl|wget)\b", line) and not (('"$BASE/' in line or '"$SRC_URL"' in line) and "http" not in line):
                badnet.append(i)
    res.append(("static-downloads-only-via-BASE-and-SRC_URL", not badnet, "in %s" % badnet))
    res.append(("static-repo-line", re.search(r"^REPO='?(<owner>|[A-Za-z0-9_.-]+)/bridlework-harness'?$", blocks.get("vars", ""), re.M) is not None))
    if readme is not None:
        res.append(("static-readme-links-guide", "docs/install.md" in readme))
        res.append(("static-readme-no-stale-placeholder",
                    not [l for l in readme.split("\n") if re.search(r"(examples|docs|plugins/claude)/.*placeholder", l)]))
    return res


def arm_static():
    for r in static_checks(rd("docs/install.md"), rd("README.md")):
        check(r[0], r[1], r[2] if len(r) > 2 else "")


# ------------------------------------------------------------------ setup: scratch repo, two dists, two source archives
def run(args, **kw):
    return subprocess.run(args, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, **kw)


def out(r):
    return r.stdout.decode("utf-8", "replace")


def setup():
    global S, D0, D1, SRC0, SRC1, TRIPLE
    S = tempfile.mkdtemp(prefix="install-guide-smoke-")
    r = run(["git", "-C", ROOT, "ls-files", "-z", "-c"])
    files = [f for f in out(r).split("\0") if f]
    if not files or len(files) > MAX_COPY:
        raise Setup("tracked file count out of bounds")
    repo = os.path.join(S, "repo")
    for f in files:
        src = os.path.join(ROOT, f)
        if not os.path.isfile(src):
            continue
        dst = os.path.join(repo, f)
        os.makedirs(os.path.dirname(dst), exist_ok=True)
        shutil.copy2(src, dst)
    ctoml = os.path.join(repo, "core/rust/harness/Cargo.toml")
    clock = os.path.join(repo, "core/rust/Cargo.lock")
    pjson = os.path.join(repo, "plugins/claude/.claude-plugin/plugin.json")
    cargo = open(ctoml, encoding="utf-8").read()
    ver = re.search(r'^version\s*=\s*"([^"]+)"', cargo, re.M).group(1)
    if ver != OLD:
        raise Setup("the guide's example versions are %s -> %s but Cargo.toml says %s" % (OLD, NEW, ver))
    dom = "example"
    dom += ".invalid"
    env = dict(os.environ, GIT_AUTHOR_NAME="smoke", GIT_AUTHOR_EMAIL="smoke@" + dom,
               GIT_COMMITTER_NAME="smoke", GIT_COMMITTER_EMAIL="smoke@" + dom)
    g = ["git", "-C", repo, "-c", "core.hooksPath=/dev/null", "-c", "commit.gpgsign=false", "-c", "tag.gpgsign=false"]

    def git(*a):
        r = run(g + list(a), env=env)
        if r.returncode != 0:
            raise Setup("scratch git %s failed: %s" % (a[0], out(r)[-300:]))

    def build(v, dist, srcarc):
        r = run(["cargo", "build", "--release", "--locked"], cwd=os.path.join(repo, "core/rust"))
        if r.returncode != 0:
            raise Setup("engine build failed:\n" + out(r)[-800:])
        r = run(["sh", "ci/release-build.sh", "refs/tags/v" + v, dist], cwd=repo)
        if r.returncode != 0:
            raise Setup("release-build failed:\n" + out(r)[-800:])
        git("archive", "--format=tar.gz", "--prefix=bridlework-harness-%s/" % v, "-o", srcarc, "v" + v)

    D0, D1 = os.path.join(S, "dist-" + OLD), os.path.join(S, "dist-" + NEW)
    SRC0, SRC1 = os.path.join(S, "src-%s.tar.gz" % OLD), os.path.join(S, "src-%s.tar.gz" % NEW)
    git("init", "-q", "-b", "master")
    git("add", "-A")
    git("commit", "-q", "-m", "scratch")
    git("tag", "v" + OLD)
    build(OLD, D0, SRC0)
    # bump the scratch copy only: Cargo.toml, the harness entry of Cargo.lock (--locked), then plugin.json from the new engine
    open(ctoml, "w", encoding="utf-8").write(cargo.replace('version = "%s"' % OLD, 'version = "%s"' % NEW, 1))
    lock = open(clock, encoding="utf-8").read()
    lock2 = re.sub(r'(name = "harness"\nversion = ")%s(")' % re.escape(OLD), r"\g<1>%s\g<2>" % NEW, lock, count=1)
    if lock2 == lock:
        raise Setup("Cargo.lock has no harness entry at " + OLD)
    open(clock, "w", encoding="utf-8").write(lock2)
    r = run(["cargo", "build", "--release", "--locked"], cwd=os.path.join(repo, "core/rust"))
    if r.returncode != 0:
        raise Setup("engine rebuild failed:\n" + out(r)[-800:])
    pj = run([os.path.join(repo, "core/rust/target/release/harness"), "plugin", "plugin-json"])
    if pj.returncode != 0:
        raise Setup("plugin-json failed")
    open(pjson, "wb").write(pj.stdout)
    git("add", "-A")
    git("commit", "-q", "-m", "bump")
    git("tag", "v" + NEW)
    build(NEW, D1, SRC1)
    m = glob.glob(os.path.join(D0, "bridlework-harness-engine-%s-*.tar.gz" % OLD))
    if len(m) != 1:
        raise Setup("expected exactly one engine asset in dist-" + OLD)
    TRIPLE = os.path.basename(m[0])[len("bridlework-harness-engine-%s-" % OLD):-len(".tar.gz")]
    for d in (D0, D1):
        os.chmod(d, 0o755)
        for f in os.listdir(d):
            os.chmod(os.path.join(d, f), 0o644)
    for f in (SRC0, SRC1):
        os.chmod(f, 0o644)


# ------------------------------------------------------------------ run the guide's blocks in one shell
def tool_path():
    dirs = ["/usr/bin", "/bin", "/usr/sbin", "/sbin"]
    for t in ("curl", "sha256sum", "tar", "git", "bash", "python3", "cargo"):
        p = shutil.which(t)
        if p and os.path.dirname(p) not in dirs:
            dirs.append(os.path.dirname(p))
    return ":".join(dirs)


COUNTER = [0]


def override(step, block, dist, srcarc, triple, mutate):
    lines = []
    for line in block.split("\n"):
        for var, val in (("BASE", "file://" + dist), ("SRC_URL", "file://" + srcarc), ("TRIPLE", triple)):
            if line.startswith(var + "="):
                line = var + "=" + val
        lines.append(line)
    b = "\n".join(lines)
    return mutate(step, b) if mutate else b


def run_guide(guide, only=None, extra_env=None, dists=None, mutate=None):
    """Run the guide's blocks (in order, optionally only the ids in `only`) in ONE sh. Returns [(id, passed, why)]."""
    COUNTER[0] += 1
    d = os.path.join(S, "run%d" % COUNTER[0])
    home = os.path.join(d, "h")
    os.makedirs(home)
    os.makedirs(os.path.join(d, "cargo-home"))
    os.makedirs(os.path.join(d, "rustup-home"))
    d0, s0, d1, s1 = dists or (D0, SRC0, D1, SRC1)
    steps, _ = parse(guide)
    drv = []
    sel = []
    for n, (i, block, exp) in enumerate(steps):
        if only is not None and i not in only:
            continue
        if i.startswith("upgrade"):
            blk = override(i, block, d1, s1, TRIPLE, mutate)
        else:
            blk = override(i, block, d0, s0, TRIPLE, mutate)
        with open(os.path.join(d, "step%d.sh" % n), "w", encoding="utf-8") as fh:
            fh.write(blk)
        drv.append('. "%s/step%d.sh" > "%s/out%d" 2>&1\necho $? > "%s/rc%d"' % (d, n, d, n, d, n))
        sel.append((n, i, exp))
    with open(os.path.join(d, "driver.sh"), "w", encoding="utf-8") as fh:
        fh.write("\n".join(drv) + "\n")
    env = {"HOME": home, "PATH": tool_path(), "CARGO_HOME": os.path.join(d, "cargo-home"), "RUSTUP_HOME": os.path.join(d, "rustup-home")}
    if shutil.which("rustup") and os.environ.get("RUSTUP_HOME"):
        env["RUSTUP_HOME"] = os.environ["RUSTUP_HOME"]
    if extra_env:
        env.update(extra_env)
    args = ["env", "-i"] + ["%s=%s" % kv for kv in env.items()] + ["sh", os.path.join(d, "driver.sh")]
    run(args, cwd=d)
    res = []
    for n, i, exp in sel:
        try:
            rc = int(open(os.path.join(d, "rc%d" % n)).read().strip())
            text = open(os.path.join(d, "out%d" % n), encoding="utf-8", errors="replace").read()
        except OSError:
            res.append((i, False, "step did not finish (the shell ended early)"))
            continue
        okk, why = expect(exp, rc, text)
        res.append((i, okk, why + "\n" + text[-600:] if not okk else ""))
    return res


def expect(exp, rc, text):
    exp = exp.replace(DEFAULT_TRIPLE, TRIPLE)
    m = re.search(r"\bexit (\d+)", exp)
    want = int(m.group(1)) if m else 0
    if rc != want:
        return False, "exit %d, expected %d" % (rc, want)
    lines = [l.strip() for l in text.split("\n") if l.strip()]
    m = re.search(r"first output line is `([^`]+)`", exp)
    if m and (not lines or lines[0] != m.group(1)):
        return False, "first line is not %r" % m.group(1)
    for tok, n in re.findall(r"`([^`]+)`(?: ×(\d+))?", exp):
        if n:
            if sum(1 for l in lines if tok in l) != int(n):
                return False, "expected %s lines with %r" % (n, tok)
        elif tok.isdigit():
            if tok not in lines:
                return False, "no output line equal to %s" % tok
        elif tok not in text:
            return False, "output lacks %r" % tok
    return True, ""


# ------------------------------------------------------------------ run
def arm_run():
    res = run_guide(rd("docs/install.md"))
    for i, passed, why in res:
        check(i, passed, why)
    check("run-all-steps-ran", [r[0] for r in res] == IDS, "ran %s" % [r[0] for r in res])


# ------------------------------------------------------------------ planted
def arm_planted():
    guide = rd("docs/install.md")
    # (a) one flipped byte in the plugin tarball: verify must go red
    d = os.path.join(S, "tampered")
    shutil.copytree(D0, d)
    p = glob.glob(os.path.join(d, "bridlework-harness-claude-plugin-*.tar.gz"))[0]
    b = bytearray(open(p, "rb").read())
    b[len(b) // 2] ^= 0xFF
    open(p, "wb").write(bytes(b))
    res = dict((i, c) for i, c, _ in run_guide(guide, only=("prereqs", "vars", "download", "verify"), dists=(d, SRC0, D1, SRC1)))
    check("planted-a-tampered-tarball-fails-verify", res.get("download") and res.get("verify") is False, "results %s" % res)
    # (b) a HARNESS_ value at overlay time: overlay must go red (the real output, no special case)
    val = "plan" + "ted"
    res = dict((i, c) for i, c, _ in run_guide(guide, only=("prereqs", "vars", "download", "verify", "install", "overlay"),
                                               extra_env={"HARNESS_PRIVACY_LIST": val}))
    check("planted-b-harness-overlay-fails-overlay", res.get("install") and res.get("overlay") is False, "results %s" % res)
    # (c) a deleted marker: static must go red
    broken = guide.replace("<!-- step: verify -->\n", "", 1)
    sres = static_checks(broken, None)
    check("planted-c-deleted-marker-fails-static", broken != guide and any(not r[1] for r in sres), "static stayed green")
    # (d) an upgrade version with no dist: download must go red (curl -f)
    res = dict((i, c) for i, c, _ in run_guide(guide, only=("vars", "download", "upgrade-download"),
                                               mutate=lambda s, b: b.replace("VERSION=" + NEW, "VERSION=9.9.9", 1) if s == "upgrade-download" else b))
    check("planted-d-missing-dist-fails-download", res.get("download") and res.get("upgrade-download") is False, "results %s" % res)


def main():
    global S
    if ARM not in ARMS:
        sys.stderr.write("usage: install-guide-smoke.sh <static|run|planted|all>\n")
        return 2
    try:
        if ARM in ("static", "all"):
            arm_static()
        if ARM in ("run", "planted", "all"):
            setup()
            if ARM in ("run", "all"):
                arm_run()
            if ARM in ("planted", "all"):
                arm_planted()
    except Setup as e:
        sys.stderr.write("setup error: %s\n" % e)
        return 2
    finally:
        if S:
            shutil.rmtree(S, ignore_errors=True)
    print("%d ok, %d failed" % (oks, fails))
    return 0 if fails == 0 else 1


sys.exit(main())
PY
