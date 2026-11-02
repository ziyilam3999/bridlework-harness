# The friction inbox (K-1 slice 1)

The friction inbox is where the harness writes down what broke while it is being built: one row per friction or
lesson, captured the same turn it is found (T-9). This page is the public contract of the row, the verbs that read
and write it, and the `lesson:` close line. The rule itself is the knowledge-routing rule (K-1, ticket 153); this
ticket (154) builds its first slice: capture first, so the store grows while the harness is built.

**The inbox file is not in this repo.** It lives only in the private overlay (5.2 item 4): lesson text is real
operational text, so it is private data. This repo holds the contract, the reader and the lint — never the rows.

Status: contract plus `harness friction add | lint | close-check` (ticket 154) and the pattern half — `kind`,
`repeats`, `friction patterns` and the trial record (ticket 192). The weekly listing script is 154b in the private
companion; the generated lessons index (`harness find`) is slice 2; see "Not in slice 1" below.

## 1. The row (the one contract everything else reads)

One JSON object per line: exactly these six keys, every one required (a missing, empty or extra key is refused), plus
two OPTIONAL keys, `kind` and `repeats` (ticket 192), omitted at their defaults:

```
{"id":"les-001","date":"2026-11-01","ticket":"154","where-it-lives":"memory","breaks":"","text":"the lesson"}
{"id":"pat-001","date":"2026-11-01","ticket":"192","where-it-lives":"memory","breaks":"","text":"the procedure that worked","kind":"pattern"}
{"id":"pat-001-r1","date":"2026-11-02","ticket":"192","where-it-lives":"memory","breaks":"","text":"it worked again","kind":"pattern","repeats":"pat-001"}
```

| field | rule |
|---|---|
| `id` | a plain token: lowercase letters, digits, `-`, `_`, `.`; never empty, never `.` or `..`; unique in the inbox |
| `date` | `YYYY-MM-DD` naming a real calendar date (`2026-02-30` is refused; leap years are honoured) |
| `ticket` | a plain token, the ticket that captured the row |
| `where-it-lives` | exactly one of `skill`, `gate`, `memory`, `wiki` (K-1.1; see the table below) |
| `breaks` | empty, or the `id` of a lesson this row says broke again (K-1 rule 3); it must name another row that exists in the inbox, never itself |
| `text` | the lesson, non-empty; one line (a newline inside it is stored escaped, the file stays one row per line) |
| `kind` (optional) | empty (= `friction`), `friction` or `pattern`; anything else is refused naming the field. `friction` is a lesson learned the hard way; `pattern` is a repeatable procedure that worked. Omitted at its default, so every six-key row written before this key existed re-encodes byte-identical |
| `repeats` (optional) | empty, or a plain row id token. The pointer rules mirror `breaks`: it must name a row that exists in the inbox, never itself; the row carrying it must itself be `kind: pattern`; and the row it names must be a `kind: pattern` row with an empty `repeats` — the chain ROOT — so a chain's repeat count is one unambiguous number. A `pattern` row with no `repeats` is legal: it is the root of its chain |

The count of a pattern chain is mechanical, never judged: it is the number of rows whose `repeats` names the root,
counted by the verb, remembered nowhere. When the count reaches N (`friction.pattern_threshold`, the config contract's one
optional key; default 3), `harness friction patterns` lists the chain as a candidate for a reusable control — and listing it creates
zero files under any skill or ticket path (the P8 factory judges what to build, later).

The tag is the ROUTE chosen at capture, nothing more (K-1 rule 2):

- `skill` — a repeatable procedure;
- `gate` — a machine-checkable never-break rule;
- `memory` — an operational lesson (before P3, this inbox row is its home);
- `wiki` — project knowledge.

## 2. The verbs

The inbox path comes from the config (`friction.inbox`, config contract v3), resolved exactly as `harness config
check` resolves the file: `--config <file>`, else `HARNESS_CONFIG`, no third source, an empty value refused and
never defaulted. The path is never printed.

Exit codes for the whole family: **0** ok; **1** a refusal; **2** a usage or setup error. Nothing is written to
stdout when the exit is non-zero. A refusal names the field and the reason on stderr and **never** echoes the
lesson text, the matched privacy bytes, or the inbox path. A refused write leaves the inbox byte-identical — and
if the inbox did not exist yet, it is still not created.

### `harness friction add`

```
harness friction add --id <id> --date <YYYY-MM-DD> --ticket <t> \
  --where-it-lives <skill|gate|memory|wiki> [--breaks <lesson-id>] \
  [--kind <friction|pattern>] [--repeats <row-id>] --text <lesson> [--config <file>]
```

Appends one row. Everything is checked before anything is written, in this order: the flags, the row rules
(section 1), every field of the new row through the same privacy engine as the rest of the harness, then — under
one exclusive writer lock (section 4) — the same privacy engine over every stored row and the inbox cross rules
(a duplicate `id`; a `breaks` naming no lesson or naming itself; the `repeats` pointer rules of section 1; the
existing inbox must itself be clean, untorn and privacy-clean). Only then is one whole line (plus newline) appended and synced; the file is created when
absent, its folder is not; a refused row never creates the file (a first row whose only possible refusal is a
`breaks` with nothing to name is refused before the file is opened). On success stdout is `stored id=<id>`. The new row is scanned before any cross rule
runs, so a refusal can never echo an unscanned byte of it.

A missing required field flag (`--where-it-lives` absent) is a row refusal (1) naming the field, not a usage
error: K-1.1 counts "no tag" as refused. With no privacy list configured the verb fails closed (exit 2) — an
unscanned row is never stored, and a usage error never echoes the raw flag or argument bytes either: it names
the problem, not the value, on every verb's unknown-option and stray-positional paths. One refusal names a
value by contract: a CONFIG refusal may name a lowercase token-shaped key (never a value), because that is the
shared config contract (`docs/config-contract.md`) — so the never-echo claim here is scoped to the friction
verbs' own arguments, rows and note lines.

### `harness friction lint`

```
harness friction lint [--config <file>]
```

Lints the whole inbox with the same rules: per-row rules with 1-based line numbers, then the cross-row rules
(duplicate `id`, a `breaks` naming no lesson, a `breaks` naming its own row), then the same privacy engine over
every field of every row — a hand-written row holding private text is refused exactly like a new row would be
(line, field and class on stderr, never the bytes). Exit 0 clean (stdout `inbox ok rows=<n>`); 1 any bad row, a
privacy hit or a torn tail, each named with its line on stderr. With no privacy list configured lint fails
closed (exit 2): an inbox is never linted clean unscanned. Hand-written rows from before this contract and the
154b weekly script share this one checker. A missing inbox is a setup error (2): `lint` is how a real inbox is
audited, so it must exist.

### `harness friction patterns`

```
harness friction patterns [--config <file>] [--record <file>]
```

The N-threshold listing (ticket 192). It runs the FULL lint first — any lint failure is the same exit and the same
stderr as `friction lint`, with nothing on stdout — then prints one line per chain-root pattern row whose count of
`repeats` rows is >= N, sorted by id:

```
pattern id=<id> repeats=<n>
patterns ok threshold=<N> listed=<k>
```

Ids and counts only: a row's `text` is never printed, and the ids it prints have all passed the same privacy
engine as the lint. N is `friction.pattern_threshold` from the config (the one optional key in the config
contract; absent = 3). Being listed creates NOTHING: no skill, no ticket, no file under any skill or ticket path —
the listing is a candidate list for the P8 factory, which judges what to build.

The verb writes nothing, with one exception: when `--record <file>` is given, exactly one JSONL line is APPENDED
to that file — `{"date":"YYYY-MM-DD","threshold":N,"rows":<rows>,"listed":["<id>",...]}` (`schemas/friction-trial.schema.json`
is the shape; the date is the run's UTC day, written by the harness). The trial record is the keep-or-remove
evidence for this feature: if four weekly records show nothing ever reaching N, Governance removes `kind` and
`repeats` (the 4-run ablation decision is Governance's, not a verb's). Exit codes as the family: 0 ok; 1 the lint
leg refused; 2 a usage or setup error — including a failed record append, so a half-written trial never reads as
a clean run.

### `harness friction close-check`

```
harness friction close-check <close-note> --ticket <t> [--config <file>]
```

K-1.5: at ticket end, the close note names each lesson the ticket captured, or says none. Exit 0 the note matches
the inbox (stdout `close-check ok ticket=<t> lessons=<n>`); 1 refused (stdout empty); 2 a usage or setup error. The
`--ticket` value is a plain token and is also run through the privacy engine BEFORE anything is printed: a hit exits
1 with `field ticket has a privacy hit (<class>)` on stderr (the class only, never the bytes) and nothing on stdout,
so the success line only ever carries a ticket that passed the scan. A
missing inbox reads as no rows (a fresh install), so only `lesson: none` can pass; a damaged inbox (torn, bad
rows, unreadable, a privacy hit in a stored row) fails the check closed (2), because it cannot be compared
against. The `lesson:` lines themselves go through the same privacy engine before the check, so an id the check
names back on a refusal has always passed the scan; with no privacy list configured the verb fails closed
(exit 2), exactly like `add` and `lint`.

Orchestration runs this by hand at the S-1.6 session-removal step until P1b automates removal (154c); a non-zero
exit blocks removal.

## 3. The `lesson:` close-line grammar

- A close-line is a note line whose trimmed form begins `lesson:` (case-sensitive; where it sits in the note does
  not matter — the check is mechanical).
- Exactly one close line per note: none is refused, two are refused.
- The value is `none`, or a comma-separated list of lesson ids (`lesson: les-001, les-002`); an empty list, an
  empty element, a duplicate id or a non-token id is refused.
- `lesson: none` passes only when the inbox holds NO row with this ticket.
- `lesson: <ids>` passes only when the named ids are exactly the inbox rows whose `ticket` is this ticket:
  an unknown id, an id belonging to another ticket, or a captured row missing from the line is refused.

## 4. Privacy, caps, torn tails

- Every field of a new row is scanned by the existing privacy engine (`harness privacy`'s classes); a hit refuses
  the row naming the field and the class, never the bytes. Every reader of the inbox runs the same scan over
  every stored row (`add`, `lint`, `close-check`), and `close-check` scans its `lesson:` lines too.
- One writer at a time: `add` takes ONE exclusive kernel advisory lock (`flock(2)`, the std file lock) on the inbox
  file itself, and does the read, scan, cross check and append through that same open descriptor, so two
  concurrent adds can never both store the same id (the check and the append are one critical section) — however
  many writers arrive at once and however the inbox path is spelled. The lock lives on the inbox's inode, so every
  spelling of the path shares it: a symlinked folder, a symlinked file, a `./` or doubled-slash detour, a hard
  link, and a not-yet-created file behind a symlink (the first append creates the target and locks it). There is
  no side lock file, no path built for locking and nothing canonicalized. The kernel releases the lock the moment
  the holder exits — return, panic, or a kill -9 — so a crashed writer can never leave the inbox locked: there is
  NO steal rule and no stale-lock age at all. A file or folder named `<inbox>.lock` is not a lock and is simply
  ignored (never read, locked, renamed or deleted: the harness never deletes anything it did not create). A
  lock held longer than 10 seconds by a live writer fails the verb closed (exit 2, `the inbox is busy`, no
  path). The lock call (`File::try_lock`) needs Rust 1.89 or newer; the workspace pins 1.99.1. `lint` and
  `close-check` only read and take no lock; the lock is advisory, so it protects against other harness writers,
  not against an editor that ignores it. A refused row never creates the inbox file.
- Input bounds, all failing closed past the bound: one row line is capped at 16 KiB; the inbox is read no further
  than 4 MiB; a close note no further than 1 MiB. Over the bound the verb refuses (1 for a row, 2 for a file).
  `patterns` never writes the inbox at all, and its only output file is the one `--record` line; the listing
  itself cannot loop or grow past its bound (one pass over the already-capped inbox, one line per root).
- Each row is written as one whole line plus newline. A last line with no newline is a torn tail (a crashed
  append): reported by `lint` with its line number, never read as a row; `add` refuses to append onto it.

## 5. Not in slice 1 (where each piece went)

- The weekly printout (lessons at 2 `breaks`, open gate tickets with their age, the pattern candidates at N
  repeats) — 154b, in the private companion repo, because its printed list carries lesson text; it calls
  `harness friction patterns --record` (ticket 192) for the pattern half, so the public trial record grows
  one line per weekly run.
- Wiring `close-check` into the automated S-1.6 removal step — 154c (until then it runs by hand).
- The generated lessons index, `harness find` over the inbox — slice 2 (K-1 rule 4, X-81a).
- Promotion of lessons to the wiki / W3 Lesson Store, and the tag-vs-`controls` lint (K-1.3) — P3.
- Automatic gate-ticket filing and skills extraction — the P8 factory.
- Migrating existing hand-written inbox rows to this shape — a private-overlay chore by hand (`friction lint`
  names each bad line; no public code touches the file).
