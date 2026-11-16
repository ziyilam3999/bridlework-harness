# Lanes

`harness lane claim | release | list | park | admission-check | stale` keeps the live tickets and their lane under a cap (`stale` reports which lanes belong to a gone session with nothing unsaved and, with `--apply`, parks them when its park step still says so; see *The stale check*). The cap is 1 without a valid admission record; with one it is the smaller of the record's admitted lane count and the config `lanes.count`. No constant is edited to raise the cap: the same code runs at 1, 2, 4 or any admitted count. A lane may also be freed by `park`, which keeps the ticket's file list reserved (see *The parked file list*).

```
harness lane claim <ticket-file> --lanes-dir <abs dir> --config <file> --scope-file <file> [--admission <file>]
harness lane release <ticket-id> --lanes-dir <abs dir> --config <file> [--reason <text> --at <YYYY-MM-DDTHH:MM:SSZ> --records-dir <abs dir>]
harness lane list --lanes-dir <abs dir> --config <file> [--admission <file>]
harness lane admission-check <file> --config <file>
harness lane park <ticket-id> --lanes-dir <abs dir> --config <file> [--reason <text> --at <YYYY-MM-DDTHH:MM:SSZ> --records-dir <abs dir>]
harness lane stale --lanes-dir <abs dir> --config <file> --host <abs file> --launch-records <abs file> --manifest-dir <abs dir> --repo-map <repo-id>=<abs dir> [--repo-map ...] [--allow <tree-relative path> ...] [--apply --reason <text> --at <YYYY-MM-DDTHH:MM:SSZ> --records-dir <abs dir>]
```

- `claim` prints `ticket=<id> lane=<n>` (the lowest free lane from 1 to the cap, when fewer tickets are live than the cap and no live ticket of the same repo holds a scope entry that overlaps one of the claim's — see *The scope clash*; a clash refuses the claim even when a lane is free, and so does an overlapping parked record of another ticket of the same repo, with `parked`). The ticket goes through the strict ticket reader. `--admission <path>` names an admission record: with no option, or with a path where nothing exists, the cap is 1; with a record that exists and is not valid the claim is refused with `bad-admission` and nothing is recorded, even when no ticket is live. When the config sets the disk floor `lanes.min_free_gb` (`docs/config-contract.md`) and the free space of the lanes dir file system is under it, the claim is refused with `low-disk` before the lanes dir is touched.
- The scope file holds one relative path per line. A truly empty line is skipped; a line of only whitespace is refused. A missing or empty file, an entry starting with `/` or `\`, a `..` part (split on `/` and `\`), an empty part (`a//b`) or a control character is refused with exit 2 and nothing is recorded. One trailing `/` is allowed (a folder, `a/`). Entries are stored as given (`./a`, `a/`).
- `list` prints one line per live ticket, `ticket=<id> lane=<n> repo_id=<id> scope=<n>` (n is the entry count), then one line per parked record in ticket id byte order, `parked ticket=<id> repo_id=<id> scope=<n>`, then a last line `live=<n> cap=<n>` that counts live records only, where the cap is computed as for a claim (1 without `--admission`, the smaller of the admitted count and `lanes.count` with a valid record; a record that exists and is not valid refuses the list with `bad-admission`). When more tickets are live than the cap, `list` still prints them and exits 0 (a claim is what gets refused).
- `release` prints `ticket=<id> released`. It takes `--config <file>` (required, like `list`): the legal record names follow the config, not the admission, so a release works for a ticket in lane 2 whether or not an admission is around. It also takes `--reason <text>`, `--at <YYYY-MM-DDTHH:MM:SSZ>` and `--records-dir <absolute-dir>` — all three or none — and appends one release record line (see *The release record*); the printed line stays `ticket=<id> released`. It removes the ticket's parked file too, when there is one (see *The parked file list*).
- `admission-check` checks one admission record against the config: a valid record exits 0 and prints one line `admission ok lanes=<n> cap=<c>` with c the smaller of n and the config `lanes.count`; a record that exists and is not valid exits 1 with one `lane: refused reason=bad-admission` line; a path that does not exist exits 2, and so does an admission file that exists but cannot be read, or a path that cannot be inspected (for example a path that runs through a regular file, a folder on it that cannot be searched, or a link loop). A usage error (a missing or repeated file argument, an unknown option) and a missing, empty or unreadable `--config` exit 2 as well. This list names the main exit-2 cases of the verb; it does not claim to be complete.
- `park` prints `ticket=<id> parked lane=<n>` and moves the record of a live ticket onto `<lanes-dir>/parked-<ticket-id>` with its content unchanged: the lane is freed and the ticket's file list stays reserved (see *The parked file list*). The ticket id goes through the same check as `release`. It takes the record options of `release` — all three or none, the same checks — and appends `<at> lane=<n> reason=<reason>` to the same release record.

A `--lanes-dir` or `--records-dir` value that ends with `/` or holds a `.` or `..` part is exit 2 with one `lane: error:` line before anything is touched, in each verb. Such a spelling can resolve through a symbolic link at the end of the path (`<link>/` and `<link>/.` name the link target), so a lanes dir or a records dir given that way is refused instead of followed, and the link target is left untouched.

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

Order of the checks in a claim: the exit-2 input errors come first — the usage, the `--lanes-dir` path, the ticket file, the config and the scope file are all checked before any refusal; then the admission record is read, and `bad-admission` is the first refusal, an input check that comes before the lanes dir is touched, as before. Then, when the config sets `lanes.min_free_gb`, the free space of the lanes dir file system is read: `low-disk` when it is under the floor, still before the lanes dir is touched and before `already-live`; a floor is set and the reading cannot be taken is exit 2 with `lane: error: the free disk space cannot be read`. With no floor set the disk is not read at all. Under the lock: `already-live` first, as before; then the cap (`no-admission` with no valid admission, else `cap-full`); then `clash`; then `parked` (a scope that overlaps a parked record of another ticket of the same repo, see *The parked file list*). The clash stands after the cap because the cap decides whether the claim may hold a lane at all under the admission it was given, and that refusal does not look at the scope; a claim that passes it and would break a live ticket of the same repo is refused with `clash` before anything is written. So a clash claim made with a valid admission that has a free lane is refused with `clash`, and a first claim with no live ticket is never a clash.

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

## The parked file list

`park` moves the record of a live ticket onto `<lanes-dir>/parked-<ticket-id>` with its content unchanged, in the record format above: the lane is freed and the ticket's file list stays reserved, so no overlapping job can start, until the ticket claims a lane again or is released. Parked records take no lane number and do not count as live: the cap, `already-live`, `no-admission` and `cap-full` look at live records only.

The move does not replace an existing file. When the dir already holds an entry the file system resolves as `parked-<ticket-id>` — the ticket's own parked file, or on a case-ignoring disk the parked file of a ticket whose id differs only in ASCII case — `park` is refused with `already-parked` and the lanes dir is byte-unchanged. A ticket with no live record (one that is parked only) is `not-live`; a missing lanes dir is not created. A ticket may sit live and parked at once; the parked file stays a reservation until the ticket is released.

Reading: `list`, `claim`, `release` and `park` read an entry named `parked-<rest>` like a lane record — a regular file (a link is refused), at most 1 MiB, utf-8, the same header and fields. One that is not a regular file, cannot be read, does not parse, or whose `<rest>` is not byte-equal to the `ticket=` value inside it, is exit 2 with one `lane: error: ...` line that names the cause, and nothing is changed.

A claim whose scope overlaps a parked record of another ticket of the same repo — the same overlap rule and normal form as the clash — is refused with `parked`, after `clash` and before anything is written:

```
lane: refused reason=parked ticket=<parked-ticket> parked=<parked-entry> new=<new-entry>
```

When several match, the line names the first in this order: the parked tickets by ticket id in byte order, then their entries in stored order, then the new entries in stored order. A claim of the parked ticket itself is not refused by its own parked file (matched by the `ticket=` value, not by the file name): when that claim succeeds, its parked file is removed after the lane record is written, and a failure of that removal keeps the parked file while the claim still prints `ticket=<id> lane=<n>` and exits 0.

`release` removes the lane record and the parked file of the ticket in this order: the lane file, then the parked file, then the record line. A failure removing the parked file after the lane file is gone is exit 2 with `lane: error: the lane was freed and the parked file cannot be removed`, and no record line. A ticket with only a parked file is released too: the parked file is removed, the stdout is `ticket=<id> released`, and the record line says `lane=parked`. A ticket whose id differs only in ASCII case from a parked one does not match it (the parked file is matched by the `ticket=` value, not by the file name).

## Concurrency

`claim`, `release`, `park` and `list` each run as one critical section under an exclusive lock (`flock`) on the lanes dir itself; no lock file is created. Under the lock a claim checks `already-live`, then the cap (`no-admission` when no valid admission is given, else `cap-full`), then `clash`, then `parked`, then writes the record to `.claim.tmp` and renames it onto `lane-<n>`, and after that removes the ticket's own parked file; a park checks the dir, refuses `not-live` and `already-parked`, then renames the record onto the parked name; a release removes only the records it just read. The park step of `stale --apply` is one such section too: it holds the lock from the lanes dir re-read to the park (see *The apply step*). So with any cap a ticket never holds two lanes, a claim never makes live exceed the cap that claim runs under (a later call given a different admission, or none, may see live above its own cap), and a release never removes a record that now belongs to another ticket, whatever number of `harness` processes run at once. A `.claim.tmp` left by a killed claim is removed under the lock by the next claim that takes a lane (a claim that is refused leaves it), so at most one is left. No directory fsync is done: a claim, release or park is not guaranteed to survive a crash of the machine.

## Reading the lanes dir (fail closed)

`list`, `claim`, `release` and `park` exit 2 with one `lane: error: ...` line, and change nothing in the dir, when the lanes dir is a symbolic link or not a directory, or holds an entry named `lane-...` that is not a regular file, cannot be read, does not parse, is not named `lane-<n>` with n from 1 to the config `lanes.count` (so `lane-0`, `lane-01`, `lane-99` are refused under a count of 4), or repeats a ticket held by another record. A record whose `repo_id` is not a plain repo id is a bad record the same way (so `repo_id=R`, `repo_id=r ` with its trailing space, or an empty `repo_id=` each exit 2): the clash is decided by the repo id, and a record that spells another repo id must not read as another repo. An entry named `parked-<rest>` is read as a parked record with the rules of *The parked file list* (its `<rest>` must be byte-equal to the `ticket=` value inside it); other names (such as a stale `.claim.tmp`) are ignored.

## The stale check

`harness lane stale` says which live lanes belong to a job whose builder session is gone and that left nothing unsaved. It decides and prints. Without `--apply` it changes nothing: the lanes dir, the manifests, the trees, the branches and the host session list are the same after it. With `--apply` each record judged `stale gone-clean-pushed` goes to the park step, which parks its lane only when the verdict still holds there; nothing else is written (see *The apply step*). It runs no stop, no rm, no fetch and no network call. Nothing calls the verb yet.

```
harness lane stale --lanes-dir <abs dir> --config <file> --host <abs file> --launch-records <abs file> --manifest-dir <abs dir> --repo-map <repo-id>=<abs dir> [--repo-map ...] [--allow <tree-relative path> ...] [--apply --reason <text> --at <YYYY-MM-DDTHH:MM:SSZ> --records-dir <abs dir>]
```

- `--repo-map` is given 1 to 16 times, one repo id at most once; the id is a plain repo id.
- `--allow` is given at most 16 times (see *The `--allow` rule*).
- Each path value is absolute, does not end with `/`, and holds no `.` or `..` part, else exit 2 before anything is read. `--lanes-dir`, `--manifest-dir` and each `--repo-map` dir are real directories (a link or a non-directory is refused; a missing lanes dir holds no record, as in `list`). `--launch-records` is a regular file and not a link. `--host` may be a link and must resolve to a regular file.
- An unknown option, a missing required option, or a non-repeatable option given twice is exit 2.
- `--apply` is a flag with no value. It and `--reason`, `--at` and `--records-dir` go together or none is given (else exit 2 before anything is read); each is given once. See *The apply step* for their checks.

### The facts

The lanes dir is read as `list` reads it, under its lock. In a run without `--apply` the lock is released before the host or git runs and the host is called once; with `--apply` the park step reads again under the lock (see *The apply step*). The host is called as `<host> agents --json --all` (10 second limit). Parked records are not judged. For each live record in lane order, the first rule that matches gives the verdict and the reason. The slug is the lower-cased ticket id with each character outside `a-z0-9` dropped. A session name is read with the session name check at any lane number (the bound is the 15-digit cap of a lane number, so a number above `lanes.count` counts).

An exec launch is a launch row whose name reads back the role `exec` (or the long form `execution`) and the ticket equal to the slug. An exec entry is a listing entry whose name reads back the same way. An entry is ended when its state is `done` or `stopped`; any other state, or no state field, is running.

A ticket id with no letter or digit (such as `_` or `--`) has an empty slug. No launcher name can carry it, so it gets `hand no-exec-launch` before the rules below, with no other rule read.

1. Two or more distinct ticket ids among the live and parked records share the slug (`a-b` and `ab`, `Foo` and `foo`): `unknown slug-shared`.
2. The ticket also has a parked record of its own (its `ticket=` value byte-equal): `unknown live-and-parked`.
3. An exec launch is in state `pending`: `alive launch-pending`.
4. No exec launch is in state `bound`: `hand no-exec-launch` (failed and refused launch rows are not sessions; the tree is not read).
5. The lane record file was last modified after the `ts_ms` of the newest bound exec launch: `alive claimed-after-launch` (a start whose seat is not listed yet).
6. A running exec entry: `alive listed-running`.
7. A running entry whose name holds a `-` separated part equal, ignoring ASCII case, to `t<slug>` and that does not read back the ticket `<slug>` in any role, or whose hyphenated read-back equals the slug once its `-` are removed: `unknown name-unclear`.
8. Otherwise the session is gone (no exec entry, or ended ones only); an entry of another role for the slug, such as a reviewer, does not hold the lane. Then the tree is judged: no `--repo-map` for the record repo id is `unknown no-repo-map`. The clean check is `harness workspace check <ticket> --manifest-dir <dir>`, run as a child of the same binary: exit 0 with stdout exactly `workspace=clean ticket=<ticket>` is clean; exit 2 whose stdout is `stray <kind> <path>` lines then `workspace=stray ticket=<ticket> count=<n>` is clean when each stray line has the kind `untracked` or `ignored` and a path byte-equal to an `--allow` value, else `unsaved stray`; any other result (exit 3, another code, the time limit, a cap, output not in that form) is `unknown clean-check-failed`. Then the pushed check, the same command with `--pushed --repo <map dir>`: exit 0 is `stale gone-clean-pushed`; exit 4 is `unsaved unpushed`; exit 3 whose stderr is the line `harness: error: the ticket branch has no upstream` is `unknown no-upstream`; anything else is `unknown pushed-check-failed`.

Each check child has a 30 second limit with stdin closed and stdout and stderr each read under a 1 MiB cap; past the limit or a cap the child and its process group are killed and the call counts as failed.

### Verdicts and reasons

| verdict | reason | meaning |
| --- | --- | --- |
| `unknown` | `slug-shared` | two tickets share the slug; sessions cannot be told apart |
| `unknown` | `live-and-parked` | the ticket is live and parked at once (`park` refuses it; `release` ends both) |
| `alive` | `launch-pending` | an exec launch row is still pending |
| `hand` | `no-exec-launch` | no bound exec launch row, or a ticket id with no letter or digit: left out |
| `alive` | `claimed-after-launch` | the lane file is newer than the newest bound exec launch |
| `alive` | `listed-running` | a running exec entry is listed |
| `unknown` | `name-unclear` | a running entry may belong to the ticket but its name does not say so |
| `unknown` | `no-repo-map` | no `--repo-map` for the record repo id |
| `unknown` | `clean-check-failed` | the clean check gave no usable answer |
| `unsaved` | `stray` | the tree holds a path that is not allowed |
| `unsaved` | `unpushed` | a commit of the ticket branch is not pushed |
| `unknown` | `no-upstream` | the ticket branch has no upstream (not decided in this version; not parked) |
| `unknown` | `pushed-check-failed` | the pushed check gave no usable answer |
| `stale` | `gone-clean-pushed` | the session is gone and nothing is unsaved |
| `unknown` | `record-changed` | `--apply` only: the record, the launch rows or the lane changed between the report and the park step |
| `unknown` | `recheck-failed` | `--apply` only: the lock, the lanes dir, the host listing or the launch records could not be read again; nothing is parked after it |
| `unknown` | `park-refused` | `--apply` only: the park body refused (`not-live` or `already-parked`); nothing changed |

### Output and exit codes

Stdout holds one line per live record in lane order:

```
stale ticket=<id> lane=<n> verdict=<hand|alive|unknown|unsaved|stale> reason=<token> action=<none|parked>
```

then one line `stale checked=<n> parked=<k> unknown=<u>`, where n is the number of live records printed, k the number of parks done in this run (0 without `--apply`) and u the number with verdict `unknown`. A parked record prints `verdict=stale reason=gone-clean-pushed action=parked`; each other line has `action=none`. With no live record only the last line is printed. Exit 0 when no printed verdict is `unknown`; exit 3 when one or more are (the other lanes are still judged and printed); exit 2 for a usage error, a whole-run failure or, with `--apply`, a park body error.

### The apply step

`--apply` takes `--reason <text>`, `--at <YYYY-MM-DDTHH:MM:SSZ>` and `--records-dir <absolute-dir>`: the four go together or none is given, else exit 2 with one `lane: error:` line and empty stdout before anything is read. `--apply` and each of the three given twice is exit 2. The three values take the checks of the release record options (*The release record*, with the same messages), and `--reason` is at most 900 bytes. Without `--apply` the output and the exit code are those of the report.

Only a live record whose verdict is `stale gone-clean-pushed` goes to the park step. Each other verdict is printed with `action=none` and its lane is untouched; a branch with no upstream stays `unknown no-upstream`. For each live record in lane order the verdict is judged on the first read; for a stale one the park step runs at once, then its line is printed. The park step is one critical section under the lanes dir lock, taken once and held from (a) to (g):

1. (a) take the lock;
2. (b) read the lanes dir (live and parked records, under the config name bound) and the mtime of the ticket's lane file;
3. (c) the ticket must be live in the same lane with the same repo id and scope as in the first read, else `unknown record-changed`;
4. (d) call the host listing again;
5. (e) read the launch records again, after the listing;
6. (f) judge the ticket again on the facts of (b), (d) and (e), the clean and pushed checks run again; when the verdict is again `stale`, the exec launches of the slug are compared with the first read, and a launch that was not there or a changed state is `unknown record-changed`;
7. (g) only when the verdict of (f) is `stale gone-clean-pushed`, park the record with the body of `lane park` (run under the held lock) and the record options, then release the lock.

When (f) gives another verdict nothing is written and the line shows that verdict with `action=none`. A failure of (a), (b), (d) or (e) parks nothing: that record and each later record whose first verdict is `stale` print `unknown recheck-failed` with `action=none`, no later park step runs, the other records are still judged and printed, and the exit is 3. A park body refusal prints `unknown park-refused` and changes nothing. The config is loaded once per run.

A park renames `<lanes-dir>/lane-<n>` onto `<lanes-dir>/parked-<ticket-id>` with its bytes unchanged and appends one line to `<records-dir>/<ticket-id>.release` (the records dir is created when missing, as in `park`):

```
<at> lane=<n> reason=stale-check listed=<none|stopped|done>: <given reason>
```

`listed` is `done` when an exec entry of the slug in state `done` was listed in (d), else `stopped` when one in state `stopped` was listed, else `none`. Nothing else is written: no other file in the lanes dir, no tree, branch, ref, manifest or launch record, and no host session.

A park body error stops the run: exit 2, one stderr line `lane: error: ticket=<id>: <cause>`, stdout holds the lines of the records handled before it and no last line, and the parks done before it stay. A failed park leaves what `park` leaves (*The release record*): an error before the rename leaves the lane held (a records dir or an empty record file it created may stay); an append error after the rename leaves the ticket parked with the record cut back, and its text says the ticket was parked.

### Whole-run failures

Each is exit 2 with one stderr line `lane: error: <cause>` and empty stdout, before any lane is judged:

- bad options or paths;
- a config that cannot be read or is invalid (a fifo at the config path is refused at once);
- launch records that are missing, not a regular file, a link, unreadable, over the reader cap, torn or corrupt (the file is opened without following a link and without blocking, and the open handle must be a regular file before the records are read; they are then read through `/dev/fd` of that handle where that path exists, else through the path);
- a host listing that fails: it cannot run, exits non-zero, passes the time limit, is not JSON, is not an array, is over the entry cap, holds an entry with no string id and name, or holds an entry id that is not a plain token (output past the 1 MiB read cap is cut and then reads as not JSON);
- a lanes dir that `list` refuses, a parked record that `list` refuses, or a lane record that cannot be inspected.

### The `--allow` rule

An `--allow` value is a tree-relative path of `A-Z a-z 0-9 . _ - /`, not starting or ending with `/`, with no empty, `.` or `..` part. A stray path is allowed only when its kind is `untracked` or `ignored` and the path is byte-equal to an `--allow` value: a modified, deleted, staged, hidden, missing or submodule path is not covered, and an `--allow` of `a` does not cover `a/b`. Nothing is deleted.

### Limits of this version

- A killed seat that the host lists as running is kept. In one probe run on 2026-11-16 (one session started outside the launcher) the host listed a seat as `working` with no pid right after `kill -9`, restarted it under a new pid within 30 seconds, and it later read `done`. A reboot, a host service crash, a seat started through the launcher and a second kill were not measured.
- A `done` entry counts as ended, though in that probe a `done` session kept a live idle process.
- A launch row left pending by a crashed launch keeps the lane.
- Launch rows and listing entries are tied to a ticket by its slug only, so the rows of a finished ticket with the same slug count.
- Listing entries are matched by name, so a session started by hand under a seat name counts.
- The name-unclear rule sees a name only when a `-` separated part equals `t<slug>` (any letter case) or when its read-back ticket, with the `-` removed, equals the slug; other spellings of the ticket in a name are not seen.
- A file inside an allowed folder goes unseen (nothing is deleted).
- The pushed answer reads local tracking refs only (no fetch).
- A branch with no upstream is `unknown` in this version.
- Reviewer sessions do not hold a lane.
- Jobs with no exec launch are not checked.
- Lanes dir entries whose names are not UTF-8 are skipped, as in `list`.
- Nothing calls the verb yet.
- With `--apply` the lock is held through the park step, which runs the host listing (10 second limit) and two workspace checks (30 seconds each): `claim`, `release`, `park` and `list` wait up to about 80 seconds per lane being parked (the lock wait has no time limit of its own).
- A commit, a file or a woken session after the tree check of (f) and before the rename of (g) is not seen: git and the host take no lanes dir lock, so the window narrows and does not close. A launch that writes its launch row after (e) starts its seat after the re-check.
- A session in state `done` can be woken and work while its lane is free (operator-accepted, 2026-11-16 11:58 UTC). A seat stopped on purpose counts as gone.
- The cap counts live records only, so a parked job is not counted and jobs in flight can exceed the cap (operator-accepted, 2026-11-16 09:47 and 10:00 UTC).
- The file list of a parked job stays reserved by the claim check `parked`. A parked job takes a lane again only by a claim of the same ticket, which the cap can refuse (the job then waits with its file list reserved), and is ended by `release`. Nothing in this verb un-parks.
- The park lines of the stale check go to `<records-dir>/<ticket-id>.release`, beside the `release` and `park` lines.

## Exit codes

| code | meaning |
| --- | --- |
| 0 | done |
| 1 | policy refusal: exactly one stderr line starting `lane: refused reason=<token>` |
| 2 | usage error, unreadable or refused input, free disk space that cannot be read (with a floor set), or a lanes dir in a bad state (see above); with `stale --apply` also a park body error, with the lines printed before it |
| 3 | `stale` only: the report is printed and one or more verdicts are `unknown` |

## Refusal tokens

- `cap-full`: a claim for another ticket when as many tickets are live as the cap, under a valid admission record. Checked after `already-live` and before `clash`.
- `no-admission`: a claim for another ticket while one ticket is live, with no valid admission (no `--admission`, or a path where nothing exists); the lanes dir is unchanged. Checked after `already-live` and before `clash`.
- `bad-admission`: a claim or list given an admission file that exists and is not valid, or `admission-check` on one; nothing is recorded. The line names the field when the problem is a field. Checked before the lanes dir is touched.
- `already-live`: the ticket already holds a lane. Checked before the cap and the clash.
- `clash`: a claim whose scope overlaps the scope of a live ticket of the same repo (see *The scope clash*). The line names the live ticket and the overlapping pair as stored. Checked after the cap; the lanes dir is unchanged.
- `parked`: a claim whose scope overlaps a parked record of another ticket of the same repo (see *The parked file list*). The line names the parked ticket and the overlapping pair as stored (`ticket=<id> parked=<entry> new=<entry>`). Checked after `clash`; the lanes dir is unchanged.
- `already-parked`: `park` for a ticket whose parked name the file system already resolves — the ticket's own parked file, or on a case-ignoring disk the parked file of a ticket whose id differs only in ASCII case. The move does not replace a file. Checked after `not-live`; the lanes dir is byte-unchanged.
- `low-disk`: a claim refused because the free space of the lanes dir file system is under the config floor `lanes.min_free_gb` (in GB, 1 GB = 1000000000 bytes; absent means no floor and the disk is not read). The line also gives the free bytes rounded down to GB and the floor: `lane: refused reason=low-disk free_gb=<n> min_free_gb=<n>`. Nothing is created; a missing lanes dir stays missing (it is read through its nearest existing ancestor). Checked after `bad-admission`, before the lanes dir is touched, so before `already-live`. `lane release`, `lane park`, `lane list` and `lane admission-check` read no disk: a running job is not stopped, and its lane can be freed on a full disk. V1 limit: the disk can fill between the check and the job, and only the lanes dir file system is measured.
- `not-live`: `release` for a ticket with neither a live record nor a parked file; `park` for a ticket with no live record (a parked-only ticket too).

## The release record

`release` and `park` take three options that go together — `--reason <text>`, `--at <YYYY-MM-DDTHH:MM:SSZ>` and `--records-dir <absolute-dir>`; one or two of them without the rest is exit 2. The lane code reads no clock, so the record time is the `--at` value, a real UTC instant in the shape the `friction` inbox checks. The record is one file per ticket, `<records-dir>/<ticket-id>.release`, and one line is appended per release and per park:

```
<at> lane=<n> reason=<reason>
```

`<n>` is the lane number of the record the release removed, the lane number the ticket held when it was parked, or `parked` for the release of a ticket that held only a parked file. `reason=` is the last field and runs to the line end. Lines already in the file are kept byte for byte; their content is not checked, only the file as a whole (step (d)).

`--reason` is checked before the lanes dir is touched, in this order: not empty or only white space; no newline; no control character and no U+2028 / U+2029; at most 1024 bytes. Each refusal is exit 2 with one `lane: error:` line, the lane stays held, no record is written and the records dir is not created. `--reason --at <ts>` takes `--at` as the reason text, so the timestamp becomes a second positional argument and the call is exit 2 as well.

Under the lane lock (a missing lanes dir is `not-live` before the lock), in this order: (a) the ticket's live record and parked file are looked up — a ticket with neither is `not-live` (exit 1), which writes no record and creates no records dir (for `park`: a ticket with no live record is `not-live`, then an existing parked name is `already-parked`); (b) the records dir is created with its parents when missing, and a link or a non-folder is exit 2; (c) a record path that is there and is not a regular file (a link, a folder, a fifo) is exit 2; (d) the record is opened without following a link and without blocking, and an existing record over 1 MiB, not utf-8, or without a final newline is exit 2 (an empty file is accepted); (e) the lane file is removed, then the parked file when there is one (a failure removing it after the lane file is gone is exit 2 with the lane freed and no record line); for `park` step (e) is the move onto the parked name; (f) the line is written and synced — on a write or sync error the file is cut back to its old length (a failure of the cut is ignored) and the exit is 2 with `lane: error: <text> and the release record cannot be written`, where `<text>` is `the lane was freed` for a release that removed a live record, `the parked file was removed` for the release of a ticket that was parked only, and `the ticket was parked` for `park`. A failure before (e) leaves the lane held.

The records dir must not be the lanes dir or inside it. A given path with a `.` or `..` part, or one whose leading parts are the given `--lanes-dir`, is exit 2 before the lock. Under the lock the check runs on resolved paths (a link counts): after (a) and before anything is created, on the nearest part of the path that exists (the path itself when it exists; a link counts as existing); and again after (b) on the records dir. When that second check refuses and this call created the records dir, the call removes it (folders it created above it stay). In both checks a path that cannot be resolved counts as inside, so a records dir that is a dangling link gets this message. A ticket id such as `lane-1` would put its record among the lane records, which the dir reader refuses. Exit 2, nothing freed.

Stricter than the wrapper writer it replaces:

- a reason over 1024 bytes is refused;
- U+0085 and other C1 characters are refused (the wrapper result depends on its locale);
- U+2028 and U+2029 are refused (the wrapper accepts them on macOS and in the C locale);
- a reason of only no-break or other Unicode spaces is blank here (the C-locale wrapper accepts it);
- an existing record over 1 MiB, not UTF-8, or without a final newline is refused;
- a records dir that is the lanes dir or inside it is refused;
- a records dir or record problem is exit 2, where the wrapper exits 1.

`not-live` is checked before the records dir and the record, while the wrapper checks its logs folder and the record first. The record gains `lane=<n>`; the wrapper line has no lane number.

## Release is explicit

A lane is freed by `harness lane release` and by `harness lane park` (which keeps the ticket's file list reserved until the ticket claims a lane again or is released; a claim whose removal of the parked file fails leaves the reservation — see *The parked file list*). `harness lane stale --apply` frees a lane through the park body. A dead process frees none in this slice.
