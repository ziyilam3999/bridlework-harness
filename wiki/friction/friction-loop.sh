#!/usr/bin/env bash
# friction-loop.sh (ticket 3601, A5): the weekly friction runner. A TEMPLATE: every path comes from a private env
# file (or the environment); nothing is hard-coded and no data lives in this repo.
#
#   friction-loop.sh --week <iso-year>-w<ww> [--env <file>]
#
# The env file is sourced; it must define (private values, never committed here):
#   HARNESS_BIN     the harness binary
#   HARNESS_CONFIG  the private harness config (its friction.inbox points at <wiki>/raw/friction-inbox.jsonl)
#   WIKI_DIR        the private friction wiki (a git work tree)
#   HANDOFF_DIR     where the ticket-contract hand-off files are written (an existing folder)
#   RECONCILE_PY    ticket-reconcile's reconcile.py
#   TASKS_ROOT      the task store root (<root>/<session>/<id>.json)
#   SESSION_ID      the orchestrator session whose bucket reconcile reports (a launchd run has none of its own)
# and may define: EVENT_LOG (an event log to sweep first), WIKI_LINT_CMD (the wiki lint command; the wiki dir is
# appended), CLOSED_FEED_PY (default: closed-feed.py beside this script), TOP (the --top K override).
#
# One cycle: closed feed (reconcile --json  UNION  completed bound records) -> `harness friction triage` ->
# `harness friction page` -> wiki lint -> ONE commit to the private wiki. Every step is fail-closed (set -e): a
# failed step stops the run before the commit, so the wiki never holds a half-written week.
set -euo pipefail

WEEK=""
ENVFILE=""
while [ $# -gt 0 ]; do
  case "$1" in
    --week) WEEK="${2:-}"; shift 2 ;;
    --env) ENVFILE="${2:-}"; shift 2 ;;
    *) echo "usage: friction-loop.sh --week <iso-year>-w<ww> [--env <file>]" >&2; exit 2 ;;
  esac
done
if ! printf '%s' "$WEEK" | grep -Eq '^[0-9]{4}-w[0-9]{2}$'; then
  echo "friction-loop: --week must look like 2026-w45" >&2
  exit 2
fi
if [ -n "$ENVFILE" ]; then
  # shellcheck disable=SC1090
  . "$ENVFILE"
fi
for v in HARNESS_BIN HARNESS_CONFIG WIKI_DIR HANDOFF_DIR RECONCILE_PY TASKS_ROOT SESSION_ID; do
  if [ -z "${!v:-}" ]; then
    echo "friction-loop: $v is not set (put it in the private env file)" >&2
    exit 2
  fi
done
HERE="$(cd "$(dirname "$0")" && pwd)"
FEED="${CLOSED_FEED_PY:-$HERE/closed-feed.py}"
STATE="$WIKI_DIR/raw/triage-state.jsonl"
PAGE="$WIKI_DIR/triage-$WEEK.md"
export HARNESS_CONFIG

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# 1. the closed feed: reconcile's bucket UNION the completed records of the bound groups
python3 -I "$FEED" --reconcile "$RECONCILE_PY" --tasks-root "$TASKS_ROOT" --state "$STATE" --session "$SESSION_ID" > "$WORK/closed.json"

# 2. triage (the optional event-log sweep runs first, inside the verb)
set -- friction triage --week "$WEEK" --state "$STATE" --handoff-dir "$HANDOFF_DIR" --closed "$WORK/closed.json"
if [ -n "${EVENT_LOG:-}" ]; then set -- "$@" --log "$EVENT_LOG"; fi
if [ -n "${TOP:-}" ]; then set -- "$@" --top "$TOP"; fi
"$HARNESS_BIN" "$@"

# 3. the weekly page (the lesson text goes only to the page file)
"$HARNESS_BIN" friction page --week "$WEEK" --state "$STATE" --out "$PAGE"

# 4. index and log lines (once per week), then the wiki lint
TODAY="$(date -u +%F)"
if ! grep -q "(triage-$WEEK.md)" "$WIKI_DIR/index.md" 2>/dev/null; then
  printf -- '- [triage-%s](triage-%s.md) -- friction triage %s (updated %s; verified no)\n' "$WEEK" "$WEEK" "$WEEK" "$TODAY" >> "$WIKI_DIR/index.md"
fi
if ! grep -q "ingest | friction triage $WEEK\$" "$WIKI_DIR/log.md" 2>/dev/null; then
  printf '\n## [%s] ingest | friction triage %s\n' "$TODAY" "$WEEK" >> "$WIKI_DIR/log.md"
fi
if [ -n "${WIKI_LINT_CMD:-}" ]; then
  # shellcheck disable=SC2086
  $WIKI_LINT_CMD "$WIKI_DIR"
fi

# 5. ONE commit to the private wiki (nothing when the week changed nothing)
git -C "$WIKI_DIR" add raw "triage-$WEEK.md" index.md log.md
if git -C "$WIKI_DIR" diff --cached --quiet; then
  echo "friction-loop: nothing to commit for $WEEK"
else
  git -C "$WIKI_DIR" commit -q -m "friction triage $WEEK"
  echo "friction-loop: committed $WEEK"
fi
