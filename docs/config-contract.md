# Config contract (v4)

The harness reads one settings file. This page is the contract in plain words. `config/harness.example.yaml` is the public example of it, with placeholders only.

Status: contract plus its strict reader (PRE-5b) and `harness config check` (PRE-5c). v2 added the required `lanes.count` key; v3 added the required `friction.inbox` key (ticket 154): the path of the friction inbox file (`docs/friction-inbox.md`); v4 adds the required `factory.design_tier` key (ticket 190): the tier the factory designs new fixes at (`docs/fix-tier.md`). A config written against an older version is refused until it gains the key (intended, fail closed): the harness refuses to guess where the inbox is or which tier the factory designs at. Ticket 192 then adds the contract's FIRST OPTIONAL key, `friction.pattern_threshold` (absent = 3): an optional key does not change the version — v4 stays v4 — because an optional key breaks no existing file; a v4 file needs no change. Ticket 3601 adds the SECOND optional key, `friction.triage_top` (absent = 3), on the same terms. The operator's real file is updated in the private overlay the day a change merges.

## 1. One file, one reader, read-only

The config is one text file. The program reads it. The ONLY write verb is `harness config write`, which refuses unless the write carries a human-origin approval (`docs/human-origin.md`); there is no set or install verb, and the installer never writes the real file.

## 2. Where it lives

- Public: `config/harness.example.yaml` in this repo. Placeholders only (section 7).
- Private: the operator's real file, kept outside this repo. It is named by `--config <file>`; when the flag is absent, by the environment variable `HARNESS_CONFIG`. The flag wins.
- There is no third source: no search, no default location, no file beside the binary, no fallback to the example, no current-folder lookup.
- No source configured is a refusal, not a default. An empty `--config` value or an empty `HARNESS_CONFIG` is a refusal too; it never falls back to the other source or to the example.

## 3. Shape

Six required sections, in this order of meaning: `repos`, `budgets`, `shadow`, `lanes`, `friction`, `factory`, plus one OPTIONAL seventh, `builder_state` (ticket 3703). Two levels only: a section line (`name:` with nothing after it), then indented `key: value` lines (two spaces). Whole-line `#` comments and blank lines are allowed. Spaces only.

### Keys

Every key below is required, in its section, exactly once. Nothing else is allowed — with ONE exception, named
here explicitly because it is the first of its kind in this contract: `friction.pattern_threshold` is OPTIONAL
(ticket 192). With `friction.triage_top` (ticket 3601) these are the only two keys a file may omit; when present
each follows every other rule (its section, exactly once, the value grammar below). Absent means the default, 3,
for both.

```
budgets.s_wall_seconds
budgets.s_tokens
budgets.m_wall_seconds
budgets.m_tokens
budgets.l_wall_seconds
budgets.l_tokens
shadow.p1a_cap
shadow.p4a_cap
shadow.d2a_cap
shadow.p7_cap
shadow.p1a_token_budget
shadow.p4a_token_budget
shadow.d2a_token_budget
shadow.p7_token_budget
lanes.count
friction.inbox
friction.pattern_threshold   <- OPTIONAL: a key a file may omit; absent = 3
friction.triage_top          <- OPTIONAL (ticket 3601): the second key a file may omit; absent = 3
factory.design_tier
```

`repos` has no fixed keys: each key is a repo id (section 5). It may be empty.

`builder_state` (OPTIONAL section, ticket 3703; the version stays v4, like ticket 192) has no fixed keys either: each line is `  <entry-name>: <text>`. The entry name must be a token (lowercase letters, digits, `-`, `_`, `.`) of at most 64 bytes; it is the `matched_entry` of a `launch_refused` row. The text is a plain value (the grammar below), non-empty, with no control character. A duplicate entry name is refused. A missing section loads as zero entries; the library exposes it as `builder_state()` (file order). A signer launch (`docs/launcher.md`, step 3b) with zero entries is refused, so the section is needed wherever signers launch. Refusals name the line, section and key, never the text.

- `budgets`: per ticket size S, M, L (`s_`, `m_`, `l_`), a wall-time limit in seconds and a token limit. Flat keys.
- `shadow`: per capability, a ticket or dispatch cap (`p1a_cap`, `p4a_cap`, `d2a_cap`, `p7_cap`) and a token budget (`..._token_budget`).
- `lanes`: `count`, the number of lanes. The session-name check reads it as the upper bound of a legal lane number (P1B-16); the value is policy, not private: the example ships 4.
- `friction`: `inbox`, the path of the friction inbox file. It is a path value, not a number: the same shape rules as a `repos` folder (section 5). The real file lives only in the private overlay; `harness friction` (ticket 154) is its reader and writer. The section's second key, `pattern_threshold`, is the contract's one OPTIONAL key (ticket 192): the N a friction pattern chain's repeat count must reach before `harness friction patterns` lists it (`docs/friction-inbox.md`); a positive whole number exactly like every numeric key, absent = 3. The section's third key, `triage_top`, is the second OPTIONAL key (ticket 3601): the K of the top-K groups `harness friction rank` lists and `harness friction triage` proposes (an explicit `--top` flag wins); same grammar, absent = 3.
- `factory`: `design_tier`, the tier the factory designs new fixes at (G8.3, `docs/fix-tier.md`). It is one of `T1`, `T2`, `T3` — a tier, never a brand. `T4` is refused: the factory designs at a cheaper tier and escalates on purpose, it never starts at the top. When the model registry exists (P4a) the key may move there; until then this is its home.

### The plain-value grammar (closed)

A value is plain text, or plain text wrapped in one pair of double quotes. Nothing is interpreted: no escapes, no types, no multi-line values. So the hand-written reader and a real YAML reader can never disagree, a value is **refused**, never interpreted, if it:

1. has trailing spaces;
2. contains ` #` (a space then `#`, which YAML reads as a comment);
3. contains `: ` (a colon then a space);
4. contains a `"` other than the one pair wrapping the whole value;
5. ends in `:`;
6. is double-quoted and contains a backslash.

The rules apply to the text inside the quotes when the value is quoted. Only whole-line comments exist; there are no end-of-line comments.

## 4. Fail closed

Anything outside this contract is a refusal. The refusal names the section and key and the line number, and never prints a value (a value may be a private folder). Refused: a missing, duplicate or unknown section or key; a tab; a flow, anchor or multi-document form (`{`, `[`, `&`, `*`, `---`, `|`, `>`); a value of the wrong kind; trailing text; a file that is empty, over 1 MiB, or not UTF-8.

## 5. Repo map rules

`repos` maps a repo id to a folder.

- The key must be a valid repo id (the same rule as the ticket contract: lowercase letters, digits, `-`, `_`, `.`; not empty, not `.` or `..`).
- The folder must be absolute: it starts with `/`. A `~` start is therefore refused as relative.
- The folder has no `..` component.
- The folder has no control byte.
- The folder does not end in `/`, so `/x` and `/x/` can never be two entries.
- The same id twice is refused. Two ids on the same folder text are refused (one repo, one identity).
- The loader does not touch the filesystem for the map. Whether a folder exists or is a git work tree is checked by the ticket intake, not here. Symlink aliasing is intake's problem too.
- An empty `repos` section is valid. The public example ships it empty.

## 6. Values

`friction.inbox` is a path value, checked by the path rules of section 5. `factory.design_tier` is a tier value: exactly one of `T1`, `T2`, `T3`, unquoted (a quoted tier is refused, like a quoted number). Anything else in the slot — `T4`, a lowercase tier, a model name — is refused naming the section, the key and the line, never the value. Every budget, shadow and lanes value — and the optional `friction.pattern_threshold` and `friction.triage_top` — is a positive whole number: digits only, no sign, no leading zero, at most 15 digits. `0` is refused, because a zero budget could be read as "unlimited"; "not seeded" is the safe reading. Refused examples: `0`, `-1`, `1.5`, `007`, a 16-digit number, `abc`, an empty value.

## 7. Public file content

The example holds no repo entries, no folder and no name. Every budget value and every shadow token-budget value is the placeholder `1`: an unseeded install stops, it does not overspend. The four shadow caps are policy numbers, not private: `p1a_cap` 5 tickets, `p4a_cap` 10 dispatches, `d2a_cap` 20 tickets, `p7_cap` 10 tickets. `lanes.count` is policy too and ships as 4. `friction.inbox` ships as the placeholder path `/placeholder/friction-inbox.jsonl`: no inbox of this repo, only of the overlay. `friction.pattern_threshold` ships as its default value, `3`, written out so the optional key is visible in the public example; `friction.triage_top` ships as its default, `3`, on the same terms; omitting the line means the same thing. `factory.design_tier` ships as `T3`, the spec default (G8.3): a default lives in the public example, the same pattern as `lanes.count`. The real values live only in the private file. `builder_state` is never shipped in the public example (entries are private paths, and a placeholder could falsely refuse a real brief that quotes it): it lives only in the private overlay.
## 8. Who reads it

The library exposes pure lookups: `repo_dir(&RepoId)`, `budget(size)`, `shadow()`, `lane_count()`, `friction_inbox()` and `design_tier()`. The one command over the config itself is `harness config check [--config <file>] [--repo <repo_id>] [--budget <S|M|L>]`.

- Exit 0: the config loaded. One stdout line `config ok repos=<n> shadow=ok`; with `--repo` also `repo_id=<id> mapped`; with `--budget` also `budget size=<S> wall_seconds=<n> tokens=<n>`. It never prints a folder.
- Exit 1: a lookup refusal only. `--repo` names an id that is not in the map. Stdout empty; stderr names the id.
- Exit 2: no config configured, an empty `--config` or `HARNESS_CONFIG`, a missing, unreadable, directory, oversized, non-UTF-8, empty or malformed file, or a bad command line. Stdout empty.

A broken config is a setup error (2), not a ticket refusal (1). Budgets are only supplied here; enforcing them is another ticket's job.
