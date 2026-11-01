# Quarantine (P1B-13)

`harness quarantine` is the never-delete half of cleanup: when a ticket ends, anything left in the ticket's roots that the manifest does NOT own is carried, whole and untouched, to a quarantine shelf — it might be somebody's real work. Deleting the things the manifest DOES own as scratch (`temp_dir` entries) is a second, separate, explicit verb that names exactly one id. The sweep itself never deletes anything.

Code: `core/rust/harness/src/quarantine.rs` (Unix only), the verb dispatch in `core/rust/harness/src/cli.rs`. The roots come from the ticket's manifest (`docs/resource-manifest.md`), the workspace shape from `docs/workspace.md`, and the `quarantined` event from `docs/event-log.md`.

## `harness quarantine sweep`

```
harness quarantine sweep <ticket> --manifest-dir <abs> --workspace-root <abs> --quarantine-dir <abs>
    --log <abs> --repo <token> --role <token> --model <token> [--max-items <n>] [--ts-ms <n>]
```

- **Roots** = the `root` lines of the ticket's manifest, read through the manifest reader — never from a CLI argument, never guessed. A damaged manifest (missing, torn tail, corrupt) owns nothing, so the sweep refuses: exit 2, nothing moved.
- **Allowed base.** `--workspace-root` must be an existing, real (never a link), canonicalizable directory, not `/` — the same rules as `workspace create`. Every root must itself be a real directory checked with a no-follow stat (a root that IS a symlink → exit 2) and must resolve strictly inside `--workspace-root` (outside → exit 2). Every refusal fires before anything is written.
- **What a leftover is.** A top-down walk of each root using no-follow stats only. An entry a live path entry owns (equals or inside) is skipped whole; an entry a live path entry lies inside is descended into; anything else is a leftover and is moved as one unit (whole subtree, one rename). Stray files inside the worktree `tree` are NOT this verb's business — that is the clean-state check (P1B-11, ticket #155).
- **Symlinks are never followed.** A symlink in a root is a leftover like any other and is moved as the link itself; its target is never read, stat-followed or touched. The listed `node_modules` link is owned and therefore skipped.
- **Shelf layout.** `<quarantine-dir>/<ticket>/<batch>/<item>`: `<batch>` is a fresh token `b-<ts_ms>-<pid>-<seq>` minted with an EXCLUSIVE create (exists → next seq, never reused) at mode 0700, and `<item>` is `i-<n>` inside that fresh batch. Original names are never used as quarantine names (they may not be tokens or UTF-8). Nothing already on the shelf is ever renamed onto, replaced or removed; the item name is checked absent with a no-follow stat immediately before the rename. The shelf itself (`<quarantine-dir>/<ticket>`) is examined with a no-follow stat before it is read or made: a symlink or a regular file there is refused (exit 2) — otherwise every batch, journal and item would land outside the quarantine dir.
- **Write-ahead order per item.** (a) one intent line appended to `<batch>/journal.jsonl` under an exclusive lock, one write, `sync_all` — the item token, the source path relative to the workspace root as hex-encoded raw bytes, and the reason; (b) the rename; (c) the `quarantined` event through the library record writer (`reason` `unmanifested`, `to` = `<ticket>/<batch>/<item>`, `from` = the source relpath when every segment is a token, else `<ticket>/untokenable/<item>` — the journal holds the exact bytes either way). The event is written after the move because it states a move that happened; the journal line is the crash trace.
- **Idempotent re-run / crash recovery.** Before walking, the sweep reconciles every journal under the ticket's shelf. An intent that already has its matching event is settled (whatever sits at the source path now is a later leftover of the same name — the walk's business). An intent whose item is on the shelf and whose source is gone, with no event, gets its event written. An intent whose source still exists and whose item is absent is simply found again by the walk. An intent with no event whose source AND item both exist, or neither exists, cannot be decided: exit 3, nothing moved. A second sweep after a complete one moves nothing, writes no event, exits 0.
- **Event log placement.** `--log` must lie OUTSIDE every root (resolved through its nearest existing ancestor, so a not-yet-created log dir inside a root is refused too): a log written inside a root would itself be an unmanifested leftover the next sweep carries off. Inside a root → exit 2, nothing moved.
- **Cross-device.** Before any move the device id of every root is compared with the quarantine dir's (no-follow stat). Different → exit 3, nothing moved. There is NO copy fallback: copy-then-delete would be a delete. A rename that still fails mid-run (including a real cross-device error) stops at once with exit 3; already-moved items stay moved (journalled and evented).
- **Bounds.** The walk is read-only and completes before the first move. Leftovers are capped at `--max-items` (default 1000), entries visited at 100,000, depth at 64. Over any cap → exit 3, nothing moved, a stderr line naming the cap.
- **Names.** All path handling uses raw bytes; a non-UTF-8 name never panics and is quarantined like any other (hex bytes in the journal, the `untokenable` form in the event).

## `harness quarantine delete-listed`

```
harness quarantine delete-listed <ticket> --id <id> --manifest-dir <abs> [--ts-ms <n>]
```

Deletes exactly ONE live manifest entry of class `temp_dir`, named by `--id`. The fence is made on the RESOLVED path, never on the stored spelling: any `..` or `.` component is refused (the raw text is scanned — `Path` hides interior `.`); every root is re-validated as the sweep validates it (a no-follow stat, a real dir, canonicalizable, never a link); and the canonicalized PARENT of the listed path must equal or lie inside a canonical root, so a symlinked intermediate component or a root swapped for a symlink resolves the entry OUTSIDE the roots and nothing is deleted (exit 2). The final component is examined with a no-follow stat: a symlink entry is removed as the link only (never its contents — the `node_modules` rule); a real directory is removed recursively, POST-ORDER (a directory only after its children — a nested subtree goes in one run), WITHOUT following any symlink inside it (a link inside is unlinked, its target untouched). Only after the path is gone is the manifest `remove` line written — a crash between leaves a listed half-deleted thing, never an orphan; a re-run on an already-absent path just writes the remove. Any other class, an unknown or non-live id → exit 2.

**P1B-14 (finalize teardown) MUST call this ticket's library function `quarantine::delete_listed` for manifested `temp_dir` entries instead of building a second delete path** — two delete paths drift.

## Exit codes and output

`0` done (including zero leftovers); `2` refused (usage, bad or misplaced folders — including the event log inside a root — a root that is a symlink or outside the base, a damaged manifest, a quarantine shelf that is a link or a file, a listed path with a `..`/`.` component or one that resolves outside every root, wrong class or id); `3` cannot proceed (cross-device, over a cap, an undecidable journal intent, a rename/journal/event write failure mid-run). Success prints one stdout line of fixed fields — `quarantine=done ticket=<t> moved=<n>` / `delete=done ticket=<t> id=<id>` — and no folder path is ever printed on either stream (the workspace privacy rule, `docs/workspace.md`).

## v1 limits (honest)

- No no-replace rename syscall in std: the no-overwrite promise rests on the fresh exclusive batch dir, generated item names, and the no-follow absent check right before the rename. A same-user writer inside the fresh mode-0700 batch dir between check and rename could still race; the batch is created by this process, which bounds that to same-user code.
- The event lands after the move: a crash between the two leaves a moved item with no event until the next sweep reconciles it from the journal. A consumer reading the event log before a re-run sees one fewer event than items on the shelf; finalize (P1B-15) runs the sweep, so reconcile runs first.
- The cross-device refusal is exercised in CI through a library seam (`sweep_with_device_probe`), not a real second filesystem; the real cross-device rename error is covered fail-closed by construction (any rename error → exit 3).
