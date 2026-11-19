# Review-round cap

After the third recorded review FAIL on a ticket, a further fix round is refused and the ticket waits for the operator. The engine keeps its own count because bw keeps one review record per ticket and overwrites it, so no history of FAILs exists there.

Code: `core/rust/harness/src/review_cap.rs`; the verbs are wired in `core/rust/harness/src/cli.rs`.

## What is counted

- **Counted:** a review FAIL head, once per head. Recording the same head again changes nothing.
- **Not counted:** rounds started from word-check notes (`bw seat --notes-by`), PASS reviews (a PASS has no verb and does not reset the count), and gate runs (the fix-round cap of `ticket gate` is a separate cap).

## The record

`<caps>/<ticket id>.reviews` holds `{"v":1,"ticket":"<id>","repo_id":"<repo>","fail_heads":[...]}` plus a newline, nothing else. `fail_heads` is 0 to 16 distinct full lowercase shas (40 or 64 hex) in the order recorded.

The reader takes the canonical bytes for the expected ticket and repo only. Refused (exit 2, not read as 0): a record of another ticket or repo, a reordered, widened or extra-key record, a duplicate head, an uppercase or short sha, more than 16 heads, an oversize file, a link or non-regular entry, and a differently-cased twin name. No record means 0 FAILs.

Known gap: a record that is moved or deleted reads as 0 FAILs.

Writes go through a temp file and a rename, under the per-ticket lock of the caps folder. A busy lock is exit 2 (`a run for this ticket is in progress`).

## Verbs

Both take `<ticket-file> --repo-map <repo_id>=<absolute-dir> --caps-dir <absolute-dir>`.

`harness ticket review-fail ... --head <full-sha>` records one FAIL for that head.

- New head: `review-fail ticket=<id> head=<sha> fails=<n> counted=yes`, exit 0.
- Head already recorded: the same line with `counted=no`, exit 0, nothing written.
- A 17th distinct head: exit 2, nothing written.

`harness ticket fix-round-check ... --role <token> --model <token>`:

- Fewer than 3 FAILs: `ticket=<id> result=allowed fails=<n>`, exit 0, nothing written.
- 3 or more: appends one escalation event to `<caps>/events.jsonl` (plus a `run_issued` line first when the run id is self-issued) with reason `review-round-cap`, prints `ticket=<id> result=refused reason=review-round-cap fails=<n> next=operator-waiting`, exit 1. Each refused call appends one escalation event.
- The append fails: exit 2, `ESCALATION NOT RECORDED` on stderr, no refused line.

A ticket whose repo id is not in the map is a ticket refusal (exit 1). Usage faults, a relative or dot-dot `--caps-dir`, a caps folder inside the repo, a bad `--head`, and a bad role or model token are exit 2. No folder is printed.

## Role and model are flags

The event needs a role and a model. `Caps::escalate` reads them from `HARNESS_ROLE` and `HARNESS_MODEL`. bw sets those two names on its engine `ticket gate` call only, from the `role=` and `model=` values of the seat record, and removes both when there is no seat record. `fix-round-check` takes `--role` and `--model` and calls `Caps::escalate_with`; it does not read those two variables. Both are event tokens (lowercase letters, digits, `-`, `_`, `.`), the same rule as `ticket finalize`. The run id follows `Caps::escalate`: `HARNESS_RUN_ID` when set and issued in the log, else a self-issued id. `ticket gate` reads the two names from its environment for its own refusals (the fix-round cap and the re-plan threshold); when either name is missing or not a plain token, such a refusal exits 2 with `ESCALATION NOT RECORDED`.

## How bw will call the verbs (not built)

Not built yet. After a review FAIL, bw would call `review-fail` with the head that failed; before a fix round starts, it would call `fix-round-check` and stop on exit 1, opening a WAITING item for the Orchestrator. Until bw makes these calls the cap is not live.
