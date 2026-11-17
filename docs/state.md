# TALOS Handoff — Current State

> Canonical checkpoint for every future session and agent. Read this file with `docs/plan.md`, `docs/tasks.md`, and `docs/decisions.md`. The chat transcript is not required to continue.

## Current phase

**R3 — One autonomous Writer is complete for the verified macOS target.** T3.1–T3.7 are done. `task run`, safe terminal-error `task continue`, fail-closed `task recover`, audited `task resolve`, and one-shot `up --auto` are implemented. `up --auto` handles at most one queued task from a clean Git worktree. Writer success always remains `reviewing`; there is no Critic, project-check runner, or task completion gate yet. `up --semi` remains deferred to R5. Backlog count: 25 of 61 tasks done (41.0% by task count, unweighted). Stop here at the round boundary; next planned task is T4.1.

## Goal and fixed direction

TALOS is a Rust local CLI orchestrator for installed AI coding CLIs. The target workflow is a durable task queue, one Writer, an independent read-only Critic, real checks, safe recovery, and eventually an optional GitHub PR without automatic merge.

- Local modular monolith; SQLite WAL is authoritative.
- Each discovered project owns `<project-root>/.talos/state.sqlite3`; there is no shared/global database.
- Preferred stack: Rust stable, Tokio, rusqlite, serde/serde_json, clap, tracing, Git CLI, and optional tmux.
- No required MCP, Python runtime, Docker, Redis, PostgreSQL, Kubernetes, or separate local service.
- macOS is the currently verified development/runtime platform. Linux-specific implementation remains in scope; if Linux-target build, runtime, or Agy-sandbox checks require cumbersome setup on macOS, defer those checks to a lightweight run on the user's Linux server. No Docker or VM is required.
- All TALOS code, system instructions, prompts, CLI output, logs, reports, and documentation are English. Human conversation may use another language.
- Agent Starter Kit is workflow guidance: recommendations plus explicit safety and preservation boundaries.
- pair-mailbox is a matrix of engineering lessons, not a code dependency. Its source remains untouched.
- No code is published or merged without the user's explicit instruction.

## R1 — CLI spike

R1 is complete with the following deferred checks recorded in `docs/tasks.md` and `docs/research/cli-spike.md`:

- Antigravity CLI `agy 1.5.10` returned parseable headless JSON and resumed the same public conversation ID in a second process. Combined local token usage reported by the CLI was 31,318; billing is unknown. The session ID is intentionally not stored in project docs.
- The Antigravity TUI stopped at a project-trust prompt. It was declined to avoid changing persistent user trust settings. TUI-to-headless continuation is unverified and remains T6.6.
- At the R1 checkpoint, safe unattended writes were unverified. T3.7 later passed for installed `agy 1.5.10` under the exact session-only policy recorded in `cli-spike.md`; this alone does not enable Auto.
- Native subagent behavior is not live-tested. Process timeout, Ctrl-C, SIGTERM, and process-group cleanup are tested with fake CLIs under T3.2; Antigravity-specific cancellation and restart recovery remain open.
- Codex and alternative CLI probes stopped before inference or were not run because of sandbox/auth constraints. Do not claim those adapters are verified.
- ADR-011 marks Antigravity as the provisional first adapter only.

Three native agents were used for the R1 Antigravity, Codex, and alternative CLI survey. Independent read-only Codex-agent reviews were also used during R2 and R3.2. The R2 findings were addressed; the R3.2 follow-up found no remaining material runner issue.

## R2 — Rust core and durable state

Implemented in `Cargo.toml`, `Cargo.lock`, and `src/`:

- Clap CLI includes `up --observe/--auto`, `task add/list/run/continue/recover/resolve`, `status`, `logs`, `doctor`, global `--json`, `--help`, and `--version`.
- Project root discovery: Git root first; then existing `.talos/`, common project markers, and current directory. Git environment overrides are removed from the probe. Invalid Git metadata reports an error rather than silently choosing a different root.
- SQLite WAL schema version 1 with foreign keys, transactional migration, `tasks`, append-only `task_events`, and a singleton supervisor lease.
- Validated task state transitions record timestamp, actor, reason, and from/to status atomically. `complete` is blocked until a later evidence gate exists.
- Supervisor lease acquisition uses an immediate transaction, prevents a second concurrent owner while unexpired, persists a monotonically increasing fencing token across expiry/release, and only releases a matching owner/token pair. Heartbeats and process-liveness reconciliation are not implemented yet.
- Future database schema versions are rejected before WAL or schema migration changes the database.
- On Unix, runtime paths must be real paths owned by the current effective user. TALOS restricts `.talos/` to `0700` and `state.sqlite3` to `0600`, including existing paths. Symlinks are rejected.
- `up --observe` initializes/reports state and does not start a process. One-shot `up --auto` is enabled after R3 acceptance; `up --semi` remains unavailable until R5.
- Active verification profile is `scripts/verification-profile.tsv`.

R3 introduced schema v2 for durable `writer_attempts` and schema v3 for separate resolution metadata and continuation source links. It stores no prompt or transcript. The Writer uses a single supervisor lease, checks Agy 1.5.10, creates a session-only policy file, persists the PID before sending the prompt, records a public conversation ID as soon as the `init` event arrives, and moves success only to `reviewing`.

## Verification evidence

Before R3 process-supervisor changes, the active command `./scripts/verify-project.sh` passed all five required checks:

1. `cargo fmt --all -- --check`
2. `cargo clippy --locked --all-targets --all-features -- -D warnings`
3. `cargo test --locked --all-targets` — 13 tests passed.
4. `cargo build --locked --release` — passed on macOS ARM64.
5. `./scripts/security-scan.sh` — `security-scan: clean`.

Additional checks passed:

- Release-binary smoke test in `/private/tmp/talos-r2-smoke-final`: help/version, repeated `up --observe`, task add/list, status, logs, doctor, and JSON output.
- Nested Git-root smoke test in `/private/tmp/talos-git-smoke/nested`: database resolved to `/private/tmp/talos-git-smoke/.talos/state.sqlite3`, even with an unrelated `GIT_DIR` set.
- Auto/Semi guard smoke test in `/private/tmp/talos-auto-guard`: both returned exit code 1; no `.talos/` directory or agent was created.
- `bash -n scripts/verify-project.sh scripts/security-scan.sh` passed.
- An English-only source/documentation scan returned no matches (expected exit code 1).

The macOS C toolchain prints sandbox-related `xcrun` cache warnings during SQLite bundled builds; Cargo still completed the release build successfully. No Rust or Clippy diagnostics remained at that R2 checkpoint.

After adding Tokio 1.54.1, SHA-2, schema v2, and the single-task Writer path, the full active profile `./scripts/verify-project.sh` passed all five checks:

- `cargo fmt --all -- --check` — passed.
- `cargo clippy --locked --all-targets --all-features -- -D warnings` — passed.
- `cargo test --locked --all-targets` — 35 passed; 1 helper test is ignored in the normal suite and invoked by its signal-forwarding test. Coverage includes v1-to-v2 migration, durable attempts, fake-CLI task integration, NDJSON stdin/result parsing, timeout and reaping, Ctrl-C, SIGTERM, project-scope guards, and cleanup of a surviving process-group descendant.
- `cargo build --locked --release` — passed on macOS ARM64.
- `./scripts/security-scan.sh` — `security-scan: clean`.

An independent read-only review found and prompted fixes for timeout rounding, cancellation cleanup, stdin task cleanup, and surviving descendants. Its final review found no remaining material issue in the runner or the later T3.3/T3.7 changes. A disposable CLI smoke under `/private/tmp/talos-r3-task-run` ran `task add`, `task run 1`, `task list`, and `logs`; the fake CLI's session ID and outcome were persisted, and the task remained `reviewing`.

### Prior T3.6 checkpoints (superseded by the R3 closure record below)

Before editing recovery logic, ADR-015 recorded the invariants and failure windows: `launching` before spawn is indistinguishable from spawn-before-PID-commit; a recorded PID can be reused; prompt delivery and project edits may precede the final database commit; the conversation ID may arrive before or after the final result; and no portable process-start identity is currently persisted. Context7 documentation was checked for rusqlite 0.41.1 immediate transactions and Tokio 1.54.1 `Child::id`; the latter is only the OS PID while the child is running.

The new `task recover` command takes the supervisor lease. Its immediate SQLite transaction validates the current unexpired lease owner and fencing token, changes only `launching`/`running` attempts to `unknown`, blocks their still-implementing tasks, and appends recovery/task-transition events. It preserves recorded PID and session metadata, leaves unknown attempts unresolved, and is idempotent. It does not inspect or signal processes, resume a conversation, or retry work. A process-runner `Err` also leaves the attempt unresolved and retains the session policy file instead of recording an unproven terminal failure. An unresolved attempt prevents a Writer launch for any other queued task.

Isolated tests cover reopening a `launching` attempt with no PID, quarantining a recorded PID that points to the live TALOS test process without signaling it, idempotent recovery, forged fencing-token rejection without state mutation, lease contention, a runner launch error, and the global no-new-Writer gate. The post-spawn/before-PID crash is represented by its identical persisted database shape; the test does not keep a separate child alive across a simulated crash. A literal OS PID reuse event is not induced; recovery does not inspect process identity at all. Full process/session/project reconciliation, a safe operator resolution path, an actual post-spawn injected runner error, and Linux runtime behavior are still unverified. `task recover` may be blocked until the existing 35-minute lease expires after a hard TALOS crash. This stage does not complete T3.6.

The next T3.6 research stage is complete and recorded in `docs/research/recovery-platform-identity.md`. Official Linux procfs documentation provides a candidate identity tuple `(boot_id, PID, process_group_id, starttime_ticks)`. On macOS, XNU exposes PID/group/start-time fields through `proc_pidinfo`, but Apple's `libproc.h` labels these process-information interfaces private and subject to change. No stable public macOS replacement was confirmed, so TALOS must report identity unavailable there and remain unresolved. No process-identity schema change has been made.

Two live TALOS-driven Antigravity 1.5.10 runs were completed in disposable Git projects; no TALOS source files were exposed to the Writer:

- A normal host-sandbox launch failed before inference because the environment denied Antigravity's user-local log/crash writes and ephemeral localhost binding. TALOS recorded a generic nonzero-exit failure and made no project marker file.
- An approved process launch with a session-only TALOS policy created `TALOS_LIVE_RESULT.md` with the requested fixed content. The database recorded a successful attempt with PID and public conversation ID, and the task remained `reviewing`. Temporary policy settings were removed after resolution.
- A read-boundary run asked the Writer to reveal sentinel values stored under `.talos/` and `.git/` and to write a result marker. Both reads were denied; the marker contained only `READS_DENIED`, no sentinel value appeared in the response, and the database recorded the attempt. Temporary policy settings were removed after resolution.

The host sandbox limitation and exact observations are in `docs/research/cli-spike.md`. `./scripts/verify-project.sh` completed successfully after the latest code changes; its release build emits macOS `xcrun` cache warnings because that cache path is inaccessible, but Cargo exits successfully.

T3.6 quarantine stage verification on 2026-11-17:

- `cargo fmt --all && cargo test --locked --all-targets` — passed; 41 passed, 1 helper ignored in the normal suite and invoked by the signal-forwarding test.
- `cargo fmt --all && ./scripts/verify-project.sh` — passed all five active checks: format, Clippy with `-D warnings`, `cargo test --locked --all-targets` (41 passed, 1 ignored), `cargo build --locked --release`, and `security-scan: clean`. The release build printed sandbox-related `xcrun` SDK-cache warnings; Cargo exited successfully.
- Disposable release-binary smoke in `/private/tmp/talos-t36-final-smoke.*`: `up --auto` returned exit code 1 and created no `.talos/`; `task recover --json` on an empty project returned `no_unresolved_attempts`, `writer_launched: false`, and `automatic_retry: false`; `task recover --help` showed the new command.
- Separate Auto/Semi guard smoke in `/private/tmp/talos-t36-guard-smoke.*`: both `up --auto` and `up --semi` returned exit code 1 and created no `.talos/` runtime directory.
- Platform API research used the [Linux kernel procfs documentation](https://docs.kernel.org/filesystems/proc.html), the [Linux `proc_pid_stat` manual](https://man7.org/linux/man-pages/man5/proc_pid_stat.5.html), the [Linux boot ID documentation](https://kernel.org/doc/html/latest/admin-guide/sysctl/kernel.html), and Apple's [XNU process headers](https://github.com/apple-oss-distributions/xnu/blob/main/bsd/sys/proc_info.h) and [libproc header](https://github.com/apple-oss-distributions/xnu/blob/main/libsyscall/wrappers/libproc/libproc.h). This research changed documentation only; no Rust verification was required beyond the successful profile above.
- `rustup target list --installed` — only `aarch64-apple-darwin` is installed; Linux collector compilation/runtime verification remains unavailable in this environment.
- Exact files changed in the preceding recovery/research batch: `README.md`, `docs/decisions.md`, `docs/plan.md`, `docs/tasks.md`, `docs/state.md`, `docs/research/recovery-platform-identity.md`, `src/app.rs`, `src/cli.rs`, `src/db.rs`, and `src/supervisor.rs`. Schema remains version 2; no migration or dependency was added.
- The workspace has no writable Git metadata, so `git status` is unavailable. No commit, remote, publication, PR, or primary-branch change was made.

Historical T3.6 identity-parser test slice on 2026-11-17 (schema-v2 checkpoint, superseded by the schema-v3 R3 closure below):

- Added `src/process_identity.rs` with a Linux `/proc/<pid>/stat` parser/collector candidate, boot UUID validation, a fingerprint containing boot ID, PID, process-group ID, and start ticks, explicit unavailable behavior on non-Linux platforms, and deterministic fixture tests. Wired the module only into test builds from `src/lib.rs`; production recovery behavior and the database schema remain unchanged.
- Added a Linux-only test that captures and compares the current test process. It was not run because only the `aarch64-apple-darwin` target is installed. The macOS test verified that process identity capture returns unavailable.
- `cargo fmt --all && ./scripts/verify-project.sh` — passed format, Clippy (`-D warnings`), tests (47 passed, 1 ignored), macOS ARM64 release build, and `security-scan: clean`. The OS toolchain emitted non-fatal `xcrun` cache-file warnings because its cache path is not writable in this sandbox.
- `python3` backlog count from `docs/tasks.md`: 59 task rows total — 22 done, 1 in progress, 33 planned, 3 deferred. This is an unweighted task-count measure, not a feature/effort estimate.
- At that checkpoint, schema stayed at version 2 and no process identity was stored or consumed by `task recover`. R3 closure later added schema-v3 operator-resolution metadata; no process identity is stored or consumed by recovery. Linux compilation/runtime acceptance remains scheduled for T6.5/T9.3.
- Exact files changed in this slice: `src/process_identity.rs`, `src/lib.rs`, `docs/research/recovery-platform-identity.md`, `docs/tasks.md`, and `docs/state.md`.

## R3 closure acceptance — 2026-11-17

### Implemented

- `src/db.rs`: schema v3 migration from v1/v2; immutable operator-resolution fields; linked continuation attempt; lease/fencing checks; idempotent early conversation-ID persistence.
- `src/cli.rs`, `src/app.rs`: `task continue`, `task resolve`, one-shot `up --auto`, clean-Git precondition, Writer response and changed-path reporting. `--semi` remains disabled.
- `src/supervisor.rs`, `src/adapter/agy_process.rs`: failure classification, inspection-first continuation prompt, exact `--conversation` resume, matching `init` handshake before prompt, process-group liveness check that never signals during resolution.
- `Cargo.toml`: Tokio `sync` feature for the bounded continuation handshake event channel.
- Project status and acceptance criteria were updated in `README.md`, `docs/plan.md`, `docs/tasks.md`, `docs/decisions.md`, `docs/architecture.md`, `docs/project-structure.md`, `docs/research/cli-spike.md`, and this handoff.

### Verification results

- `./scripts/verify-project.sh` — passed: format, Clippy with `-D warnings`, **66 passed / 1 ignored**, macOS ARM64 release build, and `security-scan: clean`. The ignored helper is invoked by its signal-forwarding test. macOS `xcrun` emitted non-fatal SDK-cache permission warnings during SQLite builds.
- Independent read-only R3 closure audit confirmed the documented acceptance evidence and found no open R3 exit criterion. It found stale proposed-CLI wording and unclear task-status effects for `task resolve`; `docs/plan.md`, `docs/decisions.md`, README, task acceptance, and architecture now state the implemented commands and resolution behavior explicitly.
- Disposable release CLI recovery flow: injected spawn-before-PID persistence failure left `launching` with no PID; `task recover --json` quarantined it as `unknown`; missing attestations were refused; explicit `authorize-retry` preserved the attempt as `unknown`, changed the task to `queued`, and did not start a Writer.
- Disposable fake-CLI continuation flow: terminal `ERROR` with exit 0 resumed the same validated conversation exactly once after required inspection confirmation; task remained `reviewing`.
- Disposable fake-CLI Auto flow: one queued task produced the expected changed path and response; a separate `task list` process saw `reviewing`; a repeated invocation did not launch another Writer. A two-task queue smoke processed only task 1 and left task 2 queued.
- Live Antigravity 1.5.10 `up --auto`: in a clean disposable Git project, created `TALOS_LIVE_AUTO.md` with the exact requested content, reported that changed path, persisted `reviewing`, and a separate process observed the task. A repeated `up --auto` returned `no_queued_tasks`; recovery returned `no_unresolved_attempts`.
- Live Antigravity 1.5.10 continuation: a local wrapper changed only the first real CLI result status to `ERROR`; after the operator-side content check, TALOS resumed the same public conversation ID and accepted its matching `init` before sending one follow-up. This fault-injection verifies TALOS continuation behavior, not that Agy naturally emits the `ERROR` status.
- Linux targets were neither installed nor built; no Docker/VM was started. Linux process-identity, build, runtime, and Agy sandbox acceptance remain for the user's server under T6.5/T9.3.

### Known limitations and backlog

- A process-group probe is liveness evidence only. It does not identify the Writer or detect escaped descendants; with no saved PID, the stopped-Writer attestation is entirely operator-provided. The audit actor is recorded as the generic role `operator`.
- The CLI application checks process-group liveness before resolution; the lower-level database method assumes its caller has performed that probe. Keep resolution behind the checked application path. Resolution immediately sets task status (`reviewing`, `queued`, or `failed`) while preserving the attempt as `unknown`; `authorize-retry` only prepares a later explicit Writer command.
- No literal PID-reuse event or actual post-spawn/pre-PID crash was forced. Tests cover equivalent persisted states and fail-closed behavior. A hard TALOS crash may leave the 35-minute supervisor lease active until expiry.
- Independent Critic review, automated project checks, evidence-gated completion, background workers, Semi approval, Linux verification, and packaging remain later rounds.
- User-approved direction for R4/R7: select Writer and independent Critic CLIs separately from verified adapters; the later concurrency acceptance includes one Writer, one Critic, and at least one additional scoped agent. This is not implemented in R3.
- TALOS currently relies on each Git project to ignore `.talos/`; `up --auto` rejects a dirty worktree, but `task add` may leave runtime files visible as untracked until the project adds an ignore rule. T5.7 tracks exclusion handling.
- An independent read-only review flagged a narrow numeric PGID reuse window in the process-runner's cleanup signal path. It did not identify a confirmed T3.6 resolution defect. T6.3 now tracks a focused review/fix; until resolved, do not claim OS-verified process identity.

## Repository and runtime condition

- A local Git repository is initialized on the `master` branch. The project remains local; no remote is configured, and no push or publication is planned until the user creates a hosted repository.
- No `.talos/` runtime state was created in the TALOS workspace. Smoke databases are isolated under `/private/tmp`.
- TALOS-driven Antigravity runs happened only in disposable projects. No external AI CLI has written to the TALOS source tree. There is no background worker or tmux session; `up --auto` is one-shot and `up --semi` is not implemented.
- The original Agent Starter Kit and pair-mailbox repositories remain untouched.

## Next action

Next task: **T4.1 — define the independent Critic input/output contract and revision identity.** R3 remains closed. Implement Linux-specific code as its planned tasks are reached; defer Linux-native acceptance checks to the user's server if local setup would slow macOS development. Do not add Docker/VM infrastructure.

## Session close checklist

Before ending a future session, update this handoff and `docs/tasks.md` with the current task IDs, exact file changes, verification commands/results, verified limitations, decisions, and one next action. All project artifacts remain English.
