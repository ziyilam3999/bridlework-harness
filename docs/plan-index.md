# Plan index

The plan index is a file of JSON lines. Each line is one row of the plan. `harness ticket intake --plan-index <file>` reads it to check that a ticket names rows that exist. The reader is `core/rust/harness/src/plan_index.rs` (std only, runs nothing, writes nothing).

The index is private data of the operator. It is passed by path and is not kept in this repo.

## Row format

One JSON object per line, with exactly these 8 keys, each once:

| Key | Shape |
|---|---|
| `id` | 1 to 64 bytes of ASCII letters, digits, `.`, `_` or `-` |
| `title` | non-empty string |
| `plan_line` | JSON integer from 0 to 2^64-1, written with digits only |
| `state` | one of `not-built`, `in-build`, `built`, `deviation`, `later`, `unknown` |
| `tickets` | array of strings with the id character rule, may be empty |
| `deviation` | `null`, or an object whose keys are among `reason`, `decided_by`, `date`, `source`, each once and each a string |
| `trigger` | `null` or a string |
| `code` | array of strings |

`unknown` means no source shows the state yet. `null` is told apart from a number, `true`, `false`, an array and an object: a `deviation` of `1.5`, `true` or `[]` is a wrong type, not `null`.

## Read rules and refusals

- At most 1 MiB + 1 bytes are read; a larger file is refused without being read to its end.
- A line ends at LF. The last row may have no newline after it.
- A line ending in CR LF is read (the CR counts as JSON whitespace). A test pins this.
- An empty or blanks-only line is refused (a lone newline is one).
- A line must be one JSON object with nothing after it.
- Refused in any object: a repeated key, a lone surrogate escape, a raw control byte or NUL in a string, a byte order mark, a trailing comma, nesting deeper than 32.
- Refused: a missing key, an unknown key, a wrong type, an `id` outside the character rule, a `state` outside the six.
- Refused for the file: missing, a folder or not readable; over 1 MiB; not UTF-8; zero rows.
- A function lists the ids that appear on more than one row. Intake refuses an index with one.

A refusal of a line names the line number, and the field when it has one; a refusal of the whole file names no line. No refusal prints a value or a path. Each reason text is distinct for: missing or unreadable file, over the cap, not UTF-8, zero rows, and a line that does not parse.

## Intake flag

```
harness ticket intake <ticket-file> --repo-map <id>=<dir> [--repo-map ...] [--plan-index <absolute-file>]
```

`--plan-index` is optional and at most once. Given twice, given with no value, written as `--plan-index=<x>`, or not absolute or holding a `..` part: exit 2, stdout empty, one stderr line `harness: error: <what>`.

Order, with the exit code of each step:

1. Usage errors: exit 2.
2. The index read (each refusal of the reader, and an index with a repeated id): exit 2, stderr `harness: error: plan index: <reason>`.
3. The ticket file read: exit 2.
4. The ticket parse (both link keys, an empty list, a bad element or reason): exit 1.
5. The plan link, only when `--plan-index` is given: exit 1 with `harness: ticket refused: no plan link: main_plan_ref or off_plan is required`, or with `harness: ticket refused: main_plan_ref names <element>, which is not in the plan index` for the first element that is not an id of the index. An `off_plan` ticket passes this step.
6. The repo lookup.

Without `--plan-index`, no link is required and none is checked against an index; the shape of a link that is given is still checked at step 4. The accept line is the same in both modes: `ticket=<id> repo_id=<id> head=<sha>`.

## What it does not do

- It does not check whether a row is built, or that the row fits the work. Intake checks that an id exists.
- `plan check` and `plan status` are a later ticket.
- The MCP intake tool passes no index.
- An `off_plan` reason is not judged.
