# `harness ticket finalize` (P1B-15)

The one command that decides whether a ticket may be called Done. It builds no check of its own: every check is an existing verb that
finalize runs as a child process of the same binary. A child's stdout is captured under a 4 MiB cap and never printed; its stderr is dropped.
Finalize prints only fixed lines of validated tokens, never a folder.

```
harness ticket finalize <ticket-file> --manifest-dir <abs> --repo-dir <abs> --workspace-root <abs> --quarantine-dir <abs>
    --result-dir <abs> --caps-dir <abs> --log <abs> --role <token> --model <token> --host <abs> --config <abs> [--grace-ms <n>] [--ts-ms <n>]
```

`--host` is the host CLI (an absolute path to an existing regular file, no PATH lookup); `--config` is the harness config (its `lanes.count` bounds session names).

## Checks, in order (the first "no" stops)

The step numbers are this repo's reading of the P1B split (P1B-14 = 4, 5, 7, 8; P1B-15 = 1, 2, 3, 6, 9); output names checks, not numbers.

| # | Check | Step | Child verb | Pass |
|---|---|---|---|---|
| a | `gate` | 1 | `status` with the repo id mapped to the ticket's worktree | exit 0, `last_gate=pass`, `gate_current=yes` |
| b | `pushed` | 3 | `workspace check <t> --pushed --repo <repo-dir>` | exit 0 |
| c | `clean` | 6 | `workspace check <t>` | exit 0 |
| c2 | `sessions` | S-1.6, S-1.6.1 | `session close --ticket <t>`, then `session done-check --done <t>` | close exit 0, then done-check exit 0 |
| d | `sweep` | 6 | `quarantine sweep` | exit 0 and `moved=0` |
| e | `teardown` | 4, 5, 7, 8 | `workspace teardown` | exit 0 and `manifest_left=0` |
| f | `evidence` | 2 | `evidence check` | exit 0 |
| g | `done` | 9 | in-process | manifest re-read is clean with 0 live entries |

The worktree path comes only from the manifest's live `worktree` entry. The gate must have been run with the repo id mapped to that path.

## Output and exit codes

- Exit 0, one line: `finalize=done ticket=<t> repo_id=<r> head=<sha>`. Exit 0 plus this line IS "marked Done" (the `ticket gate` convention).
  Done is an exit code, not a stored event.
- Exit 1, one line: `finalize=refused ticket=<t> check=<name> reason=<fail|cannot-check>`, and a stderr line `harness: finalize refused: check=<name>`.
  `fail` is the check's own "no". `cannot-check` means the child could not answer (usage or read error, spawn failure, signal, over-cap output,
  or `status` exit 1 or 2).
- Exit 2: a usage or input error (missing, including `--host` or `--config`, duplicate or unknown flag, a relative path or one with `.` or `..`, a non-token role or model, an unreadable
  ticket, a missing or damaged manifest, no live `worktree` entry). Nothing was run and no log file is created.

## Order, and why

- **Sweep before teardown.** Teardown releases `evidence` entries from the manifest but keeps the files, so a sweep after it would shelve them.
  The sweep's move to the shelf is itself the action, so it runs even though Done is then refused (`moved>0` is `check=sweep reason=fail`).
  Re-run: the next sweep sees `moved=0` and finalize goes on.
- **Sessions after `clean`, before `sweep`.** The ticket's host sessions are stopped and removed once checks a to c pass, so nothing is removed under a
  failed gate. A failure there can be re-run (the manifest is still live); after teardown a re-run would stop with exit 2. It also stops a live builder
  before sweep and teardown, so it cannot write into the tree between them. The step then re-reads the listing: a session still listed for this ticket
  refuses Done. The refused line carries one more field: `finalize=refused ticket=<t> check=sessions reason=fail session=<name>` (`session=-` when no valid name is available).
  See `docs/session-close.md`.
- **Evidence after teardown.** A live process makes `evidence check` refuse Done (`running`), and only teardown stops processes. After teardown the
  run dirs are kept and still read, so a stopped run passes and a failed, unknown or damaged run still refuses.
- **Nothing destructive runs under a failed gate, unpushed commits or a dirty tree** (checks a to d come first).
- **A cleanup pass can sit beside an evidence refusal.** The log then holds `cleanup_result` `"result":"pass"` while finalize exits 1 with
  `check=evidence`. Cleanup passed and Done is refused; these are separate facts. The branch is pushed, the tree was clean and evidence is kept, so no work is lost.

## Limits

- No finalize lock: a `process start` racing teardown is caught by teardown's final manifest re-read, not prevented.
- No review check: the kernel has no review record yet.
- Done is not stored; `harness status` does not show it. Inside finalize the done set is exactly this ticket.
- Host verbs (`agents --json --all`, `stop`, `rm`) are tested against stub hosts only until the S-1.6 host re-probe.
- Finalize must not run from inside one of the ticket's own sessions: it would remove itself partway through. This is documented, not enforced.

## Each check's own doc

`docs/workspace.md` (clean-state, pushed, teardown), `docs/quarantine.md` (sweep), `docs/evidence.md` (evidence check).
