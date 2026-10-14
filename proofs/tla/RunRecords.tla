---------------------------- MODULE RunRecords ----------------------------
\* Workers write progress records to their own files for a live reader, and summary lines that reach summary.json.
\* Worker w <= NH starts harness w, and worker w > NH restarts harness w - NH; file 0 has an other nonce.
EXTENDS Integers, FiniteSets
CONSTANTS
    \* @type: Int;
    NH,
    \* @type: Int;
    NW,
    \* @type: Int;
    NP
VARIABLES
    \* @type: Int -> Str;
    pc,
    \* @type: Int -> (Int -> Str);
    f,
    \* @type: Int -> Int;
    n,
    \* @type: Int -> Int;
    pid,
    \* @type: Int -> Int;
    rd,
    \* @type: Int -> Int;
    bind,
    \* @type: Str;
    rs,
    \* @type: Int -> { p: Int, ok: Bool, l: Bool };
    sl,
    \* @type: Int -> Int;
    sj,
    \* @type: Bool;
    jn,
    \* @type: Int -> Str;
    sv
Hs == 1..NH
Cur == 1..NW
Wk == 0..NW
Term == {"tok", "tbad"}
H(w) == IF w > NH THEN w - NH ELSE w
Last(h) == IF h + NH <= NW /\ pc[h + NH] # "new" THEN h + NH ELSE h
Alive(w) == pc[w] \in {"run", "test", "epi", "fin"}
Has(w, k) == \E i \in 1..4 : i <= n[w] /\ f[w][i] = k
Act(w) == Has(w, "start") /\ ~Has(w, "end")
Whole(w) == IF n[w] > 0 /\ f[w][n[w]] = "part" THEN n[w] - 1 ELSE n[w]
Put(w, k, s) == f' = [f EXCEPT ![w][n[w] + 1] = k] /\ n' = [n EXCEPT ![w] = @ + 1] /\ pc' = [pc EXCEPT ![w] = s]
\* sl[w].p: -1 in the staging log that pass_to empties for worker w, 0 not written, k > 0 line k of the real log.
Pos == 1 + Cardinality({v \in Cur : sl[v].p > 0})
Parsed(w) == sl[w].p > 0 /\ sl[w].ok /\ (sl[w].l \/ \A v \in Cur : sl[v].p = sl[w].p - 1 => sl[v].ok)
Count(h) == Cardinality({w \in Cur : H(w) = h /\ Parsed(w)})
Init == /\ pc = [w \in Cur |-> "new"] /\ f = [w \in Wk |-> [i \in 1..4 |-> "hdr"]]
        /\ n \in {[w \in Wk |-> IF w = 0 THEN m ELSE 0] : m \in 0..1} /\ pid \in {[w \in Wk |-> p] : p \in 1..NP}
        /\ rd = [w \in Wk |-> 0] /\ bind = [w \in Wk |-> -1] /\ rs = "read" /\ sj = [h \in Hs |-> -1]
        /\ sl = [w \in Cur |-> [p |-> 0, ok |-> TRUE, l |-> TRUE]] /\ jn \in BOOLEAN /\ sv = [h \in Hs |-> "loop"]
\* code: mutest-runtime/src/supervisor.rs::supervise, mutest-runtime/src/journal.rs::Journal::pass_to
Start(w) == sv[H(w)] = "loop" /\ pc[w] = "new" /\ (w > NH => jn /\ pc[w - NH] \in {"bad", "dead"})
    /\ (1 < w /\ w <= NH => sv[w - 1] # "loop") /\ Put(w, "hdr", "run") /\ UNCHANGED sl
    /\ \E p \in 1..NP : (\A v \in Cur : Alive(v) => pid[v] # p) /\ pid' = [pid EXCEPT ![w] = p]
\* code: mutest-runtime/src/progress.rs::Writer::start_test
Begin(w) == pc[w] = "run" /\ Put(w, "start", "test") /\ UNCHANGED <<pid, sl>>
\* code: mutest-runtime/src/progress.rs::Writer::end_test
End(w) == pc[w] = "test" /\ \E k \in {"end", "endp"} : Put(w, k, "epi") /\ UNCHANGED <<pid, sl>>
\* code: mutest-runtime/src/summary.rs::append_line
Sum(w, a, d) == pc[w] = "epi" /\ UNCHANGED <<f, n, pid>> /\ \E ok \in BOOLEAN :
    /\ pc' = [pc EXCEPT ![w] = IF ok THEN "fin" ELSE "dead"]
    /\ sl' = IF jn /\ ~d /\ ~ok THEN sl ELSE [sl EXCEPT ![w] = [p |-> IF jn /\ ~d THEN -1 ELSE Pos, ok |-> ok, l |-> a]]
\* code: mutest-runtime/src/completion.rs::finish
Finish(w) == pc[w] = "fin" /\ (IF Act(w) THEN Put(w, "tbad", "bad") ELSE Put(w, "tok", "ok")) /\ UNCHANGED <<pid, sl>>
\* code: none; a signal ends the worker, maybe in the middle of a progress write
Crash(w) == Alive(w) /\ UNCHANGED <<pid, sl>>
    /\ (Put(w, "part", "dead") \/ (pc' = [pc EXCEPT ![w] = "dead"] /\ UNCHANGED <<f, n>>))
\* code: ../chock/src/gates/mutation/watch.rs::Watch::poll
Poll(w, nl, nc, tk) == rs = "read" /\ IF nc /\ w = 0 THEN n[0] > 0 /\ rs' = "err" /\ UNCHANGED <<rd, bind>> ELSE
    LET m == IF nl THEN Whole(w) ELSE n[w] IN m > rd[w] /\ rd' = [rd EXCEPT ![w] = m] /\ \E v \in Wk :
    /\ (v = 0 <=> \A u \in Cur : pid[u] = pid[w] => ~Alive(u)) /\ (v > 0 => Alive(v) /\ pid[v] = pid[w])
    /\ IF bind[w] >= 0 THEN UNCHANGED <<bind, rs>> ELSE IF tk /\ v \notin {0, w} THEN rs' = "err" /\ UNCHANGED bind
       ELSE bind' = [bind EXCEPT ![w] = v] /\ UNCHANGED rs
\* code: ../chock/src/gates/mutation/watch.rs::Watch::complete
Complete == rs = "read" /\ sj[1] >= 0 /\ (\A w \in Wk : rd[w] = Whole(w)) /\ UNCHANGED <<rd, bind>>
    /\ rs' = IF \A w \in Wk : rd[w] = n[w] /\ (n[w] > 0 => f[w][n[w]] \in Term) THEN "done" ELSE "err"
\* code: mutest-runtime/src/summary.rs::hand_over; sv[h] becomes the harness result; a signal can end it in its write
Hand(h) == sv[h] = "loop" /\ pc[h] # "new" /\ (\A w \in Cur : H(w) = h => ~Alive(w)) /\ \E ok \in BOOLEAN :
    /\ sv' = [sv EXCEPT ![h] = IF ok /\ pc[Last(h)] = "ok" THEN "ok" ELSE "bad"]
    /\ sl' = LET e == sl[Last(h)] IN IF e.p = -1 THEN [sl EXCEPT ![Last(h)] = [p |-> Pos, ok |-> ok, l |-> e.l]] ELSE sl
\* code: cargo-mutest/src/summary.rs::run_summary
Gather == sj[1] < 0 /\ (\A h \in Hs : sv[h] # "loop") /\ sj' = [h \in Hs |-> Count(h)]
Sys(nl, nc, tk, a, d) ==
    \/ \E w \in Cur : (Start(w) \/ Begin(w) \/ End(w) \/ Sum(w, a, d) \/ Finish(w) \/ Crash(w))
       /\ UNCHANGED <<rd, bind, rs, sj, jn, sv>>
    \/ (Complete \/ \E w \in Wk : Poll(w, nl, nc, tk)) /\ UNCHANGED <<pc, f, n, pid, sl, sj, jn, sv>>
    \/ \E h \in Hs : Hand(h) /\ UNCHANGED <<pc, f, n, pid, rd, bind, rs, sj, jn>>
    \/ Gather /\ UNCHANGED <<pc, f, n, pid, sl, rd, bind, rs, jn, sv>>
Next == Sys(TRUE, TRUE, TRUE, TRUE, FALSE)
\* bad: the reader takes bytes without their closing newline as a record.
NextNoNewline == Sys(FALSE, TRUE, TRUE, TRUE, FALSE)
\* bad: the reader reads a file with an other nonce.
NextNoNonce == Sys(TRUE, FALSE, TRUE, TRUE, FALSE)
\* bad: the reader takes the live process at the header pid without the start ticks check.
NextNoTicks == Sys(TRUE, TRUE, FALSE, TRUE, FALSE)
\* bad: no newline before a summary line, so a torn line and the next line become one line that does not parse.
NextGlued == Sys(TRUE, TRUE, TRUE, FALSE, FALSE)
\* bad, the old code: each worker appends its summary to the real log, so a restarted worker appends it again.
NextTwice == Sys(TRUE, TRUE, TRUE, TRUE, TRUE)
TypeOK == /\ pc \in [Cur -> {"new", "run", "test", "epi", "fin", "ok", "bad", "dead"}] /\ n \in [Wk -> 0..4]
          /\ f \in [Wk -> [1..4 -> {"hdr", "start", "end", "endp", "tok", "tbad", "part"}]] /\ pid \in [Wk -> 1..NP]
          /\ rd \in [Wk -> 0..4] /\ bind \in [Wk -> -1..NW] /\ rs \in {"read", "err", "done"}
          /\ sl \in [Cur -> [p: -1..NW, ok: BOOLEAN, l: BOOLEAN]] /\ sj \in [Hs -> -1..NW]
          /\ jn \in BOOLEAN /\ sv \in [Hs -> {"loop", "ok", "bad"}]
\* The reader takes no record from a file of an other nonce, and ties each file only to its own writer process.
NoForeign == rd[0] = 0 /\ \A w \in Wk : bind[w] \in {-1, 0, w}
\* The reader takes no record that has no closing newline.
NoPartial == \A w \in Wk : rd[w] <= Whole(w)
\* A complete read took all records of each file in order, and each file ends with a final record.
NoLoss == rs = "done" => \A w \in Wk : rd[w] = n[w] /\ (n[w] > 0 => f[w][n[w]] \in Term)
\* A final record is the last record, and the worker ended in that state.
TerminalTruth == \A w \in Cur, i \in 1..4 : i <= n[w] /\ f[w][i] \in Term =>
    i = n[w] /\ (f[w][i] = "tok" => pc[w] = "ok" /\ ~Act(w)) /\ (f[w][i] = "tbad" => pc[w] = "bad")
\* summary.json has at most one line for each harness.
SummaryOnce == \A h \in Hs : sj[h] <= 1
\* summary.json has each harness that ended well; so a missing harness shows as failed.
NoSilentDrop == sj[1] >= 0 => \A h \in Hs : sv[h] = "ok" => sj[h] >= 1
Safe == NoForeign /\ NoPartial /\ NoLoss /\ TerminalTruth /\ SummaryOnce /\ NoSilentDrop
IndInv == /\ TypeOK /\ Safe /\ (rs = "done" => sj[1] >= 0)
          /\ \A w \in Wk, i \in 1..4 : i <= n[w] /\ f[w][i] \in Term \cup {"part"} => i = n[w] /\ w > 0 /\ ~Alive(w)
          /\ \A w \in Cur : (pc[w] = "new" => n[w] = 0) /\ (pc[w] = "run" => n[w] = 1) /\ (pc[w] = "test" => n[w] = 2)
          /\ \A w \in Cur : sl[w].l /\ (pc[w] \in {"new", "run", "test", "epi"} => sl[w].p = 0) /\ (sl[w].p = -1 => jn)
          /\ \A w \in Cur : (Alive(w) => n[w] < 4 /\ sv[H(w)] = "loop") /\ (w > NH /\ ~jn => pc[w] = "new")
          /\ \A w \in Cur : pc[w] \in {"fin", "ok"} => sl[w].p # 0 /\ (sl[w].ok \/ sv[H(w)] = "bad")
          /\ \A w \in Cur : sl[w].p > 0 /\ jn => sv[H(w)] # "loop" /\ w = Last(H(w))
          /\ \A h \in Hs : sv[h] = "ok" => sl[Last(h)].p > 0 /\ sl[Last(h)].ok
          /\ sj[1] >= 0 => (\A h \in Hs : sv[h] # "loop") /\ sj = [h \in Hs |-> Count(h)]
=============================================================================
