# Lanes

`harness lane claim | release | list | admission-check` keeps the live tickets and their lane under a cap. The cap is 1 without a valid admission record; with one it is the smaller of the record's admitted lane count and the config `lanes.count`. No constant is edited to raise the cap: the same code runs at 1, 2, 4 or any admitted count.

```
harness lane claim <ticket-file> --lanes-dir <abs dir> --config <file> --scope-file <file> [--admission <file>]
harness lane release <ticket-id> --lanes-dir <abs dir> --config <file> [--reason <text> --at <YYYY-MM-DDTHH:MM:SSZ> --records-dir <abs dir>]
harness lane list --lanes-dir <abs dir> --config <file> [--admission <file>]
harness lane admission-check <file> --config <file>
```

- `claim` prints `ticket=<id> lane=<n>` (the lowest free lane from 1 to the cap, when fewer tickets are live than the cap and no live ticket of the same repo holds a scope entry that overlaps one of the claim's — see *The scope clash*; a clash refuses the claim even when a lane is free). The ticket goes through the strict ticket reader. `--admission <path>` names an admission record: with no option, or with a path where nothing exists, the cap is 1; with a record that exists and is not valid the claim is refused with `bad-admission` and nothing is recorded, even when no ticket is live.
- The scope file holds one relative path per line. A truly empty line is skipped; a line of only whitespace is refused. A missing or empty file, an entry starting with `/` or `\`, a `..` part (split on `/` and `\`), an empty part (`a//b`) or a control character is refused with exit 2 and nothing is recorded. One trailing `/` is allowed (a folder, `a/`). Entries are stored as given (`./a`, `a/`).
- `list` prints one line per live ticket, `ticket=<id> lane=<n> repo_id=<id> scope=<entries>`, then a last line `live=<n> cap=<n>`, where the cap is computed as for a claim (1 without `--admission`, the smaller of the admitted count and `lanes.count` with a valid record; a record that exists and is not valid refuses the list with `bad-admission`). When more tickets are live than the cap, `list` still prints them and exits 0 (a claim is what gets refused).
- `release` prints `ticket=<id> released`. It takes `--config <file>` (required, like `list`): the legal record names follow the config, not the admission, so a release works for a ticket in lane 2 whether or not an admission is around. It also takes `--reason <text>`, `--at <YYYY-MM-DDTHH:MM:SSZ>` and `--records-dir <absolute-dir>` — all three or none — and appends one release record line (see *The release record*); the printed line stays `ticket=<id> released`.
- `admission-check` checks one admission record against the config: a valid record exits 0 and prints one line `admission ok lanes=<n> cap=<c>` with c the smaller of n and the config `lanes.count`; a record that exists and is not valid exits 1 with one `lane: refused reason=bad-admission` line; a path that does not exist exits 2, and so does an admission file that exists but cannot be read, or a path that cannot be inspected (for example a path that runs through a regular file, a folder on it that cannot be searched, or a link loop). A usage error (a missing or repeated file argument, an unknown option) and a missing, empty or unreadable `--config` exit 2 as well. This list names the main exit-2 cases of the verb; it does not claim to be complete.

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

## The scope clash

A claim for ticket T in repo R is refused with `clash` when a live record of another ticket holds the same `repo_id` (compared as stored) and at least one of its scope entries overlaps one of T's scope entries. The refusal is one stderr line that names the live ticket and the overlapping pair of entries as stored, the live ticket's entry first:

```
lane: refused reason=clash ticket=<live-ticket> live=<live-entry> new=<new-entry>
```

When several live tickets or several pairs clash, the line names the first one in this order: the live records by lane number (lowest first; `list` prints them in the same order), then each live record's scope entries in stored order, then the new ticket's scope entries in stored order. The lanes dir is unchanged after the refusal, and `lane list` shows the same live tickets as before. A `live=` or `new=` value is an entry as stored, and the scope file accepts an entry that holds a space or an `=`, so such a value can hold them too: the line is written to be read, and cannot always be parsed back into its three parts.

Two entries overlap when, after one normal form, the path parts of one are a prefix of the path parts of the other, counted in whole parts. So equal entries overlap (`docs/a.md` against `docs/a.md`); a folder entry (it ends in `/`) overlaps every entry inside it, in both orders (`docs/` against `docs/x/y.md`, and `docs/x/y.md` against `docs/`; `docs/` against `docs/sub/` in both orders); and a file entry overlaps a folder entry of the same name and every entry inside it (`docs` against `docs/`, and `docs` against `docs/x.md`, in both orders), because the same name cannot be both a file and a folder in one tree, so refusing is the safe side. `docs/a.md` against `docs/a.md.bak` is not a clash (`a.md` and `a.md.bak` differ as whole parts), and `docs/` against `docs2/x.md` is not a clash (`docs` and `docs2` differ).

The normal form, used only for this comparison (records keep storing entries as given, and what the scope file accepts does not change): the entry is split on `/` after dropping one trailing `/`, every `.` part is dropped (`docs/./a.md` and `./docs/a.md` are `docs/a.md`), and the parts are compared without regard to ASCII letter case (`Docs/A.md` clashes with `docs/a.md`: the default macOS file system treats them as one file, and refusing too much is the safe side). An entry that is nothing but `.` parts (`.` or `./`) names the whole repo: it clashes with every entry of the same repo and with another such entry.

What the normal form does not do: no Unicode normalization (a composed and a decomposed spelling of one accented name stay two entries), no symbolic link resolution (an entry is judged by its text), and a backslash is an ordinary character (only `/` splits the parts).

Order of the checks in a claim: the exit-2 input errors come first — the usage, the `--lanes-dir` path, the ticket file, the config and the scope file are all checked before any refusal; then the admission record is read, and `bad-admission` is the first refusal, an input check that comes before the lanes dir is touched, as before. Under the lock: `already-live` first, as before; then the cap (`no-admission` with no valid admission, else `cap-full`); then `clash`. The clash stands after the cap because the cap decides whether the claim may hold a lane at all under the admission it was given, and that refusal does not look at the scope; a claim that passes it and would break a live ticket of the same repo is refused with `clash` before anything is written. So a clash claim made with a valid admission that has a free lane is refused with `clash`, and a first claim with no live ticket is never a clash.

What the rule does not catch: two tickets with disjoint entries that still break each other (for example one edits `docs/a.md` while the other writes a generated file outside both scopes); tickets of different repos (their entries never clash, whatever they name); and anything the normal form does not do (two spellings that differ only by Unicode normalization, or by the case of a non-ASCII letter, do not clash).

## Record format

The lanes dir is created by `claim` when missing. One file per held lane, `<lanes-dir>/lane-<n>`, with n written without a leading zero and from 1 to the config `lanes.count`. Text:

```
lane-record v1
ticket=<id>
repo_id=<id>
scope=<entry>        (one line per entry, at least one)
```

A ticket id is letters, digits, `.`, `_`, `-` (at most 128, not starting with `.`). The `repo_id` follows the one rule of a ticket's `repo_id`: a plain name (lowercase letters, digits, `-`, `_`, `.`; not empty, not `.` or `..`). A record that holds anything else is a bad record (exit 2, see below), not a ticket of another repo: the clash is decided by the repo id.

## Concurrency

`claim`, `release` and `list` each run as one critical section under an exclusive lock (`flock`) on the lanes dir itself; no lock file is created. Under the lock a claim checks `already-live`, then the cap (`no-admission` when no valid admission is given, else `cap-full`), then `clash`, then writes the record to `.claim.tmp` and renames it onto `lane-<n>`; a release removes only the record it just read. So with any cap a ticket never holds two lanes, a claim never makes live exceed the cap that claim runs under (a later call given a different admission, or none, may see live above its own cap), and a release never removes a record that now belongs to another ticket, whatever number of `harness` processes run at once. A `.claim.tmp` left by a killed claim is removed under the lock by the next claim that takes a lane (a claim that is refused leaves it), so at most one is left. No directory fsync is done: a claim or release is not guaranteed to survive a crash of the machine.

## Reading the lanes dir (fail closed)

`list`, `claim` and `release` exit 2 with one `lane: error: ...` line, and change nothing in the dir, when the lanes dir is a symbolic link or not a directory, or holds an entry named `lane-...` that is not a regular file, cannot be read, does not parse, is not named `lane-<n>` with n from 1 to the config `lanes.count` (so `lane-0`, `lane-01`, `lane-99` are refused under a count of 4), or repeats a ticket held by another record. A record whose `repo_id` is not a plain repo id is a bad record the same way (so `repo_id=R`, `repo_id=r ` with its trailing space, or an empty `repo_id=` each exit 2): the clash is decided by the repo id, and a record that spells another repo id must not read as another repo. Other names (such as a stale `.claim.tmp`) are ignored.

## Exit codes

| code | meaning |
| --- | --- |
| 0 | done |
| 1 | policy refusal: exactly one stderr line starting `lane: refused reason=<token>` |
| 2 | usage error, unreadable or refused input, or a lanes dir in a bad state (see above) |

## Refusal tokens

- `cap-full`: a claim for another ticket when as many tickets are live as the cap, under a valid admission record. Checked after `already-live` and before `clash`.
- `no-admission`: a claim for another ticket while one ticket is live, with no valid admission (no `--admission`, or a path where nothing exists); the lanes dir is unchanged. Checked after `already-live` and before `clash`.
- `bad-admission`: a claim or list given an admission file that exists and is not valid, or `admission-check` on one; nothing is recorded. The line names the field when the problem is a field. Checked before the lanes dir is touched.
- `already-live`: the ticket already holds a lane. Checked before the cap and the clash.
- `clash`: a claim whose scope overlaps the scope of a live ticket of the same repo (see *The scope clash*). The line names the live ticket and the overlapping pair as stored. Checked after the cap; the lanes dir is unchanged.
- `not-live`: `release` for a ticket that holds no lane.

## The release record

`release` takes three options that go together — `--reason <text>`, `--at <YYYY-MM-DDTHH:MM:SSZ>` and `--records-dir <absolute-dir>`; one or two of them without the rest is exit 2. The lane code reads no clock, so the record time is the `--at` value, a real UTC instant in the shape the `friction` inbox checks. The record is one file per ticket, `<records-dir>/<ticket-id>.release`, and one line is appended per release:

```
<at> lane=<n> reason=<reason>
```

`<n>` is the lane number of the record the release removed. `reason=` is the last field and runs to the line end. Lines already in the file are kept byte for byte and are not checked.

`--reason` is checked before the lanes dir is touched, in this order: not empty or only white space; no newline; no control character and no U+2028 / U+2029; at most 1024 bytes. Each refusal is exit 2 with one `lane: error:` line, the lane stays held, no record is written and the records dir is not created. `--reason --at <ts>` takes `--at` as the reason text, so the timestamp becomes a second positional argument and the call is exit 2 as well.

Under the lane lock, in this order: (a) the ticket's record is looked up — `not-live` (exit 1) writes no record and creates no records dir; (b) the records dir is created with its parents when missing, and a link or a non-folder is exit 2; (c) a record path that is there and is not a regular file (a link, a folder, a fifo) is exit 2; (d) the record is opened without following a link and without blocking, and an existing record over 1 MiB, not utf-8, or without a final newline is exit 2 (an empty file is accepted); (e) the lane file is removed; (f) the line is written and synced — on a write error the file is cut back to its old length and the exit is 2, with the lane freed. A failure before (e) leaves the lane held.

The records dir must not be the lanes dir or inside it, also through a symbolic link (checked on the resolved paths): a ticket id such as `lane-1` would put its record among the lane records, which the dir reader refuses. Exit 2, nothing freed.

Stricter than the wrapper writer it replaces:

- a reason over 1024 bytes is refused;
- U+0085 and other C1 characters, and U+2028 / U+2029, are refused (the wrapper result depends on its locale);
- a reason of only no-break or other Unicode spaces is blank here (the C-locale wrapper accepts it);
- an existing record over 1 MiB, not UTF-8, or with a torn tail is refused;
- a records dir that is the lanes dir or inside it is refused;
- a records dir or record problem is exit 2, where the wrapper exits 1.

`not-live` is checked before the records dir and the record, while the wrapper checks its logs folder and the record first. The record gains `lane=<n>`; the wrapper line has no lane number.

## Release is explicit

A lane is freed only by `harness lane release`. A dead process does not free its lane in this slice.
