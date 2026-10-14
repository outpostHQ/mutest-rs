--------------------------- MODULE MutantScheduling ---------------------------
\* Compatible mutants share one substitution map; a test thread drops what it read once the run abandons its test.
\* Isolated mutants and unrepeatable tests run in children; timed-out mutations run again after the loop.
EXTENDS Integers, FiniteSets
CONSTANTS
    \* @type: Set(Int);
    Mut,
    \* @type: Set(Int);
    Tests
VARIABLES
    \* @type: Int -> Set(Int);
    reach,
    \* @type: Set(Int);
    iso,
    \* @type: Set(Int);
    unrep,
    \* @type: Set(Int);
    handle,
    \* @type: Int -> Str;
    job,
    \* @type: Int -> Set(Int);
    left,
    \* @type: Int -> Str;
    res,
    \* @type: Int -> Int;
    rec,
    \* @type: <<Int, Int>> -> Str;
    th,
    \* @type: <<Int, Int>> -> Bool;
    sig,
    \* @type: <<Int, Int>> -> Int;
    seen,
    \* @type: Int;
    cnt
top == <<reach, iso, unrep>>
Threads == Mut \X Tests
Busy == {m \in Mut : job[m] \in {"pick", "run", "ab", "abn", "kid", "end"}}
Conflict(a, b) == a /= b /\ {a, b} \cap iso = {} /\ reach[a] \cap reach[b] /= {}
Init ==
    /\ reach \in [Mut -> (SUBSET Tests) \ {{}}] /\ iso \in SUBSET Mut /\ unrep \in SUBSET Tests /\ handle = {}
    /\ job = [m \in Mut |-> "todo"] /\ left = [m \in Mut |-> {}] /\ res = [m \in Mut |-> "und"] /\ cnt = 0
    /\ th = [x \in Threads |-> "no"] /\ sig = [x \in Threads |-> FALSE] /\ seen = [x \in Threads |-> 0] /\ rec = [m \in Mut |-> 0]
\* code: mutest-runtime/src/harness.rs::run_mutation_analysis
Schedule(new, check) ==
    /\ new /= {} /\ \A m \in new : job[m] = "todo"
    /\ (check => \A a, b \in Busy \cup new : ~Conflict(a, b)) /\ handle' = (Busy \cup new) \ iso
    /\ left' = [m \in Mut |-> IF m \in new /\ m \notin iso THEN reach[m] \ unrep ELSE left[m]]
    /\ job' = [m \in Mut |-> IF m \in new THEN "pick" ELSE job[m]] /\ UNCHANGED <<top, res, rec, th, sig, seen, cnt>>
\* code: mutest-runtime/src/test_runner.rs::Scheduler::start_queued_tests
Start(m, t) == job[m] = "pick" /\ t \in left[m] /\ job' = [job EXCEPT ![m] = "run"]
    /\ th' = [th EXCEPT ![<<m, t>>] = "run"] /\ sig' = [sig EXCEPT ![<<m, t>>] = TRUE]
    /\ UNCHANGED <<top, handle, left, res, rec, seen, cnt>>
Look(x, n) == seen' = [seen EXCEPT ![x] = n] /\ UNCHANGED <<top, handle, job, left, res, rec, th, sig, cnt>>
\* code: mutest-runtime/src/test_runner.rs::ActiveMutantHandle::subst_at
Read(x, n) == th[x] = "run" /\ seen[x] = 0 /\ n \in handle /\ x[2] \in reach[n] /\ Look(x, n)
\* code: mutest-runtime/src/test_runner.rs::active_subst
Use(x) == seen[x] /= 0 /\ Look(x, 0)
\* code: mutest-runtime/src/test_runner.rs::run_test_in_process
End(x, o) == th[x] = "run" /\ seen[x] = 0 /\ th' = [th EXCEPT ![x] = o] /\ sig' = [sig EXCEPT ![x] = FALSE]
    /\ cnt' = (IF sig[x] THEN cnt ELSE cnt - 1) /\ UNCHANGED <<top, handle, job, left, res, rec, seen>>
Note(m, o, rest) == left' = [left EXCEPT ![m] = IF o \in {"fail", "to"} THEN {} ELSE rest]
    /\ res' = [res EXCEPT ![m] = IF @ = "det" \/ o = "pass" THEN @ ELSE IF o = "fail" THEN "det" ELSE o]
\* code: mutest-runtime/src/test_runner.rs::Scheduler::finish_next_test
Receive(m, t) == job[m] = "run" /\ t \in left[m] /\ th[<<m, t>>] \in {"pass", "fail"}
    /\ job' = [job EXCEPT ![m] = "pick"] /\ Note(m, th[<<m, t>>], left[m] \ {t})
    /\ UNCHANGED <<top, handle, rec, th, sig, seen, cnt>>
\* code: mutest-runtime/src/test_runner.rs::abandon_timed_out_test
Abandon(m, t, fix) == job[m] = "run" /\ t \in left[m] /\ th[<<m, t>>] /= "no" /\ cnt' = (IF fix THEN cnt + 1 ELSE cnt)
    /\ job' = [job EXCEPT ![m] = IF fix \/ sig[<<m, t>>] THEN "ab" ELSE "abn"]
    /\ sig' = (IF fix THEN sig ELSE [sig EXCEPT ![<<m, t>>] = FALSE])
    /\ UNCHANGED <<top, handle, left, res, rec, th, seen>>
\* code: mutest-runtime/src/test_runner.rs::abandon_signal
Count(m, t, fix) == job[m] \in {"ab", "abn"} /\ t \in left[m] /\ th[<<m, t>>] /= "no"
    /\ sig' = (IF fix THEN [sig EXCEPT ![<<m, t>>] = FALSE] ELSE sig)
    /\ cnt' = cnt + (IF fix THEN (IF sig[<<m, t>>] THEN 0 ELSE -1) ELSE IF job[m] = "ab" THEN 1 ELSE 0)
    /\ Note(m, "to", left[m] \ {t}) /\ job' = [job EXCEPT ![m] = "pick"] /\ UNCHANGED <<top, handle, rec, th, seen>>
\* code: mutest-runtime/src/harness/unrepeatable.rs::not_yet_detected
ToChildren(m) == job[m] = "pick" /\ left[m] = {} /\ UNCHANGED <<top, handle, res, rec, th, sig, seen, cnt>>
    /\ LET k == IF res[m] = "und" THEN reach[m] \cap (IF m \in iso THEN Tests ELSE unrep) ELSE {}
       IN left' = [left EXCEPT ![m] = k] /\ job' = [job EXCEPT ![m] = IF k = {} THEN "end" ELSE "kid"]
\* code: mutest-runtime/src/harness/unrepeatable.rs::in_child_processes
Child(m, t, o) == job[m] = "kid" /\ t \in left[m] /\ Note(m, o, left[m] \ {t})
    /\ job' = [job EXCEPT ![m] = IF left'[m] = {} THEN "end" ELSE "kid"]
    /\ UNCHANGED <<top, handle, rec, th, sig, seen, cnt>>
\* code: mutest-runtime/src/harness/timeouts.rs::finish_mutation
Join(m) == job[m] = "end" /\ handle' = (Busy \ {m}) \ iso /\ UNCHANGED <<top, left, res, th, sig, seen, cnt>>
    /\ \E short \in BOOLEAN : LET v == IF res[m] = "to" /\ short THEN "def" ELSE "rec"
        IN job' = [job EXCEPT ![m] = v] /\ rec' = [rec EXCEPT ![m] = IF v = "rec" THEN @ + 1 ELSE @]
\* code: mutest-runtime/src/harness/timeouts.rs::confirm_timeouts
Rerun(m) == job[m] = "def" /\ (\A n \in Mut : job[n] \in {"def", "rec"}) /\ job' = [job EXCEPT ![m] = "rec"]
    /\ handle' = (IF m \in iso THEN handle ELSE {m}) /\ rec' = [rec EXCEPT ![m] = @ + 1]
    /\ UNCHANGED <<top, left, res, th, sig, seen, cnt>>
\* code: mutest-runtime/src/harness.rs::LingeringTestMonitoringThread::finish
Finish == (\A m \in Mut : job[m] = "rec") /\ UNCHANGED <<top, handle, job, left, res, rec, th, sig, seen, cnt>>
NextWith(fix, check) ==
    \/ \E new \in SUBSET Mut : Schedule(new, check)
    \/ \E m \in Mut : Join(m) \/ Rerun(m) \/ ToChildren(m) \/ \E t \in Tests : Start(m, t) \/ Receive(m, t)
        \/ Abandon(m, t, fix) \/ Count(m, t, fix) \/ \E o \in {"pass", "fail", "to", "crash"} : Child(m, t, o)
    \/ Finish \/ \E x \in Threads : Use(x) \/ (\E o \in {"pass", "fail"} : End(x, o)) \/ \E n \in Mut : Read(x, n)
\* fix: abandon_signal counts the thread before it clears the signal, and takes the count back if the test ended.
Next == NextWith(TRUE, TRUE)
\* bad: the code today; clearing the signal first lets the ending test take the count away from another thread.
NextCountDip == NextWith(FALSE, TRUE)
\* bad: runs_alongside without the conflict check.
NextNoConflictCheck == NextWith(TRUE, FALSE)
TypeOK ==
    /\ reach \in [Mut -> SUBSET Tests] /\ iso \in SUBSET Mut /\ unrep \in SUBSET Tests /\ handle \in SUBSET Mut
    /\ job \in [Mut -> {"todo", "pick", "run", "ab", "abn", "kid", "end", "def", "rec"}] /\ cnt \in Nat
    /\ left \in [Mut -> SUBSET Tests] /\ res \in [Mut -> {"und", "det", "to", "crash"}] /\ rec \in [Mut -> 0..2]
    /\ th \in [Threads -> {"no", "run", "pass", "fail"}] /\ sig \in [Threads -> BOOLEAN] /\ seen \in [Threads -> {0} \cup Mut]
\* A test thread that holds the substitution of another mutant is abandoned and counted, so it does not run it.
Isolation == \A x \in Threads : seen[x] \notin {0, x[1]} => cnt /= 0 /\ ~sig[x]
NoConflict == handle \cap iso = {} /\ \A a, b \in handle : ~Conflict(a, b)
\* Each mutation gets one verdict, and only when it leaves the run for good.
OneVerdict == \A m \in Mut : rec[m] = IF job[m] = "rec" THEN 1 ELSE 0
Detection == \A m \in Mut : res[m] = "det" => left[m] = {}
Safe == Isolation /\ NoConflict /\ OneVerdict /\ Detection
Cur(m) == {t \in left[m] : th[<<m, t>>] /= "no"}
IndInv ==
    /\ TypeOK /\ Safe /\ Busy \ iso \subseteq handle
    /\ \A m \in Mut : job[m] /= "abn" /\ (job[m] = "todo" => res[m] = "und" /\ left[m] = {})
    /\ \A m \in Mut : job[m] \in {"pick", "run", "ab"} => left[m] \subseteq IF m \in iso THEN {} ELSE reach[m] \ unrep
    /\ cnt = Cardinality({x \in Threads : th[x] = "run" /\ ~sig[x]}) + Cardinality({m \in Mut : job[m] = "ab"})
    /\ \A m \in Mut : Cardinality(Cur(m)) = IF job[m] \in {"run", "ab"} THEN 1 ELSE 0
    /\ \A m \in Mut : job[m] \in {"run", "ab"} => \A t \in Cur(m) : sig[<<m, t>>] = (th[<<m, t>>] = "run")
    /\ \A x \in Threads : th[x] /= "no" => x[1] \notin iso /\ x[2] \in reach[x[1]] \ unrep /\ job[x[1]] /= "todo"
    /\ \A x \in Threads : (seen[x] /= 0 => th[x] = "run") /\ (th[x] = "run" /\ sig[x] => x[2] \in Cur(x[1]))
=============================================================================
