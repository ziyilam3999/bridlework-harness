# Ticket workspace (P1B-03)

Every ticket gets its own desk. If two tickets share one, one ticket's mess lands in the other's work and a later cleaner cannot tell whose mess is whose. `harness workspace create` is the desk maker: given a ticket, it looks up which repo the ticket is for, makes one fenced folder holding a fresh long-lived worktree of that repo (on the ticket's own branch) plus one scratch folder, and writes every one of those things on the ticket's resource manifest (`docs/resource-manifest.md`) BEFORE the thing exists — so nothing this command creates is ever unlisted. A crash between steps can leave listed things half-made, never an orphan.

It only makes workspaces. Clearing one away is teardown's job (P1B-14) or, after a crash, the P2 orphan reaper; both work from the manifest, never by guessing. The command never cleans up after itself and never retries in place: a re-run is refused as a second workspace.

```
harness workspace create <ticket-file>
    --repo-map <repo_id>=<absolute-dir> [--repo-map ...]
    --base <full-sha> --workspace-root <absolute-dir> --manifest-dir <absolute-dir>
    [--ts-ms <n>]
```

Every option is required except `--ts-ms` and extra `--repo-map`s. No default folder, no config file, no current-folder lookup — the same explicit-arguments rule as `harness ticket gate` and the manifest verb's `--dir`. The repo comes from the ticket and the repo map only, never from the folder the command runs in.

## What it does, in order

1. Reads the ticket with the strict reader and runs intake (`harness ticket intake`'s rules) with the given repo map. A bad ticket file, an unknown repo id, or a mapped folder that is not a top-level git repo with a commit is refused with exit 1. The repo must also be a **main** work tree, not a linked one (the `tree_prep.rs` rule), so a workspace is never made from another ticket's workspace.
2. The ticket id must be a plain token exactly as the manifest defines it (lowercase letters, digits, `-`, `_`, `.`; not empty, not `.` or `..`). An id that is not (for example `T-1`) is refused, never lower-cased or rewritten: lower-casing would let `T-1` and `t-1` share one desk.
3. `--base` must be a full commit sha that exists in the ticket's repo.
4. `--workspace-root` and `--manifest-dir` are each resolved once: absolute, existing, real directories (never links), not `/`, canonical, outside the repo's work tree and its git common dir (a workspace inside the repo would show up as an untracked change in the primary clone). The manifest dir must lie outside the workspace root (the record must survive removal of the roots) and the workspace root outside the manifest dir; equality counts as inside in both directions.
5. If the ticket already has a manifest file in `--manifest-dir`, it already has a workspace: refused. This is a read-only pre-check; the race-proof check is the next step.
6. Creates the ticket root `<workspace-root>/<ticket>` as a new, empty, real directory, mode 0700, with an exclusive create (fails if anything — including a symlink — already sits at that name). Two racing `create` calls for one ticket: exactly one wins here. From this step on, a failure leaves only the ticket root, the manifest and its listed entries (some paths may not exist yet); the command never cleans up.
7. Initialises the ticket's manifest in `--manifest-dir` with the ticket root as its ONE root.
8. Write-ahead: adds the entries BEFORE the things exist — id `worktree` (class `worktree`, path `<ticket-root>/tree`) and id `tmp` (class `temp_dir`, path `<ticket-root>/tmp`).
9. Makes `<ticket-root>/tmp` (mode 0700) and runs `git worktree add -b <branch> <ticket-root>/tree <base>` in the ticket's repo, with no hooks and no inherited `GIT_*` variable (the `intake.rs` / `tree_prep.rs` git-call rule). The branch is `ws/<ticket>`; if it already exists in the repo the command is refused (before anything is written) — a branch is never reused, never reset.
10. Reads the manifest back and confirms both entries are live and both paths exist; only then exits 0.

## Fixed names

Stable identifiers for the later tickets (P1B-04 node_modules link, P1B-11 clean-state check, P1B-13 quarantine, P1B-14 teardown):

| Thing | Name |
|---|---|
| ticket root | `<workspace-root>/<ticket>` |
| worktree | `<ticket-root>/tree`, manifest entry id `worktree` (class `worktree`) |
| scratch folder | `<ticket-root>/tmp`, manifest entry id `tmp` (class `temp_dir`), mode 0700 |
| branch | `ws/<ticket>` (left behind after teardown for the merge policy and the unpushed check) |

Later tickets find the paths with `harness manifest list <ticket> --dir <manifest-dir>`; they never rebuild them from text. The base sha is not stored (the manifest has no field for it); a later consumer takes `--base` explicitly, as `harness ticket gate` does, or uses the merge-base of `ws/<ticket>`.

## Output and exit codes

On success, exactly one stdout line of fixed fields:

```
workspace=created ticket=<ticket> repo_id=<id> base=<sha> branch=ws/<ticket>
```

No folder is ever printed on stdout or stderr, in either the given or the canonicalized spelling — the intake and config privacy rule: a folder may be private. The paths live in the manifest, which sits in a caller-chosen private folder.

- `0` — created.
- `1` — the ticket is refused at intake (bad ticket file, unknown repo id, repo not found); the same meaning as `harness ticket intake`.
- `2` — every other refusal or error (usage, bad or misplaced folders, non-token ticket id, unknown or non-full base, second workspace, a branch that already exists, git failure).

Any non-zero exit means "no usable workspace". Before step 6 nothing is written at all.

## Events

None. No P1b event kind names workspace creation, and the manifest file is the durable record.
