# Lanes

`harness lane claim | release | list` keeps the live tickets and their lane under a cap. The cap is one constant (`CAP` in `lane.rs`), fixed at 1 in this slice (no admission exists yet).

```
harness lane claim <ticket-file> --lanes-dir <abs dir> --config <file> --scope-file <file>
harness lane release <ticket-id> --lanes-dir <abs dir>
harness lane list --lanes-dir <abs dir> --config <file>
```

- `claim` prints `ticket=<id> lane=<n>` (the lowest free lane). The ticket goes through the strict ticket reader.
- The scope file holds one relative path per line. A truly empty line is skipped; a line of only whitespace is refused. A missing or empty file, an entry starting with `/` or `\`, a `..` part (split on `/` and `\`), an empty part (`a//b`) or a control character is refused with exit 2 and nothing is recorded. One trailing `/` is allowed (a folder, `a/`). Entries are stored as given (`./a`, `a/`).
- `list` prints one line per live ticket, `ticket=<id> lane=<n> repo_id=<id> scope=<entries>`, then a last line `live=<n> cap=<n>`.
- `release` prints `ticket=<id> released`.

## Record format

The lanes dir is created by `claim` when missing. One file per held lane, `<lanes-dir>/lane-<n>`, with n written without a leading zero and from 1 to the cap. Text:

```
lane-record v1
ticket=<id>
repo_id=<id>
scope=<entry>        (one line per entry, at least one)
```

A ticket id is letters, digits, `.`, `_`, `-` (at most 128, not starting with `.`).

## Concurrency

`claim`, `release` and `list` each run as one critical section under an exclusive lock (`flock`) on the lanes dir itself; no lock file is created. Under the lock a claim checks `already-live`, then `cap-full`, then writes the record to `.claim.tmp` and renames it onto `lane-<n>`; a release removes only the record it just read. So with any cap a ticket never holds two lanes, live never exceeds the cap, and a release never removes a record that now belongs to another ticket, whatever number of `harness` processes run at once. A stale `.claim.tmp` of a killed claim is removed by the next claim under the lock, so at most one is left. No directory fsync is done: a claim or release is not guaranteed to survive a crash of the machine.

## Reading the lanes dir (fail closed)

`list`, `claim` and `release` exit 2 with one `lane: error: ...` line, and change nothing in the dir, when the lanes dir is a symbolic link or not a directory, or holds an entry named `lane-...` that is not a regular file, cannot be read, does not parse, is not named `lane-<n>` with n from 1 to the cap (so `lane-0`, `lane-01`, `lane-99` are refused), or repeats a ticket held by another record. Other names (such as a stale `.claim.tmp`) are ignored.

## Exit codes

| code | meaning |
| --- | --- |
| 0 | done |
| 1 | policy refusal: exactly one stderr line starting `lane: refused reason=<token>` |
| 2 | usage error, or unreadable or refused input |

## Refusal tokens

- `cap-full`: every lane under the cap is held.
- `already-live`: the ticket already holds a lane.
- `not-live`: `release` for a ticket that holds no lane.

## Release is explicit

A lane is freed only by `harness lane release`. A dead process does not free its lane in this slice.
