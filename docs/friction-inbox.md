# The friction inbox (K-1 slice 1)

The friction inbox is where the harness writes down what broke while it is being built: one row per friction or
lesson, captured the same turn it is found (T-9). This page is the public contract of the row, the verbs that read
and write it, and the `lesson:` close line. The rule itself is the knowledge-routing rule (K-1, ticket 153); this
ticket (154) builds its first slice: capture first, so the store grows while the harness is built.

**The inbox file is not in this repo.** It lives only in the private overlay (5.2 item 4): lesson text is real
operational text, so it is private data. This repo holds the contract, the reader and the lint — never the rows.

Status: contract plus `harness friction add | lint | close-check` (ticket 154). The weekly twice-broken printout is
154b in the private companion; the generated lessons index (`harness find`) is slice 2; see "Not in slice 1" below.

## 1. The row (the one contract everything else reads)

One JSON object per line, exactly these six keys, every one required (a missing, empty or extra key is refused):

```
{"id":"les-001","date":"2026-11-01","ticket":"154","where-it-lives":"memory","breaks":"","text":"the lesson"}
```

| field | rule |
|---|---|
| `id` | a plain token: lowercase letters, digits, `-`, `_`, `.`; never empty, never `.` or `..`; unique in the inbox |
| `date` | `YYYY-MM-DD` naming a real calendar date (`2026-02-30` is refused; leap years are honoured) |
| `ticket` | a plain token, the ticket that captured the row |
| `where-it-lives` | exactly one of `skill`, `gate`, `memory`, `wiki` (K-1.1; see the table below) |
| `breaks` | empty, or the `id` of a lesson this row says broke again (K-1 rule 3); it must name another row that exists in the inbox, never itself |
| `text` | the lesson, non-empty; one line (a newline inside it is stored escaped, the file stays one row per line) |

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
  --where-it-lives <skill|gate|memory|wiki> [--breaks <lesson-id>] --text <lesson> [--config <file>]
```

Appends one row. Everything is checked before anything is written, in this order: the flags, the row rules
(section 1), the inbox cross rules (a duplicate `id`; a `breaks` naming no lesson or naming itself; the existing
inbox must itself be clean and untorn), then every field goes through the same privacy engine as the rest of the
harness. Only then is one whole line (plus newline) appended and synced; the file is created when absent, its
folder is not. On success stdout is `stored id=<id>`.

A missing required field flag (`--where-it-lives` absent) is a row refusal (1) naming the field, not a usage
error: K-1.1 counts "no tag" as refused. With no privacy list configured the verb fails closed (exit 2) — an
unscanned row is never stored.

### `harness friction lint`

```
harness friction lint [--config <file>]
```

Lints the whole inbox with the same rules: per-row rules with 1-based line numbers, then the cross-row rules
(duplicate `id`, a `breaks` naming no lesson, a `breaks` naming its own row). Exit 0 clean (stdout
`inbox ok rows=<n>`); 1 any bad row or a torn tail, each named with its line on stderr. Hand-written rows from
before this contract and the 154b weekly script share this one checker. A missing inbox is a setup error (2):
`lint` is how a real inbox is audited, so it must exist.

### `harness friction close-check`

```
harness friction close-check <close-note> --ticket <t> [--config <file>]
```

K-1.5: at ticket end, the close note names each lesson the ticket captured, or says none. Exit 0 the note matches
the inbox (stdout `close-check ok ticket=<t> lessons=<n>`); 1 refused (stdout empty); 2 a usage or setup error. A
missing inbox reads as no rows (a fresh install), so only `lesson: none` can pass; a damaged inbox (torn, bad
rows, unreadable) fails the check closed (2), because it cannot be compared against.

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
  the row naming the field and the class, never the bytes.
- Input bounds, all failing closed past the bound: one row line is capped at 16 KiB; the inbox is read no further
  than 4 MiB; a close note no further than 1 MiB. Over the bound the verb refuses (1 for a row, 2 for a file).
- Each row is written as one whole line plus newline. A last line with no newline is a torn tail (a crashed
  append): reported by `lint` with its line number, never read as a row; `add` refuses to append onto it.

## 5. Not in slice 1 (where each piece went)

- The weekly twice-broken printout (lessons at 2 `breaks`, open gate tickets with their age) — 154b, in the
  private companion repo, because its printed list carries lesson text.
- Wiring `close-check` into the automated S-1.6 removal step — 154c (until then it runs by hand).
- The generated lessons index, `harness find` over the inbox — slice 2 (K-1 rule 4, X-81a).
- Promotion of lessons to the wiki / W3 Lesson Store, and the tag-vs-`controls` lint (K-1.3) — P3.
- Automatic gate-ticket filing and skills extraction — the P8 factory.
- Migrating existing hand-written inbox rows to this shape — a private-overlay chore by hand (`friction lint`
  names each bad line; no public code touches the file).
