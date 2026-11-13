#!/bin/sh
# Install the plugin bundle and the engine from release files. Usage: install.sh <dist-dir> <source-archive> <prefix>
# Inputs: the release's plugin tarball + SHA256SUMS (in <dist-dir>) and the tag's source archive.
# Writes ONLY under <prefix>. Reads no config or overlay path. Uses no network: the engine builds with cargo --locked --offline.
# Re-run: a non-empty prefix that already holds exactly this install is checked, nothing is written or built, and the same three
# lines print (exit 0). Any difference refuses with "prefix holds a different install" and writes nothing.
# Exit 0 = installed; exit 1 = refused (one reason line on stderr, never a file's content).
# Prints HARNESS_BIN=<prefix>/bin/harness, plugin_root=<prefix>/plugins/claude and version=X.Y.Z.
if [ "$#" -ne 3 ] || [ -z "$1" ] || [ -z "$2" ] || [ -z "$3" ]; then
  echo "install: usage: install.sh <dist-dir> <source-archive> <prefix>" >&2
  exit 1
fi
exec python3 -I - "$1" "$2" "$3" <<'PY'
import hashlib, io, json, os, re, shutil, subprocess, sys, tarfile

dist, srcarc, prefix = sys.argv[1:4]
MAX_MEMBERS = 20000
MAX_BYTES = 512 * 1024 * 1024
PLUGIN_PREFIX = "bridlework-harness/plugins/claude/"
TARBALL_RE = re.compile(r"bridlework-harness-claude-plugin-(\d+\.\d+\.\d+)\.tar\.gz")


def die(msg):
    sys.stderr.write("install: %s\n" % msg)
    sys.exit(1)


if not os.path.isabs(prefix):
    die("prefix must be an absolute path")
rerun = False
if os.path.lexists(prefix):
    if os.path.islink(prefix) or not os.path.isdir(prefix):
        die("prefix exists and is not empty")
    rerun = bool(os.listdir(prefix))
if not os.path.isdir(dist):
    die("dist folder missing")
names = sorted(n for n in os.listdir(dist) if TARBALL_RE.fullmatch(n))
if len(names) != 1:
    die("expected exactly one plugin tarball in the dist folder")
tarname = names[0]
tarpath = os.path.join(dist, tarname)

# 1. checksum: the tarball's own line only (a downloaded release has no release-notes.md file)
sums = os.path.join(dist, "SHA256SUMS")
if not os.path.isfile(sums):
    die("SHA256SUMS missing")
want = None
with open(sums, encoding="utf-8", errors="replace") as fh:
    for line in fh:
        m = re.fullmatch(r"([0-9a-f]{64})  (.+)\n?", line)
        if m and m.group(2) == tarname:
            want = m.group(1)
if want is None:
    die("SHA256SUMS has no line for the plugin tarball")
h = hashlib.sha256()
with open(tarpath, "rb") as fh:
    for chunk in iter(lambda: fh.read(1 << 20), b""):
        h.update(chunk)
if h.hexdigest() != want:
    die("sha256 mismatch")


def safe_name(n):
    if n.startswith("/") or "\\" in n or "\0" in n:
        return False
    return ".." not in n.split("/")


def read_members(path, kind):
    """Return [(name, mode, data|None)] for regular files and folders; refuse anything else."""
    out = []
    total = 0
    try:
        tf = tarfile.open(path, mode="r:gz")
    except Exception:
        die("%s does not open" % kind)
    with tf:
        for ti in tf:
            if len(out) >= MAX_MEMBERS:
                die("%s has too many members" % kind)
            name = ti.name
            if not safe_name(name):
                die("unsafe member in %s" % kind)
            if ti.isdir():
                out.append((name.rstrip("/"), 0o755, None))
                continue
            if not ti.isreg():
                die("unsafe member in %s" % kind)
            total += ti.size
            if total > MAX_BYTES:
                die("%s is too large" % kind)
            data = tf.extractfile(ti).read()
            out.append((name, ti.mode, data))
    return out


plugin = read_members(tarpath, "plugin tarball")
files = [m for m in plugin if m[2] is not None]
if not files:
    die("plugin tarball is empty")
for name, _, _ in plugin:
    if not (name + "/").startswith(PLUGIN_PREFIX) and name not in ("bridlework-harness", "bridlework-harness/plugins"):
        die("unsafe member in plugin tarball (outside the plugin folder)")
source = read_members(srcarc, "source archive")
tops = set(n.split("/")[0] for n, _, _ in source)
if len(tops) != 1:
    die("source archive must have exactly one top folder")
top = tops.pop()


def src_data(rel):
    for n, _, d in source:
        if n == top + "/" + rel and d is not None:
            return d
    return None


# 2. versions: plugin.json vs Cargo.toml (the built binary is checked after the build)
pj = None
for n, _, d in files:
    if n == PLUGIN_PREFIX + ".claude-plugin/plugin.json":
        pj = d
try:
    pver = json.loads(pj.decode("utf-8")).get("version")
except Exception:
    die("plugin.json unreadable")
cargo = src_data("core/rust/harness/Cargo.toml")
if cargo is None:
    die("source archive has no core/rust/harness/Cargo.toml")
pk = re.search(r"^\[package\]\n(.*?)(?=^\[|\Z)", cargo.decode("utf-8", "replace"), re.M | re.S)
cv = re.search(r'^version\s*=\s*"([^"]+)"\s*$', pk.group(1) if pk else "", re.M)
if not cv or cv.group(1) != pver:
    die("version mismatch between the plugin and the source")
ver = pver


def modeof(mode):
    return 0o755 if mode & 0o111 else 0o644


# 2b. re-run over a non-empty prefix: compare only (pure reads); equal = print and exit 0, anything else = refuse
if rerun:
    MAX_SCAN = MAX_MEMBERS * 4
    want_files = {}
    for name, mode, data in files:
        want_files["plugins/claude/" + name[len(PLUGIN_PREFIX):]] = (data, modeof(mode))
    for name, mode, data in source:
        rel = name[len(top):].lstrip("/")
        if rel and data is not None:
            want_files["src/" + rel] = (data, modeof(mode))
    skip_dir = os.path.join(prefix, "src", "core", "rust", "target")
    have = set()
    for d, dirs, fs in os.walk(prefix):
        if os.path.normpath(d) == skip_dir:
            dirs[:] = []
            continue
        for x in list(dirs):
            if os.path.islink(os.path.join(d, x)):
                die("prefix holds a different install")
        dirs[:] = [x for x in dirs if os.path.join(d, x) != skip_dir]
        for f in fs:
            have.add(os.path.relpath(os.path.join(d, f), prefix))
        if len(have) > MAX_SCAN:
            die("prefix holds a different install")
    extra = have - set(want_files)
    if extra - {"bin/harness"}:
        die("prefix holds a different install")
    for rel, (data, mode) in want_files.items():
        p = os.path.join(prefix, rel)
        try:
            st = os.lstat(p)
            same = (not os.path.islink(p)) and os.path.isfile(p) and (st.st_mode & 0o7777) == mode and st.st_size == len(data)
            if same:
                with open(p, "rb") as fh:
                    same = fh.read() == data
        except OSError:
            same = False
        if not same:
            die("prefix holds a different install")
    binp = os.path.join(prefix, "bin", "harness")
    try:
        bst = os.lstat(binp)
        v = subprocess.run([binp, "--version"], stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
        okbin = (not os.path.islink(binp)) and os.path.isfile(binp) and (bst.st_mode & 0o7777) == 0o755 \
            and v.returncode == 0 and v.stdout.decode().strip() == "harness " + ver
    except OSError:
        okbin = False
    if not okbin:
        die("prefix holds a different install")
    print("HARNESS_BIN=" + binp)
    print("plugin_root=" + os.path.join(prefix, "plugins", "claude"))
    print("version=" + ver)
    sys.exit(0)


# 3. unpack: only now is anything written
def put(dest, data, mode):
    os.makedirs(os.path.dirname(dest), exist_ok=True)
    with open(dest, "wb") as fh:
        fh.write(data)
    os.chmod(dest, 0o755 if mode & 0o111 else 0o644)


os.makedirs(prefix, exist_ok=True)
for name, mode, data in files:
    put(os.path.join(prefix, "plugins", "claude", name[len(PLUGIN_PREFIX):]), data, mode)
for name, mode, data in source:
    rel = name[len(top):].lstrip("/")
    if not rel:
        continue
    if data is None:
        os.makedirs(os.path.join(prefix, "src", rel), exist_ok=True)
    else:
        put(os.path.join(prefix, "src", rel), data, mode)

# 4. build offline from the release source
crate = os.path.join(prefix, "src", "core", "rust")
r = subprocess.run(["cargo", "build", "--release", "--locked", "--offline"], cwd=crate, stdin=subprocess.DEVNULL,
                   stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
if r.returncode != 0:
    sys.stderr.write(r.stderr.decode("utf-8", "replace")[-2000:])
    die("cargo build failed")
built = os.path.join(crate, "target", "release", "harness")
os.makedirs(os.path.join(prefix, "bin"), exist_ok=True)
binp = os.path.join(prefix, "bin", "harness")
shutil.copyfile(built, binp)
os.chmod(binp, 0o755)
v = subprocess.run([binp, "--version"], stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
if v.returncode != 0 or v.stdout.decode().strip() != "harness " + ver:
    die("version mismatch: the built engine does not report the release version")
print("HARNESS_BIN=" + binp)
print("plugin_root=" + os.path.join(prefix, "plugins", "claude"))
print("version=" + ver)
PY
