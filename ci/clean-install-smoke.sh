#!/bin/sh
# Smoke for the clean-machine install test (ticket 3677). Usage: ci/clean-install-smoke.sh <arm>
# Arms: static fresh container planted all (all = static, fresh, container, planted)
# The driver builds a scratch repo from the worktree's tracked files (index), tags it, builds the engine, then runs #3676's own
# ci/release-build.sh to make the dist folder and `git archive` for the source. The `fresh` arm and every plant that runs the real
# install use `env -i` with a fresh HOME plus exactly two non-HARNESS_ variables, CARGO_HOME and RUSTUP_HOME, both outside HOME.
# There is no plant switch in install.sh or e2e.sh: plants mutate inputs or run the phases one by one.
# Fixture git identity and every planted value are built at run time; a home path is never typed.
# Prints "ok <id>" / "FAIL <id>" per check, then "N ok, M failed". Exit 0 only if every check holds, 1 otherwise, 2 on setup error.
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd) || exit 2
exec python3 -I - "$here/.." "$@" <<'PY'
import hashlib, io, json, os, re, shutil, subprocess, sys, tarfile, tempfile

ROOT = os.path.realpath(sys.argv[1])
ARM = sys.argv[2] if len(sys.argv) > 2 else ""
ARMS = ("static", "fresh", "container", "planted", "all")
MAX_COPY = 5000
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
    return open(os.path.join(ROOT, p), encoding="utf-8").read()


def code_only(text):
    return "\n".join(l for l in text.split("\n") if not l.lstrip().startswith("#"))


# ------------------------------------------------------------------ static
def arm_static():
    for f in ("ci/clean-install/install.sh", "ci/clean-install/e2e.sh"):
        t = code_only(rd(f))
        bad_words = [w for w in ("curl", "wget", "git clone", "gh ") if w in t]
        check("static-no-network-" + os.path.basename(f), not bad_words, "found " + ",".join(bad_words))
        check("static-no-overlay-assign-" + os.path.basename(f), not re.search(r"HARNESS_PRIVACY\w*\s*=", t))
    inst = rd("ci/clean-install/install.sh")
    cb = [l for l in inst.split("\n") if '"cargo", "build"' in l]
    check("static-cargo-locked-offline", len(cb) == 1 and '"--locked"' in cb[0] and '"--offline"' in cb[0])
    ct = code_only(rd("ci/clean-install-container.sh"))
    check("static-container-network-none", "--network none" in ct)
    check("static-container-user", re.search(r"--user \S+", ct) is not None)
    check("static-container-digest", re.search(r"^FROM \S+@sha256:[0-9a-f]{64}$", rd("ci/clean-install/Dockerfile"), re.M) is not None
          and re.search(r"IMAGE=\"[^\"]+\"", ct) is not None)
    mounts = re.findall(r'-v\s+"?[^\s]+:[^\s]+:ro', ct)
    check("static-container-three-ro-mounts", len(mounts) == 3 and len(re.findall(r"\s-v\s", ct)) == 4, "mounts=%d" % len(mounts))
    check("static-container-no-repo-mount", "target" not in ct and '"$root' not in ct and "$here/.." not in ct and '-v "$here":' not in ct)
    tk = json.loads(rd("examples/project-template/tickets/ex-1.json"))
    check("static-ticket-id", re.fullmatch(r"[a-z0-9._-]+", tk["id"]) is not None)
    pm = json.loads(rd("examples/project-template/project/gates/path-map.json"))
    text = json.dumps(pm)
    tops = os.listdir(os.path.join(ROOT, "examples/project-template/project"))
    check("static-pathmap-covers-project", all(t in text for t in tops), "tops=%s" % tops)
    r = subprocess.run(["git", "-C", ROOT, "ls-files", "-s", "examples/project-template/project/checks/hello.sh"],
                       stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
    check("static-hello-mode-100755", r.stdout.decode().startswith("100755 "))
    wf = rd(".github/workflows/clean-install.yml")
    check("static-job-named-clean-install", re.search(r"^  clean-install:\s*$", wf, re.M) is not None
          and "final-gate" not in code_only(wf) and not re.search(r"^  gate:", wf, re.M))


# ------------------------------------------------------------------ setup: scratch repo, release dist, source archive
def run(args, **kw):
    return subprocess.run(args, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, **kw)


def setup():
    global S, DIST, SRC, VER
    S = tempfile.mkdtemp(prefix="clean-install-smoke-")
    r = run(["git", "-C", ROOT, "ls-files", "-z", "-c"])
    files = [f for f in r.stdout.decode("utf-8", "replace").split("\0") if f]
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
    cargo = open(os.path.join(repo, "core/rust/harness/Cargo.toml"), encoding="utf-8").read()
    VER = re.search(r'^version\s*=\s*"([^"]+)"', cargo, re.M).group(1)
    dom = "example"
    dom += ".invalid"
    env = dict(os.environ, GIT_AUTHOR_NAME="smoke", GIT_AUTHOR_EMAIL="smoke@" + dom,
               GIT_COMMITTER_NAME="smoke", GIT_COMMITTER_EMAIL="smoke@" + dom)
    g = ["git", "-C", repo, "-c", "core.hooksPath=/dev/null", "-c", "commit.gpgsign=false", "-c", "tag.gpgsign=false"]
    for a in (["init", "-q", "-b", "master"], ["add", "-A"], ["commit", "-q", "-m", "scratch"], ["tag", "v" + VER]):
        r = run(g + a, env=env)
        if r.returncode != 0:
            raise Setup("scratch git %s failed" % a[0])
    r = run(["cargo", "build", "--release", "--locked"], cwd=os.path.join(repo, "core/rust"))
    if r.returncode != 0:
        raise Setup("engine build failed:\n" + r.stdout.decode("utf-8", "replace")[-800:])
    DIST = os.path.join(S, "dist")
    r = run(["sh", "ci/release-build.sh", "refs/tags/v" + VER, DIST], cwd=repo)
    if r.returncode != 0 or "dist_files=3" not in r.stdout.decode():
        raise Setup("release-build failed:\n" + r.stdout.decode("utf-8", "replace")[-800:])
    SRC = os.path.join(S, "src.tar.gz")
    r = run(g + ["archive", "--format=tar.gz", "--prefix=bridlework-harness-%s/" % VER, "-o", SRC, "v" + VER])
    if r.returncode != 0:
        raise Setup("git archive failed")
    os.chmod(DIST, 0o755)
    for f in os.listdir(DIST):
        os.chmod(os.path.join(DIST, f), 0o644)
    os.chmod(SRC, 0o644)


def toolchain_env(tag, drop=(), override=None):
    """The fresh env: a new empty HOME, CARGO_HOME and RUSTUP_HOME as siblings of it (never inside)."""
    base = os.path.join(S, tag)
    home = os.path.join(base, "h")
    os.makedirs(home)
    cargo = shutil.which("cargo")
    if not cargo:
        raise Setup("cargo not on PATH")
    ch = os.path.join(base, "cargo-home")
    if shutil.which("rustup"):
        rh = os.environ.get("RUSTUP_HOME") or os.path.join(os.path.expanduser("~"), ".rustup")
    else:
        rh = os.path.join(base, "rustup-home")
        os.makedirs(rh)
    os.makedirs(ch)
    d = {"HOME": home, "CARGO_HOME": ch, "RUSTUP_HOME": rh,
         "PATH": "/usr/bin:/bin:" + os.path.dirname(os.path.realpath(cargo)) if False else "/usr/bin:/bin:" + os.path.dirname(cargo)}
    dom = "example"
    dom += ".invalid"
    d.update(GIT_AUTHOR_NAME="smoke", GIT_AUTHOR_EMAIL="smoke@" + dom, GIT_COMMITTER_NAME="smoke", GIT_COMMITTER_EMAIL="smoke@" + dom)
    for k in drop:
        d.pop(k, None)
    if override:
        d.update(override)
    return d


def e2e(env, phase, dist=None, src=None):
    args = ["env", "-i"] + ["%s=%s" % kv for kv in env.items()] + ["sh", os.path.join(ROOT, "ci/clean-install/e2e.sh"),
                                                                    phase, dist or DIST, src or SRC]
    return run(args)


def out(r):
    return r.stdout.decode("utf-8", "replace")


def top(home):
    return sorted(os.listdir(home))


# ------------------------------------------------------------------ fresh
def arm_fresh():
    env = toolchain_env("fresh")
    r = e2e(env, "all")
    t = out(r)
    m = re.search(r"^clean-install: ok ticket=ex-1 version=%s core_files=\d+ unchanged$" % re.escape(VER), t, re.M)
    check("fresh-e2e", r.returncode == 0 and m is not None, "rc=%d\n%s" % (r.returncode, t[-1200:]))
    if m:
        print("     " + m.group(0))
    check("fresh-home-clean", top(env["HOME"]) == ["bridlework", "work"], "HOME top level %s" % top(env["HOME"]))


# ------------------------------------------------------------------ container
def arm_container():
    if not shutil.which("docker"):
        raise Setup("docker missing")
    r = run(["sh", os.path.join(ROOT, "ci/clean-install-container.sh"), DIST, SRC])
    t = out(r)
    if r.returncode == 2 and "docker" in t:
        raise Setup(t.strip())
    m = re.search(r"^clean-install: ok ticket=ex-1 version=%s core_files=(\d+) unchanged$" % re.escape(VER), t, re.M)
    check("container-e2e", r.returncode == 0 and m is not None, "rc=%d\n%s" % (r.returncode, t[-1500:]))
    if not m:
        return
    print("     " + m.group(0))
    with tarfile.open(SRC, "r:gz") as tf:
        nsrc = sum(1 for ti in tf if ti.isreg())
    check("container-core-files-count", int(m.group(1)) >= 15 + nsrc, "n=%s src=%d" % (m.group(1), nsrc))
    steps = ["step hook-before exit=2", "step gate-wrong exit=1 result=fail", "step hook-wrong exit=2",
             "step gate-right exit=0 done=allowed", "step hook-right exit=0", "step status last_gate=pass gate_current=yes"]
    pos = -1
    inorder = True
    for s in steps:
        p = t.find(s, pos + 1)
        if p < 0:
            inorder = False
            break
        pos = p
    check("container-steps-in-order", inorder)
    mounts = re.findall(r"^mount_source=(.*)$", t, re.M)
    check("container-three-mounts-no-repo", len(mounts) == 3 and all(os.path.realpath(x) != ROOT and "target" not in x for x in mounts),
          "mounts=%s" % mounts)


# ------------------------------------------------------------------ planted
def repack(dest, mutate, rename=None):
    """Copy DIST to dest with the plugin tarball rebuilt through mutate(members)->members; SHA256SUMS matched."""
    shutil.copytree(DIST, dest)
    name = [n for n in os.listdir(dest) if n.endswith(".tar.gz")][0]
    p = os.path.join(dest, name)
    members = []
    with tarfile.open(p, "r:gz") as tf:
        for ti in tf:
            members.append((ti, tf.extractfile(ti).read() if ti.isreg() else None))
    members = mutate(members)
    raw = io.BytesIO()
    with tarfile.open(fileobj=raw, mode="w", format=tarfile.GNU_FORMAT) as tf:
        for ti, data in members:
            tf.addfile(ti, io.BytesIO(data) if data is not None else None)
    import gzip
    with open(p, "wb") as fh:
        with gzip.GzipFile(filename="", mode="wb", fileobj=fh, mtime=0) as gz:
            gz.write(raw.getvalue())
    h = hashlib.sha256(open(p, "rb").read()).hexdigest()
    lines = open(os.path.join(dest, "SHA256SUMS")).read().splitlines()
    lines = ["%s  %s" % (h, name) if l.endswith("  " + name) else l for l in lines]
    open(os.path.join(dest, "SHA256SUMS"), "w").write("\n".join(lines) + "\n")
    return dest


def refused(i, r, reason, home=None, extra_absent=()):
    t = out(r)
    good = r.returncode != 0 and reason in t and "clean-install: ok" not in t and not any(x in t for x in extra_absent)
    check(i, good, "rc=%d want %r\n%s" % (r.returncode, reason, t[-800:]))


def arm_planted():
    # (a) one byte of the tarball flipped
    d = os.path.join(S, "pa")
    shutil.copytree(DIST, d)
    tarp = os.path.join(d, [n for n in os.listdir(d) if n.endswith(".tar.gz")][0])
    b = bytearray(open(tarp, "rb").read())
    b[len(b) // 2] ^= 0xFF
    open(tarp, "wb").write(bytes(b))
    refused("planted-a-sha256-mismatch", e2e(toolchain_env("a"), "all", dist=d), "sha256 mismatch")

    # (b) ../x member, (c) symlink member
    def add_dotdot(ms):
        ti = tarfile.TarInfo("bridlework-harness/plugins/claude/../x")
        ti.size = 1
        return ms + [(ti, b"x")]

    def add_link(ms):
        ti = tarfile.TarInfo("bridlework-harness/plugins/claude/lnk")
        ti.type = tarfile.SYMTYPE
        ti.linkname = "/etc/hostname"
        return ms + [(ti, None)]

    refused("planted-b-dotdot-member", e2e(toolchain_env("b"), "all", dist=repack(os.path.join(S, "pb"), add_dotdot)), "unsafe member")
    refused("planted-c-symlink-member", e2e(toolchain_env("c"), "all", dist=repack(os.path.join(S, "pc"), add_link)), "unsafe member")

    # (d) HOME pre-seeded with one empty file
    env = toolchain_env("d")
    open(os.path.join(env["HOME"], "stray"), "w").close()
    refused("planted-d-home-not-empty", e2e(env, "all"), "not fresh")

    # (e) the overlay present
    env = toolchain_env("e")
    lst = os.path.join(S, "e-list")
    open(lst, "w").write("sentinel\n")
    env["HARNESS_PRIVACY_LIST"] = lst
    refused("planted-e-overlay-present", e2e(env, "all"), "not fresh", extra_absent=(lst,))

    # (f) hook edited after the ticket, (g) engine source edited after the ticket
    for tag, rel, i in (("f", "plugins/claude/pre-completion-hook.sh", "planted-f-hook-edited"),
                        ("g", "src/core/rust/harness/src/lib.rs", "planted-g-source-edited")):
        env = toolchain_env(tag)
        rcs = [e2e(env, ph).returncode for ph in ("preflight", "install", "snapshot", "ticket")]
        if rcs != [0, 0, 0, 0]:
            bad(i, "setup phases rc=%s" % rcs)
            continue
        target = os.path.join(env["HOME"], "bridlework", rel)
        os.chmod(target, 0o644)
        with open(target, "ab") as fh:
            fh.write(b"\n")
        refused(i, e2e(env, "verify"), "core edited: " + rel)

    # (h) a template whose check always passes: the real both-ends gate must refuse
    env = toolchain_env("h")
    rcs = [e2e(env, ph).returncode for ph in ("preflight", "install")]
    chk = os.path.join(env["HOME"], "bridlework", "src", "examples", "project-template", "project", "checks", "hello.sh")
    if rcs != [0, 0]:
        bad("planted-h-vacuous-check", "setup phases rc=%s" % rcs)
    else:
        open(chk, "w").write("#!/bin/sh\nexit 0\n")
        os.chmod(chk, 0o755)
        e2e(env, "snapshot")
        r = e2e(env, "ticket")
        t = out(r)
        check("planted-h-vacuous-check", r.returncode == 1 and t.startswith("step ") and "ticket: gate-" in t and "clean-install: ok" not in t,
              "rc=%d\n%s" % (r.returncode, t[-800:]))
        print("     " + [l for l in t.split("\n") if l.startswith("ticket:")][0][:300] if "ticket:" in t else "")

    # (i) plugin.json version differs from the source Cargo version
    def bump(ms):
        res = []
        for ti, data in ms:
            if ti.name.endswith(".claude-plugin/plugin.json"):
                j = json.loads(data.decode())
                j["version"] = "9.9.9"
                data = json.dumps(j).encode()
                ti.size = len(data)
            res.append((ti, data))
        return res

    refused("planted-i-version-mismatch", e2e(toolchain_env("i"), "all", dist=repack(os.path.join(S, "pi"), bump)), "version mismatch")

    # (j) the toolchain variables cannot smuggle a HOME write past verify
    env = toolchain_env("j1")
    env["CARGO_HOME"] = os.path.join(env["HOME"], ".cargo")
    refused("planted-j-cargo-home-inside", e2e(env, "preflight"), "not fresh: CARGO_HOME inside HOME")
    env = toolchain_env("j2")
    env["RUSTUP_HOME"] = os.path.join(env["HOME"], ".rustup")
    refused("planted-j-rustup-home-inside", e2e(env, "preflight"), "not fresh: RUSTUP_HOME inside HOME")
    env = toolchain_env("j3", drop=("CARGO_HOME",))
    refused("planted-j-cargo-home-unset", e2e(env, "preflight"), "not fresh: CARGO_HOME unset")


if ARM not in ARMS:
    sys.stderr.write("clean-install-smoke: usage: ci/clean-install-smoke.sh <static|fresh|container|planted|all>\n")
    sys.exit(2)
rc = 0
try:
    if ARM in ("static", "all"):
        arm_static()
    if ARM != "static":
        setup()
    if ARM in ("fresh", "all"):
        arm_fresh()
    if ARM in ("container", "all"):
        arm_container()
    if ARM in ("planted", "all"):
        arm_planted()
except Setup as e:
    sys.stderr.write("clean-install-smoke: setup error: %s\n" % e)
    rc = 2
finally:
    if S:
        shutil.rmtree(S, ignore_errors=True)
if rc == 0:
    print("%d ok, %d failed" % (oks, fails))
    rc = 0 if fails == 0 else 1
sys.exit(rc)
PY
