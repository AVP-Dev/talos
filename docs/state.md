# TALOS Handoff — Current State

> Canonical checkpoint for every future session and agent. Read this file with `docs/plan.md`, `docs/tasks.md`, and `docs/decisions.md`. The chat transcript is not required to continue.

## Current phase

**R6 — Runtime and resumption remains in progress.** T6.1 and T6.2 are complete. T6.3 behavior/profile verification, T6.4 macOS SSH reconnect, T6.5 Linux server acceptance, and T6.6 interactive handoff remain open. T6.5 implementation adds schema-v9 Linux `/proc` fingerprints and recovery reporting; Linux-native checks are deferred until server access. R7 code slices T7.1–T7.4 are implemented or in progress while behavior checks and external acceptance gates remain documented. Backlog count: 40 of 61 scheduled tasks done (65.6% by task count, unweighted); two unscheduled ideas are recorded separately.

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

R3 introduced schema v2 for durable `writer_attempts` and schema v3 for separate resolution metadata and continuation source links. R4 advances the schema to v4 with Writer base revisions, linked Critic revisions, and immutable Critic review records. Full prompts, patches, and transcripts are not persisted. The Writer uses a single supervisor lease, checks Agy 1.5.10, creates a session-only policy file, persists the PID before sending the prompt, records a public conversation ID as soon as the `init` event arrives, and moves success only to `reviewing`.

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
- At the R4 close, automated project checks and evidence-gated completion were later work; R5 now implements them. Background workers, pause/resume, Linux verification, and packaging remain later rounds.
- User-approved direction for R4/R7: select Writer and independent Critic CLIs separately from verified adapters; the later concurrency acceptance includes one Writer, one Critic, and at least one additional scoped agent. This is not implemented in R3.
- R5 now checks that `.talos/state.sqlite3` is ignored before opening the database and refuses tracked or unignored state; TALOS does not rewrite project ignore files.
- An independent read-only review flagged a narrow numeric PGID reuse window in the process-runner's cleanup signal path. It did not identify a confirmed T3.6 resolution defect. T6.3 now tracks a focused review/fix; until resolved, do not claim OS-verified process identity.
- Project-check timeout currently kills and reaps the direct command process, but does not yet provide the Writer runner's process-group cleanup for any descendants. A timed-out check cannot complete its task; T6.3 tracks process-group containment and the related PGID reuse review before claiming full subprocess-tree cancellation.

## Repository and runtime condition

- A local Git repository is initialized on the `master` branch. Initial snapshot commit: `76fa40d` (`chore: initialize local project history`). R5 closeout is `1e76675`; T6.1 slice is committed as `7671f12` (`feat: add project tmux control sessions`). R6 continues with T6.2. The project remains local; no remote is configured, and no push or publication is planned until the user creates a hosted repository.
- The staged snapshot contained 38 project files; `.gitignore` excluded `.DS_Store` and `target/`. `git diff --cached --check` passed. One redundant blank line at the end of `Cargo.toml` was removed before the initial commit. No Rust tests or builds were run for this repository/documentation setup batch.
- Product history follow-up is recorded as unscheduled idea F1, to revisit after R9. The current database stores task metadata and lifecycle events plus Writer-attempt metadata; it does not persist a separate project plan, project-creation history, or full Writer transcript.
- Earlier smoke databases were isolated under `/private/tmp`; the R4 runtime database in this workspace is now documented below.
- `.talos/state.sqlite3` now exists locally from R4 smoke use; it contains no queued tasks. Two accidental harness tasks were removed after a SQLite backup was made to `/private/tmp/talos-r4-db-before-cleanup.sqlite3`. The workspace database remains ignored by Git.
- TALOS-driven Antigravity runs happened only in disposable projects. No external AI CLI has written to the TALOS source tree. There is no background worker or tmux session; `up --auto` and `up --semi` are one-shot.
- The original Agent Starter Kit and pair-mailbox repositories remain untouched.

## R4 closure acceptance — 2026-11-17

### Implemented

- Added `src/revision.rs` to capture the original Git base, complete tracked/untracked diff, sorted changed paths, and `sha256:` revision ID. `.git/` and `.talos/` are excluded; patches over 64 KiB fail without truncation.
- Added `src/review.rs` for the bounded JSON request/response contract. The review prompt is capped at 96 KiB and response at 64 KiB. Invalid, stale, mismatched, oversized, or failed reviews never approve.
- Added `.talos/config.json` role configuration. Writer and Critic roles default to Agy; unsupported/unverified adapters are rejected and `doctor` reports the selections.
- Added schema v4 with Writer base revisions and immutable Critic review history. A rejection starts a linked Writer iteration against the same base; approval transitions to `verifying`, not `complete`.
- Added `talos task review <ID>` and an isolated Antigravity 1.5.10 Critic policy that allows project reads but denies file writes, commands, MCP/URL tools, and reads of `.talos/`/`.git/`.
- Added unscheduled F2 to explore typed agent handoffs and controlled-language ideas after R9. It is not an accepted feature and makes no assumption about a Russian profile.

### Verification results

- `cargo fmt --all && ./scripts/verify-project.sh` — passed the full active profile: format, Clippy with `-D warnings`, **79 passed / 1 ignored**, macOS ARM64 release build, and `security-scan: clean`.
- macOS `xcrun` printed non-fatal cache-file permission warnings during Git/test/release build steps; every profile command exited successfully.
- Live `task review --json` in a disposable Git project — Antigravity 1.5.10 returned a valid approval for the exact diff; task moved to `verifying`; `verification_ran` was false.
- Live deliberate-defect review — Critic found that `parse_count("")` returned an error instead of `Ok(0)`, reported a blocker, and moved the task to `implementing`.
- Fake Writer fix run — attempt 2 succeeded, linked to review 1, retained the initial base commit, and moved the task to `reviewing`.
- Live negative write probe — Agy called `write_file`; the session policy returned `Permission denied` matching the deny rule; the requested marker was absent.
- One database test covers rejection → linked fix → later approval and verifies review immutability. Another confirms an active Writer for another task blocks Critic review.
- Normal sandbox did not allow Agy's local runtime needs; the successful live Critic and boundary probes ran with host access in disposable directories. No TALOS source files were passed to an external Writer. Probe token usage is not stored in the project database.
- Linux-native build/runtime/sandbox checks remain unverified and deferred to the user's server.

## R5 closure acceptance — 2026-11-18

### Implemented

- Added `src/checks.rs`, `src/policy.rs`, and `src/secrets.rs` for allowlisted project-check execution, bounded capture and hashes, Auto/Semi authorization, and deterministic credential handling.
- Added project config schema v2 with named checks. A task saves an immutable command/version/CWD snapshot using `task add --check` or `task checks`.
- Added `task verify` and `task evidence`. Verification takes the supervisor lease, requires the latest approved review and exact revision identity, records all check results, and completes the task only in the same transaction that closes a fully passing run. Failed, incomplete, changed, or authority-stale runs remain open and are visible in evidence history.
- Advanced SQLite to schema v6. Existing v4 tasks with approval records lacking new Git facts return to `reviewing` and require a fresh review; existing v5 verification rows migrate with unknown historical durations. Migrations retain task/event data.
- Added the `.talos/state.sqlite3` ignore guard before opening TALOS state. Tracked or non-ignored database files fail closed; TALOS does not edit project ignore rules.
- Semi mode requires a TTY and prompts before its one Writer launch. Critic output remains findings-only; an unknown `patch` response field is rejected.
- Updated `README.md`, `docs/plan.md`, `docs/tasks.md`, `docs/architecture.md`, `docs/critic-review-contract.md`, this handoff, and ADR-025. T5.3 is explicitly refined to preserve the findings-only Critic contract.

### Verification results

- `cargo fmt --all && ./scripts/verify-project.sh` — passed all required checks: formatting; Clippy with `-D warnings`; **103 passed / 1 ignored** (the helper is invoked by its signal-forwarding test); macOS ARM64 release build; `security-scan: clean`.
- Tests cover schema-v4/v5 migration, immutable check snapshots, secret rejection, task completion only with all matching evidence, retrieval of run evidence including durations, changed Git facts, changed task authority, and a real allowlisted `cargo --version` check.
- The macOS toolchain printed non-fatal `xcrun` cache-file permission warnings during Git and SQLite build steps; all required commands exited successfully.
- Linux was not connected, installed, built, or tested. Per ADR-023, native Linux checks remain deferred until the user provides server access under T6.5/T9.3.

### Next round

- R5 is complete for the verified macOS path. T6.1 is now complete; continue with **T6.2 — support pause/resume and manual takeover**. Keep the repository local on `master`; the user will create the hosted repository later.

## R6.1 progress — 2026-11-18

- Added `src/tmux.rs` with a read-only tmux version probe, stable project-scoped session naming, duplicate-safe detached session creation, outside attach, and inside-client switching. Added the `talos attach` command and `doctor` tmux reporting.
- A TALOS control session contains the user's shell only. It does not start a worker or create the SQLite runtime; existing foreground commands remain available if tmux is missing.
- Installed tmux is 3.8. Current docs were checked with Context7: [tmux Getting Started](https://github.com/tmux/tmux/wiki/Getting-Started) and the [tmux manual](https://github.com/tmux/tmux/blob/master/tmux.1). TALOS uses detached session creation, exact session targets, and attach/switch behavior; it does not use the `new-session -A` attach path.
- `cargo test --locked --lib tmux::tests::reuses_one_project_session_with_fake_tmux` — passed in the default sandbox and verified the project session is created once.
- `cargo test --locked --lib tmux::tests::creates_only_one_control_session_for_a_project -- --ignored --nocapture` — passed with a separately isolated tmux 3.8 server and `/private/tmp` socket. The test killed its own server. The integration test is ignored by the default suite because the sandbox blocks Unix-socket creation; the fake-client idempotence test remains in the normal suite.
- `cargo fmt --all && ./scripts/verify-project.sh` — passed: format, Clippy (`-D warnings`), **106 passed / 2 ignored**, macOS ARM64 release build, and `security-scan: clean`. One ignored helper is invoked by its signal-forwarding test; the real tmux socket smoke is run separately as above.
- macOS `xcrun` emitted non-fatal cache-file warnings; all required profile commands exited successfully. Next task is T6.2.

### Repository and next action

- Local Git remains on `master`; no remote is configured. The user will create a hosted repository later. Do not add a remote or push.
- Local `.talos/state.sqlite3` has no queued tasks. It remains ignored; an offline backup of the test DB is in `/private/tmp` until temporary probe cleanup.
- Next task: **T6.2 — support pause/resume and manual takeover.** Continue sequentially through R6–R9. Do not connect to a Linux host or run Linux-native checks until the user provides server access.

## R6.2 progress — 2026-11-18

- Added SQLite schema v7 for durable project queue control and an append-only control-event journal. Schema v1, v2, v4, v5, and v6 migration paths preserve task data and initialize an unpaused queue.
- Added `talos pause [--reason ...]`, `talos resume [--reconcile]`, and `talos logs --project`. `status` and `up --observe` report pause state. A pause blocks new Writer attempts and continuations atomically; it preserves queued tasks and does not interrupt an active Writer.
- Resume compares a stable Git revision snapshot. Changed or unavailable snapshots keep the queue paused until explicit `--reconcile`. Reconciliation reopens any `verifying` task to `reviewing`, so the changed project needs a fresh Critic review. Resume refuses while Writer execution remains active or unresolved.
- Tests cover schema migrations, idempotent/persistent pause, task preservation, Writer launch blocking, unchanged resume, changed/missing snapshot handling, approval reopening, unresolved Writer refusal, secret rejection, append-only events, and CLI parsing.
- `cargo fmt --all && ./scripts/verify-project.sh` — passed: format, Clippy with `-D warnings`, **113 passed / 2 ignored**, macOS ARM64 release build, and `security-scan: clean`.
- Release-binary smoke in a disposable Git project passed: pause → unchanged resume; pause → manual tracked-file edit → resume remains paused; explicit `resume --reconcile` resumes and the control journal records detection, reconciliation, and resume.
- macOS `xcrun` emitted non-fatal cache-file permission warnings; required checks exited successfully. No Linux host was contacted and no Linux-native check was run.
- T6.2 is committed locally as `6370893` (`feat: add durable project pause and reconciliation`) on `master`. Continue with T6.3.

## R6.3 progress — 2026-11-18

- Added schema v8 durable `stop_requested_attempt_id` state and append-only events for request, completion, closure, and recovery interruption.
- Added `talos stop`. It records a request for the active Writer and waits up to 10 seconds for an observed terminal attempt. The Writer supervisor polls the local SQLite control row every 100 ms and terminates its group when requested. A timeout is reported as unconfirmed; operator inspection and `task recover` remain necessary after TALOS loss.
- Added shared process-group cleanup for Writer runs and project checks. It sends SIGTERM, waits 750 ms, sends SIGKILL, and reaps the leader. The armed guard sends SIGKILL if cleanup is cancelled. After `Child::wait` succeeds, TALOS disarms and never signals the old numeric PGID. This prevents PGID reuse signals but means descendants left after a normal CLI exit are not cleaned up.
- Added best-effort early supervisor-lease reclaim: only a parseable `talos-task-*` owner PID that returns ESRCH can be treated as gone; an alive, reused, inaccessible, or unknown PID fails closed. Recovery advances the fencing token and still quarantines unfinished attempts as `unknown`.
- Updated README, plan, backlog, architecture, and ADR-027 with command behavior, failure boundaries, and the normal-exit descendant limitation.
- `cargo fmt --all` and `cargo check --all-targets --all-features` passed on macOS. The newly added behavior tests and full verification profile have not been run in this batch; mark T6.3 in progress until those acceptance checks are completed. No Linux host was contacted.
- R6.3 is committed locally as `173d736`; the repository remains on `master` with no remote. Linux-native checks remain deferred until the user provides server access.

## R6.4–R6.5 progress — 2026-11-18

- T6.4 uses the existing project-scoped tmux shell and durable project database. The required macOS SSH disconnect/reconnect scenario has not been run; Linux boot recovery/systemd autostart remains deferred.
- Advanced SQLite to schema v9 with an optional immutable-at-launch process fingerprint. On Linux, capture uses `/proc/<pid>/stat` fields for process group and start ticks plus `/proc/sys/kernel/random/boot_id`; on other platforms capture returns unavailable.
- Writer launch stores the fingerprint when capture succeeds. `task recover` compares saved and current values and reports `matches`, `different_process`, `unavailable`, or `not_recorded`. Recovery events retain the recorded value. This remains lifecycle evidence only and does not change quarantine, stop, resolution, or retry rules.
- Updated the README, architecture, plan, backlog, and this handoff. The v1–v8 SQLite migrations remain in sequence; schema-v9 migration coverage was added but not executed.
- `cargo fmt --all && cargo check --all-targets --all-features` passed on macOS after the T6.5 edits. Behavior tests and the schema migration tests were not executed. No Linux host was contacted. R6.4/T6.5 are not complete until the relevant reconnect/server acceptance is observed.
- T6.3 code and docs are committed locally as `173d736`; T6.5 code and docs are committed as `098f7b1`. Branch remains `master`, with no remote.

## R7.1 progress — 2026-11-18

- Advanced SQLite to schema v10 with append-only task dependency edges and an index for prerequisite lookup.
- Added `task depends <TASK> --on <PREREQUISITE>` and `task dependencies <TASK>`. Dependencies are accepted only for queued tasks; self-edges and cycles are refused, and duplicate additions are idempotent.
- Added a shared runnable-task gate: explicit Writer starts check all prerequisites inside the creation transaction, while `up --auto` selects only queued tasks whose prerequisites are `complete`. Auto reports blocked queued tasks and their prerequisites instead of claiming the queue is empty.
- Added schema-v9-to-v10 migration coverage. `cargo fmt --all && cargo check --all-targets --all-features` and `git diff --check` passed on macOS. Migration, DAG cycle, eligibility, and command behavior tests have not been run. This R7.1 implementation is not complete until those checks pass.
- R6.3, R6.5, and R7.1 code are committed locally as `173d736`, `098f7b1`, and `da33ccf`. The repository remains local on `master` with no remote; no Linux host was contacted.

## R7.2 progress — 2026-11-18

- Advanced SQLite to schema v11 with immutable child-task contracts. Each contract stores its parent ID, normalized relative path allowlist, timeout, and creation time. Creating a child inserts its task, contract, and parent-waits-for-child edge atomically.
- Added `task child`, `task children`, and `task child-report`. The report is versioned as `talos-child-task-report-v1` and is derived from existing task, Writer attempt, Critic, and verification rows; no duplicate report or full transcript is stored.
- Writer prompts include allowed paths and timeout; the saved timeout overrides the general Writer timeout. Critic review receives the scope, and TALOS adds a deterministic blocker if any changed path is outside the contract. The scope is a review gate, not an OS security boundary.
- Added v10→v11 migration coverage. `cargo fmt --all && cargo check --all-targets --all-features` passed on macOS. Migration, child creation, timeout, report, and scope-enforcement tests have not run; T7.2 remains in progress.
- Local commit: `1058870`. Repository remains local on `master`; no remote or Linux host was contacted.

## R7.3 progress — 2026-11-18

- Added `native_subagents` to Agy capability discovery and `talos doctor` output. TALOS reports it as advertised only if `agy --help` exposes the public `invoke_subagent` tool; otherwise it stays `not_tested` and native delegation stays disabled. A missing or unknown native capability does not disable manual child tasks.
- Current `agy --version` reports `1.5.10`; local `agy --help` does not expose `invoke_subagent`. The `agy agents` listing could not be inspected: this sandbox blocked the CLI's attempt to create files under `~/.gemini` and bind its local language-server port. No Agy settings or project data were changed.
- Commit: `9bbd2d7`. `cargo fmt --all && cargo check --all-targets --all-features && git diff --check` passed on macOS. Capability parser tests are recorded but not run; T7.3 remains in progress pending a harmless runtime probe. No Linux host was contacted.

## R7.4 progress — 2026-11-18

- Added schema-v12 task worktree records and append-only lifecycle events. Explicit `task worktree create/list/reconcile/remove` commands create a linked branch from a clean base, track interrupted creation, and preserve the branch after worktree removal.
- Worktrees use a private sibling `.talos-worktrees/<project-hash>` directory so Git metadata and `.talos` state stay outside the checked-out source. Removal refuses any tracked changes, untracked files, or ignored files; there is no force option or automatic deletion.
- Writer/Critic/verification still operate on the primary project root. T7.5 must separate the agent execution root from the shared project state/config root before worktree execution is enabled.
- `cargo fmt --all && cargo check --all-targets --all-features && git diff --check` passed on macOS. T7.4 implementation is uncommitted; lifecycle behavior is unverified. No worktree was created, no Linux host was contacted, and no tests were run.

## Next action

Proceed to T7.5 bounded worktree integration. T7.1–T7.4 still need behavior/migration checks, and T7.3 needs a harmless native-delegation probe. R6 acceptance gates also remain open: T6.3 behavior/full-profile checks, T6.4 macOS SSH reconnect, T6.5 Linux server acceptance, and T6.6's TUI trust/handoff scenario. No Linux connection or trust-setting change has been made. Continue R7–R9 implementation locally while keeping these checks unclaimed. Keep all work on `master`; the user will create the hosted repository later.

## Session close checklist

Before ending a future session, update this handoff and `docs/tasks.md` with the current task IDs, exact file changes, verification commands/results, verified limitations, decisions, and one next action. All project artifacts remain English.
