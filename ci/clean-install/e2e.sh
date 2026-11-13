#!/bin/sh
# Clean-machine install + one ticket end to end. Usage: e2e.sh <phase> <dist-dir> <source-archive>
# Phases: preflight install snapshot ticket verify all (the five in order). dist-dir and source-archive are absolute paths.
# Prefix $HOME/bridlework, work folder $HOME/work. HOME must be empty and the overlay absent (no HARNESS_ variable at all).
# The only non-HARNESS_ variables preflight allows are CARGO_HOME and RUSTUP_HOME, each outside HOME.
# No plant switch: the smoke plants by mutating inputs or by running the phases one by one.
# Exit 0 ok, 1 on any refusal or mismatch (one reason line on stderr), 2 on usage.
phase="$1"
dist="$2"
srcarc="$3"
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd) || exit 2
case "$phase" in
  preflight | install | snapshot | ticket | verify | all) ;;
  *) echo "e2e: usage: e2e.sh <preflight|install|snapshot|ticket|verify|all> <dist-dir> <source-archive>" >&2; exit 2 ;;
esac
case "$dist" in /*) ;; *) echo "e2e: dist-dir must be absolute" >&2; exit 2 ;; esac
case "$srcarc" in /*) ;; *) echo "e2e: source-archive must be absolute" >&2; exit 2 ;; esac

fail() {
  echo "$1" >&2
  exit 1
}

do_preflight() {
  [ -n "${HOME:-}" ] && [ -d "$HOME" ] || fail "not fresh: HOME is not a folder"
  [ -z "$(ls -A "$HOME")" ] || fail "not fresh: HOME is not empty"
  bad=$(env | sed -n 's/^\(HARNESS_[A-Za-z0-9_]*\)=.*/\1/p' | head -n 1)
  [ -z "$bad" ] || fail "not fresh: $bad is set (the overlay must be empty)"
  python3 -I - <<'PY' || exit 1
import os, shutil, sys

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
PY
  echo "preflight ok"
}

do_install() {
  sh "$here/install.sh" "$dist" "$srcarc" "$HOME/bridlework" || exit 1
}

do_snapshot() {
  mkdir -p "$HOME/work" || exit 1
  python3 -I - "$HOME/bridlework" "$HOME/work/core.snap" <<'PY' || exit 1
import hashlib, os, sys
prefix, out = sys.argv[1:3]
rows = []
for d, dirs, fs in os.walk(prefix):
    for f in fs:
        p = os.path.join(d, f)
        st = os.lstat(p)
        rel = os.path.relpath(p, prefix)
        h = hashlib.sha256(os.readlink(p).encode() if os.path.islink(p) else open(p, "rb").read()).hexdigest()
        rows.append("%s %o %s" % (h, st.st_mode & 0o7777, rel))
rows.sort(key=lambda r: r.split(" ", 2)[2])
with open(out, "w", encoding="utf-8") as fh:
    fh.write("\n".join(rows) + "\n")
PY
}

step_fail() {
  echo "ticket: $1 expected $2 got $3" >&2
  exit 1
}

do_ticket() {
  P="$HOME/bridlework"
  B="$P/bin/harness"
  W="$HOME/work"
  [ -x "$B" ] || fail "ticket: the engine is not installed"
  : "${GIT_AUTHOR_NAME:=clean-install}"
  dom=example
  dom="$dom.invalid"
  : "${GIT_AUTHOR_EMAIL:=ci@$dom}"
  : "${GIT_COMMITTER_NAME:=$GIT_AUTHOR_NAME}"
  : "${GIT_COMMITTER_EMAIL:=$GIT_AUTHOR_EMAIL}"
  export GIT_AUTHOR_NAME GIT_AUTHOR_EMAIL GIT_COMMITTER_NAME GIT_COMMITTER_EMAIL
  export GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL=/dev/null

  out=$("$B" config check --config "$P/src/config/harness.example.yaml" 2>&1)
  rc=$?
  [ "$rc" -eq 0 ] || step_fail config-check "exit 0" "exit $rc"
  case "$out" in *repos=0*) ;; *) step_fail config-check "repos=0" "$out" ;; esac
  echo "step config-check exit=0 repos=0"

  mkdir -p "$W" || exit 1
  cp -R "$P/src/examples/project-template/." "$W/" || fail "ticket: cannot copy the template"
  mkdir -p "$W/ws" "$W/man" "$W/res" "$W/caps" || exit 1
  R="$W/project"
  T="$W/ws/ex-1/tree"
  TK="$W/tickets/ex-1.json"
  git -C "$R" init -q -b main || fail "ticket: git init failed"
  git -C "$R" config maintenance.auto false
  git -C "$R" config gc.auto 0
  git -C "$R" add -A && git -C "$R" commit -q -m base || fail "ticket: base commit failed"
  BASE=$(git -C "$R" rev-parse HEAD)

  out=$("$B" ticket intake "$TK" --repo-map "example=$R" 2>&1)
  rc=$?
  [ "$rc" -eq 0 ] || step_fail intake "exit 0" "exit $rc"
  echo "step intake exit=0"
  out=$("$B" workspace create "$TK" --repo-map "example=$R" --base "$BASE" --workspace-root "$W/ws" --manifest-dir "$W/man" 2>&1)
  rc=$?
  [ "$rc" -eq 0 ] || step_fail workspace-create "exit 0" "exit $rc"
  case "$out" in *branch=ws/ex-1*) ;; *) step_fail workspace-create "branch=ws/ex-1" "$out" ;; esac
  echo "step workspace-create exit=0 branch=ws/ex-1"

  # the hook's command string, as the installed hooks.json registers it
  HOOKCMD=$(python3 -I - "$P/plugins/claude/hooks/hooks.json" <<'PY'
import json, sys
d = json.load(open(sys.argv[1], encoding="utf-8"))
for e in d["hooks"]["PreToolUse"]:
    if e.get("matcher") == "TaskUpdate":
        print(e["hooks"][0]["command"])
        break
PY
) || fail "ticket: hooks.json unreadable"
  [ -n "$HOOKCMD" ] || fail "ticket: no TaskUpdate hook in hooks.json"
  J='{"tool_name":"TaskUpdate","tool_input":{"status":"completed"}}'

  run_hook() { # $1 = repo dir for the repo map
    printf '%s' "$J" | env HARNESS_BIN="$B" HARNESS_TICKET_FILE="$TK" HARNESS_REPO_MAP="example=$1" \
      HARNESS_RESULT_DIR="$W/res" HARNESS_CAPS_DIR="$W/caps" CLAUDE_PLUGIN_ROOT="$P/plugins/claude" sh -c "$HOOKCMD" 2>&1
  }
  run_gate() {
    "$B" ticket gate "$TK" --repo-map "example=$R" --base "$BASE" --result-dir "$W/res" --caps-dir "$W/caps" 2>&1
  }

  out=$(run_hook "$T"); rc=$?
  [ "$rc" -eq 2 ] || step_fail hook-before "exit 2" "exit $rc"
  case "$out" in *"the gate has not run"*) ;; *) step_fail hook-before "no result" "$out" ;; esac
  echo "step hook-before exit=2"

  printf 'hullo\n' > "$T/src/greeting.txt"
  git -C "$T" commit -q -am "wrong greeting" || fail "ticket: wrong-change commit failed"
  git -C "$R" merge -q --ff-only ws/ex-1 || fail "ticket: fast-forward failed"
  out=$(run_gate); rc=$?
  [ "$rc" -eq 1 ] || step_fail gate-wrong "exit 1" "exit $rc: $out"
  case "$out" in *result=fail*) ;; *) step_fail gate-wrong "result=fail" "$out" ;; esac
  echo "step gate-wrong exit=1 result=fail"
  out=$(run_hook "$R"); rc=$?
  [ "$rc" -eq 2 ] || step_fail hook-wrong "exit 2" "exit $rc"
  case "$out" in *"last gate result is fail"*) ;; *) step_fail hook-wrong "last result fail" "$out" ;; esac
  echo "step hook-wrong exit=2"

  printf 'hello\n' > "$T/src/greeting.txt"
  git -C "$T" commit -q -am "right greeting" || fail "ticket: right-change commit failed"
  git -C "$R" merge -q --ff-only ws/ex-1 || fail "ticket: fast-forward failed"
  out=$(run_gate); rc=$?
  [ "$rc" -eq 0 ] || step_fail gate-right "exit 0" "exit $rc: $out"
  case "$out" in *done=allowed*) ;; *) step_fail gate-right "done=allowed" "$out" ;; esac
  echo "step gate-right exit=0 done=allowed"
  out=$(run_hook "$R"); rc=$?
  [ "$rc" -eq 0 ] || step_fail hook-right "exit 0" "exit $rc: $out"
  echo "step hook-right exit=0"
  out=$("$B" status "$TK" --repo-map "example=$R" --result-dir "$W/res" --caps-dir "$W/caps" 2>&1)
  rc=$?
  [ "$rc" -eq 0 ] || step_fail status "exit 0" "exit $rc"
  case "$out" in *last_gate=pass*gate_current=yes*) ;; *) step_fail status "last_gate=pass gate_current=yes" "$out" ;; esac
  echo "step status last_gate=pass gate_current=yes"
}

do_verify() {
  P="$HOME/bridlework"
  python3 -I - "$P" "$HOME/work/core.snap" "$dist" "$srcarc" "$HOME" <<'PY' || exit 1
import hashlib, os, re, subprocess, sys, tarfile

prefix, snap, dist, srcarc, home = sys.argv[1:6]


def die(m):
    sys.stderr.write(m + "\n")
    sys.exit(1)


def scan():
    rows = {}
    for d, dirs, fs in os.walk(prefix):
        for f in fs:
            p = os.path.join(d, f)
            st = os.lstat(p)
            rel = os.path.relpath(p, prefix)
            h = hashlib.sha256(os.readlink(p).encode() if os.path.islink(p) else open(p, "rb").read()).hexdigest()
            rows[rel] = (h, "%o" % (st.st_mode & 0o7777))
    return rows


old = {}
try:
    for line in open(snap, encoding="utf-8").read().splitlines():
        h, m, rel = line.split(" ", 2)
        old[rel] = (h, m)
except OSError:
    die("verify: no snapshot (run the snapshot phase first)")
new = scan()
for rel in sorted(set(old) | set(new)):
    if old.get(rel) != new.get(rel):
        die("core edited: " + rel)
n = len(new)

# every installed plugin file equals its tarball member
names = [x for x in os.listdir(dist) if re.fullmatch(r"bridlework-harness-claude-plugin-\d+\.\d+\.\d+\.tar\.gz", x)]
if len(names) != 1:
    die("verify: expected one plugin tarball")
pre = "bridlework-harness/plugins/claude/"
members = {}
with tarfile.open(os.path.join(dist, names[0]), "r:gz") as tf:
    for ti in tf:
        if ti.isreg():
            members[ti.name[len(pre):]] = tf.extractfile(ti).read()
installed = set(r[len("plugins/claude/"):] for r in new if r.startswith("plugins/claude/"))
if installed != set(members):
    die("core edited: plugins/claude (file set differs from the tarball)")
for rel, data in members.items():
    if open(os.path.join(prefix, "plugins", "claude", rel), "rb").read() != data:
        die("core edited: plugins/claude/" + rel)

# the config used is byte-equal to the source archive's copy
with tarfile.open(srcarc, "r:gz") as tf:
    cfg = None
    for ti in tf:
        if ti.isreg() and re.fullmatch(r"[^/]+/config/harness\.example\.yaml", ti.name):
            cfg = tf.extractfile(ti).read()
if cfg is None or open(os.path.join(prefix, "src", "config", "harness.example.yaml"), "rb").read() != cfg:
    die("core edited: src/config/harness.example.yaml")

# the engine is the installed one, not a checkout's
binp = os.path.join(prefix, "bin", "harness")
rp = os.path.realpath(binp)
if not rp.startswith(os.path.realpath(prefix) + "/"):
    die("verify: the engine does not resolve under the install prefix")
v = subprocess.run([binp, "--version"], stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
m = re.fullmatch(r"harness (\d+\.\d+\.\d+)", v.stdout.decode().strip())
if v.returncode != 0 or not m:
    die("verify: harness --version is not harness X.Y.Z")

# HOME holds only the install and the work folder
if sorted(os.listdir(home)) != ["bridlework", "work"]:
    die("verify: HOME top level is not exactly bridlework and work")
print("clean-install: ok ticket=ex-1 version=%s core_files=%d unchanged" % (m.group(1), n))
PY
}

case "$phase" in
  preflight) do_preflight ;;
  install) do_install ;;
  snapshot) do_snapshot ;;
  ticket) do_ticket ;;
  verify) do_verify ;;
  all)
    (do_preflight) || exit 1
    (do_install) || exit 1
    (do_snapshot) || exit 1
    (do_ticket) || exit 1
    (do_verify) || exit 1
    ;;
esac
