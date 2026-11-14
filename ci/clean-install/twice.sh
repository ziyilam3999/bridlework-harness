#!/bin/sh
# Install twice into one prefix beside a seeded private overlay. Usage: twice.sh <phase> <dist-dir> <source-archive>
# Phases: seed first snap second check all (the five in order). dist-dir and source-archive are absolute paths.
# Prefix $HOME/bridlework, overlay $HOME/overlay, work folder $HOME/work. HOME must start empty.
# The only HARNESS_ variables allowed are HARNESS_CONFIG and HARNESS_PRIVACY_LIST, absent or pointing at the overlay files.
# The installer must leave every overlay file (bytes, mode, size, inode, mtime) and the overlay folder's mtime alone, and a second
# install over the same prefix must change nothing. No plant switch: the smoke plants by mutating inputs or running phases one by one.
# Exit 0 ok, 1 on any refusal or mismatch (one reason line on stderr), 2 on usage.
phase="$1"
dist="$2"
srcarc="$3"
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd) || exit 2
case "$phase" in
  seed | first | snap | second | check | all) ;;
  *) echo "twice: usage: twice.sh <seed|first|snap|second|check|all> <dist-dir> <source-archive>" >&2; exit 2 ;;
esac
case "$dist" in /*) ;; *) echo "twice: dist-dir must be absolute" >&2; exit 2 ;; esac
case "$srcarc" in /*) ;; *) echo "twice: source-archive must be absolute" >&2; exit 2 ;; esac

fail() {
  echo "$1" >&2
  exit 1
}

do_seed() {
  [ -n "${HOME:-}" ] && [ -d "$HOME" ] || fail "not fresh: HOME is not a folder"
  [ -z "$(ls -A "$HOME")" ] || fail "not fresh: HOME is not empty"
  bad=$(env | sed -n 's/^\(HARNESS_[A-Za-z0-9_]*\)=.*/\1/p' | grep -v -x -e HARNESS_CONFIG -e HARNESS_PRIVACY_LIST | head -n 1)
  [ -z "$bad" ] || fail "not fresh: $bad is set"
  if [ -n "${HARNESS_CONFIG+x}" ] && [ "$HARNESS_CONFIG" != "$HOME/overlay/harness.yaml" ]; then
    fail "not fresh: HARNESS_CONFIG is set"
  fi
  if [ -n "${HARNESS_PRIVACY_LIST+x}" ] && [ "$HARNESS_PRIVACY_LIST" != "$HOME/overlay/privacy-list.txt" ]; then
    fail "not fresh: HARNESS_PRIVACY_LIST is set"
  fi
  python3 -I - "$srcarc" <<'PY' || exit 1
import os, re, shutil, sys, tarfile

home = os.path.realpath(os.environ["HOME"])
for var in ("CARGO_HOME", "RUSTUP_HOME"):
    v = os.environ.get(var)
    if v is None or v == "":
        if var == "CARGO_HOME" and shutil.which("cargo"):
            sys.stderr.write("not fresh: CARGO_HOME unset (cargo would default into HOME)\n")
            sys.exit(1)
        continue
    if not os.path.isabs(v):
        sys.stderr.write("not fresh: %s is not absolute\n" % var)
        sys.exit(1)
    r = os.path.realpath(v)
    if r == home or r.startswith(home + "/"):
        sys.stderr.write("not fresh: %s inside HOME\n" % var)
        sys.exit(1)
cfg = None
with tarfile.open(sys.argv[1], "r:gz") as tf:
    for ti in tf:
        if ti.isreg() and re.fullmatch(r"[^/]+/config/harness\.example\.yaml", ti.name):
            cfg = tf.extractfile(ti).read()
if cfg is None:
    sys.stderr.write("seed: the source archive has no config/harness.example.yaml\n")
    sys.exit(1)
ov = os.path.join(os.environ["HOME"], "overlay")
os.mkdir(ov, 0o700)
rows = {"harness.yaml": cfg,
        "privacy-list.txt": ("zz" + "overlay" + "sentinel" + "\n").encode(),
        "friction-inbox.jsonl": b"{}\n"}
for name, data in rows.items():
    p = os.path.join(ov, name)
    with open(p, "wb") as fh:
        fh.write(data)
    os.chmod(p, 0o600)
    os.utime(p, ns=(10**18, 10**18))
os.chmod(ov, 0o700)
os.utime(ov, ns=(10**18, 10**18))
print("seed ok")
PY
}

do_first() {
  mkdir -p "$HOME/work" || exit 1
  sh "$here/install.sh" "$dist" "$srcarc" "$HOME/bridlework" > "$HOME/work/first.out" || exit 1
}

do_snap() {
  sh "$here/e2e.sh" snapshot "$dist" "$srcarc" || exit 1
  python3 -I - "$HOME" "$HOME/work/twice.snap" <<'PY' || exit 1
import hashlib, json, os, sys

home, out = sys.argv[1:3]
ov = os.path.join(home, "overlay")
prefix = os.path.join(home, "bridlework")
snap = {"overlay": {}, "prefix": {}}
st = os.lstat(ov)
snap["overlay"]["."] = [None, st.st_mode & 0o7777, None, st.st_ino, st.st_mtime_ns]
for d, dirs, fs in os.walk(ov):
    for f in fs:
        p = os.path.join(d, f)
        s = os.lstat(p)
        snap["overlay"][os.path.relpath(p, ov)] = [hashlib.sha256(open(p, "rb").read()).hexdigest(), s.st_mode & 0o7777,
                                                   s.st_size, s.st_ino, s.st_mtime_ns]
for d, dirs, fs in os.walk(prefix):
    for f in fs:
        p = os.path.join(d, f)
        snap["prefix"][os.path.relpath(p, prefix)] = os.lstat(p).st_mtime_ns
with open(out, "w", encoding="utf-8") as fh:
    json.dump(snap, fh)
PY
}

do_second() {
  sh "$here/install.sh" "$dist" "$srcarc" "$HOME/bridlework" > "$HOME/work/second.out" || exit 1
}

do_check() {
  python3 -I - "$HOME" <<'PY' || exit 1
import hashlib, json, os, re, subprocess, sys

home = sys.argv[1]
ov = os.path.join(home, "overlay")
prefix = os.path.join(home, "bridlework")
work = os.path.join(home, "work")


def die(m):
    sys.stderr.write(m + "\n")
    sys.exit(1)


try:
    snap = json.load(open(os.path.join(work, "twice.snap"), encoding="utf-8"))
except (OSError, ValueError):
    die("check: no snapshot (run the snap phase first)")

# (a) overlay
now = {}
if not os.path.isdir(ov):
    die("overlay changed: .")
s = os.lstat(ov)
now["."] = [None, s.st_mode & 0o7777, None, s.st_ino, s.st_mtime_ns]
for d, dirs, fs in os.walk(ov):
    for f in fs:
        p = os.path.join(d, f)
        s = os.lstat(p)
        now[os.path.relpath(p, ov)] = [hashlib.sha256(open(p, "rb").read()).hexdigest(), s.st_mode & 0o7777, s.st_size, s.st_ino,
                                       s.st_mtime_ns]
for rel in sorted(set(now) | set(snap["overlay"])):
    if now.get(rel) != snap["overlay"].get(rel):
        die("overlay changed: " + rel)
nov = len(now) - 1

# (b) idempotent: sha256+mode+file set equal core.snap, mtimes equal twice.snap
core = {}
for line in open(os.path.join(work, "core.snap"), encoding="utf-8").read().splitlines():
    h, m, rel = line.split(" ", 2)
    core[rel] = (h, m)
cur = {}
mt = {}
for d, dirs, fs in os.walk(prefix):
    for f in fs:
        p = os.path.join(d, f)
        s = os.lstat(p)
        rel = os.path.relpath(p, prefix)
        cur[rel] = (hashlib.sha256(open(p, "rb").read()).hexdigest(), "%o" % (s.st_mode & 0o7777))
        mt[rel] = s.st_mtime_ns
for rel in sorted(set(core) | set(cur) | set(snap["prefix"]) | set(mt)):
    if core.get(rel) != cur.get(rel) or snap["prefix"].get(rel) != mt.get(rel):
        die("second install not idempotent: " + rel)

# (c) same output
a = open(os.path.join(work, "first.out"), "rb").read()
b = open(os.path.join(work, "second.out"), "rb").read()
if a != b:
    die("second install output differs")

# (d) the install never named the overlay
word = "zz" + "overlay" + "sentinel"
for t in (a, b):
    if word.encode() in t or (ov).encode() in t:
        die("install output names the overlay")

# (e) HOME top level
if sorted(os.listdir(home)) != ["bridlework", "overlay", "work"]:
    die("check: HOME top level is not exactly bridlework overlay work")
v = subprocess.run([os.path.join(prefix, "bin", "harness"), "--version"], stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                   stderr=subprocess.DEVNULL)
m = re.fullmatch(r"harness (\d+\.\d+\.\d+)", v.stdout.decode().strip())
if v.returncode != 0 or not m:
    die("check: harness --version is not harness X.Y.Z")
print("install-twice: ok version=%s overlay_files=%d unchanged core_files=%d idempotent" % (m.group(1), nov, len(cur)))
PY
}

case "$phase" in
  seed) do_seed ;;
  first) do_first ;;
  snap) do_snap ;;
  second) do_second ;;
  check) do_check ;;
  all)
    (do_seed) || exit 1
    (do_first) || exit 1
    (do_snap) || exit 1
    (do_second) || exit 1
    (do_check) || exit 1
    ;;
esac
