schema: ../hub/CLAUDE.md
schema-version: 0.2.2

# Friction wiki (local delta)

This is the CLAUDE.md delta of the private friction wiki (ticket 3601). It adds only what is local; the shared
wiki schema (layers, page front-matter, index and log rules, lint) is the schema pointed at by the first line.

## Raw sources (the one rule)

- `raw/friction-inbox.jsonl` and `raw/triage-state.jsonl` are APPEND-ONLY raw sources. They are written only by the
  `harness friction` verbs (`add`, `sweep`, `triage`, `bind`); nothing else, no agent and no editor, writes or
  rewrites a line. The lint of the harness (`harness friction lint`) is the one reader that audits them.
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
