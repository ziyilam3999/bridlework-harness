# gates/

Each gate is one file here. The harness loads this folder, turns each file into a runnable gate, and runs nothing at load time.
Each rule below names the inline test in `core/rust/harness/src/gates.rs` that enforces it.

1. A gate is one JSON object file `gates/<id>.json`, flat in this folder, and `id` equals the file name stem. (`malformed_gate_values_are_refused`, `the_gates_folder_contract_is_enforced`)
2. The object has exactly six keys, all required, none optional, no defaults: `id`, `program`, `args`, `timeout_ms`, `max_output_bytes`, `both_ends`. (`a_gate_file_missing_a_required_field_is_refused_naming_file_and_field`, `malformed_gate_values_are_refused`, `the_committed_seed_gate_and_schema_match_the_loader`)
3. `program` is a repo-relative, `/`-separated path to a file the harness can execute, and `args` is an array of strings (possibly empty). The check can be written in any language; the contract speaks only exit codes. The executable bit is not checked at load: a program that cannot be started fails when the gate runs, with class `spawn-error`. (`a_valid_gate_file_loads_and_runs_through_the_runner`, `malformed_gate_values_are_refused`, `a_gate_file_or_program_outside_the_repo_is_refused`)
4. `timeout_ms` and `max_output_bytes` are positive integers; there is no upper limit. A check past its budget is a FAIL, never a PASS, and captured output past the cap is cut and marked. (`malformed_gate_values_are_refused`, `a_gates_budget_and_cap_reach_the_runner`)
5. `both_ends` is a JSON boolean. `false`: the gate runs once, at one sha. `true`: the gate must FAIL at base and PASS at head. Base, head and the recorded head come from the caller, never from the gate file. (`the_both_ends_flag_selects_the_both_ends_judge`)
6. The result of running a gate is always the normalized gate result (status, gate_id, head_sha, failure_class, message, file, line), produced by the harness. A check never writes a result; it only exits. (`a_valid_gate_file_loads_and_runs_through_the_runner`, `the_both_ends_flag_selects_the_both_ends_judge`)
7. Loading fails closed. A missing field, an unknown or duplicate key, a wrongly typed value, a `.json` file that is not a valid gate, a subdirectory or other non-regular entry, a gate file that resolves outside the repo root, or a folder with no gate files is refused, and nothing runs. Every refusal names the file (or the folder); a refusal about one field also names that field. (`a_gate_file_missing_a_required_field_is_refused_naming_file_and_field`, `malformed_gate_values_are_refused`, `the_gates_folder_contract_is_enforced`, `a_gate_file_or_program_outside_the_repo_is_refused`)

Folder rules (`the_gates_folder_contract_is_enforced`, `a_gate_file_or_program_outside_the_repo_is_refused`):

- `path-map.json` is reserved for the path-to-check map and is skipped. A new reserved name arrives with its own ticket, never as a quiet loader edit.
- Names starting with `.` and files that do not end in `.json` (this file, `.gitkeep`) are ignored. A name ending in `.JSON` is not `.json`, so it is ignored too.
- A subdirectory or any other non-regular entry is refused. A gate file that resolves outside the repo root is refused.
- A gate file is at most 1 MiB and valid UTF-8. (`malformed_gate_values_are_refused`)

The machine-readable mirror of the six-key shape is `schemas/gate.schema.json`; it is data for people and tools, and `the_committed_seed_gate_and_schema_match_the_loader` keeps its `required` set equal to the loader's.
