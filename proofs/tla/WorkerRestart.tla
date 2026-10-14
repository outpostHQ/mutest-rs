---------------------------- MODULE WorkerRestart ----------------------------
\* A supervisor restarts a worker that journals its mutations and proves completion; the worker can die, tear a row or lose writes.
EXTENDS Integers, FiniteSets
CONSTANTS
    \* @type: Int;
    NM,
    \* @type: Int;
    NCrashy,
    \* @type: Int;
    MaxFault
VARIABLES
    \* @type: {st: Set(Int), fin: Int -> Str, iso: Set(Int), prof: Bool, bad: Bool, tail: Bool, old: {st: Set(Int), fin: Int -> Str, prof: Bool, bad: Bool}};
    j,
    \* @type: {pc: Str, efin: Set(Int), eiso: Set(Int), ran: Set(Int), run: Set(Int), tally: Int -> Int, verd: Int -> Str, ok: Bool, rec: Bool};
    w,
    \* @type: {pc: Str, restarts: Int, fin: Int, faults: Int, crashed: Set(Int), refs: Int};
    s
M == 1..NM
Crashy == 1..NCrashy
None == "none"
Exited == {"fail", "early", "code"}
OldFin == {m \in M : j.old.fin[m] /= None}
Fin == {m \in M : j.fin[m] /= None}
Unfin == j.st \ Fin
\* The tests of a crashy mutation crash the process they run in, so it finishes only when it runs isolated.
Outcomes(m) == IF m \in Crashy \ w.eiso THEN {} ELSE {"detected", IF m \in Crashy THEN "crashed" ELSE "undetected"}
Rows == [st |-> j.st, fin |-> j.fin, prof |-> j.prof, bad |-> j.bad]
\* j.old: the rows before the worker's last row. A glued row makes its line unreadable; after a failed write a worker writes nothing.
Put(jj) == IF w.ok THEN [jj EXCEPT !.bad = @ \/ j.tail, !.tail = FALSE, !.old = Rows] ELSE j
\* A crash can tear the worker's last row as if it came in that write: the row loses its newline, and more of it if b.
Crash(e, g) == \E t, b \in BOOLEAN : LET torn == t /\ g /\ j.old /= Rows /\ ~w.rec IN
    w' = [w EXCEPT !.pc = e, !.ok = @ /\ ~torn] /\ j' = IF torn THEN [j EXCEPT !.tail = TRUE, !.bad = @ \/ b] ELSE j
W0 == [pc |-> "open", efin |-> {}, eiso |-> {}, ran |-> {}, run |-> {}, tally |-> [m \in M |-> 0],
       verd |-> [m \in M |-> None], ok |-> TRUE, rec |-> FALSE]
Init == j = [st |-> {}, fin |-> [m \in M |-> None], iso |-> {}, prof |-> FALSE, bad |-> FALSE, tail |-> FALSE,
             old |-> [st |-> {}, fin |-> [m \in M |-> None], prof |-> FALSE, bad |-> FALSE]]
    /\ w = [W0 EXCEPT !.pc = "off"] /\ s = [pc |-> "start", restarts |-> 0, fin |-> 0, faults |-> 0, crashed |-> {}, refs |-> 0]
\* code: mutest-runtime/src/supervisor.rs::run_worker
StartWorker == s.pc = "start" /\ s' = [s EXCEPT !.pc = "wait"] /\ w' = W0 /\ j' = [j EXCEPT !.old = Rows]
\* code: mutest-runtime/src/journal.rs::open_for_worker
Open(iso) == w.pc = "open" /\ UNCHANGED <<j, s>> /\
    w' = IF j.bad THEN [w EXCEPT !.pc = "fail"] ELSE [w EXCEPT !.pc = "ref", !.efin = Fin, !.eiso = iso]
\* code: mutest-runtime/src/harness/profiling.rs::reference_run
Reference == w.pc = "ref" /\ j' = (IF j.prof THEN j ELSE Put([j EXCEPT !.prof = TRUE]))
    /\ s' = [s EXCEPT !.refs = @ + (IF j.prof THEN 0 ELSE 1)] /\ w' = [w EXCEPT !.pc = "earlier"]
\* code: mutest-runtime/src/harness/profiling.rs::exit_unless_all_pass
RefFails == w.pc = "ref" /\ ~j.prof /\ w' = [w EXCEPT !.pc = "fail"] /\ s' = [s EXCEPT !.refs = @ + 1] /\ j' = j
\* code: mutest-runtime/src/journal.rs::record_earlier_results
Earlier == w.pc = "earlier" /\ UNCHANGED <<j, s>> /\ w' = [w EXCEPT !.pc = "run",
    !.tally = [m \in M |-> IF m \in w.efin THEN 1 ELSE 0], !.verd = [m \in M |-> IF m \in w.efin THEN j.fin[m] ELSE None]]
\* code: mutest-runtime/src/harness.rs::journal_started
Begin(m) == w.pc = "run" /\ m \notin w.ran /\ s' = s /\ j' = Put([j EXCEPT !.st = @ \cup {m}])
    /\ w' = [w EXCEPT !.ran = @ \cup {m}, !.run = @ \cup {m}]
\* code: mutest-runtime/src/harness.rs::run_mutation_analysis
Start(m, skip) == (skip => m \notin w.efin) /\ Begin(m)
\* code: mutest-runtime/src/harness.rs::record_finished_mutation
Finish(m) == w.pc = "run" /\ m \in w.run /\ s' = s /\ \E v \in Outcomes(m) :
    /\ w' = [w EXCEPT !.run = @ \ {m}, !.tally[m] = @ + 1, !.verd[m] = v]
    /\ j' = Put([j EXCEPT !.fin[m] = IF @ = None THEN v ELSE @, !.bad = @ \/ j.fin[m] /= None])
\* code: mutest-runtime/src/harness.rs::run_tests
CrashBy(m) == w.pc = "run" /\ m \in w.run \cap Crashy /\ m \notin w.eiso /\ s' = [s EXCEPT !.crashed = @ \cup {m}]
    /\ \E e \in {"fail", "early"} : Crash(e, m \in j.old.st)  \* the tests of m run after its started row
\* code: none; a signal or an exit of tested code ends the worker at any step
Fault == w.pc \notin {"off"} \cup Exited /\ s.faults < MaxFault /\ s' = [s EXCEPT !.faults = @ + 1]
    /\ \E e \in {"fail", IF w.pc = "proved" THEN "code" ELSE IF w.pc \in {"ref", "run"} THEN "early" ELSE "fail"} : Crash(e, TRUE)
\* code: mutest-runtime/src/journal.rs::WorkerJournal::append
WriteFails == w.pc \in {"ref", "run"} /\ w.ok /\ s.faults < MaxFault /\ s' = [s EXCEPT !.faults = @ + 1]
    /\ w' = [w EXCEPT !.ok = FALSE] /\ \E t \in BOOLEAN : j' = [j EXCEPT !.old = Rows, !.tail = @ \/ t, !.bad = @ \/ t]
\* code: mutest-runtime/src/completion.rs::finish
Prove == w.pc = "run" /\ w.run = {} /\ w.ran \cup w.efin = M /\ UNCHANGED <<j, s>>
    /\ w' = IF w.ok THEN [w EXCEPT !.pc = "proved", !.rec = TRUE] ELSE [w EXCEPT !.pc = "fail"]
\* code: mutest-runtime/src/completion.rs::finish
Exit == w.pc = "proved" /\ w' = [w EXCEPT !.pc = "code"] /\ UNCHANGED <<j, s>>
\* code: mutest-runtime/src/journal.rs::cut_torn_row
Cut == s.pc = "wait" /\ w.pc \in Exited /\ j.tail /\ UNCHANGED <<w, s>>
    /\ j' = [j EXCEPT !.st = j.old.st, !.fin = j.old.fin, !.prof = j.old.prof, !.bad = j.old.bad, !.tail = FALSE]
\* code: mutest-runtime/src/supervisor.rs::supervise
Reap(proof, always, cut) ==
    LET done == w.pc \in {"code", "early"} /\ proof
        iso == Unfin \ j.iso
        go == ~done /\ ~j.bad /\ (iso /= {} \/ Cardinality(Fin) > s.fin \/ always)
    IN /\ s.pc = "wait" /\ w.pc \in Exited /\ w' = w /\ ~(cut /\ j.tail)
       /\ j' = IF go /\ iso /= {} THEN [j EXCEPT !.iso = @ \cup iso, !.bad = j.tail, !.tail = FALSE] ELSE j
       /\ s' = IF go THEN [s EXCEPT !.pc = "start", !.restarts = @ + 1, !.fin = Cardinality(Fin)]
               ELSE [s EXCEPT !.pc = IF done /\ ~j.bad /\ Unfin = {} THEN "success" ELSE "fail"]
Steps(iso, proof, always, skip, cut) == StartWorker \/ Open(iso) \/ Reference \/ RefFails \/ Earlier \/ Prove \/ Exit \/ Fault
    \/ WriteFails \/ (cut /\ Cut) \/ Reap(proof, always, cut) \/ \E m \in M : Start(m, skip) \/ Finish(m) \/ CrashBy(m)
Next == Steps(j.iso, w.rec, FALSE, TRUE, TRUE)
NextNoJournalSkip == Steps(j.iso, w.rec, FALSE, FALSE, TRUE)  \* bad: a restarted worker runs finished mutations again
NextNoCompletion == Steps(j.iso, TRUE, FALSE, TRUE, TRUE)  \* bad: a success exit code counts without the completion record
NextNoIsolate == Steps({}, w.rec, FALSE, TRUE, TRUE)  \* bad: a restarted worker runs a crashing mutation in its own process again
NextRestartAlways == Steps(j.iso, w.rec, TRUE, TRUE, TRUE)  \* bad: a restart after each failed worker, with no isolation or progress check
NextNoCut == Steps(j.iso, w.rec, FALSE, TRUE, FALSE)  \* bad: the supervisor reads a torn row, and the next row joins it
V == {None, "detected", "undetected", "crashed"}
TypeOK ==
    /\ j \in [st: SUBSET M, fin: [M -> V], iso: SUBSET M, prof: BOOLEAN, bad: BOOLEAN, tail: BOOLEAN,
              old: [st: SUBSET M, fin: [M -> V], prof: BOOLEAN, bad: BOOLEAN]]
    /\ w \in [pc: {"off", "open", "ref", "earlier", "run", "proved"} \cup Exited, efin: SUBSET M, eiso: SUBSET M,
              ran: SUBSET M, run: SUBSET M, tally: [M -> 0..2], verd: [M -> V], ok: BOOLEAN, rec: BOOLEAN]
    /\ s \in [pc: {"start", "wait", "success", "fail"}, restarts: 0..(2 * NM + 1), fin: 0..NM, faults: 0..MaxFault,
              crashed: SUBSET M, refs: 0..2]
NoDoubleCount == \A m \in M : w.tally[m] <= 1
\* A worker that runs mutations holds a verdict for each mutation the journal has finished.
NoLostResult == w.pc \in {"run", "proved"} => \A m \in Fin : w.verd[m] /= None
VerdictStable == \A m \in Fin : w.verd[m] \in {None, j.fin[m]}
SuccessProved == s.pc = "success" => w.rec /\ Fin = M /\ \A m \in M : w.tally[m] = 1 /\ w.verd[m] = j.fin[m]
EarlyExitNotSuccess == s.pc = "success" => w.pc /= "early"
BoundedRestarts == s.restarts <= 2 * NM
\* With no fault after a passing reference run, the run succeeds, and a mutation that crashed a worker is crashed or detected.
CrashedEndsCrashed == s.pc \in {"success", "fail"} /\ s.faults = 0 /\ j.prof =>
    s.pc = "success" /\ \A m \in s.crashed : j.fin[m] \in {"crashed", "detected"}
ReferenceRunOnce == s.refs <= 1
\* No crash, torn row or failed write leaves the journal unreadable when the supervisor restarts a worker or ends the run.
JournalReadable == s.pc /= "wait" => ~j.bad
Safe == NoLostResult /\ NoDoubleCount /\ VerdictStable /\ SuccessProved /\ EarlyExitNotSuccess /\ BoundedRestarts
    /\ CrashedEndsCrashed /\ ReferenceRunOnce /\ JournalReadable
IndInv ==
    /\ TypeOK /\ Safe
    /\ Fin \subseteq j.st /\ (j.st /= {} => j.old.prof) /\ s.crashed \subseteq Crashy
    /\ s.restarts <= Cardinality(j.iso) + s.fin /\ s.fin <= Cardinality(Fin) /\ (s.restarts > 0 => j.old.prof)
    /\ \A m \in M : j.fin[m] \in {None, "detected", IF m \in Crashy THEN "crashed" ELSE "undetected"}
    /\ s.faults = 0 => (w.ok \/ w.pc \in Exited) /\ (w.rec => w.pc /= "fail")
    /\ (j.bad => j.tail) /\ ~j.old.bad /\ (j.tail => s.pc = "wait" /\ ~w.ok) /\ (j.old.prof => j.prof)
    /\ j.old.st \subseteq j.st /\ OldFin \subseteq j.old.st /\ \A m \in M : j.old.fin[m] \in {None, j.fin[m]}
    /\ (s.pc = "wait" => s.fin <= Cardinality(OldFin)) /\ (w.pc \in {"off", "open"} => j.old = Rows)
    /\ w.run \subseteq w.ran /\ w.ran \cap w.efin = {} /\ (w.ok => w.ran \subseteq j.st) /\ \A m \in w.run : j.fin[m] = None
    /\ \A m \in w.ran \ w.run : j.fin[m] \in {None, w.verd[m]} /\ (w.ok => j.fin[m] = w.verd[m])
    /\ \A m \in M : w.tally[m] = (IF w.verd[m] = None THEN 0 ELSE 1) /\ (m \notin w.efin => (w.verd[m] /= None <=> m \in w.ran \ w.run))
    /\ \A m \in w.efin : j.old.fin[m] /= None /\ w.verd[m] \in {None, j.fin[m]}
    /\ (w.pc \in {"run", "proved"} \/ w.rec \/ w.ran /= {}) => \A m \in w.efin : w.verd[m] /= None
    /\ (w.pc \in {"ref", "earlier", "run", "proved"} \/ w.rec) => Fin \subseteq w.efin \cup w.ran
    /\ w.pc \in {"off", "open", "ref", "earlier"} => w.ran = {} /\ ~w.rec /\ \A m \in M : w.verd[m] = None
    /\ (w.pc \in {"earlier", "run", "proved"} /\ w.ok => j.prof) /\ (w.pc \in {"off", "open", "ref"} /\ ~j.prof => s.refs = 0)
    /\ s.pc = "wait" /\ w.pc \in {"ref", "earlier", "run", "proved"} => w.eiso = j.iso
    /\ w.rec => w.ok /\ w.run = {} /\ w.ran \cup w.efin = M /\ w.pc \in {"proved", "fail", "code"}
    /\ (w.pc \in {"proved", "code"} => w.rec) /\ (s.pc /= "wait" => w.pc \in {"off"} \cup Exited)
    /\ w.pc = "off" <=> s.pc = "start" /\ s.restarts = 0
    /\ s.pc = "wait" /\ w.pc \in Exited /\ s.faults = 0 /\ j.prof /\ ~w.rec => \E m \in w.run \cap Crashy \cap j.st : m \notin j.iso /\ (j.tail => m \in j.old.st)
=============================================================================
