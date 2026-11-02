#!/bin/sh
# Live-canary log check. Usage: ci/gate-log-check.sh <saved log file> <canary word> <run conclusion>
# Used on the saved log of a gate run for a throwaway PR (ticket 163, AC 5(d)/(g)). It passes only when the run really scanned:
#   conclusion is exactly "success", AND the engine's own lines show files=N and scanned_commits=N with N >= 1 (an echoed run
#   line proves nothing: it is printed even when the step stops early), AND the canary word appears 0 times.
# Exit 0 pass, 1 fail (prints which check, never log content), 2 usage.
if [ "$#" -ne 3 ]; then
  echo "usage: gate-log-check.sh <log> <canary> <conclusion>" >&2
  exit 2
fi
log=$1
canary=$2
concl=$3
[ -f "$log" ] || { echo "gate-log-check: log missing" >&2; exit 1; }
[ -n "$canary" ] || { echo "gate-log-check: canary word is empty" >&2; exit 1; }
[ "$concl" = "success" ] || { echo "gate-log-check: conclusion is not success" >&2; exit 1; }
maxcount() {
  awk -v k="$1=" '{
    s = $0
    while ((i = index(s, k)) > 0) {
      pre = (i > 1) ? substr(s, i - 1, 1) : " "
      rest = substr(s, i + length(k))
      if (pre !~ /[A-Za-z0-9_]/ && match(rest, /^[0-9]+/)) {
        v = substr(rest, 1, RLENGTH) + 0
        if (!got || v > m) m = v
        got = 1
      }
      s = rest
    }
  } END { if (got) print m }' "$log"
}
f=$(maxcount files)
c=$(maxcount scanned_commits)
[ -n "$f" ] && [ "$f" -ge 1 ] || { echo "gate-log-check: no files= count of 1 or more in the log" >&2; exit 1; }
[ -n "$c" ] && [ "$c" -ge 1 ] || { echo "gate-log-check: no scanned_commits= count of 1 or more in the log" >&2; exit 1; }
n=$(command grep -c -F -- "$canary" "$log")
[ "$n" -eq 0 ] 2>/dev/null || { echo "gate-log-check: canary found in the log" >&2; exit 1; }
echo "gate-log-check: ok files=$f scanned_commits=$c canary=0"
exit 0
