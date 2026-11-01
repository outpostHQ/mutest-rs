# Open defects

Step 6 of `.claude/rules/bug-free-loop.md`: one row for each open defect; a fixed row leaves the table.

| Defect | Evidence | Found by |
| --- | --- | --- |
| A thread that a test spawns has no abandon signal, so after its test times out it can read the substitutions of a later, conflicting mutant. | `mutest-runtime/src/test_runner.rs:222`, `mutest-runtime/src/test_runner.rs:258`, `mutest-runtime/src/harness.rs:715` | `proofs/tla/MutantScheduling.tla` review |
| A renamed name holds the session's crate number of its expansion, so a test crate names an opaque (macros 2.0, `def_site`) item of the externally mutated library differently than the library does. Only nightly features reach it. | `mutest-emit/src/codegen/hygiene.rs:128` | corbin `motion` fault review (11-01) |
