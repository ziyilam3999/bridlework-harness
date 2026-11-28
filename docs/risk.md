# harness ticket risk

Prints a risk class for the diff between two commits of one repo: `low` or `high`. The class is computed
from the changed paths and never from the ticket's declared `risk` field alone. The same inputs give the same
class and the same bytes. The verb decides no review and marks no finding: a caller uses the last line as one
input (the 1-in-5 spot-check picks its pool from it; `risk=high` is the definition of a risk-routed job).

The rule has two ends:

- the low end: a diff is `low` only when every changed path is covered by a `low` entry of the risk path map
  and the ticket's risk is `normal`;
- the high end: a path under `gates/`, a path the map does not cover, a path under a `high` entry, a ticket
  that declares `high`, or an accepted override that raises the class gives `high`. An accepted override
  that keeps the class leaves it as it was: `--override low` on a low diff stays `low`.

## Synopsis

```
usage: harness ticket risk <ticket-file> --repo-dir <absolute-dir> --from <full-sha> --to <full-sha> [--override <low|high> --by <jev|model|human> --reason <text> --log <absolute-file>]
```

- `<ticket-file>`: the ticket file, read with the strict ticket reader (`docs/ticket-contract.md`). Only its
  `id` and `risk` are used. The id must be a plain token (1 to 200 bytes, no blank, no control character),
  because it is printed.
- `--repo-dir`: a git repo or a linked work tree. Absolute, with no `..` component, and the top of the repo
  or work tree: a folder inside is refused (exit 2), because the changed paths are named from the top.
- `--from`: 40 lowercase hex characters, the commit whose map judges the diff (the base).
- `--to`: 40 lowercase hex characters, the other commit (the head). It need not descend from `--from`; the
  two trees are compared.
- `--override`, `--by`, `--reason`, `--log`: the override, all four or none (see Overrides).

Each flag is given at most once. A flag-shape error prints the usage line above on stderr and exits 2 with
stdout empty: an unknown flag (also `--from=x`), a flag with no value, a flag given twice, no ticket file or
more than one, a relative `--repo-dir` or `--log`, a `--from` or `--to` that is not a full sha, a partial
override, an `--override` that is not `low` or `high`, a `--by` that is not `jev`, `model` or `human`, and a
`--reason` that is empty or blank, holds a control character (so no second line) or is over 200 bytes.

## The risk path map

The map is `gates/risk-map.txt` of the repo. It is read from the `--from` commit, from the object store: never
from the work tree and never from `--to`. A diff therefore cannot change the map that judges it. An edit to
the map is itself risk-routed (the map file is high, see below).

Line format: one entry per line, `<repo-relative path> <low|high>`, with one space.

```
# a line that starts with # is a comment
docs low
README.md low
ci high
```

- A blank line and a line that starts with `#` are skipped.
- The path is plain: `/`-separated, no empty, `.` or `..` part, no `.git` part, no backslash and no control
  character. It is not a glob and it is case-sensitive.
- Match rule (the rule of `gates/path-map.json`, reused and not that file): byte-exact and whole path
  components. An entry covers the path itself and everything below it, never a substring (`docs` covers
  `docs/a.md`, not `docsx/a.md`). When entries nest, the longest (most specific) one answers: with `docs low`
  and `docs/api high`, `docs/api/x.md` is high and `docs/other/x.md` is low.
- The map must hold at least one entry. A path may be given once.

The map file must be a plain file (mode `100644` or `100755`) of at most 1 MiB. The file name ends in `.txt`
on purpose: the gate loader turns every `gates/*.json` into a gate and reserves only `path-map.json`, so a
JSON name there would break the gate load. It is not a second config file: it is one input of this verb.

## Class of one changed path

The first rule that matches answers:

| Order | Changed path | Class | why |
|---|---|---|---|
| 1 | `gates/risk-map.txt` | high | `risk-map` |
| 2 | the `gates` folder itself and any other path under `gates/` | high | `gates-path` |
| 3 | a path no map entry covers | high | `unmapped` |
| 4 | a path under a `high` entry | high | `mapped-high` |
| 5 | a path under a `low` entry | low | `mapped-low` |

Rules 1 and 2 are built in: no map entry can lower them. Rule 3 fails closed: a path the map forgot is
risk-routed, never low. A path that both a `low` and the built-in rule could cover (`gates low` in the map)
is high.

Changed paths come from a raw tree compare with renames off, so a rename gives the old and the new path and a
deleted path counts. Every entry counts, whatever its type or mode change (an added, deleted, modified,
symlink or submodule entry).

## Output

Exit 0 with stderr empty and stdout in this shape, one line per changed path in git's order, then the
summary line last:

```
path=<path> class=<low|high> why=<token>
risk=<low|high> ticket=<id> from=<sha> to=<sha> files=<n> why=<token>
```

The summary `why` token is one of:

- `override`: an override was accepted (it raised the class or kept it).
- `ticket-high`: the ticket declares `risk: high` and no override was given.
- `risk-map`, `gates-path`, `unmapped`, `mapped-high`: the why of the first high path in git's order (the
  ticket declares `normal`).
- `mapped-low`: every path is low and the ticket declares `normal`.

So the order is `override`, else `ticket-high`, else the why of the first high path, else `mapped-low`. The
per-path lines are printed in every case, so a `ticket-high` summary still shows an `unmapped` path on its
own line. `files` is the number of changed paths. A diff with no changed path (two commits with equal trees)
is `risk=low ... files=0 why=mapped-low` for a ticket that declares `normal`.

## Overrides

An override can only raise.

- An override lower than the computed class is refused: exit 1, stdout empty, nothing written to `--log`,
  and one stderr line, exactly:

  ```
  harness: risk refused: override-lowers computed=high override=low
  ```

- An override equal to or higher than the computed class is accepted: the class is the override's, the
  summary why is `override`, and one line is appended to `--log`:

  ```
  risk-override ticket=<id> from=<sha> to=<sha> computed=<c> override=<o> by=<who> effect=<raised|same> reason=<text>
  ```

The computed class is the class before the override: high when any path is high or the ticket declares
`high`. So a `low` override is refused for a ticket that declares `high`, even on a low diff: the declared
field can raise and never lower. `--by` names who overrode (`jev`, `model` or `human`); the verb checks
only that it is one of the three, it does not check who is calling. `effect` is `raised` or `same`.

The log is the record: there is no event-log kind for an override. The append is the only write of the verb.
`--log` is absolute with no `..` component and must be a plain file (it is created when missing). A symlink
is refused, also one that points at a plain file. A file that does not end with a newline gets one first, so
each record is a whole line. Each accepted call appends one line.

## Order of steps

The first failing step gives one stderr line and stops.

1. Flag shape (exit 2, the usage line).
2. The ticket file: missing, not a plain file, over 1 MiB, not a valid ticket, or an id that is not a plain
   token (exit 2).
3. Setup (exit 2, `harness: error: <what>`, no path printed): the repo dir is missing or not a folder, it is
   not a git repo, it is a folder inside the repo and not its top, git cannot run or a call fails, `--from`
   or `--to` is not a commit of the repo.
4. `--from` equal to `--to`: `--from equals --to`.
5. The map at `--from`: missing or not a plain file, over 1 MiB, not UTF-8, a malformed line, a path given
   twice, or no entry. The line names the map and the bad line by number, never its text.
6. A changed path that is not UTF-8 or holds a control character (it could not be printed as one line).
7. An override that lowers (exit 1).
8. The log append of an accepted override: `--log` is a symlink, is not a plain file or cannot be written
   (exit 2, nothing on stdout).
9. Done: stdout, exit 0.

An unmapped changed path is never a refusal: it is high.

## How git is read

Each git call runs on the repo dir, removes each inherited variable whose name starts with `GIT_`, gets no
stdin and runs with `--no-replace-objects` and `--no-lazy-fetch`. It reads only: no fetch, no ref, config,
index or work tree write. The map comes from `ls-tree --full-tree` (the path is named from the top of the
repo) and `cat-file` of `--from`, the changed paths from `diff-tree --raw` with
`--ignore-submodules=none`, so a `submodule.<name>.ignore` setting of `.gitmodules` does not hide a
submodule entry. The work tree is not read.

## Exit codes

- `0`: the class is printed.
- `1`: an override that lowers is refused (one stderr line, stdout empty, no log line).
- `2`: a usage error (the usage line) or a setup error (`harness: error: <what>`): one stderr line, stdout
  empty, no log line.

A caller that needs a pool reads the last line of a run that exited 0.

## Not covered

- The size rule and a refused size-lowering override (a separate ticket).
- The command-class rule (`rm`, force-push, history rewrite), multi-repo diffs and protected
  acceptance-criteria edits.
- Globs: the map uses whole-component paths, like `gates/path-map.json`.
- The map edit as a hand-back-is-data write: the map is a file of the repo, changed by a pull request that
  is itself risk-routed.
- The ticket's `repo_id` is not checked against `--repo-dir`; the caller passes the matching pair.
- No event-log kind for an override: the `--log` line is the record.
- Whether `--to` descends from `--from`: two commits are compared, the caller picks the pair.
- Wiring into bw, the gate or the spot-check; a risk map for another repo (each repo adds its own
  `gates/risk-map.txt`, and the verb refuses a `--from` before the map exists).
- It says nothing about whether a change is safe: a `low` diff is one input to a review decision, not the
  decision.
