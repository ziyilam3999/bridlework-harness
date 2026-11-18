# gates/

Each gate is one file here. The harness loads this folder, turns each file into a runnable gate, and runs nothing at load time.
Each rule below names the inline test in `core/rust/harness/src/gates.rs` that enforces it.

1. A gate is one JSON object file `gates/<id>.json`, flat in this folder, and `id` equals the file name stem. (`malformed_gate_values_are_refused`, `the_gates_folder_contract_is_enforced`)
2. The object has exactly six keys, all required, none optional, no defaults: `id`, `program`, `args`, `timeout_ms`, `max_output_bytes`, `both_ends`. (`a_gate_file_missing_a_required_field_is_refused_naming_file_and_field`, `malformed_gate_values_are_refused`, `the_committed_seed_gate_and_schema_match_the_loader`)
3. `program` is a repo-relative, `/`-separated path to a file the harness can execute, and `args` is an array of strings (possibly empty). The executable bit is not checked at load: a program that exists but cannot be started (for example, one that is not executable) fails when the gate runs, with class `spawn-error`. (`a_valid_gate_file_loads_and_runs_through_the_runner`, `malformed_gate_values_are_refused`, `a_gate_file_or_program_outside_the_repo_is_refused`, `a_gates_budget_and_cap_reach_the_runner` for the run-time `spawn-error`)
4. `timeout_ms` and `max_output_bytes` are positive integers that fit an unsigned 64-bit integer; nothing smaller caps them. A check past its budget is a FAIL, never a PASS, and captured output past the cap is cut and marked. (`malformed_gate_values_are_refused`, `a_gates_budget_and_cap_reach_the_runner`)
5. `both_ends` is a JSON boolean. `false`: the gate runs once, at one sha. `true`: the gate must FAIL at base and PASS at head. (`the_both_ends_flag_selects_the_both_ends_judge`)
6. The result of running a gate is always the normalized gate result (status, gate_id, head_sha, failure_class, message, file, line), produced by the harness. (`a_valid_gate_file_loads_and_runs_through_the_runner`, `the_both_ends_flag_selects_the_both_ends_judge`)
7. Loading fails closed. A missing field, an unknown or duplicate key, a wrongly typed value, a `.json` file that is not a valid gate, a subdirectory or other non-regular entry, a gate file that resolves outside the repo root, or a folder with no gate files is refused, and nothing runs. Every refusal names the file (or the folder); a refusal about one field also names that field. (`a_gate_file_missing_a_required_field_is_refused_naming_file_and_field`, `malformed_gate_values_are_refused`, `the_gates_folder_contract_is_enforced`, `a_gate_file_or_program_outside_the_repo_is_refused`)

Folder rules (`the_gates_folder_contract_is_enforced`, `a_gate_file_or_program_outside_the_repo_is_refused`):

- `path-map.json` is reserved for the path-to-check map and is skipped.
- Names starting with `.` and files that do not end in `.json` (this file, `.gitkeep`) are ignored. A name ending in `.JSON` is not `.json`, so it is ignored too.
- A subdirectory or any other non-regular entry is refused. A gate file that resolves outside the repo root is refused.
- A gate file is at most 1 MiB and valid UTF-8. (`malformed_gate_values_are_refused`)

Gate programs:

- A gate program may live in `gates/` (`test-ratchet.sh`): files that do not end in `.json` are ignored by the loader, so the program is not a gate file.
- A head-only gate program (`both_ends` false) starts with three values set by `harness ticket gate`: `HARNESS_GATE_BASE` (the base sha), `HARNESS_GATE_HEAD` (the head sha) and `HARNESS_GATE_REMOVED_TESTS` (the test ids the ticket key `removed_tests` names, newline joined, empty when none).
- For a both-ends program the engine sets none of the three. A value inherited from the caller is not removed, so a both-ends program must not read them.
- `test-ratchet` (`test-ratchet.sh`, head only) lists the tests of `core/rust` at the head (in place) and at the base (an export of the whole base commit, so files outside `core/rust` that the crate reads are there) with `cargo test --locked --offline -- --list`. A test id is the source path from the cargo `Running` line (for example `tests/ticket_gate.rs` or `src/lib.rs`) plus `::` plus the listed test path (for example `tests/ticket_gate.rs::a_lost_test`, `src/lib.rs::caps::tests::x`); doc tests are not listed. A base id that is gone at the head must be named in `removed_tests` (`<id> -- <reason>`), else class `lost-test`; a named id that is not lost is class `named-not-removed`. It fails closed when an env value is unset, empty or not a full lowercase sha (`HARNESS_GATE_REMOVED_TESTS` may be empty, not unset), when the base is not a commit, when the head value is not the folder HEAD, when a list cannot be built at either end, when the base list holds no test id, and when a compare step fails.

The machine-readable mirror of the six-key shape is `schemas/gate.schema.json`; `the_committed_seed_gate_and_schema_match_the_loader` keeps its `required` set equal to the loader's.
