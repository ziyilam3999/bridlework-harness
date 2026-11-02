# Config contract (v3)

The harness reads one settings file. This page is the contract in plain words. `config/harness.example.yaml` is the public example of it, with placeholders only.

Status: contract plus its strict reader (PRE-5b) and `harness config check` (PRE-5c). v2 added the required `lanes.count` key; v3 adds the required `friction.inbox` key (ticket 154): the path of the friction inbox file (`docs/friction-inbox.md`). A config written against v2 is refused until it gains the key (intended, fail closed): the inbox is where lessons are captured, and the harness refuses to guess where. The operator's real file is updated in the private overlay the day the change merges.

## 1. One file, one reader, read-only

The config is one text file. The program only reads it. There is no write, set or install verb: the installer never writes the real file.

## 2. Where it lives

- Public: `config/harness.example.yaml` in this repo. Placeholders only (section 7).
- Private: the operator's real file, kept outside this repo. It is named by `--config <file>`; when the flag is absent, by the environment variable `HARNESS_CONFIG`. The flag wins.
- There is no third source: no search, no default location, no file beside the binary, no fallback to the example, no current-folder lookup.
- No source configured is a refusal, not a default. An empty `--config` value or an empty `HARNESS_CONFIG` is a refusal too; it never falls back to the other source or to the example.

## 3. Shape

Five sections, in this order of meaning: `repos`, `budgets`, `shadow`, `lanes`, `friction`. Two levels only: a section line (`name:` with nothing after it), then indented `key: value` lines (two spaces). Whole-line `#` comments and blank lines are allowed. Spaces only.

### Keys

Every key below is required, in its section, exactly once. Nothing else is allowed.

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
```

`repos` has no fixed keys: each key is a repo id (section 5). It may be empty.

- `budgets`: per ticket size S, M, L (`s_`, `m_`, `l_`), a wall-time limit in seconds and a token limit. Flat keys.
- `shadow`: per capability, a ticket or dispatch cap (`p1a_cap`, `p4a_cap`, `d2a_cap`, `p7_cap`) and a token budget (`..._token_budget`).
- `lanes`: `count`, the number of lanes. The session-name check reads it as the upper bound of a legal lane number (P1B-16); the value is policy, not private: the example ships 4.
- `friction`: `inbox`, the path of the friction inbox file. It is a path value, not a number: the same shape rules as a `repos` folder (section 5). The real file lives only in the private overlay; `harness friction` (ticket 154) is its reader and writer.

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

`friction.inbox` is the one string value in the file, checked by the path rules of section 5. Every budget, shadow and lanes value is a positive whole number: digits only, no sign, no leading zero, at most 15 digits. `0` is refused, because a zero budget could be read as "unlimited"; "not seeded" is the safe reading. Refused examples: `0`, `-1`, `1.5`, `007`, a 16-digit number, `abc`, an empty value.

## 7. Public file content

The example holds no repo entries, no folder and no name. Every budget value and every shadow token-budget value is the placeholder `1`: an unseeded install stops, it does not overspend. The four shadow caps are policy numbers, not private: `p1a_cap` 5 tickets, `p4a_cap` 10 dispatches, `d2a_cap` 20 tickets, `p7_cap` 10 tickets. `lanes.count` is policy too and ships as 4. `friction.inbox` ships as the placeholder path `/placeholder/friction-inbox.jsonl`: no inbox of this repo, only of the overlay. The real values live only in the private file.

## 8. Who reads it

The library exposes pure lookups: `repo_dir(&RepoId)`, `budget(size)`, `shadow()`, `lane_count()` and `friction_inbox()`. The one command over the config itself is `harness config check [--config <file>] [--repo <repo_id>] [--budget <S|M|L>]`.

- Exit 0: the config loaded. One stdout line `config ok repos=<n> shadow=ok`; with `--repo` also `repo_id=<id> mapped`; with `--budget` also `budget size=<S> wall_seconds=<n> tokens=<n>`. It never prints a folder.
- Exit 1: a lookup refusal only. `--repo` names an id that is not in the map. Stdout empty; stderr names the id.
- Exit 2: no config configured, an empty `--config` or `HARNESS_CONFIG`, a missing, unreadable, directory, oversized, non-UTF-8, empty or malformed file, or a bad command line. Stdout empty.

A broken config is a setup error (2), not a ticket refusal (1). Budgets are only supplied here; enforcing them is another ticket's job.
