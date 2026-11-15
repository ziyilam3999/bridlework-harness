# Lanes

`harness lane claim | release | list` keeps the live tickets and their lane under a cap. In this slice the cap is a fixed 1 (no admission exists yet).

```
harness lane claim <ticket-file> --lanes-dir <abs dir> --config <file> --scope-file <file>
harness lane release <ticket-id> --lanes-dir <abs dir>
harness lane list --lanes-dir <abs dir> --config <file>
```

- `claim` prints `ticket=<id> lane=<n>` (the lowest free lane). The ticket goes through the strict ticket reader.
- The scope file holds one relative path per line (blank lines are skipped). A missing or empty file, an absolute entry or a `..` part is refused with exit 2 and nothing is recorded.
- `list` prints one line per live ticket, `ticket=<id> lane=<n> repo_id=<id> scope=<entries>`, then a last line `live=<n> cap=<n>`.
- `release` prints `ticket=<id> released`.

## Record format

The lanes dir is created when missing. One file per held lane, `<lanes-dir>/lane-<n>`, text:

```
lane-record v1
ticket=<id>
repo_id=<id>
scope=<entry>        (one line per entry, at least one)
```

A ticket id is letters, digits, `.`, `_`, `-` (at most 128, not starting with `.`). A record is written whole to a private temp file and published with a hard link, which is atomic and fails when the lane is taken, so two `harness` processes racing never both win a lane and a reader never sees a half-written record. A record that does not parse makes `list`, `claim` and `release` exit 2.

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
