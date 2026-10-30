#!/bin/sh
# CI privacy gate entry. Usage: ci/privacy-gate.sh [engine scan args]   (default: scan the tree and all history)
# The deny list comes ONLY from the HARNESS_PRIVACY_LIST_SECRET variable. Unset, empty or blank = FAIL (never skip).
# Prints only the engine's file:line:class-id lines on stdout. Exit 0 clean, 1 on any hit or error.
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd) || exit 1
eng="$here/../core/rust/target/release/harness"
if [ ! -x "$eng" ]; then
  echo "privacy-gate: engine missing; build it first" >&2
  exit 1
fi
if [ -z "${HARNESS_PRIVACY_LIST_SECRET+x}" ]; then
  echo "privacy-gate: list secret is not set" >&2
  exit 1
fi
case "$HARNESS_PRIVACY_LIST_SECRET" in
  *[![:space:]]*) ;;
  *) echo "privacy-gate: list secret is empty" >&2; exit 1 ;;
esac
rc=0
if [ "$#" -gt 0 ]; then
  "$eng" privacy scan "$@" </dev/null || rc=1
else
  "$eng" privacy scan --tree </dev/null || rc=1
  "$eng" privacy scan --range HEAD </dev/null || rc=1
fi
if [ "$rc" -ne 0 ]; then
  echo "privacy-gate: refused" >&2
fi
exit "$rc"
