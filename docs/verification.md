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
