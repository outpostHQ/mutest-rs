# Runtime progress, version 1

On Linux, a harness worker can report its progress as JSON Lines while it runs, so that a caller can
tell a slow run from a stuck one.

## Enabling it

Set both variables in the harness's environment, or neither:

| Variable | Value |
| --- | --- |
| `MUTEST_PROGRESS_DIR` | an absolute path to an existing directory; make it private to the caller (mode 0700) |
| `MUTEST_PROGRESS_NONCE` | 32 lowercase hexadecimal characters, chosen by the caller for this run |

Setting only one of them, or an invalid value, makes the harness exit with code 101, and so does
enabling progress on another platform. The worker removes both variables before it runs any test, so
commands that tested code starts do not inherit them.

`cargo mutest run --require-progress` checks the two variables before it starts Cargo, and passes the
flag on to the harness, which then refuses to run without progress. Generated code checks
`mutest_runtime::PROGRESS_PROTOCOL_VERSION == 1` when it compiles, so a stale runtime cannot be
linked in. Older harnesses ignore arguments they do not know, so passing them the flag says nothing
about whether they support it.

## Files

Each worker creates one file, `<nonce>-<pid>-<instance_id>.jsonl`, with mode 0600. A record is one
line of at most 16 KiB, newline included, and a file holds at most 64 MiB. A record is never cut
short to fit: a record over either limit, or one that cannot be written, ends the run incomplete.

## Records

Every record has these fields:

| Field | Value |
| --- | --- |
| `schema` | `"mutest-progress"` |
| `version` | `1` |
| `event` | one of the events below |
| `nonce` | the caller's nonce |
| `instance_id` | a hexadecimal id unique to this worker |
| `seq` | 0 for the header, then one more for each record |
| `elapsed_ms` | milliseconds since the header, never decreasing |

Each event adds its own fields:

| Event | Fields |
| --- | --- |
| `header` | `pid`, `process_start: {kind: "linux-proc-starttime", ticks: u64}`, `exe` (the canonical executable path), `record_limit_bytes: 16384`, `file_limit_bytes: 67108864` |
| `phase` | `phase`: `reference`, `evaluation` or `simulation` |
| `test_start` | `phase`, `invocation_id` (a u64 unique within the worker), `test_name`, `mutation_ids` (u32 array), `strategy`: `isolated` or `in_process`, `execution_timeout_ms` (u64 or null), `startup_timeout_ms: 5000`, `cleanup_timeout_ms: 10000`, `report_timeout_ms: 1000`, `join_timeout_ms: 1000` |
| `test_end` | `invocation_id`, `result`: `ok`, `failed`, `crashed`, `timed_out` or `ignored`, `cleanup`: `complete` or `pending` |
| `terminal` | `status`: `completed` or `incomplete`, `exit_code` |

## What the records promise

- `test_start` is written before the test is scheduled, or its owner process spawned.
- Tests can end out of order: pair `test_start` with `test_end` by `invocation_id`.
- Timeouts are rounded up to whole milliseconds. A null `execution_timeout_ms` means the harness sets
  no deadline, so the caller needs its own budget. Reference tests always have a null
  `execution_timeout_ms` and empty `mutation_ids`.
- The four stage timeouts apply only to isolated tests, and are 0 for `in_process` ones. They are
  what the harness allows each stage, not a promise that teardown ends within their sum: cancelling
  a test can also wait for its owner's cleanup, so a caller that enforces a budget still bounds its
  own teardown.
- An isolated test's `test_end` with `cleanup: "complete"` comes after its descendants are gone. An
  in-process test that timed out may end with `cleanup: "pending"`, and stays outstanding.
- A cancelled test ends with `result: "ignored"`.
- A `phase` record comes only when no test is outstanding.
- `status: "completed"` needs every started test to have ended with complete cleanup, and the
  worker's completion record to be written. Anything else is not success, including a missing or
  malformed `terminal`; the harness's exit code stays authoritative.

Progress tells runs apart; it does not authenticate them. Tested code running as the same user can
write to the file, and the nonce and process start time only keep one run's records from being
mistaken for another's.
