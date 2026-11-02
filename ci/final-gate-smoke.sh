#!/bin/sh
# Smoke for the remote final gate (P1A-14b). Usage: ci/final-gate-smoke.sh
# Builds scratch git repos in a temp folder from ci/fixtures/final-gate/ (a planted failing gate, a clean gate, maps) and
# runs the REAL entry (ci/final-gate.sh) against them; also checks the real workflow file, the real path map and the
# workflow lint, each with a planted failing twin. The planted gate lives only in the fixture folder.
# Prints "ok <id>" / "FAIL <id>" per arm. Exit 0 only if every arm holds, 1 otherwise, 2 on a setup error.
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd) || exit 2
root=$(CDPATH= cd -- "$here/.." && pwd) || exit 2
fx="$here/fixtures/final-gate"
entry="$here/final-gate.sh"
wf="$root/.github/workflows/final-gate.yml"

unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE HARNESS_PRIVACY_LIST HARNESS_PRIVACY_OLD_LIST HARNESS_PRIVACY_LIST_SECRET
GIT_CONFIG_GLOBAL=/dev/null
GIT_CONFIG_NOSYSTEM=1
export GIT_CONFIG_GLOBAL GIT_CONFIG_NOSYSTEM

tmp=$(mktemp -d "${TMPDIR:-/tmp}/final-gate-smoke.XXXXXX") || exit 2
trap 'rm -rf "$tmp"' EXIT HUP INT TERM

fails=0
oks=0
ok() { echo "ok   $1"; oks=$((oks + 1)); }
bad() { echo "FAIL $1"; fails=$((fails + 1)); }
has() { case $1 in *"$2"*) return 0 ;; esac; return 1; }
G() { git -c user.name=ci -c user.email=ci -c commit.gpgsign=false -c core.hooksPath=/dev/null "$@"; }

# ---------------------------------------------------------------- scratch repos
# mkrepo DIR MAPFILE GATEID...  : a one-commit repo (the base) with the fixture gates, the map and two mapped areas.
mkrepo() {
  d=$1; m=$2; shift 2
  mkdir -p "$d/ci" "$d/gates" "$d/area-fail" "$d/area-clean" || return 1
  cp "$fx/fixture-fail.sh" "$fx/fixture-clean.sh" "$d/ci/" || return 1
  chmod +x "$d/ci/fixture-fail.sh" "$d/ci/fixture-clean.sh"
  cp "$m" "$d/gates/path-map.json" || return 1
  for g in "$@"; do
    case $g in
      fixture-fail) cp "$fx/fixture-fail.json" "$d/gates/fixture-fail.json" ;;
      *) sed "s/\"id\":\"fixture-clean\"/\"id\":\"$g\"/" "$fx/fixture-clean.json" > "$d/gates/$g.json" ;;
    esac
  done
  echo base > "$d/area-fail/f.txt"
  echo base > "$d/area-clean/f.txt"
  G -C "$d" init -q || return 1
  G -C "$d" add -A || return 1
  G -C "$d" commit -q -m base || return 1
}
# chg DIR PATH : one more commit that changes PATH (so HEAD^1 is the base).
chg() {
  mkdir -p "$(dirname "$1/$2")"
  echo "changed $(G -C "$1" rev-parse HEAD)" > "$1/$2"
  G -C "$1" add -A && G -C "$1" commit -q -m change
}
# run_entry DIR : sets out (stdout and stderr together) and rc.
run_entry() { out=$(sh "$entry" "$1" 2>&1); rc=$?; }

# ---------------------------------------------------------------- engine
(cd "$root/core/rust" && cargo build --locked >/dev/null 2>&1) || { echo "smoke: engine build failed" >&2; exit 2; }
# the binary sits under CARGO_TARGET_DIR when it is set (a relative value is relative to core/rust, where cargo ran)
tdir=${CARGO_TARGET_DIR:-}
case $tdir in
  "") tdir="$root/core/rust/target" ;;
  /*) ;;
  *) tdir="$root/core/rust/$tdir" ;;
esac
bin="$tdir/debug/harness"

# ---------------------------------------------------------------- B1 planted failing gate / clean twin
mkrepo "$tmp/b1f" "$fx/map-fail.json" fixture-fail fixture-clean && chg "$tmp/b1f" area-fail/f.txt || exit 2
run_entry "$tmp/b1f"
if [ "$rc" -ne 0 ] && has "$out" "result=fail done=refused"; then ok B1-planted-fail; else bad B1-planted-fail; fi
mkrepo "$tmp/b1c" "$fx/map-clean.json" fixture-clean && chg "$tmp/b1c" area-fail/f.txt || exit 2
run_entry "$tmp/b1c"
if [ "$rc" -eq 0 ] && has "$out" "result=pass done=allowed"; then ok B1-clean-pass; else bad B1-clean-pass; fi

# ---------------------------------------------------------------- B3 unmapped new folder fails; real map covers every top-level name
mkrepo "$tmp/b3" "$fx/map-clean.json" fixture-clean && chg "$tmp/b3" zzz-new-top-level/f.txt || exit 2
run_entry "$tmp/b3"
if [ "$rc" -ne 0 ] && has "$out" "class=unmapped-path"; then ok B3-unmapped-folder-fails; else bad B3-unmapped-folder-fails; fi

# uncovered MAPFILE : prints uncovered:<name> for each top-level name of HEAD that is not a key of the map.
uncovered() {
  G -C "$root" ls-tree --name-only HEAD | while IFS= read -r n; do
    grep -qF "\"$n\":" "$1" || echo "uncovered:$n"
  done
}
u=$(uncovered "$root/gates/path-map.json")
if [ -z "$u" ]; then ok B3-real-map-covers-every-top-level; else bad B3-real-map-covers-every-top-level; fi
sed 's/"docs":\[[^]]*\],//' "$root/gates/path-map.json" > "$tmp/map-no-docs.json"
u=$(uncovered "$tmp/map-no-docs.json")
if [ "$u" = "uncovered:docs" ]; then ok B3-twin-map-missing-entry-detected; else bad B3-twin-map-missing-entry-detected; fi
if ! grep -rqi 'fixture' "$root"/gates/*.json; then ok B3-no-fixture-in-real-gates; else bad B3-no-fixture-in-real-gates; fi

# ---------------------------------------------------------------- B4 the printed gate ids come from the written result
ids_of() { sed -n "s/.*\"$2\":\\[\\([^]]*\\)\\].*/\\1/p" "$1" | tr -d '"' | sed 's/,/, /g'; }
mkrepo "$tmp/b4r" "$root/gates/path-map.json" cargo-test core-layering && chg "$tmp/b4r" docs/f.txt || exit 2
run_entry "$tmp/b4r"
want="checks passed: $(ids_of "$root/gates/path-map.json" docs)"
if [ "$rc" -eq 0 ] && has "$out" "$want" && has "$want" "cargo-test" && has "$want" "core-layering"; then ok B4-real-map-prints-both-gates; else bad B4-real-map-prints-both-gates; fi
want="checks passed: $(ids_of "$fx/map-clean.json" area-clean)"
run_entry "$tmp/b1c"
if [ "$rc" -eq 0 ] && has "$out" "$want" && [ "$want" = "checks passed: fixture-clean" ]; then ok B4-scratch-map-prints-exactly-its-ids; else bad B4-scratch-map-prints-exactly-its-ids; fi

# ---------------------------------------------------------------- B6 no private input reaches the gates
if HARNESS_PRIVACY_LIST=canary HARNESS_PRIVACY_OLD_LIST=canary HARNESS_PRIVACY_LIST_SECRET=canary sh "$fx/fixture-clean.sh" >/dev/null 2>&1; then
  bad B6-fixture-detects-canary
else
  ok B6-fixture-detects-canary
fi
out=$(HARNESS_PRIVACY_LIST=canary HARNESS_PRIVACY_OLD_LIST=canary HARNESS_PRIVACY_LIST_SECRET=canary sh "$entry" "$tmp/b1c" 2>&1); rc=$?
if [ "$rc" -eq 0 ] && has "$out" "done=allowed"; then ok B6-entry-scrubs-privacy-env; else bad B6-entry-scrubs-privacy-env; fi

# ---------------------------------------------------------------- B7 a missing base fails closed
G clone -q --depth 1 "file://$tmp/b1c" "$tmp/b7" || exit 2
run_entry "$tmp/b7"
if [ "$rc" -ne 0 ] && has "$out" "base commit missing" && ! has "$out" "done=allowed"; then ok B7-shallow-fails-closed; else bad B7-shallow-fails-closed; fi
run_entry "$tmp/b1c"
if [ "$rc" -eq 0 ] && has "$out" "done=allowed"; then ok B7-parent-present-passes; else bad B7-parent-present-passes; fi

# ---------------------------------------------------------------- B8 a dirty tree is not a pass
mkrepo "$tmp/b8" "$fx/map-clean.json" fixture-clean && chg "$tmp/b8" area-clean/f.txt || exit 2
echo stray > "$tmp/b8/stray.txt"
run_entry "$tmp/b8"
if [ "$rc" -ne 0 ] && [ -f "$tmp/b8/stray.txt" ] && ! has "$out" "done=allowed"; then ok B8-dirty-tree-refused; else bad B8-dirty-tree-refused; fi
mv "$tmp/b8/stray.txt" "$tmp/stray.moved"
run_entry "$tmp/b8"
if [ "$rc" -eq 0 ]; then ok B8-clean-tree-passes; else bad B8-clean-tree-passes; fi

# ---------------------------------------------------------------- B9 no stale pass
mkrepo "$tmp/b9" "$fx/map-clean.json" fixture-clean && chg "$tmp/b9" area-clean/f.txt || exit 2
run_entry "$tmp/b9"
h1=$(G -C "$tmp/b9" rev-parse HEAD)
r1=$rc; o1=$out
printf '#!/bin/sh\nexit 1\n' > "$tmp/b9/ci/fixture-clean.sh"
G -C "$tmp/b9" add -A && G -C "$tmp/b9" commit -q -m "gate now fails" || exit 2
run_entry "$tmp/b9"
h2=$(G -C "$tmp/b9" rev-parse HEAD)
if [ "$r1" -eq 0 ] && has "$o1" "head=$h1 result=pass" && [ "$rc" -ne 0 ] && has "$out" "head=$h2 result=fail" && [ "$h1" != "$h2" ]; then
  ok B9-pass-then-fail-no-stale
else
  bad B9-pass-then-fail-no-stale
fi

# ---------------------------------------------------------------- B10 the synthetic ticket is valid and stays valid
sh "$entry" --print-ticket > "$tmp/ticket.json" || exit 2
head_b1c=$(G -C "$tmp/b1c" rev-parse HEAD)
t=$("$bin" ticket intake "$tmp/ticket.json" --repo-map "core=$tmp/b1c" 2>&1); trc=$?
if [ "$trc" -eq 0 ] && [ "$t" = "ticket=ci-pr repo_id=core head=$head_b1c" ]; then ok B10-ticket-accepted; else bad B10-ticket-accepted; fi
twin() { # NAME SED-EXPR FIELD
  sed "$2" "$tmp/ticket.json" > "$tmp/ticket-$1.json"
  cmp -s "$tmp/ticket.json" "$tmp/ticket-$1.json" && { bad "B10-twin-$1-not-built"; return; }
  tt=$("$bin" ticket intake "$tmp/ticket-$1.json" --repo-map "core=$tmp/b1c" 2>&1); trc=$?
  if [ "$trc" -eq 1 ] && has "$tt" "$3"; then ok "B10-twin-$1-refused"; else bad "B10-twin-$1-refused"; fi
}
twin extra-key 's/"id":"ci-pr",/"id":"ci-pr","zzz":"x",/' zzz
twin missing-key 's/"group":"ci",//' group
twin repo-id-upper 's/"repo_id":"core"/"repo_id":"Core"/' repo_id
twin repo-id-path 's#"repo_id":"core"#"repo_id":"a/b"#' repo_id
twin owner-role 's/"owner_role":"execution_verification"/"owner_role":"nobody"/' owner_role

# ---------------------------------------------------------------- workflow checks (B2, B5, B6 static, B11)
# wf_bad FILE : prints one token per violation of the hand checklist; empty means clean.
wf_bad() {
  f=$1
  for tok in 'continue-on-error' '||' 'set +e' 'set +o errexit' 'shell:' 'types' 'paths' 'branches' 'cache' 'secrets.' 'pull_request_target' 'if:'; do
    if grep -qF -- "$tok" "$f"; then echo "has:$tok"; fi
  done
  if grep -qE 'run:[[:space:]]*[|>]' "$f"; then echo "multiline-run"; fi
  runs=$(grep -E '^[[:space:]]*(-[[:space:]]+)?run:' "$f" | sed -E 's/^[[:space:]]*(-[[:space:]]+)?run:[[:space:]]*//')
  if [ -z "$runs" ]; then echo "no-run"; fi
  if printf '%s\n' "$runs" | grep -qvE '^sh ci/[a-z0-9_-]+\.sh$'; then echo "run-not-allowlisted"; fi
  n=$(grep -cE '^[[:space:]]+timeout-minutes:[[:space:]]*[0-9]+[[:space:]]*$' "$f")
  v=$(sed -n -E 's/^[[:space:]]+timeout-minutes:[[:space:]]*([0-9]+)[[:space:]]*$/\1/p' "$f" | head -n 1)
  if [ "$n" -ne 1 ] || [ "${v:-0}" -lt 45 ]; then echo "timeout"; fi
  grep -qxF '  contents: read' "$f" || echo "permissions-not-read"
  grep -qF 'persist-credentials: false' "$f" || echo "credentials-persist"
}
# wf_order FILE : the lint step line is before the smoke step line, which is before the gate step line; nothing else is a run.
wf_order() {
  a=$(grep -nxE '[[:space:]]*run: sh ci/workflow-lint\.sh' "$1" | head -n 1 | cut -d: -f1)
  b=$(grep -nxE '[[:space:]]*run: sh ci/final-gate-smoke\.sh' "$1" | head -n 1 | cut -d: -f1)
  c=$(grep -nxE '[[:space:]]*run: sh ci/final-gate\.sh' "$1" | head -n 1 | cut -d: -f1)
  [ -n "$a" ] && [ -n "$b" ] && [ -n "$c" ] && [ "$a" -lt "$b" ] && [ "$b" -lt "$c" ]
}
# reported_names DIR : "<file> <job reported name>" per job: the job's name: when set, else its id.
reported_names() {
  for f in "$1"/*.y*ml; do
    [ -f "$f" ] || continue
    awk -v F="$(basename "$f")" '
      function flush() { if (id != "") print F, (nm != "" ? nm : id) }
      /^jobs:/ { injobs = 1; next }
      /^[^ #]/ { if (injobs) { flush(); id = ""; nm = "" } injobs = 0 }
      injobs && /^  [A-Za-z0-9_-]+:[ ]*$/ { flush(); id = $1; sub(/:$/, "", id); nm = ""; next }
      injobs && /^    name:/ { v = $0; sub(/^    name:[ ]*/, "", v); gsub(/["\047]/, "", v); nm = v }
      END { flush() }
    ' "$f"
  done
}
# names_ok DIR : final-gate.yml has exactly one job reporting final-gate; no other file reports final-gate; only privacy-gate.yml and privacy-gate-next.yml report gate (ticket 163 PR-B).
names_ok() {
  r=$(reported_names "$1")
  [ "$(printf '%s\n' "$r" | grep -c '^final-gate.yml ')" -eq 1 ] || return 1
  printf '%s\n' "$r" | grep -qx 'final-gate.yml final-gate' || return 1
  printf '%s\n' "$r" | grep -v '^final-gate.yml ' | grep -q ' final-gate$' && return 1
  printf '%s\n' "$r" | grep -v -e '^privacy-gate.yml ' -e '^privacy-gate-next.yml ' | grep -q ' gate$' && return 1
  printf '%s\n' "$r" | grep -qx 'privacy-gate.yml gate' || return 1
  printf '%s\n' "$r" | grep -qx 'privacy-gate-next.yml gate' || return 1
  return 0
}

b=$(wf_bad "$wf")
if [ -z "$b" ]; then ok B5-real-workflow-checklist-clean; else bad B5-real-workflow-checklist-clean; fi
if wf_order "$wf"; then ok B11-lint-step-before-smoke-before-gate; else bad B11-lint-step-before-smoke-before-gate; fi
if names_ok "$root/.github/workflows"; then ok B2-reported-names; else bad B2-reported-names; fi
# base for the old-workflow comparison: origin/master when present, else the first parent (CI checks out only the PR merge
# commit, so there is no origin/master); with neither, print skip rather than a silent ok.
if G -C "$root" rev-parse --verify -q origin/master >/dev/null; then oldbase=origin/master
elif G -C "$root" rev-parse --verify -q 'HEAD^1' >/dev/null; then oldbase='HEAD^1'
else oldbase=""; fi
# ticket 163 changes privacy-gate.yml and privacy-text.yml on purpose, and ONLY these ways: each job declares
# `environment: privacy-gate`, and privacy-text.yml loses its `release:` trigger. priv_frozen NAME CURRENT-FILE compares the
# current file with the base copy after dropping those two allowed differences from the base copy, and the one environment line
# from the current copy (which must carry exactly one); any other change in either file is red. rust-test.yml stays fully frozen.
priv_frozen() {
  [ -n "$oldbase" ] || return 1
  G -C "$root" show "$oldbase:.github/workflows/$1" > "$tmp/priv-old.yml" 2>/dev/null || return 1
  python3 -I - "$1" "$tmp/priv-old.yml" "$2" <<'PYX'
import re, sys
name, oldp, curp = sys.argv[1:4]
env = "    environment: privacy-gate"
old = open(oldp, encoding="utf-8").read().split("\n")
cur = open(curp, encoding="utf-8").read().split("\n")
if cur.count(env) != 1:
    sys.exit(1)
o = [l for l in old if l != env]
if name == "privacy-text.yml":
    t = "\n".join(o)
    t = re.sub(r"^  release:\n    types: \[created, edited, published\]\n", "", t, flags=re.M)
    o = t.split("\n")
sys.exit(0 if o == [l for l in cur if l != env] else 1)
PYX
}
if [ -z "$oldbase" ]; then
  echo "skip B2-old-workflows-untouched (no base to compare with)"
else
  fz=ok
  G -C "$root" diff --quiet "$oldbase" -- .github/workflows/rust-test.yml 2>/dev/null || fz=bad
  priv_frozen privacy-gate.yml "$root/.github/workflows/privacy-gate.yml" || fz=bad
  priv_frozen privacy-text.yml "$root/.github/workflows/privacy-text.yml" || fz=bad
  $fz B2-old-workflows-untouched
  # planted twins: one extra line / a lost allowed change in a scratch copy of each privacy workflow must go red
  for n in privacy-gate.yml privacy-text.yml; do
    cp "$root/.github/workflows/$n" "$tmp/priv-cur.yml"
    printf '# x\n' >> "$tmp/priv-cur.yml"
    if priv_frozen "$n" "$tmp/priv-cur.yml"; then bad "B2-twin-$n-extra-line-detected"; else ok "B2-twin-$n-extra-line-detected"; fi
    sed -E '/^    environment: privacy-gate$/d' "$root/.github/workflows/$n" > "$tmp/priv-cur.yml"
    if priv_frozen "$n" "$tmp/priv-cur.yml"; then bad "B2-twin-$n-no-environment-detected"; else ok "B2-twin-$n-no-environment-detected"; fi
    sed -E 's/^(    environment: privacy-gate)$/\1\n\1/' "$root/.github/workflows/$n" > "$tmp/priv-cur.yml"
    if priv_frozen "$n" "$tmp/priv-cur.yml"; then bad "B2-twin-$n-two-environment-lines-detected"; else ok "B2-twin-$n-two-environment-lines-detected"; fi
  done
  sed -E 's/^(  issues:)$/  release:\n    types: [created, edited, published]\n\1/' "$root/.github/workflows/privacy-text.yml" > "$tmp/priv-cur.yml"
  if priv_frozen privacy-text.yml "$tmp/priv-cur.yml"; then bad B2-twin-privacy-text-release-trigger-back-detected; else ok B2-twin-privacy-text-release-trigger-back-detected; fi
fi
if sh "$here/workflow-lint.sh"; then ok B5-lint-passes-all-workflows; else bad B5-lint-passes-all-workflows; fi

# planted twins for the checklist: each is a scratch copy of the real workflow with one form added; the checklist must go red.
wtwin() { # NAME SED-EXPR
  sed -E "$2" "$wf" > "$tmp/wf-$1.yml"
  if cmp -s "$wf" "$tmp/wf-$1.yml"; then bad "B5-twin-$1-not-built"; return; fi
  if [ -n "$(wf_bad "$tmp/wf-$1.yml")" ]; then ok "B5-twin-$1-detected"; else bad "B5-twin-$1-detected"; fi
}
wtwin continue-on-error 's/^(    timeout-minutes: 60)$/\1\n    continue-on-error: true/'
wtwin or-colon 's#(run: sh ci/final-gate\.sh)$#\1 || :#'
wtwin or-true 's#(run: sh ci/final-gate\.sh)$#\1 || true#'
wtwin set-plus-e 's#(run: sh ci/final-gate\.sh)$#set +e; \1#'
wtwin shell-key 's#^(      - name: Final gate)$#\1\n        shell: bash {0}#'
wtwin multiline-run 's#^(        run: sh ci/final-gate\.sh)$#        run: |\n          sh ci/final-gate.sh\n          true#'
wtwin step-if 's#^(      - name: Final gate)$#\1\n        if: false#'
wtwin types 's#^  pull_request:$#  pull_request:\n    types: [labeled]#'
wtwin paths 's#^  pull_request:$#  pull_request:\n    paths: ["core/**"]#'
wtwin branches 's#^  pull_request:$#  pull_request:\n    branches: [master]#'
wtwin cache 's#^(      - name: Final gate)$#      - uses: actions/cache@v4\n\1#'
wtwin secrets 's#^(      - name: Final gate)$#\1\n        env:\n          X: ${{ secrets.X }}#'
wtwin target 's#^  pull_request:$#  pull_request_target:#'
wtwin timeout-low 's#timeout-minutes: 60#timeout-minutes: 10#'
wtwin write-perm 's#contents: read#contents: write#'
# the carried allowlist rule: a single-line run can still swallow the gate's exit code (MC3g / M5f).
wtwin allowlist-pipe-tee 's#(run: sh ci/final-gate\.sh)$#\1 | tee gate.log#'
wtwin allowlist-and-true 's#(run: sh ci/final-gate\.sh)$#\1 \&\& echo ok; true#'
wtwin allowlist-bang-true 's#run: sh ci/final-gate\.sh$#run: ! sh ci/final-gate.sh; true#'
# order twins: the lint step removed, or after the gate.
grep -v 'workflow-lint' "$wf" | grep -v 'name: Workflow lint' > "$tmp/wf-nolint.yml"
if wf_order "$tmp/wf-nolint.yml"; then bad B11-twin-no-lint-step-detected; else ok B11-twin-no-lint-step-detected; fi
sed -e 's#run: sh ci/workflow-lint\.sh#run: sh ci/TMPSWAP.sh#' -e 's#run: sh ci/final-gate\.sh#run: sh ci/workflow-lint.sh#' -e 's#run: sh ci/TMPSWAP.sh#run: sh ci/final-gate.sh#' "$wf" > "$tmp/wf-lint-after.yml"
if wf_order "$tmp/wf-lint-after.yml"; then bad B11-twin-lint-after-gate-detected; else ok B11-twin-lint-after-gate-detected; fi

# reported-name twins (B2): a scratch copy of the real workflows folder with one change each.
ntwin() { # NAME  (reads the change from the function nt_NAME)
  rm -rf "$tmp/wfs-$1"; mkdir "$tmp/wfs-$1" && cp "$root"/.github/workflows/*.yml "$tmp/wfs-$1/" || { bad "B2-twin-$1-not-built"; return; }
  "nt_$1" "$tmp/wfs-$1"
  if names_ok "$tmp/wfs-$1"; then bad "B2-twin-$1-detected"; else ok "B2-twin-$1-detected"; fi
}
nt_job_id_gate() { sed -i.bak 's/^  final-gate:$/  gate:/' "$1/final-gate.yml"; rm -f "$1/final-gate.yml.bak"; }
nt_job_name() { sed -i.bak 's/^    runs-on: ubuntu-latest$/    name: Final gate\n    runs-on: ubuntu-latest/' "$1/final-gate.yml"; rm -f "$1/final-gate.yml.bak"; }
nt_other_gate() { sed -i.bak 's/^  test:$/  test:\n    name: gate/' "$1/rust-test.yml"; rm -f "$1/rust-test.yml.bak"; }
nt_yaml_ext() { printf 'name: x\non:\n  pull_request:\npermissions:\n  contents: read\njobs:\n  final-gate:\n    runs-on: ubuntu-latest\n    steps:\n      - run: sh ci/rust-test.sh\n' > "$1/x.yaml"; }
ntwin job_id_gate
ntwin job_name
ntwin other_gate
ntwin yaml_ext

# B11 planted twin: a rust-test.yml with continue-on-error makes the real lint exit 1; the real tree exits 0 (checked above).
mkdir "$tmp/lintw" && sed 's/^    runs-on: ubuntu-latest$/    runs-on: ubuntu-latest\n    continue-on-error: true/' "$root/.github/workflows/rust-test.yml" > "$tmp/lintw/rust-test.yml"
if sh "$here/workflow-lint.sh" "$tmp/lintw/rust-test.yml" >/dev/null; then bad B11-twin-lint-refuses-soft-fail; else ok B11-twin-lint-refuses-soft-fail; fi

# ---------------------------------------------------------------- R1 fresh caps folder every run, R2 final newline
# Five runs on the failing fixture: a caps folder reused across runs would hit the fix-round cap on the fifth and say something other than result=fail.
ok_all=1
for i in 1 2 3 4 5; do
  run_entry "$tmp/b1f"
  if [ "$rc" -eq 0 ] || ! has "$out" "result=fail done=refused"; then ok_all=0; fi
done
if [ "$ok_all" -eq 1 ]; then ok R1-caps-fresh-each-run-five-fails; else bad R1-caps-fresh-each-run-five-fails; fi
sh "$entry" "$tmp/b1c" > "$tmp/nl.out" 2>&1
if [ "$(tail -c 1 "$tmp/nl.out" | od -An -tx1 | tr -d ' ')" = "0a" ] && grep -q '^checks passed: ' "$tmp/nl.out"; then ok R2-output-ends-with-newline; else bad R2-output-ends-with-newline; fi

# ---------------------------------------------------------------- C (P1A-14c) workflow lint rules, each with a planted failing twin
# ltwin NAME SRC DEST SED-EXPR RULE... : a scratch copy of workflow SRC saved as DEST with one change; the lint must exit 1 and print DEST:<line>:<RULE> for every RULE.
ltwin() {
  n=$1; src=$2; dst=$3; e=$4; shift 4
  d="$tmp/lt-$n"; mkdir "$d" || { bad "C-twin-$n-not-built"; return; }
  sed -E "$e" "$root/.github/workflows/$src" > "$d/$dst"
  if cmp -s "$root/.github/workflows/$src" "$d/$dst"; then bad "C-twin-$n-not-built"; return; fi
  lo=$(sh "$here/workflow-lint.sh" "$d/$dst" 2>&1); lrc=$?
  miss=""
  for r in "$@"; do printf '%s\n' "$lo" | grep -qE "^$dst:[0-9]+:$r\$" || miss="$miss $r"; done
  if [ "$lrc" -eq 1 ] && [ -z "$miss" ]; then ok "C-twin-$n-refused"; else bad "C-twin-$n-refused"; fi
}
# lclean NAME SRC DST SED-EXPR : a changed scratch copy that must NOT be refused (clean twin).
lclean() {
  n=$1; src=$2; dst=$3; e=$4
  d="$tmp/lc-$n"; mkdir "$d" || { bad "C-clean-$n-not-built"; return; }
  sed -E "$e" "$root/.github/workflows/$src" > "$d/$dst"
  if cmp -s "$root/.github/workflows/$src" "$d/$dst"; then bad "C-clean-$n-not-built"; return; fi
  if sh "$here/workflow-lint.sh" "$d/$dst" >/dev/null 2>&1; then ok "C-clean-$n-passes"; else bad "C-clean-$n-passes"; fi
}
FG=final-gate.yml
G_STEP='^(      - name: Final gate)$'
G_RUN='(run: sh ci/final-gate\.sh)$'
PRL='^  pull_request:$'

# C1 the real workflows pass (clean twin of every row below); privacy-text.yml has pull_request_target / pull_request_review `types:`
if sh "$here/workflow-lint.sh" "$root"/.github/workflows/*.yml >/dev/null 2>&1; then ok C1-real-workflows-clean; else bad C1-real-workflows-clean; fi
if sh "$here/workflow-lint.sh" "$root/.github/workflows/privacy-text.yml" >/dev/null 2>&1; then ok C2-clean-types-outside-pull_request; else bad C2-clean-types-outside-pull_request; fi

# C2 trigger filters under pull_request
ltwin pr-paths $FG $FG "s#$PRL#  pull_request:\n    paths: [\"core/**\"]#" pull-request-filter
ltwin pr-paths-ignore $FG $FG "s#$PRL#  pull_request:\n    paths-ignore: [\"docs/**\"]#" pull-request-filter
ltwin pr-branches $FG $FG "s#$PRL#  pull_request:\n    branches: [master]#" pull-request-filter
ltwin pr-branches-ignore $FG $FG "s#$PRL#  pull_request:\n    branches-ignore: [x]#" pull-request-filter
ltwin pr-types $FG $FG "s#$PRL#  pull_request:\n    types: [labeled]#" pull-request-filter
ltwin pr-filter-in-other-workflow rust-test.yml rust-test.yml "s#$PRL#  pull_request:\n    paths: [\"core/**\"]#" pull-request-filter
lclean push-branches rust-test.yml rust-test.yml "s#$PRL#  pull_request:\n  push:\n    branches: [master]#"

# C2b any if: in final-gate.yml, whatever the key spelling
ltwin if-gate-step $FG $FG "s#$G_STEP#\1\n        if: false#" if-in-final-gate
ltwin if-gate-step-quoted $FG $FG "s#$G_STEP#\1\n        \"if\": false#" if-in-final-gate
ltwin if-gate-step-single-quoted $FG $FG "s#$G_STEP#\1\n        'if': false#" if-in-final-gate
ltwin if-gate-step-space-colon $FG $FG "s#$G_STEP#\1\n        if : false#" if-in-final-gate
ltwin if-lint-step $FG $FG "s#^(      - name: Workflow lint)\$#\1\n        if: always()#" if-in-final-gate
ltwin if-job-level $FG $FG "s#^(    timeout-minutes: 60)\$#\1\n    if: false#" if-in-final-gate job-level-if
ltwin job-level-if-quoted rust-test.yml rust-test.yml "s#^(    runs-on: ubuntu-latest)\$#\1\n    \"if\": false#" job-level-if

# C2c .yaml workflows are read too
ltwin yaml-final-gate-name $FG x.yaml 's#^name: final-gate$#name: x#' required-name-final-gate
ltwin yaml-continue-on-error rust-test.yml x.yaml "s#^(    runs-on: ubuntu-latest)\$#\1\n    continue-on-error: true#" continue-on-error
mkdir "$tmp/dl" "$tmp/dl/ci" "$tmp/dl/.github" "$tmp/dl/.github/workflows" && cp "$here/workflow-lint.sh" "$tmp/dl/ci/" && cp "$root"/.github/workflows/*.yml "$tmp/dl/.github/workflows/" || exit 2
if sh "$tmp/dl/ci/workflow-lint.sh" >/dev/null 2>&1; then ok C2c-default-list-clean-without-yaml; else bad C2c-default-list-clean-without-yaml; fi
sed 's/^    runs-on: ubuntu-latest$/    runs-on: ubuntu-latest\n    continue-on-error: true/' "$root/.github/workflows/rust-test.yml" > "$tmp/dl/.github/workflows/x.yaml"
lo=$(sh "$tmp/dl/ci/workflow-lint.sh" 2>&1); lrc=$?
if [ "$lrc" -eq 1 ] && printf '%s\n' "$lo" | grep -qE '^x\.yaml:[0-9]+:continue-on-error$'; then ok C2c-default-list-reads-yaml; else bad C2c-default-list-reads-yaml; fi

# C-coe: continue-on-error anywhere in final-gate.yml
ltwin coe-gate-step $FG $FG "s#$G_STEP#\1\n        continue-on-error: true#" continue-on-error
ltwin coe-quoted-key $FG $FG "s#$G_STEP#\1\n        \"continue-on-error\": true#" continue-on-error

# C3 fail-swallow (any workflow)
ltwin swallow-or-true rust-test.yml rust-test.yml 's#(run: sh ci/rust-test\.sh)$#\1 || true#' fail-swallow
ltwin swallow-or-exit-0 rust-test.yml rust-test.yml 's#(run: sh ci/rust-test\.sh)$#\1 || exit 0#' fail-swallow

# C3b the exit code cannot be swallowed in final-gate.yml
ltwin or-colon $FG $FG "s#$G_RUN#\1 || :#" final-gate-or
ltwin or-echo $FG $FG "s#$G_RUN#\1 || echo ok#" final-gate-or
ltwin set-plus-e $FG $FG 's#run: sh ci/final-gate\.sh$#run: set +e; sh ci/final-gate.sh#' final-gate-set-plus-e
ltwin shell-key $FG $FG "s#$G_STEP#\1\n        shell: bash {0}#" final-gate-shell
ltwin multiline-run $FG $FG 's#^(        run: sh ci/final-gate\.sh)$#        run: |\n          sh ci/final-gate.sh\n          true#' final-gate-run-allowlist final-gate-block-scalar
ltwin allow-tee $FG $FG "s#$G_RUN#\1 | tee gate.log#" final-gate-run-allowlist
ltwin allow-and-true $FG $FG "s#$G_RUN#\1 \&\& echo ok; true#" final-gate-run-allowlist
ltwin allow-bang-true $FG $FG 's#run: sh ci/final-gate\.sh$#run: ! sh ci/final-gate.sh; true#' final-gate-run-allowlist
ltwin allow-quoted-newline $FG $FG 's#run: sh ci/final-gate\.sh$#run: "sh ci/final-gate.sh\\ntrue"#' final-gate-run-allowlist
ltwin allow-subshell $FG $FG 's#run: sh ci/final-gate\.sh$#run: echo $(sh ci/final-gate.sh)#' final-gate-run-allowlist
ltwin allow-background $FG $FG "s#$G_RUN#\1 \&#" final-gate-run-allowlist
# A1 key-spelling evasions: a quoted run key with a decoy run line inside a block-scalar env value
ltwin quoted-run-key-decoy $FG $FG 's#^        run: sh ci/final-gate\.sh$#        "run": sh ci/final-gate.sh | tee gate.log\n        env:\n          X: |\n            run: sh ci/final-gate.sh#' final-gate-quoted-key final-gate-block-scalar final-gate-run-allowlist
ltwin env-bash-env $FG $FG "s#$G_STEP#\1\n        env:\n          BASH_ENV: ci/x.sh#" final-gate-shell-env
ltwin env-env $FG $FG "s#$G_STEP#\1\n        env:\n          ENV: ci/x.sh#" final-gate-shell-env
ltwin flow-step $FG $FG 's#^      - name: Final gate$#      - {name: Final gate, "if": false}#' final-gate-flow-style
# C3c strict line shape (final-gate.yml): YAML syntax the line matcher cannot see is refused (continuation, explicit key, anchor, alias, tag, merge key, odd line breaks)
ltwin shape-continuation-tee $FG $FG 's#^(        run: sh ci/final-gate\.sh)$#\1\n          | tee gate.log#' final-gate-line-shape
ltwin shape-continuation-true $FG $FG 's#^(        run: sh ci/final-gate\.sh)$#\1\n          ; true#' final-gate-line-shape
ltwin shape-explicit-key-if $FG $FG 's#^(      - name: Final gate)$#\1\n        ? if\n        : false#' final-gate-line-shape
ltwin shape-anchor-key $FG $FG 's#^(      - name: Final gate)$#\1\n        \&a if: false#' final-gate-line-shape
ltwin shape-anchor-value $FG $FG 's#^(  pull_request:)$#\1 \&p#' final-gate-line-shape
ltwin shape-alias-value $FG $FG 's#^(    timeout-minutes: 60)$#\1\n    needs: *p#' final-gate-line-shape
ltwin shape-tag-key $FG $FG 's#^(      - name: Final gate)$#\1\n        !!str if: false#' final-gate-line-shape
ltwin shape-tag-value $FG $FG 's#^(    timeout-minutes: 60)$#    timeout-minutes: !!int 60#' final-gate-line-shape
ltwin shape-merge-key $FG $FG 's#^(      - name: Final gate)$#\1\n        <<: *p#' final-gate-line-shape
ltwin shape-lone-cr $FG $FG "s#$G_RUN#\1$(printf '\r')| tee gate.log#" final-gate-line-shape
ltwin shape-unicode-line-sep $FG $FG "s#$G_RUN#\1$(printf '\342\200\250')| tee gate.log#" final-gate-line-shape
ltwin shape-doc-marker $FG $FG 's#^(name: final-gate)$#---\n\1#' final-gate-line-shape
lclean shape-comments-and-blank-lines $FG $FG 's#^(        run: sh ci/final-gate\.sh)$#\1  \# trailing comment\n\n        \# a comment line#'
# C3d exact skeleton (final-gate.yml): the file must equal the expected skeleton, comments and blank lines aside. Every twin
# below passes the older per-line rules (or is only partly caught by them); only the skeleton compare names all of them.
SK=final-gate-skeleton
# F2: the gate's run line nested under the Smoke step's env: (an env var called run, the gate never runs), and inside a multi-line quoted name
ltwin skel-f2-run-under-env $FG $FG 's#^      - name: Final gate$#        env:\n          name: Final gate#;s#^        run: sh ci/final-gate\.sh$#          run: sh ci/final-gate.sh#' $SK
ltwin skel-f2-run-in-quoted-name $FG $FG 's#^      - name: Final gate$#      - name: "Final gate#;s#^        run: sh ci/final-gate\.sh$#        run: sh ci/final-gate.sh\n        x: y"\n      - uses: actions/checkout@v4#' $SK
ltwin skel-f2-run-in-single-quoted-name $FG $FG "s#^      - name: Final gate\$#      - name: 'Final gate#;s#^        run: sh ci/final-gate\.sh\$#        run: sh ci/final-gate.sh\n        x: y'\n      - uses: actions/checkout@v4#" $SK
ltwin skel-f2-run-under-job-env $FG $FG 's#^(    timeout-minutes: 60)$#\1\n    env:\n      run: sh ci/final-gate.sh#;s#^        run: sh ci/final-gate\.sh$#        run: sh ci/workflow-lint.sh#' $SK
# reordered step, added step (all run lines still allowlisted), extra key, changed action version, changed names, changed timeout
ltwin skel-reordered-step $FG $FG 's#run: sh ci/workflow-lint\.sh#run: sh ci/TMPSWAP.sh#;s#run: sh ci/final-gate-smoke\.sh#run: sh ci/workflow-lint.sh#;s#run: sh ci/TMPSWAP\.sh#run: sh ci/final-gate-smoke.sh#' $SK
ltwin skel-added-step $FG $FG "s#$G_STEP#      - name: Extra\n        run: sh ci/extra.sh\n\1#" $SK
ltwin skel-added-step-last $FG $FG "s#$G_RUN#\1\n      - name: After\n        run: sh ci/extra.sh#" $SK
ltwin skel-removed-gate-step $FG $FG 's#^(      - name: Final gate)$#      - name: Smoke again#;s#^        run: sh ci/final-gate\.sh$#        run: sh ci/final-gate-smoke.sh#' $SK
ltwin skel-extra-key-step $FG $FG "s#$G_STEP#\1\n        id: gate#" $SK
ltwin skel-extra-key-job $FG $FG 's#^(    timeout-minutes: 60)$#\1\n    concurrency: x#' $SK
ltwin skel-working-directory $FG $FG "s#$G_STEP#\1\n        working-directory: ci/evil#" $SK
ltwin skel-env-path $FG $FG "s#$G_STEP#\1\n        env:\n          PATH: ci/evil#" $SK
ltwin skel-local-uses-step $FG $FG "s#$G_STEP#      - uses: ./ci/act\n\1#" $SK
ltwin skel-second-job $FG $FG "s#$G_RUN#\1\n  other:\n    runs-on: ubuntu-latest\n    steps:\n      - run: sh ci/rust-test.sh#" $SK
ltwin skel-matrix $FG $FG 's#^(    timeout-minutes: 60)$#\1\n    strategy:\n      matrix:\n        a: [1]#' $SK
ltwin skel-action-version $FG $FG 's#actions/checkout@v4#actions/checkout@v5#' $SK
ltwin skel-action-unpinned $FG $FG 's#actions/checkout@v4#actions/checkout@main#' $SK
ltwin skel-fetch-depth $FG $FG 's#fetch-depth: 2#fetch-depth: 1#' $SK
ltwin skel-timeout $FG $FG 's#timeout-minutes: 60#timeout-minutes: 10#' $SK
ltwin skel-workflow-name $FG $FG 's#^name: final-gate$#name: Final gate#' $SK
ltwin skel-job-id $FG $FG 's#^  final-gate:$#  final-gate2:#' $SK final-gate-job-name
ltwin skel-runs-on $FG $FG 's#runs-on: ubuntu-latest#runs-on: self-hosted#' $SK
ltwin skel-trigger-added $FG $FG "s#$PRL#  pull_request:\n  push:#" $SK
ltwin skel-step-name $FG $FG 's#^      - name: Final gate$#      - name: Gate#' $SK
ltwin skel-crlf $FG $FG "s#\$#$(printf '\r')#" $SK final-gate-line-shape
ltwin skel-trailing-ls $FG $FG "s#$G_RUN#\1$(printf '\342\200\250')#" $SK final-gate-line-shape
ltwin skel-bom $FG $FG "1s#^#$(printf '\357\273\277')#" $SK final-gate-line-shape
ltwin skel-tab-in-value $FG $FG "s#^(        run: sh ci/final-gate\.sh)\$#        run:$(printf '\t')sh ci/final-gate.sh#" $SK
ltwin skel-end-marker $FG $FG 's#^(        run: sh ci/final-gate\.sh)$#\1\n...#' $SK
# clean twins: whole-line comments (quotes, pipes and run-looking text inside them), blank lines, trailing comments, trailing spaces, a tab-indented comment
lclean skel-comments-blanks-trailing-space $FG $FG "s#$G_STEP#\# note: run: sh ci/evil.sh | \"x\"\n\n\1   #;s#^(    runs-on: ubuntu-latest)\$#\1 \# trailing#;s#^(      - name: Smoke)\$#$(printf '\t')\# tab comment\n\1#"
# a symlinked final-gate.yml is refused whatever it points at (even a byte-for-byte good target)
mkdir "$tmp/lsym" && cp "$wf" "$tmp/lsym/real.yml" && ln -s real.yml "$tmp/lsym/final-gate.yml"
lo=$(sh "$here/workflow-lint.sh" "$tmp/lsym/final-gate.yml" 2>&1); lrc=$?
if [ "$lrc" -eq 1 ] && printf '%s\n' "$lo" | grep -qE '^final-gate\.yml:1:final-gate-skeleton$'; then ok C-twin-skel-symlink-refused; else bad C-twin-skel-symlink-refused; fi
# maintainer path: the printed skeleton of the real file, saved as final-gate.yml, is itself clean (the embedded copy is what the real file prints)
mkdir "$tmp/ps" && sh "$here/workflow-lint.sh" --print-skeleton "$wf" > "$tmp/ps/final-gate.yml" 2>/dev/null
if [ -s "$tmp/ps/final-gate.yml" ] && sh "$here/workflow-lint.sh" "$tmp/ps/final-gate.yml" >/dev/null 2>&1; then ok C3d-print-skeleton-round-trip-clean; else bad C3d-print-skeleton-round-trip-clean; fi

# C4 caches and required-name collisions
ltwin cache-actions $FG $FG "s#$G_STEP#      - uses: actions/cache@v4\n\1#" cache-action
ltwin cache-swatinem $FG $FG "s#$G_STEP#      - uses: Swatinem/rust-cache@v2\n\1#" cache-action
ltwin cache-input $FG $FG 's#^(          fetch-depth: 2)$#\1\n          cache: npm#' cache-input
ltwin name-gate-other-workflow rust-test.yml rust-test.yml 's#^  test:$#  test:\n    name: gate#' required-name-gate
ltwin job-id-gate $FG $FG 's#^  final-gate:$#  gate:#' required-name-gate final-gate-job-name
ltwin name-final-gate-other-workflow rust-test.yml rust-test.yml 's#^  test:$#  test:\n    name: final-gate#' required-name-final-gate
ltwin job-id-final-gate-other-workflow rust-test.yml rust-test.yml 's#^  test:$#  final-gate:#' required-name-final-gate
ltwin final-gate-job-renamed $FG $FG 's#^    runs-on: ubuntu-latest$#    name: Final gate\n    runs-on: ubuntu-latest#' final-gate-job-name
lclean step-named-gate rust-test.yml rust-test.yml 's#name: Build and test#name: gate#'
lclean step-named-final-gate rust-test.yml rust-test.yml 's#name: Build and test#name: final-gate#'

if [ "$fails" -eq 0 ]; then
  echo "final-gate smoke: all arms ok ($oks arms)"
  exit 0
fi
echo "final-gate smoke: $fails arm(s) FAILED"
exit 1
