#!/bin/sh
# Test-list ratchet gate (head only). Usage: gates/test-ratchet.sh
# Lists the tests of core/rust at the head (in place) and at the base (an export in a temp folder), then
# fails when a base test is gone and the ticket does not name it (lost-test), or when the ticket names a
# test that is not lost (named-not-removed). A test id is <source path>::<listed test path>.
# Env from the engine: HARNESS_GATE_BASE, HARNESS_GATE_HEAD (full lowercase sha), HARNESS_GATE_REMOVED_TESTS
# (newline joined ids, may be empty but must be set). Any other state fails closed.
unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE GIT_COMMON_DIR
fail() {
    echo "test-ratchet: $*" >&2
    exit 1
}
sha_ok() {
    case $1 in
    *[!0123456789abcdef]*) return 1 ;;
    esac
    [ "${#1}" -eq 40 ]
}
sha_ok "${HARNESS_GATE_BASE-}" || fail "HARNESS_GATE_BASE is unset or not a full lowercase sha"
sha_ok "${HARNESS_GATE_HEAD-}" || fail "HARNESS_GATE_HEAD is unset or not a full lowercase sha"
[ -n "${HARNESS_GATE_REMOVED_TESTS+x}" ] || fail "HARNESS_GATE_REMOVED_TESTS is unset"
base=$HARNESS_GATE_BASE
head=$HARNESS_GATE_HEAD
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd) || fail "cannot find the repo folder"
git -C "$root" cat-file -e "$base^{commit}" 2>/dev/null || fail "the base is not a commit of this clone"
[ "$(git -C "$root" rev-parse HEAD 2>/dev/null)" = "$head" ] || fail "HARNESS_GATE_HEAD is not the folder HEAD"

tmp=$(mktemp -d "${TMPDIR:-/tmp}/test-ratchet.XXXXXX") || fail "cannot make a temp folder"
trap 'rm -rf "$tmp"' EXIT
trap 'exit 1' HUP INT TERM

# list_tests <workspace parent> <cargo target folder or empty> <out file>: sorted unique test ids.
list_tests() {
    (
        cd "$1/core/rust" || exit 1
        if [ -n "$2" ]; then
            CARGO_TARGET_DIR=$2
            export CARGO_TARGET_DIR
        fi
        cargo test --locked --offline -- --list >"$tmp/raw" 2>&1 || exit 1
    ) || {
        tail -n 20 "$tmp/raw" >&2
        return 1
    }
    awk '
        /^ +Running / { s = $0; sub(/^ +Running /, "", s); sub(/ \(.*$/, "", s); sub(/^unittests /, "", s); cur = s; next }
        /^ +Doc-tests / { cur = ""; next }
        /: test$/ { if (cur != "") { t = $0; sub(/: test$/, "", t); print cur "::" t } }
    ' "$tmp/raw" | sort -u >"$3"
}

mkdir "$tmp/base" || fail "cannot make the base folder"
git -C "$root" archive --format=tar -o "$tmp/base.tar" "$base" core/rust || fail "cannot export the base"
tar -xf "$tmp/base.tar" -C "$tmp/base" || fail "cannot unpack the base"
list_tests "$tmp/base" "$tmp/target-base" "$tmp/base.ids" || fail "the test list cannot be built at the base"
[ -s "$tmp/base.ids" ] || fail "the base test list holds no test id"
list_tests "$root" "" "$tmp/head.ids" || fail "the test list cannot be built at the head"

printf '%s\n' "$HARNESS_GATE_REMOVED_TESTS" | sed '/^$/d' | sort -u >"$tmp/named.ids"
comm -23 "$tmp/base.ids" "$tmp/head.ids" >"$tmp/lost.ids"
comm -23 "$tmp/lost.ids" "$tmp/named.ids" >"$tmp/unnamed.ids"
comm -23 "$tmp/named.ids" "$tmp/lost.ids" >"$tmp/kept.ids"
bad=0
if [ -s "$tmp/unnamed.ids" ]; then
    echo "test-ratchet: class lost-test: a base test is gone and the ticket does not name it:" >&2
    head -n 20 "$tmp/unnamed.ids" >&2
    bad=1
fi
if [ -s "$tmp/kept.ids" ]; then
    echo "test-ratchet: class named-not-removed: the ticket names a test that is not lost:" >&2
    head -n 20 "$tmp/kept.ids" >&2
    bad=1
fi
exit $bad
