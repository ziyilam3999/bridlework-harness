# gates/

Each gate is one file here. The harness loads this folder, turns each file into a runnable gate, and runs nothing at load time.
Each rule below names the test that enforces it (`core/rust/harness/src/gates.rs`).

1. A gate is one JSON object file `gates/<id>.json`, flat in this folder, and `id` equals the file name stem. (AC 3, AC 4)
2. The object has exactly six keys, all required, none optional, no defaults: `id`, `program`, `args`, `timeout_ms`, `max_output_bytes`, `both_ends`. (AC 2, AC 3, AC 8)
3. `program` is a repo-relative, `/`-separated path to an executable file, and `args` is an array of strings (possibly empty). The check can be written in any language; the contract speaks only exit codes. (AC 1, AC 3, AC 5)
4. `timeout_ms` and `max_output_bytes` are positive integers. A check past its budget is a FAIL, never a PASS, and captured output past the cap is cut and marked (test `a_gates_budget_and_cap_reach_the_runner`). (AC 1, AC 3)
5. `both_ends` is a JSON boolean. `false`: the gate runs once, at one sha. `true`: the gate must FAIL at base and PASS at head. Base, head and the recorded head come from the caller, never from the gate file. (AC 6)
6. The result of running a gate is always the normalized gate result (status, gate_id, head_sha, failure_class, message, file, line), produced by the harness. A check never writes a result; it only exits. (AC 1, AC 6)
7. Loading fails closed. A missing field, an unknown or duplicate key, a wrongly typed value, an entry this folder does not recognize, or a folder with no gate files is refused, naming the file and the field, and nothing runs. (AC 2, AC 3, AC 4)

Folder rules (AC 4, AC 5):

- `path-map.json` is reserved for the path-to-check map and is skipped. A new reserved name arrives with its own ticket, never as a quiet loader edit.
- Names starting with `.` and files that do not end in `.json` (this file, `.gitkeep`) are ignored.
- A subdirectory or any other non-regular entry is refused. A gate file that resolves outside the repo root is refused.
- A gate file is at most 1 MiB and valid UTF-8.

The machine-readable mirror of the six-key shape is `schemas/gate.schema.json`; it is data for people and tools, and a test (AC 8) keeps its `required` set equal to the loader's.
