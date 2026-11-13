# MCP tool list (v1, read-only)

`mcp/tools.json` is the menu of `harness` commands an AI app may call through MCP. Version 1 is read-only: no tool
here writes anything. The MCP server that serves this list is not built yet. It comes in P11-09b and P11-09c.

## The 7 tools

| name | argv | inputs (required in bold) |
|---|---|---|
| harness_version | `--version` | none |
| harness_status | `status` | **ticket_file**, **repo_map** (`repo_id=abs-dir`, at least 1), **result_dir**, **caps_dir** |
| harness_ticket_intake | `ticket intake` | **ticket_file**, **repo_map** |
| harness_events_list | `events list` | **log** (absolute path) |
| harness_evidence_show | `evidence show` | **ticket**, **manifest_dir** |
| harness_privacy_scan_text | `privacy scan --text` | **text** (sent on stdin), surface (`^[a-z_.]+$`) |
| harness_config_check | `config check` | config, repo, budget (`S`, `M` or `L`) |

How each input maps to argv is the server's job (P11-09c). This file fixes only the input names and types.

## Output schema

Every tool returns the same object, and the lint checks it:
`{"exit": integer, "stdout": string, "stderr": string}`, all three required, no other keys.

## The known-verb rule

Each row has a `probe`: an argv, an expected exit code, and a text the named stream must hold. The lint runs the
probe against a real `harness` and it must answer as written. It then runs a control: the same probe with the last
word of the row's argv replaced by `zz-no-such-verb`. The control must NOT give the same answer. The control exists
because a probe that only checks an error message can pass for a made-up verb too (a usage error looks the same).
A row whose control also matches is refused as `control-matches`.

Probe children run with every `HARNESS_*` variable removed from the environment, then the row's `probe.env` added.
A leftover `HARNESS_PRIVACY_LIST_SECRET` or `HARNESS_PRIVACY_OLD_LIST` therefore cannot steer a probe.

## Deny list

A row is refused if its argv starts with one of these (`deny-verb`), or is a proper prefix of one (`deny-prefix`):
`events append`; `config write`; `controls write`; `permission write`; `session launch`, `session close`,
`session record-start`; `ticket finalize`; `process start`, `process stop`; `workspace create`,
`workspace teardown`; `quarantine sweep`, `quarantine delete-listed`; `manifest init`, `manifest add`,
`manifest remove`; `friction add`, `friction triage`, `friction bind`, `friction page`, `friction sweep`.
Deny and prefix rows are never probed.

## Running the lint

```
sh ci/mcp-tools-lint.sh --build               # build harness from this tree, then lint mcp/tools.json
sh ci/mcp-tools-lint.sh --harness <abs-path>  # use an explicit binary (never searched on PATH, never skipped)
sh ci/mcp-tools-lint.sh --build --self-test   # plant bad rows and check each one is refused
```

Exit 0 is clean, 1 is at least one refusal (one `<row-name>:<rule>` line per refusal on stderr), 2 is a usage
error, an unreadable list, or a harness that is missing or will not build. CI runs the lint and the self-test in
the `rust-test` workflow.
