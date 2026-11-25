# Open defects

Step 6 of `.claude/rules/bug-free-loop.md`: one row for each open defect; a fixed row leaves the table.

| Defect | Evidence | Found by |
| --- | --- | --- |
| A job runs with the signal of the thread that runs it, not of the test that gave it. A job that a test gives to a thread that it did not start (a pool made before the test, a `Builder::no_hooks` thread) is not cancelled after the test is abandoned; a pool that an abandoned test made and kept in a `static` cancels the jobs of later tests. | `mutest-runtime/src/test_runner.rs:269`, `mutest-runtime/src/test_runner.rs:340` | review of the thread signal fix |
| `confirm_timeouts_of_isolated_mutations_side_by_side` failed once in the Windows coverage run of c8753ed and passed in the other 5 Windows runs of that test. The CI log omits the diff, so the cause is unknown; it is the only UI test whose two isolated children run at the same time. | `tests/ui/evaluation/confirm_timeouts_of_isolated_mutations_side_by_side.rs:5`, CI `tests` run 45569579402 attempt 1 | CI on Windows |
