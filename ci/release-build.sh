#!/bin/sh
# Release build. Usage: ci/release-build.sh <github-ref> <dist-dir>
# Packs the tracked plugins/claude bundle at HEAD into a reproducible tarball and writes release-notes.md and SHA256SUMS.
# It also packs the engine at core/rust/target/release/harness (the same binary that ran the scan and the plugin check) into a
# reproducible tarball named from the engine's own header (ELF or Mach-O), never from the host.
# Exit 0 clean, 1 on any refusal (fail closed). Prints only dist_files=4 and the file names.
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd) || exit 1
root="$here/.."
eng="$root/core/rust/target/release/harness"
ref="$1"
dist="$2"
if [ "$#" -ne 2 ] || [ -z "$ref" ] || [ -z "$dist" ]; then
  echo "release-build: usage: ci/release-build.sh <github-ref> <dist-dir>" >&2
  exit 1
fi
if [ ! -x "$eng" ]; then
  echo "release-build: engine missing; build it first" >&2
  exit 1
fi
exec python3 -I - "$root" "$eng" "$ref" "$dist" <<'PY'
import gzip, io, json, os, re, subprocess, sys, tarfile

root, eng, ref, dist = sys.argv[1:5]
MAX_FILES = 2000
MAX_ENGINE = 64 * 1024 * 1024


def die(msg):
    sys.stderr.write("release-build: %s\n" % msg)
    sys.exit(1)


def git(*a, raw=False):
    r = subprocess.run(["git", "-C", root] + list(a), stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
    if r.returncode != 0:
        die("git %s failed" % a[0])
    return r.stdout if raw else r.stdout.decode("utf-8", "replace")


m = re.fullmatch(r"refs/tags/v((?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*))", ref)
if not m:
    die("ref is not a strict semver release tag")
ver = m.group(1)
cargo = open(os.path.join(root, "core/rust/harness/Cargo.toml"), encoding="utf-8").read()
pk = re.search(r"^\[package\]\n(.*?)(?=^\[|\Z)", cargo, re.M | re.S)
cv = re.search(r'^version\s*=\s*"([^"]+)"\s*$', pk.group(1) if pk else "", re.M)
if not cv or cv.group(1) != ver:
    die("tag version differs from the harness package version")
try:
    pv = json.load(open(os.path.join(root, "plugins/claude/.claude-plugin/plugin.json"), encoding="utf-8")).get("version")
except Exception:
    die("plugin.json unreadable")
if pv != ver:
    die("tag version differs from the plugin.json version")
head = git("rev-parse", "HEAD").strip()
tagc = subprocess.run(["git", "-C", root, "rev-parse", "-q", "--verify", ref + "^{commit}"], stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
if tagc.returncode != 0 or tagc.stdout.decode().strip() != head:
    die("tag does not name the checked-out commit")


def target_of(path):
    """Name the target from the binary's own header; refuse anything unknown."""
    with open(path, "rb") as fh:
        h = fh.read(32)
    if h[:4] == b"\x7fELF" and len(h) >= 20 and h[4] == 2 and h[5] == 1:
        mach = int.from_bytes(h[18:20], "little")
        if mach == 0x3E:
            return "x86_64-unknown-linux-gnu"
        if mach == 0xB7:
            return "aarch64-unknown-linux-gnu"
    if h[:4] == b"\xcf\xfa\xed\xfe":
        cpu = int.from_bytes(h[4:8], "little")
        if cpu == 0x01000007:
            return "x86_64-apple-darwin"
        if cpu == 0x0100000C:
            return "aarch64-apple-darwin"
    die("engine is not a known target")


size = os.path.getsize(eng)
if size > MAX_ENGINE:
    die("engine is over the size bound")
triple = target_of(eng)
if subprocess.run([eng, "plugin", "check", "--repo", root], stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode != 0:
    die("plugin check failed")
if os.path.lexists(dist) and (not os.path.isdir(dist) or os.listdir(dist)):
    die("dist folder exists and is not empty")
ctime = int(git("show", "-s", "--format=%ct", "HEAD").strip())

entries = []
for line in git("ls-tree", "-r", "-z", "HEAD", "plugins/claude", raw=True).split(b"\0"):
    if not line:
        continue
    meta, _, path = line.partition(b"\t")
    mode, typ, sha = meta.decode().split()
    if typ != "blob" or mode not in ("100644", "100755"):
        die("tracked plugin entry is not a regular file")
    entries.append((path.decode("utf-8"), sha))
if not entries or len(entries) > MAX_FILES:
    die("plugin file count out of bounds")
entries.sort()

os.makedirs(dist, exist_ok=True)
name = "bridlework-harness-claude-plugin-%s.tar.gz" % ver
raw = io.BytesIO()
with tarfile.open(fileobj=raw, mode="w", format=tarfile.GNU_FORMAT) as tf:
    for path, sha in entries:
        data = git("cat-file", "blob", sha, raw=True)
        ti = tarfile.TarInfo("bridlework-harness/" + path)
        ti.size = len(data)
        ti.mtime = ctime
        ti.uid = ti.gid = 0
        ti.uname = ti.gname = ""
        ti.mode = 0o755 if path.endswith(".sh") else 0o644
        ti.type = tarfile.REGTYPE
        tf.addfile(ti, io.BytesIO(data))
with open(os.path.join(dist, name), "wb") as fh:
    with gzip.GzipFile(filename="", mode="wb", fileobj=fh, mtime=ctime, compresslevel=9) as gz:
        gz.write(raw.getvalue())
ename = "bridlework-harness-engine-%s-%s.tar.gz" % (ver, triple)
edir = "bridlework-harness-engine-%s-%s/" % (ver, triple)
edata = open(eng, "rb").read()
if len(edata) > MAX_ENGINE:
    die("engine is over the size bound")
license_blob = git("cat-file", "blob", "HEAD:LICENSE", raw=True)
eraw = io.BytesIO()
with tarfile.open(fileobj=eraw, mode="w", format=tarfile.GNU_FORMAT) as tf:
    for mname, mdata, mmode in ((edir + "harness", edata, 0o755), (edir + "LICENSE", license_blob, 0o644)):
        ti = tarfile.TarInfo(mname)
        ti.size = len(mdata)
        ti.mtime = ctime
        ti.uid = ti.gid = 0
        ti.uname = ti.gname = ""
        ti.mode = mmode
        ti.type = tarfile.REGTYPE
        tf.addfile(ti, io.BytesIO(mdata))
with open(os.path.join(dist, ename), "wb") as fh:
    with gzip.GzipFile(filename="", mode="wb", fileobj=fh, mtime=ctime, compresslevel=9) as gz:
        gz.write(eraw.getvalue())
notes = (
    "# bridlework-harness v%s\n\n"
    "Assets: %s (the plugins/claude bundle at this tag) and %s (the harness engine, built from this tag).\n\n"
    "See docs/release.md.\n" % (ver, name, ename)
)
with open(os.path.join(dist, "release-notes.md"), "w", encoding="utf-8", newline="\n") as fh:
    fh.write(notes)
import hashlib
sums = ""
for fn in sorted([name, ename, "release-notes.md"]):
    sums += "%s  %s\n" % (hashlib.sha256(open(os.path.join(dist, fn), "rb").read()).hexdigest(), fn)
with open(os.path.join(dist, "SHA256SUMS"), "w", encoding="utf-8", newline="\n") as fh:
    fh.write(sums)
print("dist_files=4")
for fn in sorted(os.listdir(dist)):
    print(fn)
PY
