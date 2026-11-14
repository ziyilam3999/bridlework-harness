# Example project

A tiny repo for one ticket. `src/greeting.txt` holds `todo`; the gate `hello` (`gates/hello.json`) runs `checks/hello.sh`,
which passes only when the file says `hello`. The gate is `both_ends`: it must fail before the change and pass after it.

The ticket is `../tickets/ex-1.json`. It lives outside this repo because the pre-completion hook refuses a ticket file inside
the repo it guards.

## The commands the clean-install test runs

`ci/clean-install/e2e.sh` (phase `ticket`) does the following, with `HARNESS_BIN` pointing at the installed engine:

1. `git init`, commit this folder as the base commit.
2. `harness ticket intake ../tickets/ex-1.json --repo-map example=<this repo>`
3. `harness workspace create ../tickets/ex-1.json --repo-map example=<this repo> --base <sha> --workspace-root <dir> --manifest-dir <dir>`
4. The pre-completion hook (`plugins/claude/pre-completion-hook.sh`) before any gate: refuses (exit 2).
5. Commit a wrong change on branch `ws/ex-1`, fast-forward it into this repo, `harness ticket gate ...`: exit 1, `result=fail`.
   The hook refuses (exit 2).
6. Commit the right change (`hello` in `src/greeting.txt`), fast-forward, `harness ticket gate ...`: exit 0, `done=allowed`.
   The hook allows (exit 0).
7. `harness status ...` prints `last_gate=pass gate_current=yes`.

No privacy list, no config overlay and no network are needed.
