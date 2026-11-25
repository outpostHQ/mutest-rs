# Open defects

Step 6 of `.claude/rules/bug-free-loop.md`: one row for each open defect; a fixed row leaves the table.

| Defect | Evidence | Found by |
| --- | --- | --- |
| A job runs with the signal of the thread that runs it, not of the test that gave it. A job that a test gives to a thread that it did not start (a pool made before the test, a `Builder::no_hooks` thread) is not cancelled after the test is abandoned; a pool that an abandoned test made and kept in a `static` cancels the jobs of later tests. | `mutest-runtime/src/test_runner.rs:262`, `mutest-runtime/src/test_runner.rs:337` | review of the thread signal fix |
