# Lanes

`harness lane claim | release | list | admission-check` keeps the live tickets and their lane under a cap. The cap is 1 without a valid admission record; with one it is the smaller of the record's admitted lane count and the config `lanes.count`. No constant is edited to raise the cap: the same code runs at 1, 2, 4 or any admitted count.

```
harness lane claim <ticket-file> --lanes-dir <abs dir> --config <file> --scope-file <file> [--admission <file>]
harness lane release <ticket-id> --lanes-dir <abs dir> --config <file>
harness lane list --lanes-dir <abs dir> --config <file> [--admission <file>]
harness lane admission-check <file> --config <file>
```

- `claim` prints `ticket=<id> lane=<n>` (the lowest free lane from 1 to the cap, when fewer tickets are live than the cap). The ticket goes through the strict ticket reader. `--admission <path>` names an admission record: with no option, or with a path where nothing exists, the cap is 1; with a record that exists and is not valid the claim is refused with `bad-admission` and nothing is recorded, even when no ticket is live.
- The scope file holds one relative path per line. A truly empty line is skipped; a line of only whitespace is refused. A missing or empty file, an entry starting with `/` or `\`, a `..` part (split on `/` and `\`), an empty part (`a//b`) or a control character is refused with exit 2 and nothing is recorded. One trailing `/` is allowed (a folder, `a/`). Entries are stored as given (`./a`, `a/`).
- `list` prints one line per live ticket, `ticket=<id> lane=<n> repo_id=<id> scope=<entries>`, then a last line `live=<n> cap=<n>`, where the cap is computed as for a claim (1 without `--admission`, the smaller of the admitted count and `lanes.count` with a valid record; a record that exists and is not valid refuses the list with `bad-admission`). When more tickets are live than the cap, `list` still prints them and exits 0 (a claim is what gets refused).
- `release` prints `ticket=<id> released`. It takes `--config <file>` (required, like `list`): the legal record names follow the config, not the admission, so a release works for a ticket in lane 2 whether or not an admission is around.
- `admission-check` checks one admission record against the config: a valid record exits 0 and prints one line `admission ok lanes=<n> cap=<c>` with c the smaller of n and the config `lanes.count`; a record that exists and is not valid exits 1 with one `lane: refused reason=bad-admission` line; a path that does not exist exits 2, and so does an admission file that exists but cannot be read, or a path that runs through a regular file.

## The admission record

A small text file: a regular file (a symbolic link is refused too), utf-8, at most 64 KiB, every line ending with a newline. The first line is the header `lane-admission v1`. Each of the other lines is `field=value` (split at the first `=`), the fields may come in any order, and each field appears exactly once:

| field | holds | shape |
| --- | --- | --- |
| `lanes` | the admitted lane count | a whole number of at least 2 (digits only, no leading zero, at most 9 digits) |
| `baseline` | the baseline reference | one token of printable ASCII (no space, no control character), at most 256 bytes |
| `step2` | the cited step-2 numbers | non-empty plain text, at most 256 bytes, no control character and no U+2028 / U+2029 |
| `reviewer` | the reviewer token | letters, digits, `.`, `_`, `-`, at most 128 bytes |
| `verdict` | the reviewer verdict | non-empty plain text, at most 256 bytes, no control character and no U+2028 / U+2029 |
| `operator` | the operator go reference | one token of printable ASCII (no space, no control character), at most 256 bytes |
| `l3` | the L3 check result | non-empty plain text, at most 256 bytes, no control character and no U+2028 / U+2029 |
| `signed` | the signing date | a real calendar date spelled YYYY-MM-DD |

A full example:

```
lane-admission v1
lanes=2
baseline=main@4cbdb66
step2=p1a 5 p4a 10 d2a 20
reviewer=r.violet
verdict=approve-two-lanes
operator=go-2026-11-14b
l3=pass
signed=2026-11-14
```

A field that is missing, empty, repeated or out of range, an unknown line, a wrong header, a file that is not a regular file, is not utf-8, or is larger than 64 KiB makes the record invalid: one stderr line `lane: refused reason=bad-admission` that also names the field when the problem is a field.

The reviewer, verdict and operator fields are self-asserted text: the engine checks that they are present and well formed, not who wrote them and not whether they are true.

## How the cap is computed

With no `--admission` option, or with `--admission <path>` where nothing exists, the cap is 1: the first claim of an empty lanes dir gets lane 1 with no admission at all, and a claim for another ticket while one is live is refused with `no-admission`. With a valid record the cap is the smaller of the record's `lanes` value and the config `lanes.count` (a lane number is at most nine digits, so a count beyond that bounds at 999999999).

## Record format

The lanes dir is created by `claim` when missing. One file per held lane, `<lanes-dir>/lane-<n>`, with n written without a leading zero and from 1 to the config `lanes.count`. Text:

```
lane-record v1
ticket=<id>
repo_id=<id>
scope=<entry>        (one line per entry, at least one)
```

A ticket id is letters, digits, `.`, `_`, `-` (at most 128, not starting with `.`).

## Concurrency

`claim`, `release` and `list` each run as one critical section under an exclusive lock (`flock`) on the lanes dir itself; no lock file is created. Under the lock a claim checks `already-live`, then the cap (`no-admission` when no valid admission is given, else `cap-full`), then writes the record to `.claim.tmp` and renames it onto `lane-<n>`; a release removes only the record it just read. So with any cap a ticket never holds two lanes, a claim never makes live exceed the cap that claim runs under (a later call given a different admission, or none, may see live above its own cap), and a release never removes a record that now belongs to another ticket, whatever number of `harness` processes run at once. A `.claim.tmp` left by a killed claim is removed under the lock by the next claim that takes a lane (a claim that is refused leaves it), so at most one is left. No directory fsync is done: a claim or release is not guaranteed to survive a crash of the machine.

## Reading the lanes dir (fail closed)

`list`, `claim` and `release` exit 2 with one `lane: error: ...` line, and change nothing in the dir, when the lanes dir is a symbolic link or not a directory, or holds an entry named `lane-...` that is not a regular file, cannot be read, does not parse, is not named `lane-<n>` with n from 1 to the config `lanes.count` (so `lane-0`, `lane-01`, `lane-99` are refused under a count of 4), or repeats a ticket held by another record. Other names (such as a stale `.claim.tmp`) are ignored.

## Exit codes

| code | meaning |
| --- | --- |
| 0 | done |
| 1 | policy refusal: exactly one stderr line starting `lane: refused reason=<token>` |
| 2 | usage error, unreadable or refused input, or a lanes dir in a bad state (see above) |

## Refusal tokens

- `cap-full`: a claim for another ticket when as many tickets are live as the cap, under a valid admission record.
- `no-admission`: a claim for another ticket while one ticket is live, with no valid admission (no `--admission`, or a path where nothing exists); the lanes dir is unchanged.
- `bad-admission`: a claim or list given an admission file that exists and is not valid, or `admission-check` on one; nothing is recorded. The line names the field when the problem is a field.
- `already-live`: the ticket already holds a lane. Checked before the cap.
- `not-live`: `release` for a ticket that holds no lane.

## Release is explicit

A lane is freed only by `harness lane release`. A dead process does not free its lane in this slice.
