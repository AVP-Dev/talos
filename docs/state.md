# TALOS Handoff — Current State

> Canonical checkpoint for every future session and agent. Read this file with `docs/plan.md`, `docs/tasks.md`, and `docs/decisions.md`. The chat transcript is not required to continue.

## Current phase

**Current work is the R9.5 current-source ARM64 artifact build; R6–R9 remain open for acceptance.** R6.1–R6.2 are implemented; T6.3 behavior/profile verification, T6.4 macOS SSH reconnect, T6.5 Linux server acceptance, and T6.6 live interactive handoff remain open. R7 implementation now includes schema-v18 Writer execution slots; migration, behavior, and three-agent acceptance remain open. R8.1–R8.3 are implemented locally, but command/migration acceptance and repository-specific remote behavior remain pending. R9.1 has a 15-scenario evidence inventory; T9.2 adds bounded-memory paths, but migration, execution, and measurements remain pending. T9.3 install lifecycle and T9.5 artifact acceptance remain open. A package build from `49eb3ed` succeeded and created an ARM64 artifact, but reported a release-only dead-code warning; the wrapper is now marked `cfg(test)` and the quiet rebuild is pending. No tests were run in this batch. No GitHub authentication or remote operation has been performed. Linux access and Linux-native checks remain deferred to the user's server. Backlog counts remain 42/61 scheduled tasks done, 16 in progress, 0 planned, and 3 deferred; two unscheduled ideas remain recorded separately.

**Next action:** commit the `cfg(test)` warning fix and update this handoff, then rerun `scripts/package-macos.sh` from the clean commit. The independent R6–R9 audit's package-script finding was fixed in `9b79fe7`; it reported no other local code-only gaps. Keep behavior/migration acceptance, macOS SSH/TUI, GitHub repository flows, and Linux server checks open until their required environment is available. No remote is configured. The release build succeeded at `49eb3ed`, with one dead-code warning that is now fixed in the working tree; no tests were run.

## Goal and fixed direction

TALOS is a Rust local CLI orchestrator for installed AI coding CLIs. The target workflow is a durable task queue, a default of one Writer with opt-in bounded concurrency in separate managed worktrees, an independent read-only Critic, real checks, safe recovery, and an optional policy-gated draft GitHub PR with commit-bound CI evidence; merge remains manual.

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

R3 introduced schema v2 for durable `writer_attempts` and schema v3 for separate resolution metadata and continuation source links. R4 advances the schema to v4 with Writer base revisions, linked Critic revisions, and immutable Critic review records. Full prompts, patches, and transcripts are not persisted. Initially the Writer held the singleton supervisor lease for its full run; the R7 schema-v18 design replaces that with a durable per-attempt execution slot while keeping ordinary operations exclusive against active Writers. The Writer checks Agy 1.5.10, creates a session-only policy file, persists the PID before sending the prompt, records a public conversation ID as soon as the `init` event arrives, and moves success only to `reviewing`.

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
- Initial early-reclaim support recognized only `talos-task-*` owner IDs; a dead owner PID could be reclaimed after ESRCH, while alive, reused, inaccessible, or unknown owners failed closed.
- R6 audit found that worktree lifecycle operations use `talos-worktree-*` lease owner IDs, which the early-reclaim parser did not recognize. Commit `5f75c7e` extends parsing to the internal task/worktree formats with non-empty operation segments and positive numeric PID and timestamp suffixes; liveness checks remain fail-closed. `cargo fmt --all` passed; no tests or build were run.
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
- `cargo fmt --all && cargo check --all-targets --all-features && git diff --check` passed on macOS. Lifecycle behavior is unverified. No worktree was created, no Linux host was contacted, and no tests were run.
- T7.4 was committed locally as `2b672ea` (`feat: manage task worktree lifecycle`) on `master`.

## R7.5 progress — 2026-11-18

- Added `Project.state_root` so task execution can use a linked worktree while SQLite and `.talos/config.json` remain in the primary project. The runtime state is still one local database per primary project.
- Advanced SQLite to schema v14. Schema v13 stores durable task-worktree integration identity/state and append-only task events. Schema v14 records the first Writer-routing timestamp, so worktrees with preexisting Writer history keep the primary-tree flow after migration. Integration intent is recorded before Git applies the patch; `reconcile` checks an interrupted operation and only reapplies the exact source patch when the primary tree is still clean at the saved base.
- Active managed worktrees route Writer attempts, Critic review, and checks to their proper execution root. `task worktree integrate` requires a clean primary base, checks child scope, applies/stages the exact patch, and leaves review to the Critic on the primary tree. A post-integration Writer fix runs in the primary tree.
- A passing post-integration verification leaves the task `verifying`. The user commits the primary changes manually; `task worktree finalize` verifies the current committed revision against the latest Critic approval and passing checks, then marks the task complete. Dependency scheduling requires integrated completion for prerequisites with a managed worktree.
- The existing supervisor lease serializes TALOS Writer, Critic, verification, and all mutating worktree lifecycle operations. Review/verification are refused before integration. An unused queued worktree may be removed, after which the task runs in the primary project. Native Agy subagent delegation remains disabled because public invocation and agent roster have not been verified; three-agent acceptance remains open.
- `cargo fmt --all && cargo check --all-targets --all-features && git diff --check` passed on macOS for T7.5. A later Clippy pass found four style warnings in the T7.5 implementation; those were fixed in the T8.1 batch and Clippy now passes with `-D warnings`. No tests or Linux-native checks were run. T7.1–T7.5 migration/behavior acceptance remains pending. No task worktree was created and no primary project files were integrated during implementation.
- T7.5 is committed locally as `7f7fece` (`feat: route and integrate task worktrees`) on `master`; no remote is configured.

## R7.5 follow-up — bounded Writer slots — 2026-11-18

- Added project config schema v4 with `max_concurrent_writers` defaulting to 1 and a hard range of 1–3. Existing config versions retain the sequential default.
- Advanced SQLite to schema v18 with durable per-attempt Writer execution slots. Attempt/continuation creation, first managed-worktree routing, and slot insertion are atomic. Finished and quarantined attempts release their slot in the same transaction.
- SQLite admits parallel Writers only for different active managed worktrees. Primary-tree Writers remain exclusive. Ordinary supervisor operations are serialized against active Writers. `talos stop` retains one requested attempt at a time; repeating it can stop another concurrent Writer.
- Recovery uses a fenced recovery lease, checks every recorded TALOS owner PID, and refuses if an owner is alive or cannot be checked. The v17→v18 migration creates slots for active attempts; it copies an owner PID only when exactly one attempt is active and the singleton owner ID parses as an internal TALOS operation. Other migrated slots have an unknown owner and remain fail-closed.
- Removed the old separate worktree-route mutation so the first route marker cannot be committed without the corresponding Writer attempt. An independent audit found a race between choosing an execution root and the DB claim; the claim now compares that expected root against the current route in the same transaction. Updated the existing recovery fixture for the new slot and owner-liveness rules.
- Updated ADR-042, README, architecture, plan, and task backlog. This R7 slice is still in progress: no tests, build, Clippy, or Linux checks were run. `cargo fmt --all` passed. Native Agy delegation is still disabled, and a separate Guild subsystem is not part of the plan.
- Recovery currently treats PID liveness conservatively; PID reuse may delay recovery, but it cannot free an active slot. Linux-native identity/runtime behavior and end-to-end three-agent acceptance remain deferred.
- Committed locally as `837b82c` (`feat: add bounded parallel Writer slots`) on `master`. The independent root-binding review is complete; schema migration and behavior acceptance remain open.

## R9.2 follow-up — bounded package-script clean checks — 2026-11-18

- The independent R6–R9 source audit found that both package scripts captured all `git status` output in shell memory before rejecting a dirty checkout.
- Replaced the full capture with a FIFO-backed Git status process. The scripts read one byte, stop Git immediately when the tree is dirty, and wait for Git's exit status when the output is empty. The first process-substitution version could not be waited on by macOS Bash and stopped before compilation; the FIFO version uses a normal background child. No new dependency was added.
- Updated the R9.2 backlog and clarified that the existing ARM64 package is historical and must be rebuilt for the current source.
- No tests or shell syntax checks were run after this edit. The first macOS packaging attempt exited before compilation due to the process-substitution wait issue; a current artifact build is the next step after committing the FIFO fix. Linux packaging remains deferred to the user's server.

## R9.5 follow-up — current macOS artifact build — 2026-11-18

- After `9b79fe7`, `scripts/package-macos.sh` passed its clean-check and started the locked ARM64 release build. Compilation reported E0507 at `src/app.rs:546`: the evidence display loop tried to move a check vector through a shared reference.
- Changed the loop to borrow each check row. The correction has not yet been committed; rerun the package build only after committing so its source commit and clean-tree manifest are accurate.
- A retry from clean commit `49eb3ed` succeeded and created `dist/talos-v0.1.0-aarch64-apple-darwin-49eb3ed383e8/`. Cargo reported a non-fatal dead-code warning for the helper used only by tests, plus the known `xcrun` cache warnings. The helper now has `#[cfg(test)]`; the artifact should be rebuilt from the corrected commit.
- The failed first build stopped before writing an artifact. The previous artifact at source commit `045352a` remains historical. No automated tests were run. The generated checksum manifest has not been independently checked, and the binary has not been smoke-tested.

## R8.1 progress — GitHub CLI capability diagnostics

- Added `doctor` reporting for local `gh` availability/version. Added `talos github status`; it stays local by default. `--check-auth` explicitly runs `gh auth status --active --json hosts`, bounds execution to 15 seconds and captured output to 32 KiB, discards raw stdout/stderr, and reports only an aggregate authentication state. It does not invoke `gh auth token` or inspect credential environment variables.
- Checked current official GitHub CLI documentation with Context7. `gh auth status --json hosts` can include account names, token source, and expiration metadata, so TALOS only deserializes each entry's state and never prints raw output. During an explicit check, `gh` may use configured credentials internally; TALOS never requests, parses, stores, or exposes token values. The active auth command has not been run during implementation; no GitHub request, repository, remote, push, or PR was made.
- Updated README, roadmap, task backlog, decisions, and this handoff. A read-only review found a missing-field parsing ambiguity and documentation overstatement; both were corrected. R8.1 remains in progress pending local CLI behavior acceptance. Linux checks and all automated tests remain deferred; `cargo fmt --all`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo check --all-targets --all-features`, and `git diff --check` passed on macOS.
- T8.1 implementation is committed locally as `0a8c9e7` (`feat: add safe GitHub CLI diagnostics`) on `master`. No remote is configured.

## R8.2 progress — publication policy and local feature branches

- Extended project config schema to v3 with an optional GitHub policy pinned to `owner/repository`, a named Git remote, the user-required `master` base, and a feature prefix rooted at `talos/`. Push and PR flags default to false; config rejects unknown fields and credential-like policy fields.
- Added local `talos github policy` and `talos github branch create <slug>`. Branch creation requires the configured policy, a clean Git checkout currently on `master`, a valid unique feature ref, and uses `git switch --create` from `master`. It does not access GitHub or push.
- Added `talos github publish` and `talos github reconcile`. Publication requires both policy permissions and explicit `--confirm-push` / `--confirm-pr`, verifies fetch and push URLs against the configured GitHub repository, verifies local/remote `master`, and records the exact head commit and diff digest before pushing only `refs/heads/<named-feature-branch>`. It requests a draft PR and does not merge.
- Schema v15 stores immutable publication identity, state, PR URL, and append-only transition events. Push/PR outcomes that are not confirmed become unknown and require reconciliation; TALOS will not repeat an uncertain push or create a second PR without checking the recorded branch/PR first. Conflicts stop in `needs_attention`.
- A read-only code review caught two edge cases before continuing: Git's `push.followTags` configuration could add annotated tags, and case differences in the configured repository name could prevent recognition of a successful PR. The push now explicitly disables tag following; PR URL parsing compares repository identity case-insensitively while requiring the exact numeric PR path.
- Reviewed the state transitions and consolidated CLI arguments to satisfy Clippy. Updated README and plan/task/decision/handoff docs. No repository or remote exists in the workspace, so no auth, network read, push, or PR operation has been invoked.
- Current macOS checks after the review fixes: `cargo fmt --all`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo check --all-targets --all-features`, and `git diff --check` passed. Automated tests and Linux-native checks were not run.
- The initial T8.2 implementation is committed as `efaaa75` (`feat: add policy-gated GitHub publishing`). Read-only review identified tag following and case-sensitive PR URL matching; both were fixed and committed as `8728ce4` (`fix: restrict GitHub publication side effects`).

## R8.3 progress — commit-bound CI snapshots

- Added `talos github ci <publication_id>` for an already open recorded PR. Before and after reading CI status, TALOS checks that the remote feature ref and the single matching open PR still use the immutable recorded commit and branch/base identity. If the commit moves during polling, CI rows are discarded and the saved outcome is `unknown`.
- Added optional `--wait` polling with a 1–3600-second polling window and 5–120-second interval. Each `gh pr checks --json bucket,completedAt,name,startedAt,state,workflow` request is capped at the smaller of 60 seconds and the remaining polling window, and output is capped at 32 KiB. Exit code 8 is accepted as a pending result; other command errors and malformed output produce an `unknown` snapshot.
- The polling window bounds only repeated CI status queries. Pre/post branch and PR identity checks are separate bounded calls and may add up to roughly four minutes in a slow network case.
- Schema v16 stores compact, append-only CI snapshots bound to the open publication's exact head commit. Identical summaries are deduplicated. Snapshot records include only check name, workflow, bucket, state, and timestamps. Raw output, descriptions, links, and credential metadata are not stored. `pass` requires at least one passing check and no failed/cancelled/pending checks; no-checks, timeout, pending, and unknown remain incomplete. CI evidence does not change task status.
- ADR-036 records schema invariants and failure handling before the migration. Official GitHub CLI `gh pr checks` docs were fetched with Context7 and specify the JSON fields, buckets, and pending exit code. No repository/auth/network operation or test was run.
- `cargo fmt --all`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo check --all-targets --all-features`, and `git diff --check` passed on macOS after the schema-v16 and CLI changes. SQLite migration behavior is not yet runtime-verified.

## R8.4 closure — manual merge and branch cleanup

- `talos github publish` creates draft pull requests only. The CLI has no merge or remote branch deletion command, and no automatic merge path exists.
- `talos github policy` now reports `manual_only` merge and `unsupported` remote branch deletion in human and JSON output, even when publication policy is absent.
- No schema or network operation is required. T8.4 implementation is complete; T8.1–T8.3 live behavior remains pending until the user creates a repository.

## R9.1 progress — end-to-end scenario inventory

- Added `docs/acceptance-scenarios.md` with 15 separate scenarios from the original request. Each row distinguishes recorded historical evidence, partial coverage, unverified work, and infrastructure deferred to the Linux server or hosted GitHub repository.
- The inventory does not mark prior results as current-schema acceptance. Rerun the applicable scenarios against the release candidate and record exact commands/platform/evidence. No scenario was run while creating the inventory.
- R9.1 remains in progress; Linux boot, SSH reconnect, native delegation/parallel acceptance, CI, and final closeout criteria remain open.

## R9.2 progress — hardening scenario plan

- Extended `docs/verification.md` with separate database growth, migration-fault, process-limit, worktree-recovery, GitHub-recovery, CI-storage, backup/restore, and reproducible-build scenarios.
- No database growth threshold or compaction/retention policy has been selected. Record actual size/memory measurements; keep archival and pruning decisions under F1 rather than claiming bounded long-term database size.
- Changed Auto/Semi scheduling to select the first eligible queued task in SQLite instead of loading every task and checking dependencies one by one. Schema v17 adds a non-authoritative `(status, id)` index; its v16→v17 migration has not been runtime-verified. Task, dependency, child-contract, and worktree listings use bounded cursor pages; `talos logs` and `talos task evidence` are paginated newest-first without deleting records. Auto/Semi blocked-queue output contains the total waiting count plus a bounded task/prerequisite preview. Git status now streams path records; Writer output and dirty-tree errors keep at most 50 path previews, with each displayed path prefix capped at 256 bytes. Resume reconciliation uses set-based task updates and per-task `INSERT ... SELECT` audit events, keeping only the first 50 task IDs in memory while preserving the full count; ADR-040 records the timestamp, atomicity, and failure invariants. Recovery selects only unresolved Writer attempts. Task-ID lookups, stop polling, Writer review, child-task reports, worktree routing/removal/finalization, and explicit attempt resolution use primary-key or latest-row queries instead of materializing full task or history lists. The large-history measurement remains pending; no retention or compaction policy is implied.
- Critic revision capture now streams Git output instead of buffering a full diff/path list before checking limits. It caps the complete patch at 64 KiB, each path-list response at 128 KiB, changed-path JSON at 256 KiB, paths at 4096 entries, and each path at 1024 bytes; over-limit Git children are killed/reaped where possible. Dirty-tree checks, managed-worktree removal, and the tracked `.talos/` guard read only the first output byte before refusing. GitHub remote discovery caps output at 64 KiB, 64 URLs, and 2 KiB per URL. Agy `--version`/`--help` probing now drains stdout/stderr concurrently with a 64 KiB per-stream cap and a 15-second timeout. ADR-041 records the whole-revision-or-reject invariant. Boundary, child-cleanup, and no-partial-evidence acceptance remain pending; no tests or checks were run for this slice.
- `task evidence` now defaults to 5 runs, caps pages at 10, and reports changed-path count plus at most 10 bounded previews per run in JSON. The complete path set remains stored in SQLite for revision verification; large pages no longer echo every saved path into CLI/agent context.
- `cargo fmt --all` passed for the evidence-output change. No tests, build, or Linux checks were run.
- This is a test plan only. No long-running, fault-injection, or database-growth tests were run.

## R9.4 closure — documentation alignment

- Reviewed README, roadmap/backlog/handoff, architecture, project structure, decisions, operator and agent-routing guides, Critic contract, and acceptance/verification documents against current commands and limits.
- Corrected stale Cargo install-root guidance, `.talos/` ignore behavior, implemented-feature wording, and Linux scope. README now links the operator, architecture, handoff, onboarding, and research documents.
- TALOS project artifacts remain English. GitHub remote actions and Linux acceptance remain explicitly pending.

## R9.3 progress — install and native package preparation

- Added a runbook with Cargo install, update, and uninstall commands. These operate on the binary installation root and preserve project `.talos/` databases. It also documents backups, recovery, worktrees, GitHub workflow, and current platform limits.
- Added `scripts/package-macos.sh` for ARM64 by default and optional Intel builds. The script requires a clean committed checkout, builds with `--locked`, records version/target/commit/source date/Rust compiler/Cargo.lock digest, copies README, and writes SHA-256 checksums to ignored `dist/` output.
- Added `scripts/package-linux.sh` for native `x86_64-unknown-linux-gnu` and `aarch64-unknown-linux-gnu` hosts. It requires a clean committed checkout and writes a binary, README, build manifest, and checksums to ignored `dist/`. `bash -n` passed on macOS; the script has not been run, and no Linux build or host connection occurred.
- Local commit: `a58813a` (`feat: add native Linux package script`). This commit adds packaging code and documentation only; it does not establish Linux build/runtime support.
- Generated the ARM64 artifact at `dist/talos-v0.1.0-aarch64-apple-darwin-9fafb4c9d3fa/`. The executable is 4.6 MiB; Cargo's release build succeeded with the known non-fatal macOS `xcrun` cache warnings. All three entries in `SHA256SUMS` passed `shasum -a 256 -c` from the artifact directory.
- The artifact is unsigned and not notarized; the runbook labels it a local build output, not a publish-ready macOS distribution.
- Linux package generation remains after T6.5 native server acceptance; no Linux system was contacted or packaged.

## R9.5 progress — macOS artifact preparation

- Set `rust-version = "1.94"` in `Cargo.toml`; the installed compiler is Rust 1.94.0. An exact `rust-toolchain.toml` pin was removed because rustup tried to sync into the sandbox-protected `~/.rustup` directory. Cargo works with the existing installed toolchain.
- The macOS package script records the inputs needed to compare builds. One historical ARM64 artifact from `045352a` exists, but it predates later source commits and is not the current release candidate. Byte-for-byte reproducibility has not been checked. The binary was not run, and Cargo install/update/uninstall behavior was not exercised. Linux artifacts wait for T6.5/T9.3 acceptance.
- Script syntax, formatting, Clippy, `cargo check`, and `git diff --check` passed. Automated tests and Linux-native checks were not run.
- The first packaging attempt after T6.6 exited with status 141 before compilation because `awk` stopped reading `rustc -vV` under `pipefail`; the committed fix and successful rerun are recorded below.
- Fixed the host-extraction pipeline and committed it locally as `fb2b4d3` (`fix: handle rustc host output under pipefail`). The package script completed from that commit and created `dist/talos-v0.1.0-aarch64-apple-darwin-fb2b4d323c2f/` (4.6 MiB binary). After the worktree-safety refinement in `045352a`, the package was rebuilt at `dist/talos-v0.1.0-aarch64-apple-darwin-045352a22a52/`; `shasum -a 256 -c SHA256SUMS` passed for `talos`, `README.md`, and `BUILD-INFO.txt`. The current binary was not run; the artifact is unsigned and not notarized.

## R6.6 progress — compact task handoff

- Added `talos task handoff <ID>` as a read-only, generated `talos-task-handoff-v1` packet. It includes bounded task text, the latest Writer attempt, latest Critic summary, latest verification summary, up to 32 prerequisite rows, and the current managed-worktree integration state.
- Added targeted database queries for one task, latest Writer attempt, latest verification summary, and a capped prerequisite summary. Handoff generation no longer loads the entire task list or full attempt/verification histories. Text values are redacted and capped at 4 KiB each. Prompts, patches, transcripts, raw check output, and the current Git diff are omitted; no report is persisted.
- The packet recommends an explicit next command based on saved state. It does not launch a process, authorize an operation, or prove continuity of the interactive CLI conversation. The live TUI→headless probe remains pending because the workspace-trust decision has not been made.
- Updated README, runbook, architecture, plan, task backlog, project structure, agent-routing guide, and ADR-038. `cargo fmt --all` and `cargo check --locked --all-features --lib --bin talos` passed on macOS. No tests were added or run, and no Linux host was contacted.
- Local implementation/documentation commit: `8bd5b49` (`feat: add compact task handoff packet`).
- Tightened next-action guidance for interrupted `applying` and `needs_attention` worktree integrations so the packet directs the operator to inspect/reconcile before review or verification. `cargo fmt --all` and the production-target `cargo check` passed again; behavior tests remain unrun.
- Follow-up commit: `045352a` (`fix: handle interrupted worktree handoff state`).

## Next action

The user directed us to continue R6–R9 in order, skip Linux access and Linux checks until their server is available, and proceed code-first. R9.2 includes SQL-side queue selection, bounded pages for tasks and relations, paged event/evidence history, capped blocked-queue and resume previews, streaming Git status and Critic revision capture, early-refusal checks for dirty trees and tracked `.talos/` files, bounded path metadata, bounded GitHub remote discovery, bounded Agy capability probes, unresolved-only recovery reads, targeted latest-row lookups, and one-byte dirty-tree checks in both package scripts. The independent R6–R9 source audit found and fixed the package-script output capture issue; its first implementation could not be waited on by macOS Bash, so it is now FIFO-backed in `9b79fe7`. The audit reported no other local code-only gaps. R7 slot/root-binding changes are committed as `837b82c`; migration and behavior acceptance remain pending. The first release compile found and fixed E0507 in the task evidence display loop. A subsequent package build succeeded from `49eb3ed`, but emitted a test-helper dead-code warning; `#[cfg(test)]` is now applied and a quiet rebuild is pending. R6–R9 still need evidence for macOS install/upgrade/uninstall, final-schema behavior/migration and fault scenarios, reproducibility and binary smoke, SSH/TUI continuity, plus Linux server and GitHub repository flows when available. Do not mark these gates complete without evidence. Next exact action: commit the dead-code warning fix and handoff update on `master`, then rerun the macOS packaging build. When the Linux server is available, run Linux build/runtime acceptance and record exact outcomes. GitHub operations remain local/offline until the user creates/provides a repository and authorizes remote work. The Linux package script is in local commit `a58813a`. A historical ARM64 artifact built from source commit `045352a` is under `dist/talos-v0.1.0-aarch64-apple-darwin-045352a22a52/`; an intermediate artifact from `49eb3ed` also exists but has the warning. T6.6 base implementation is `8bd5b49`; T8.2 commits are `efaaa75` and `8728ce4`; T8.3 is `877ae10`; T8.4 is `6c6ba78`; earlier artifact prep is `9fafb4c`/`ea73675`. The latest local scale commits include `4de7534`, `b90ad63`, `614cd07`, `1b3f6e6`, `d5b5867`, `c2ed034`, `225122c`, `cfe6c26`, `16ba9ee`, `0e89d5f`, `4c2a679`, and `23dee89`. The release build succeeded from `49eb3ed` with warnings; no tests were run. `cargo fmt --all` ran before the package-script edit. Keep all work local on `master`.

## Session close checklist

Before ending a future session, update this handoff and `docs/tasks.md` with the current task IDs, exact file changes, verification commands/results, verified limitations, decisions, and one next action. All project artifacts remain English.
