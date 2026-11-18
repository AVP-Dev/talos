# TALOS verification policy

## Gates

Run the active project verification profile (`./scripts/verify-project.sh`) after a change. Its command list is generated from checks that exist in this crate. The intended Rust gates are:

1. Formatting: `cargo fmt --all -- --check`.
2. Static analysis: `cargo clippy --locked --all-targets --all-features -- -D warnings`.
3. Behavioral tests: `cargo test --locked --all-targets`.
4. Release build: `cargo build --locked --release`.
5. Secret baseline: `./scripts/security-scan.sh`.

Do not mark a stage green while a required command is absent or has not run. Record blocked checks with the exact command and reason.

## Evidence layers

- **State and queue:** malformed state, concurrent claims, duplicate submissions, expired leases, interrupted transactions, schema-version mismatch, and restart reconciliation.
- **Process boundary:** arguments preserved without shell interpretation, stdout/stderr drained concurrently, nonzero exit, timeout, SIGINT/SIGTERM, kill/reap, child disappearance, and no duplicate process on recovery.
- **Adapters:** capability parser uses captured help fixtures, while a separate opt-in integration probe confirms the actual installed public CLI. Fixtures never count as proof of vendor behavior.
- **Git and review:** dirty baseline, new/untracked files, patch hash mismatch, review rejection, branch conflict, and no destructive reset.
- **End-to-end:** one first-run initialization, repeat `up`, queued task, writer, independent critic, failed/successful verification, interruption/recovery, and final report.

Use temporary directories and repositories. Never put actual credentials in fixtures. Tests must not contact model APIs unless explicitly marked live, isolated, and run with the user's intent. An agent's text claim and a fake adapter cannot satisfy execution evidence.

## Completion gate

The task must have a concrete acceptance list. At closeout TALOS verifies current repository facts, records actual check results, confirms the critic reviewed the same revision, and leaves an audit trail. A missing CLI feature is reported as unsupported and blocks only the workflow that depends on it; it must not be silently emulated by parsing terminal art.

## R9.2 hardening acceptance plan

Run these as separate scenarios and record the host, database size before/after, peak process memory where available, exact command, and result. No hard long-term database size target or retention policy exists yet; measure growth and keep archival/pruning decisions in F1 rather than claiming the database is bounded.

| Area | Required scenario | Pass evidence |
|---|---|---|
| Database growth | Add a large task/event history, many verifying tasks, many child/dependency/worktree records, a large queued set, and a large changed/untracked Git path set; close/reopen the database and measure WAL/checkpoint behavior. Exercise paged task/dependency/child/worktree/log/evidence lists, `task recover`, `resume --reconcile`, `up --auto`/`--semi`, repeated `talos stop` polling, and latest-attempt/verification queries with large histories. | All rows remain readable; measured size is recorded; no automatic deletion occurs; list/evidence and blocked-queue output stay within their page/preview bounds; Git status streams paths and output shows the total plus at most 50 path previews whose prefixes are capped at 256 bytes; recovery reads only unresolved attempts; resume reconciliation stores only a bounded ID preview while changing every eligible row and appending one event per row; scheduler/stop/latest-record paths avoid materializing full histories. |
| Resume reconciliation fault | Create many `verifying` tasks, pause the queue, then fault the status update or task-event insertion inside `resume --reconcile`. | On success, every task becomes `reviewing` with exactly one matching append-only event; the audit stores a bounded preview and full count. On failure, the transaction rolls back, tasks remain `verifying`, no partial task events appear, and the queue stays paused. |
| Migration fault | Interrupt or fault-inject a schema migration at transaction boundaries, including v16→v17, then reopen. | The database is either at the prior complete version or the new complete version; task/event/dependency rows remain intact and the v17 queue index exists only after a complete migration. |
| Process limits | Exercise large stdout/stderr, a stuck child, timeout, SIGINT/SIGTERM, and a surviving descendant. | Captures remain bounded, children are reaped where possible, and ambiguous work stays `unknown`. |
| Worktree recovery | Interrupt creation, integration, and finalization at each filesystem/database boundary. | Reconciliation uses recorded identity; it never resets or deletes operator data. |
| GitHub recovery | Interrupt push and draft PR creation, then reconcile repeated requests. | TALOS pushes only the recorded ref, does not auto-retry unknown effects, and does not create duplicates. |
| CI storage | Repeat identical CI snapshots and collect multiple distinct runs. | Identical summaries deduplicate; a changed commit or status produces separate evidence; raw output stays absent. |
| Backup/restore | Create a consistent SQLite backup while WAL is active and restore into a disposable project. | Task state and migration behavior are readable after restore; permissions remain private. |
| Release reproducibility | Build twice from the same clean commit/toolchain/target and compare binary checksums. | Identical checksums or a documented toolchain/linker source of variation. |

These scenarios are not run by `scripts/verify-project.sh`. T9.2 remains in progress until each result is recorded. Linux-native cases stay deferred to the user's server.
