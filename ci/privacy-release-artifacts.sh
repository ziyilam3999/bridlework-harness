#!/bin/sh
# Release artifact scan. Usage: ci/privacy-release-artifacts.sh <dist-dir>
# Scans every top-level file of the dist folder (name plus content) and every member of every .tar.gz (name plus decompressed
# content) with `harness privacy scan --text --surface release.artifact`. Same fail-closed prologue as ci/privacy-text.sh.
# Output: the engine's surface:class-id lines, `hit in item #<k>` (an index, never content), and counts on stderr.
# Exit 0 only when every item is clean and at least one artifact was scanned; 1 otherwise.
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd) || exit 1
eng="$here/../core/rust/target/release/harness"
dist="$1"
if [ "$#" -ne 1 ] || [ -z "$dist" ]; then
  echo "privacy-release-artifacts: usage: ci/privacy-release-artifacts.sh <dist-dir>" >&2
  exit 1
fi
if [ ! -x "$eng" ]; then
  echo "privacy-release-artifacts: engine missing; build it first" >&2
  exit 1
fi
if [ -z "${HARNESS_PRIVACY_LIST_SECRET+x}" ]; then
  echo "privacy-release-artifacts: list secret is not set" >&2
  exit 1
fi
case "$HARNESS_PRIVACY_LIST_SECRET" in
  *[![:space:]]*) ;;
  *) echo "privacy-release-artifacts: list secret is empty" >&2; exit 1 ;;
esac
exec python3 -I - "$eng" "$dist" <<'PY'
import os, stat, subprocess, sys, tarfile, unicodedata

eng, dist = sys.argv[1:3]
MAX_ITEMS = 5000
MAX_BYTES = 256 * 1024 * 1024      # total decompressed bytes read, all items together


def die(msg):
    sys.stderr.write("privacy-release-artifacts: %s\n" % msg)
    sys.exit(1)


def text_of(name, data):
    """name + newline + content, as UTF-8 text; non-text content is decoded lossily with controls turned into newlines."""
    try:
        if b"\0" in data:
            raise ValueError
        body = data.decode("utf-8")
    except ValueError:
        body = "".join("\n" if (unicodedata.category(c) == "Cc" and c not in "\n\t") else c
                       for c in data.decode("utf-8", errors="replace"))
    return (name + "\n" + body).encode("utf-8", errors="replace")


if os.path.islink(dist) or not os.path.isdir(dist):
    die("dist folder missing")
names = sorted(os.listdir(dist))
if not names:
    die("dist folder is empty")
items = []        # (name, data)
nart = 0
nmem = 0
total = 0
for fn in names:
    p = os.path.join(dist, fn)
    st = os.lstat(p)
    if not stat.S_ISREG(st.st_mode):
        die("dist holds a non-regular entry")
    nart += 1
    if fn.endswith(".tar.gz"):
        items.append((fn, b""))
        try:
            with tarfile.open(p, "r:gz") as tf:
                for ti in tf:
                    parts = ti.name.split("/")
                    if not ti.isreg() or ti.name.startswith("/") or ".." in parts:
                        die("archive member is not a plain relative file")
                    nmem += 1
                    if len(items) >= MAX_ITEMS:
                        die("too many items")
                    data = tf.extractfile(ti).read()
                    total += len(data)
                    if total > MAX_BYTES:
                        die("archive content too large")
                    items.append((ti.name, data))
        except (tarfile.TarError, OSError, EOFError, ValueError):
            die("archive failed to open")
    else:
        data = open(p, "rb").read()
        total += len(data)
        if total > MAX_BYTES:
            die("dist content too large")
        items.append((fn, data))
if nart < 1:
    die("nothing to scan")
rc = 0
for k, (name, data) in enumerate(items):
    r = subprocess.run([eng, "privacy", "scan", "--text", "--surface", "release.artifact"],
                       input=text_of(name, data), stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
    if r.stdout:
        sys.stdout.write(r.stdout.decode("utf-8", "replace"))
        sys.stdout.flush()
    if r.returncode != 0:
        sys.stderr.write("privacy-release-artifacts: hit in item #%d\n" % k if r.returncode == 1 else
                         "privacy-release-artifacts: engine error in item #%d\n" % k)
        rc = 1
sys.stderr.write("artifacts_scanned=%d members_scanned=%d\n" % (nart, nmem))
sys.exit(rc)
PY
