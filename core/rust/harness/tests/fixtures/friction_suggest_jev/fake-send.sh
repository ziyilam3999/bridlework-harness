#!/usr/bin/env bash
# The fake send program of the friction suggest Jev tests (one fixture, many cases). It makes no network
# call, holds no real key and speaks synthetic text only. The engine clears its environment, so it learns
# its case from files beside its own copy: a `case` file, and where the case needs them `answer.json`,
# `real-answer.json`, `stderr.txt`, `sleep-secs` and `record-path`. On each start it writes down, in `notes/` beside it: that
# it ran, its arguments, its environment, the value of OPENROUTER_KEY_FILE, its current folder, the mode
# strings of the temp folder and of the request file, a copy of the request, and a copy of the engine's
# stdout file when a file named `engine-stdout` beside it holds that file's path.
set -u
here="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
notes="$here/notes"
mkdir -p "$notes"
# The group-kill case starts its background sleep and writes its id first: a short deadline may fire early.
if [ "$(cat "$here/case")" = "hang-bg" ]; then
  sleep 30 &
  printf '%s\n' "$!" > "$notes/bg-pid"
fi
: > "$notes/ran"
printf '%s\n' "$@" > "$notes/args"
env | sort > "$notes/env"
env | cut -d= -f1 | sort > "$notes/env-names"
printf '%s\n' "${OPENROUTER_KEY_FILE-}" > "$notes/key-file"
printf '%s\n' "$PWD" > "$notes/cwd"
payload=""
out=""
prev=""
for a in "$@"; do
  case "$prev" in
    --payload) payload="$a" ;;
    --out) out="$a" ;;
  esac
  prev="$a"
done
mode_of() {
  if stat -f "%Sp" "$1" >/dev/null 2>&1; then stat -f "%Sp" "$1"; else stat -c "%A" "$1"; fi
}
if [ -n "$payload" ]; then
  mode_of "$(dirname -- "$payload")" > "$notes/dir-mode"
  mode_of "$payload" > "$notes/request-mode"
  cp "$payload" "$notes/request-copy"
fi
if [ -f "$here/engine-stdout" ]; then
  cp "$(cat "$here/engine-stdout")" "$notes/engine-stdout-copy"
fi
case_word="$(cat "$here/case")"
case "$case_word" in
  answer)
    cp "$here/answer.json" "$out"
    exit 0
    ;;
  answer-link)
    # A symbolic link in the place of the answer file: the engine must read it as bad-answer.
    ln -s "$here/real-answer.json" "$out"
    exit 0
    ;;
  delay-answer)
    sleep "$(cat "$here/sleep-secs")"
    cp "$here/answer.json" "$out"
    exit 0
    ;;
  bg-answer)
    # A background sleep that holds the stderr pipe open, then a good answer all the same.
    sleep 30 >&2 &
    printf '%s\n' "$!" > "$notes/bg-pid"
    cp "$here/answer.json" "$out"
    exit 0
    ;;
  hang)
    # Leaves a file behind in its TMPDIR, then hangs until the deadline kills the group.
    : > "${TMPDIR:-/tmp}/left-by-fake"
    ls -A "${TMPDIR:-/tmp}" > "$notes/tmpdir-listing"
    sleep 30
    ;;
  hang-bg)
    sleep 30
    ;;
  record-dir)
    # Replaces the engine's record file by a folder, then answers: the second record append must fail.
    rp="$(cat "$here/record-path")"
    rm -f "$rp"
    mkdir "$rp"
    cp "$here/answer.json" "$out"
    exit 0
    ;;
  no-file)
    exit 0
    ;;
  signal)
    kill -TERM "$$"
    sleep 5
    ;;
  exit*)
    cat "$here/stderr.txt" >&2
    cat "$here/stderr.txt"
    exit "${case_word#exit}"
    ;;
esac
exit 0
