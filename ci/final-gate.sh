#!/bin/sh
# Remote final gate entry. Usage: ci/final-gate.sh [repo-dir] | ci/final-gate.sh --print-ticket
# Builds the engine from locked inputs, writes a fixed synthetic ticket into a fresh temp folder outside the repo
# (nothing in it comes from the repo under test), runs `harness ticket gate` on repo-dir (default: this repo) with
# --base = the first parent of its HEAD (the merge commit's base), and exits with the runner's exit code, unchanged.
# After a PASS it prints the written result's message line (gate ids only), read from its own fresh result folder.
# Fails closed when the base cannot be resolved (a shallow checkout): it never falls back to an empty diff.
# --print-ticket prints the ticket this script would write and exits 0 (used by ci/final-gate-smoke.sh).
# Exit: the runner's code (0 PASS only, 1 FAIL, 2 error); 2 on a missing base or any setup error.
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd) || exit 2

emit_ticket() {
  printf '%s\n' '{"id":"ci-pr","repo_id":"core","title":"Remote final gate","group":"ci","size":"M","objective":"Re-check the pull request on a clean machine with the same gate runner.","acceptance_criteria":["Every selected gate passes."],"dependencies":[],"risk":"normal","owner_role":"execution_verification","evidence_links":[]}'
}

if [ "${1:-}" = "--print-ticket" ]; then
  emit_ticket
  exit 0
fi

# No private input: the same privacy env names ci/rust-test.sh scrubs.
unset HARNESS_PRIVACY_LIST HARNESS_PRIVACY_OLD_LIST HARNESS_PRIVACY_LIST_SECRET

repo=${1:-"$here/.."}
repo=$(CDPATH= cd -- "$repo" && pwd -P) || { echo "final-gate: repo folder not found" >&2; exit 2; }

(cd "$here/../core/rust" && cargo build --locked) || { echo "final-gate: engine build failed" >&2; exit 2; }
# cargo puts the binary under CARGO_TARGET_DIR when it is set (a relative value is relative to core/rust, where cargo runs).
tdir=${CARGO_TARGET_DIR:-}
case $tdir in
  "") tdir="$here/../core/rust/target" ;;
  /*) ;;
  *) tdir="$here/../core/rust/$tdir" ;;
esac
bin="$tdir/debug/harness"

base=$(git -C "$repo" rev-parse --verify --quiet 'HEAD^1^{commit}' 2>/dev/null) || base=""
case "$base" in
  *[!0-9a-f]*) base="" ;;
esac
if [ "${#base}" -ne 40 ] && [ "${#base}" -ne 64 ]; then
  echo "final-gate: base commit missing (no first parent of HEAD; shallow checkout?)" >&2
  exit 2
fi

work=$(mktemp -d "${TMPDIR:-/tmp}/final-gate.XXXXXX") || { echo "final-gate: no temp folder" >&2; exit 2; }
trap 'rm -rf "$work"' EXIT HUP INT TERM
mkdir "$work/result" || exit 2
mkdir "$work/caps" || exit 2
emit_ticket > "$work/ticket.json" || exit 2

out=$("$bin" ticket gate "$work/ticket.json" --repo-map "core=$repo" --base "$base" --result-dir "$work/result" --caps-dir "$work/caps")
rc=$?
printf '%s\n' "$out"
if [ "$rc" -eq 0 ]; then
  msg=$(sed -n 's/.*"message":"\(checks passed: [^"]*\)".*/\1/p' "$work/result/ci-pr.json")
  # printf adds the final newline the result file lacks, so a following line never joins this one.
  [ -z "$msg" ] || printf '%s\n' "$msg"
fi
exit "$rc"
