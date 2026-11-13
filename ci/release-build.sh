#!/bin/sh
# Release build. Usage: ci/release-build.sh <github-ref> <dist-dir>
# Packs the tracked plugins/claude bundle at HEAD into a reproducible tarball and writes release-notes.md and SHA256SUMS.
# It builds NO shipped binary: the engine at core/rust/target/release/harness is the scanner and plugin checker only (never published).
# Exit 0 clean, 1 on any refusal (fail closed). Prints only dist_files=3 and the file names.
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
notes = (
    "# bridlework-harness v%s\n\n"
    "Asset: %s (the plugins/claude bundle at this tag).\n\n"
    "The harness engine is not shipped yet: build it from source at this tag (cargo build --release --locked in core/rust), then set HARNESS_BIN to the built binary.\n\n"
    "See docs/release.md.\n" % (ver, name)
)
with open(os.path.join(dist, "release-notes.md"), "w", encoding="utf-8", newline="\n") as fh:
    fh.write(notes)
import hashlib
sums = ""
for fn in sorted([name, "release-notes.md"]):
    sums += "%s  %s\n" % (hashlib.sha256(open(os.path.join(dist, fn), "rb").read()).hexdigest(), fn)
with open(os.path.join(dist, "SHA256SUMS"), "w", encoding="utf-8", newline="\n") as fh:
    fh.write(sums)
print("dist_files=3")
for fn in sorted(os.listdir(dist)):
    print(fn)
PY
