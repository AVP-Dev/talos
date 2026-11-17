# Initial design manifest

## Invariants

- Accepted work survives process and machine restarts in SQLite.
- The R2 core allows one active supervisor lease; worker/task leases and recovery reconciliation are later gates.
- Agents never receive authority to publish, merge, change secrets, or operate on production by default.
- The critic cannot write to the active implementation checkout.
- Only observed process exits, source-tree facts, and executed checks count as evidence.
- A capability is enabled only after the exact public CLI flow has been exercised successfully.

## State transitions

`queued → planning → implementing → reviewing → verifying → complete`.

The R2 core validates task transitions and appends the timestamp, actor, reason, and from/to state in the same transaction as the task update. A critic rejection returns `reviewing → implementing`. Task completion stays disabled until R5 adds evidence-gated closeout. A worker crash preserves the last state and lease metadata; restart first reconciles process and repository state, then resumes by a proven session ID or starts a new attempt with the checkpoint context.

## Failure handling and idempotency

- SQLite writes use transactions; supervisor lease acquisition is an atomic compare-and-set against the current owner and expiry, with a persistent monotonically increasing fencing token.
- Child output is read while the process runs; stdout/stderr are bounded in memory and full logs stay in local ignored state.
- A lost process handle is not treated as task failure until process identity and filesystem changes are reconciled.
- A retried external CLI operation uses a recorded operation key and prior result where available. Git changes are inspected before retry; no destructive reset is used to restore a clean tree.
- Unsupported resume, structured output, sandbox, or interrupt behavior is surfaced as a limitation and keeps the task recoverable rather than falsely complete.
- Review and verification failures keep the task open with actionable evidence.

## Acceptance checks

1. Fresh `talos up --auto` creates one state database and can be run again without duplicate workers or tmux sessions.
2. A task is queued once, claimed by a single writer, and remains visible after process restart.
3. An expired lease is reclaimed only after its process is proven absent; an active PID is never duplicated.
4. Critic findings can reject a change; only the writer can apply an approved, digest-matching patch.
5. A failed test or missing evidence blocks completion.
6. Closing the terminal or disconnecting SSH does not discard durable task state.
7. SIGTERM and forced termination leave a state that can be reconciled without repeating completed effects.
8. Unsupported CLI capabilities fail with a clear message and preserve the queued task.

These checks are design targets, not claims that current code already passes them.

## Verified R2 behavior

- `up --observe` initializes the local database and reports status without starting a worker.
- On Unix, `.talos/` and `state.sqlite3` are restricted to the current user's ownership and `0700`/`0600` permissions; symlinked runtime paths are rejected.
- A future `user_version` is rejected before WAL or schema migration changes the database.
- Auto and Semi are unavailable until the adapter and safe-write gates pass.
