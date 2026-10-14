# Bug-free loop — the same in every op/* repository

Loaded on every session. Each op/* repository has this text unchanged; scm keeps the source.
The goal is 100% bug-free software, written by people or by agents. Each step has a measure.

1. **Map.** List the parts where data can be lost or a result can be wrong. Check them first.
2. **Design.** Write a TLA+ spec for each design where two processes, a crash or a retry meet.
   Map each action to its function (`\* code:`), run TLC and Apalache, and keep a known-bad
   variant that the check must refute.
3. **Code.** Give each function the strongest check its language has: a Verus law (proven), a
   Kani harness (bounded), Miri, loom or a fuzz target. Label each function proven, bounded,
   design checked, tested, assumed or unverified. A gate refuses a label that goes down.
4. **Oracle.** Where another tool is the reference, a ledger of cases must agree with it exactly.
5. **Tests.** `chock run mutest` decides which tests stay. A surviving mutant gets a check.
6. **Bug loop.** First a check that fails on the bug, then the fix where the defect is (no shim,
   seam or alias), then the gate. Each open defect is one row, with `file:line` evidence, in
   the repository's open-defect table; a fixed row leaves the table.
7. **Gate.** One full gate run per batch, as the repository's workflow defines it, all green;
   `cannot_run` is not a pass.
8. **Lean** (owner, 2026-10-11). Each part stays under its line ceiling, which only goes down.
   Proofs and specs count, so a new spec comes with a larger deletion in the same commit. No
   code that keeps an old form working, no dead or duplicate code, no feature cut for a ceiling.
9. **Laws.** An agent writes a law; it counts only when it refutes each known-bad mutant of its
   function. The owner approves it, and after that no agent edits it.
10. **Report.** "The goal is 100% bug free; today: label counts, open defects, gates; next: the
    steps; reached when each critical function is proven or bounded, with 0 open defects and
    all gates green."
