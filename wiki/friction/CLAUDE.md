schema: ../hub/CLAUDE.md
schema-version: 0.2.2

# Friction wiki (local delta)

This is the CLAUDE.md delta of the private friction wiki (ticket 3601). It adds only what is local; the shared
wiki schema (layers, page front-matter, index and log rules, lint) is the schema pointed at by the first line.

## Raw sources (the one rule)

- `raw/friction-inbox.jsonl` and `raw/triage-state.jsonl` are raw sources that the `harness friction` verbs append
  to (`add`, `sweep`, `triage`, `bind`). The one in-place writer is `harness friction set-status`: it changes the
  `status` and `status_ref` of one inbox row and writes the other lines back from their original bytes. Agents and
  editors do not write or rewrite a line. A `raw/friction-inbox.jsonl.status-write` file next to the inbox is a
  left-over recovery copy: the verbs that read the inbox refuse until it is moved aside by hand. The lint of the
  harness (`harness friction lint`) is the reader that audits them.
- Pages are `triage-<yyyy>-w<ww>.md`, written by `harness friction page` (front-matter `kind: note`,
  `verified: no`). The page is the only place lesson text is printed: the verbs' stdout carries ids and numbers.
- `index.md` and `log.md` follow the shared schema; `bin/friction-loop.sh` appends one line to each per week.

## The loop

`bin/friction-loop.sh --week <w>` runs weekly: closed feed, `harness friction triage`, `harness friction page`,
the wiki lint, one commit. A hand-off file is a normal ticket; after the orchestrator files it, it binds the
returned task id with `harness friction bind` so the next run can resolve the group from the closed feed.

## Privacy

This wiki is private (`"private": true` in the hub registry). The public harness repo holds code and templates
only; no row, state line or page is ever copied there.
