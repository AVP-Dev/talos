# Architecture decisions

## ADR-001 — Local modular monolith in Rust

- **Status:** accepted.
- **Decision:** ship one local Rust executable; use Tokio for process work and Git/tmux CLIs for their public interfaces.
- **Reason:** the product must be easy to install on macOS/Linux and coordinate local processes without a separate service or runtime.
- **Consequence:** the core owns durable state and lifecycle; agent CLIs remain external replaceable executables.

## ADR-002 — SQLite WAL is the source of task state

- **Status:** accepted.
- **Decision:** keep the project database in ignored `.talos/state.sqlite3`; enable WAL and transactions.
- **Reason:** task ownership and recovery need durable atomic writes with no background database service.
- **Consequence:** keep synchronous `rusqlite` calls off Tokio's async executor and use a schema version from the first database.

## ADR-003 — Do not transplant pair-mailbox MCP architecture

- **Status:** accepted.
- **Decision:** port its proven invariants into native Rust state transitions and CLI process supervision, but do not port MCP room state, mailbox tools, composer typing, or compatibility migrations.
- **Reason:** TALOS owns both CLI lifecycles and can communicate through each CLI's public process interface directly.
- **Consequence:** each adapter must expose its limits honestly; there is no generic assumption that one CLI can wake another.

## ADR-004 — Runtime-independent from Agent Starter Kit

- **Status:** accepted.
- **Decision:** use Starter Kit documentation/scripts only for engineering workflow. The binary has no dependency on those files or on user-wide agent configuration.
- **Reason:** users should install and run TALOS as an independent product.
- **Consequence:** the project carries adapted development rules, but product install docs will not require Starter Kit.

## ADR-005 — No inferred session continuity

- **Status:** accepted.
- **Decision:** persist public session identifiers only; enable resume after testing the exact CLI sequence.
- **Reason:** pair-mailbox recovery history shows that a live TUI and a restarted headless process are distinct lifecycle states, and successful process restart does not prove context recovery.
- **Consequence:** fall back to durable task checkpoints only when the CLI's own session cannot be resumed; mark that as a new session.

## ADR-006 — Use pair-mailbox as a source of lessons, not a code dependency

- **Status:** accepted by the user.
- **Decision:** use the years of pair-mailbox experience as a comparison matrix for TALOS. Transfer proven invariants and lessons; reimplement the product architecture for a Rust local orchestrator.
- **Reason:** the user asked to start fresh because the existing branch evolved toward a different application vector, while retaining its hard-earned engineering knowledge.
- **Consequence:** pair-mailbox remains read-only; the full transfer/decline matrix is `docs/research/pair-mailbox-assessment.md`.

## ADR-007 — Plan before the first product implementation

- **Status:** accepted by the user.
- **Decision:** establish directions, rounds, dependencies, task list, and acceptance criteria before writing product Rust code.
- **Reason:** this is a large project and should be implemented step by step with user visibility at round boundaries.
- **Consequence:** the user approved implementation after the plan was established. R1 research and R2 core are complete; continue one round at a time and keep the durable plan/backlog current.

## ADR-008 — Development rules are guidance with explicit safety boundaries

- **Status:** accepted by the user.
- **Decision:** adapt Starter Kit as practical recommendations. Keep only security, data preservation, repository boundaries, and explicit approval limits as non-negotiable rules.
- **Reason:** the kit should help agents develop consistently without turning every project convention into a hard gate.
- **Consequence:** `AGENTS.md` distinguishes hard boundaries from recommendations and requires a file-based handoff for new sessions.

## ADR-009 — Project files are the cross-session source of truth

- **Status:** accepted by the user.
- **Decision:** maintain plan, tasks, decisions, and current state in project files that every agent can read.
- **Reason:** work will span multiple sessions and tools; a chat transcript or private CLI history cannot be required for continuity.
- **Consequence:** update `docs/state.md` and affected task statuses at every completed work batch and before session end.

## ADR-010 — English is the project language

- **Status:** accepted by the user.
- **Decision:** every TALOS artifact and product message is in English, including source comments, prompts, CLI output, logs, reports, and documentation.
- **Reason:** contributors and agents may change across sessions and tools; one shared project language keeps the system and its operating context consistent.
- **Consequence:** keep all project files in English even when human conversation is in another language. Verify the repository for non-English text before closing a documentation round.

## ADR-011 — Antigravity is the first verified R3 Writer adapter

- **Status:** accepted and implemented for the initial macOS R3 Writer path; only the tested `agy 1.5.10` version is write-verified.
- **Decision:** start the R3 adapter around Antigravity CLI (`agy`) for headless operation and public session resume.
- **Evidence:** installed `agy 1.5.10` returned structured JSON in plan/sandbox mode and resumed the same conversation by public ID in a separate process.
- **Limits:** the interactive TUI stopped at a project-trust prompt that was declined. `task run` has now completed a live one-task flow through TALOS in a disposable Git project. Process interruption and cleanup remain tested with fake CLIs; Antigravity-specific cancellation and TUI-to-headless continuity remain unverified. The live write/read boundary evidence and host sandbox limitation are in `docs/research/cli-spike.md`.
- **Consequence:** R3 implements the explicit and one-shot Writer paths only for the tested version. The `up --auto` recovery/end-to-end gates passed for macOS; `up --semi` remains R5 work. TUI handoff remains a later acceptance item. Codex is a comparison/fallback candidate, but its live call was blocked before inference in this environment.

## ADR-012 — R2 remains observe-only

- **Status:** accepted for the R2 implementation; superseded for R3 by ADR-014.
- **Decision:** the R2 CLI may persist and report task state, but `up --observe` does not launch agents or acquire a worker lease. `up --auto` and `up --semi` return an explicit unavailable error.
- **Reason:** Antigravity's safe unattended write behavior and process lifecycle have not passed R3 checks.
- **Consequence:** no model or external coding CLI could modify a project through the R2 binary.

## ADR-013 — Protect local runtime state

- **Status:** accepted.
- **Decision:** reject symlinked `.talos` and database paths. On Unix, require current-user ownership and enforce `0700` on the runtime directory and `0600` on the SQLite file, including existing paths.
- **Reason:** task descriptions and future prompts may contain private project information.
- **Consequence:** startup fails with a clear error when an existing runtime path cannot be safely restricted.

## ADR-014 — Explicit single-task Antigravity Writer

- **Status:** accepted and implemented for the initial macOS R3 workflow.
- **Decision:** `talos task run <ID>` may launch one queued task using only Antigravity 1.5.10, with a session-only policy that allows project writes and denies reads and writes under `.talos/` and `.git/`, terminal commands, MCP tools, and URL tools. The prompt is sent over stdin; the attempt and public session ID are recorded durably.
- **Reason:** this makes the Writer path concrete while preserving restart recovery and review as separate gates.
- **Consequence:** a successful Writer leaves the task in `reviewing`; failures leave it `blocked`. TALOS does not run checks or complete the task through this command. R3's one-shot `up --auto` became available after the recovery and end-to-end acceptance passed. `up --semi` and its approval policy belong to R5.

## ADR-015 — Restart recovery never infers a safe retry

- **Status:** accepted and implemented for R3.
- **Decision:** a `launching`, `running`, or `unknown` attempt must not be retried automatically after a TALOS restart. Process presence/absence is only lifecycle evidence; it does not prove whether project files changed. Preserve ambiguous outcomes as `unknown` until an operator explicitly attests that the Writer stopped, the project was inspected, and a reason is recorded.
- **Reason:** TALOS can crash after spawning Antigravity but before persisting its PID, after prompt delivery but before receiving a session ID, or after a file change but before committing the final attempt result. PID reuse and surviving descendants add ambiguity.
- **Consequence:** recovery may leave work blocked for operator inspection. A replacement Writer must not launch while a recorded process group is live or cannot be probed. T3.6 records operator resolution separately and never changes the attempt's `unknown` outcome. R3 one-shot Auto is enabled for the verified macOS path after the acceptance gates passed; ambiguous attempts still block Writer execution until explicitly resolved.

### T3.6 recovery invariants and bounded implementation

Historical implementation facts that motivated T3.6 (the final R3 behavior is summarized below):

- `begin_writer_attempt` commits `launching` and moves the task to `implementing` before process creation.
- The runner spawns Antigravity before the callback commits `running` and its PID. A crash in this gap leaves the same database shape as a crash before spawn.
- Before the R3 continuation work, the public conversation ID was parsed from final output and committed only with the terminal outcome. R3 now persists a validated public ID as soon as the `init` event arrives, so recovery can retain it if TALOS later loses the process result.
- The attempt has no boot ID, process start identity, durable process-group identity, or pre-run project baseline/diff digest. A supervisor lease is ownership coordination, not proof that a child is alive or dead.
- Tokio's child PID is an OS-assigned identifier for a running child; it is not durable identity across TALOS restarts. A PID may be reused after the original process exits.
- A process-runner error does not carry enough evidence to prove that no child was started or that no project effect occurred. Such an error must leave the attempt unresolved instead of recording a terminal failure.

Failure windows and required behavior:

1. After the attempt transaction but before spawn, `launching` with no PID does not prove that no process started.
2. After spawn but before the PID transaction, `launching` with no PID may already have an external process. TALOS must not guess a PID or signal one.
3. After the PID transaction but before, during, or after prompt delivery, `running` does not establish how much work the Writer performed.
4. After project edits or a final CLI result but before the terminal database transaction, the process may be gone while the result and edits are not fully recorded.
5. A crash during terminal persistence leaves SQLite either with the previous unresolved row or the complete terminal transaction; it must not partially promote an attempt or task.

Invariants for every recovery path:

- Presence or absence of a PID, lease, session ID, or project diff alone never determines the Writer outcome.
- Do not signal, resume, retry, or mark an unresolved attempt successful or failed based only on those observations.
- Keep unresolved attempts inside the uniqueness guard so a future Writer cannot launch for the task or while any project attempt remains unresolved.
- Perform database quarantine in one immediate transaction and append a durable event. A repeated recovery must not append duplicate quarantine events.
- Recovery requires the current unexpired supervisor lease, validated by owner and fencing token inside the same database transaction. If another supervisor owns it, leave all attempt and task rows unchanged.
- Do not claim that the project is unchanged or that the Writer is stopped. Unknown work stays blocked for operator inspection.

T3.6 is complete for the initial macOS acceptance. `task recover` quarantines persisted `launching` and `running` attempts as `unknown` under the current fencing lease. `task resolve` records a separate immutable operator decision and attestations without changing the attempt outcome. The CLI refuses resolution when a recorded process group is live or cannot be probed. The probe only reports process-group liveness; it does not identify the Writer or rule out escaped descendants. No private `libproc` API or `ps` parsing is used. Linux process identity and runtime acceptance remain T6.5/T9.3 work.

### Platform identity research — 2026-11-17

The official Linux `/proc` interfaces provide a candidate process identity: boot UUID, PID, process-group ID, and process start ticks since boot. The detailed source notes are in `docs/research/recovery-platform-identity.md`.

On macOS, XNU exposes process start time and process-group fields through `proc_pidinfo`, but Apple's XNU `libproc.h` labels those process-information interfaces private and subject to change. TALOS must not build on that interface under the project's public-interface boundary. No stable, documented macOS process-start identity suitable for PID-reuse checks has been confirmed in this research. The recovery path must remain fail-closed and report identity as unavailable on macOS until a supported public interface is verified. Parsing `ps` output is not an acceptable substitute.

No schema change for process identity is selected or implemented. The separate schema-v3 operator-resolution migration is complete. The initial macOS recovery remains fail-closed with operator attestation because no supported public process identity source has been confirmed. The Linux parser/collector candidate and platform-specific identity work are deferred to T6.5; they do not block T3.6 or R3. A process identity match remains lifecycle evidence only; it does not establish a Writer outcome or authorize resume/retry.

## ADR-016 — macOS is the initial supported release platform

- **Status:** superseded for implementation scope by ADR-023; macOS remains the currently verified platform.
- **Decision:** the initial supported TALOS release/runtime was scoped to macOS only. Linux process identity, build, runtime, and packaging support were deferred to T6.5, T9.3, and T9.5.
- **Reason:** the initial release was to have one platform with verified process and packaging behavior instead of claiming untested cross-platform support.
- **Consequence:** missing Linux targets or Linux runtime tests do not block R3. Preserve `docs/research/recovery-platform-identity.md` and the existing T6.5/T9.3/T9.5 IDs for Linux work. The current implementation direction is clarified by ADR-023.

## ADR-017 — macOS restart recovery uses fail-closed operator attestation

- **Status:** accepted by the user; macOS policy remains active. Linux process lifecycle reporting is extended by ADR-028.
- **Decision:** on macOS, process identity is unavailable unless a stable public API is verified. Recovery quarantines ambiguous attempts and does not use private `libproc`, parse `ps`, send process signals, resume an unknown attempt, or retry automatically. To resolve an unknown attempt, require an auditable operator attestation that the Writer is stopped, the project was inspected, and a non-empty reason was supplied.
- **Reason:** no acceptable public macOS process-start identity source has been established, and PID/session/diff observations alone cannot prove the Writer outcome.
- **Consequence:** durably record actor, timestamp, chosen action, reason, and the operator's stopped/inspected attestations. Resolution immediately sets the task to `reviewing` for `accept-for-review`, `queued` for `authorize-retry`, or `failed` for `abandon`; the Writer attempt itself remains `unknown`. No resolution action launches a process or completes/reviews the task. A retry can start only through a later explicit `task run` or one-shot `up --auto`; it never makes `task continue` eligible for an `unknown` attempt. The attestation is an operator declaration, not an OS-verified fact.

## ADR-018 — Continue only an explicit terminal Agy error conversation

- **Status:** accepted by the user and implemented in R3.
- **Decision:** safe continuation is a separate `task continue <ID> --confirm-project-inspected` operation. It is eligible only when the latest Writer attempt has a terminal Agy `ERROR` result, exit status zero, and a validated public conversation ID. The command resumes that exact ID through Agy's public `--conversation` interface, waits for the `init` event and verifies its conversation ID matches before sending exactly one follow-up, and requires inspection of the existing workspace without replaying the original task prompt.
- **Reason:** an explicit operator action can continue a known terminal CLI error while retaining session context and acknowledging that prior project changes need inspection.
- **Consequence:** never continue an `unknown`, `launching`, or `running` attempt; a timeout, nonzero exit, malformed result, or missing/invalid session ID is also ineligible. A mismatch or missing resumed ID must not fall back to a new conversation or send the follow-up prompt. The operator's project-inspected flag is required. A successful continuation leaves the task in `reviewing`; it does not complete or verify the task. No automatic continuation is allowed.

## ADR-019 — R3 Auto is one-shot; Semi policy is deferred

- **Status:** accepted by the user and implemented in R3.
- **Decision:** R3 implements only a one-shot `up --auto` flow: one explicit invocation from a clean temporary Git project may process at most one eligible queued task. `up --semi` and policy-boundary approval behavior are deferred to R5.
- **Reason:** this bounds the first autonomous workflow and its acceptance evidence while recovery and verification remain under development.
- **Consequence:** one-shot Auto is enabled only after T3.4, T3.5, T3.6, and the R3 end-to-end criteria pass. It does not install a background worker, continuously poll, or imply task completion after a Writer response. Semi remains disabled until R5.

## ADR-020 — Keep Linux validation on the user's server

- **Status:** accepted; implementation and validation timing clarified by ADR-023.
- **Decision:** keep shared Unix code portable and implement Linux code when the relevant task reaches it. Do not add Docker, a VM, or a heavy local Linux test matrix. Run Linux-specific build, runtime, process-identity, and Agy sandbox acceptance on the user's Linux server when local setup would slow macOS development.
- **Reason:** continue the macOS development workflow without making local Linux infrastructure a prerequisite, while retaining a server-side acceptance gate before claiming Linux support.
- **Consequence:** macOS is the only currently verified platform. Linux remains explicitly unverified until T6.5/T9.3 acceptance evidence is recorded; its pending validation does not keep R3 open.

## ADR-021 — Assign verified CLI adapters by role

- **Status:** accepted product direction; implementation planned for R4/R7.
- **Decision:** project configuration will select Writer and Critic roles independently from the CLIs that have implemented and passed the required adapter and authority checks. The Critic must run independently and have a verified read-only boundary. A later bounded multi-agent flow should support one Writer, one independent Critic, and additional scoped agents, with explicit concurrency limits and isolated workspaces.
- **Reason:** TALOS should allow users to choose suitable CLIs for each role without treating help output or model claims as proof of capability or isolation.
- **Consequence:** the current R3 build has only Antigravity 1.5.10 verified as a Writer; it has no role-selection configuration or Critic yet. R4 adds one independent Critic; T4.6 records role selection, and T7.5 adds the three-agent acceptance scenario. Do not describe arbitrary CLI selection or three-agent execution as implemented until those tasks pass.

## ADR-022 — Keep Git history local until the user creates a remote

- **Status:** accepted by the user.
- **Decision:** initialize Git in the TALOS project and keep its history local. Do not add a remote or publish/push changes; the user will create the hosted repository later.
- **Reason:** preserve a reviewable change history during local development without creating an external repository before the user is ready.
- **Consequence:** commit project source and documentation locally, keep `.talos/` and build artifacts ignored, and configure no remote until requested.

## ADR-023 — Implement Linux while keeping macOS as the active development platform

- **Status:** accepted by the user.
- **Decision:** continue implementation and launch work on macOS while adding Linux-specific code in its planned round. Do not connect to, boot, provision, or run Linux-native checks before the user provides server access. Do not add Docker, a VM, or a heavy local Linux test matrix.
- **Reason:** avoid delaying implementation and macOS launch for platform-specific test infrastructure while still building Linux support into TALOS.
- **Consequence:** macOS remains the verified platform. Linux code can be implemented before server validation; all Linux-native checks are deferred until server access is available, and Linux support remains unverified until build/runtime/sandbox acceptance passes. This supersedes ADR-016's macOS-only implementation scope and clarifies ADR-020.

## ADR-024 — Use `master` as the default branch

- **Status:** accepted by the user.
- **Decision:** create and maintain `master` as TALOS's default branch.
- **Reason:** this is the user's explicit branch naming preference.
- **Consequence:** initialize the local repository and any later hosted repository with `master` as the default branch unless the user changes this direction.

## ADR-025 — Verify R5 completion through saved project-check evidence

- **Status:** accepted by the user and implemented in R5.
- **Decision:** project checks are named, allowlisted argv arrays saved immutably to each task. A task may become `complete` only after all saved checks pass for the latest approved Critic revision and the Git facts remain unchanged before and after verification. Persist exact check metadata, version/check durations, statuses, and output hashes; do not persist raw stdout/stderr.
- **Reason:** a successful Writer or Critic response does not prove project acceptance, and later sessions need compact evidence that can be inspected without retaining potentially sensitive output.
- **Consequence:** `task verify` runs the recorded checks under the supervisor lease; `task evidence` displays durable run evidence. The Critic remains findings-only; an unexpected patch field is rejected. Auto and Semi permissions are deterministic, and Semi requires interactive approval before a Writer launch.

## ADR-026 — Pause new Writer assignments and reconcile manual project edits

- **Status:** accepted implementation decision for R6.2.
- **Decision:** store queue pause state and an optional Git revision identity in SQLite. A pause blocks new Writer attempts and continuations. It does not interrupt an already-running process. Resume compares the current Git revision identity with the saved pause snapshot; if they differ or either snapshot is unavailable, keep the queue paused until the operator explicitly requests `resume --reconcile`.
- **Reason:** pausing task assignment is separate from stopping a process, and manual edits must not silently invalidate a previous Critic approval.
- **Failure handling:** if a writer attempt is still launching, running, or unresolved, do not resume the queue. If manual changes are reconciled, move tasks in `verifying` back to `reviewing` and require a fresh Critic review of their current revision. Append control events so pause, detected changes, reconciliation, and resume remain auditable. New tasks may still be added while paused.

## ADR-027 — Stop Writers cooperatively and fail closed after process reaping

- **Status:** implemented in T6.3; behavior/profile verification pending.
- **Decision:** persist an operator stop request against the active Writer attempt and let its supervisor terminate the process group. On Unix, keep the leader unreaped while sending SIGTERM, waiting a bounded grace interval, and sending SIGKILL. If cleanup is cancelled, an armed guard sends SIGKILL. Disarm only after the leader is reaped. Never signal a numeric PGID after reap because the identifier may have been reused. Reclaim an unexpired supervisor lease early only when its owner ID has a recognized task or worktree operation prefix, a valid positive PID and timestamp suffix, and that PID is absent; otherwise fail closed.
- **Reason:** process-group IDs are numeric and can be recycled. Recovery should shorten the stale-lease wait after a hard crash when evidence is available without inferring an attempt outcome or taking a live supervisor's lease.
- **Consequence:** `talos stop` is cooperative and may report that shutdown was not confirmed. `task recover` remains required after TALOS loss and still quarantines ambiguous Writer outcomes. Detached descendants after normal CLI exit may remain because safely signalling their old group cannot be proved. PID reuse or unsupported identity keeps the lease protected until its regular expiration.

## ADR-028 — Persist Linux process identity as lifecycle evidence only

- **Status:** implemented in T6.5; Linux-native verification pending.
- **Decision:** on Linux, capture `/proc/<pid>/stat` process-group and start-time fields plus `/proc/sys/kernel/random/boot_id` when the Writer starts. Persist the versioned fingerprint in the attempt row. During `task recover`, compare the saved fingerprint with current procfs data and report a match, different process, unavailable identity, or missing record. On platforms without a supported public collector, report unavailable.
- **Reason:** after PID reuse, operators need to know whether a recorded numeric PID still describes the same process, while preserving the rule that process identity cannot establish what the Writer did to project files.
- **Consequence:** identity comparison is diagnostic only. It does not permit TALOS to signal, resume, retry, accept, abandon, or resolve an attempt; unresolved work still requires process/project inspection and explicit operator attestations. Linux support remains unverified until native server acceptance passes.

## ADR-029 — Schedule tasks from an append-only dependency DAG

- **Status:** implemented in T7.1; behavior/migration verification pending.
- **Decision:** persist task-to-prerequisite edges in SQLite. Reject self-dependencies and any edge that closes a cycle. Allow adding an edge only while the dependent task is queued. A task is runnable only after every prerequisite reaches `complete`; explicit Writer launch and Auto share this gate.
- **Reason:** the queue must represent task order durably and must not start dependent work early or produce a scheduler that can deadlock on a cycle.
- **Consequence:** dependency edges are append-only and visible through `task dependencies`. Auto reports queued tasks waiting on open prerequisites. Dependencies do not change task success criteria, Critic review, or verification evidence.

## ADR-030 — Bound child tasks with contracts and compact reports

- **Status:** implemented in T7.2; migration and behavior verification pending.
- **Decision:** create child tasks with one queued parent, normalized project-relative allowed paths, explicit acceptance criteria, and a Writer timeout capped at 30 minutes. Add the parent→child prerequisite edge in the same transaction. Pass scope and timeout to the Writer, and make any changed path outside the scope a deterministic Critic blocker. Generate `talos-child-task-report-v1` from existing task/review/verification rows instead of storing duplicate report text or transcripts.
- **Reason:** delegated work needs an owner, bounded file scope, time limit, and predictable result packet while keeping the durable database compact.
- **Consequence:** the scope is a review gate, not an OS sandbox. A child does not unblock its parent until Critic review and project verification mark it complete. Existing reports remain derived data; full Writer transcripts are not saved.

## ADR-031 — Keep native subagent delegation disabled until verified

- **Status:** implemented in T7.3; live invocation and configured-agent discovery remain unverified.
- **Decision:** report native subagent invocation as `advertised` only when the CLI help exposes its public invocation tool. Otherwise report `not_tested` unless the CLI explicitly reports that the feature is unsupported. Do not start native delegation until the installed CLI, configured agent roster, and a harmless live invocation are verified.
- **Reason:** an agent-listing command or an agent-definition format does not prove that TALOS can invoke a scoped subagent in a headless run, or that the installed session exposes the delegation tool.
- **Consequence:** native delegation stays disabled while its capability is unknown. Manual child tasks, their contracts, and DAG scheduling remain available independently.

## ADR-032 — Manage task worktrees explicitly and preserve dirty directories

- **Status:** implemented in T7.4; lifecycle verification and Writer routing remain pending.
- **Decision:** create a linked worktree only through an explicit task command, from a clean Git base and for a queued task. Store it in a private sibling `.talos-worktrees/<project-hash>` directory and persist its branch, path, base commit, state, and transitions in SQLite. Reconcile interrupted lifecycle states explicitly. Remove only an explicitly selected active worktree after checking tracked, untracked, and ignored files; never force removal or delete its branch.
- **Reason:** worktrees provide separate files for later scoped delegation, but they are not a sandbox and may contain operator data. An interrupted filesystem operation must remain inspectable.
- **Consequence:** the primary project owns the database and configuration. T7.5 routes agent execution to a verified worktree while preserving shared state at the primary root.

## ADR-033 — Integrate task worktrees through explicit, evidence-gated steps

- **Status:** integration workflow implemented in T7.5 (`7f7fece`); its one-operation concurrency limit is superseded by ADR-042. Behavior and migration acceptance remain pending.
- **Decision:** keep one active TALOS agent/check/worktree-mutation operation per project under the existing supervisor lease. A newly routed task with an active managed worktree runs its Writer there. Schema v14 records the first routed attempt; migrated worktrees with earlier Writer attempts retain their existing primary-tree execution path. `task worktree integrate` records an immutable patch identity before applying and staging that patch to the primary tree, which must be clean at the same base commit. Critic review and project checks run on the primary tree after integration. Rejected revisions may be fixed in the primary tree. Passing checks do not complete a managed-worktree task until the user commits and `task worktree finalize` verifies the committed revision against the latest approval and passing checks.
- **Reason:** task workspaces need a reviewable path into the shared project without automatic commits, hidden cleanup, or dependency release before the integrated code is verified.
- **Failure handling:** an interrupted integration is reconciled only from recorded facts. TALOS may safely reapply the exact source patch only when the primary tree remains clean at its saved base; mismatches move to `needs_attention` without rollback. Lifecycle mutations and agent/check runs share the supervisor lease. An unused queued worktree can be explicitly removed; its task then runs in the primary tree. Worktree removal still refuses dirty contents. Native parallel delegation remains disabled until the installed CLI interface and agent roster are verified.

## ADR-042 — Run bounded Writers concurrently in separate managed worktrees

- **Status:** implemented locally in `837b82c`; migration and behavior acceptance remain pending.
- **Decision:** support parallel manual TALOS Writer runs only when each active task has its own active managed worktree. Keep direct primary-tree Writer execution exclusive. Add a project setting `max_concurrent_writers` with default `1` and a hard maximum of `3`; increasing it is an explicit project choice. Native Agy delegation remains a separate capability and stays disabled until verified.
- **Invariants:** one unresolved Writer attempt per task; one durable execution lease per active attempt; every concurrent attempt must resolve to a distinct active managed worktree; a primary-tree attempt cannot overlap any other Writer; SQLite must atomically enforce the project cap and match the execution root selected by the caller to the current task route before launch. Review, verification, worktree integration, and recovery remain serialized by the project supervisor lease and cannot start new writes into an active Writer set.
- **Recovery and failure handling:** persist the owning TALOS PID with each execution lease. A failed launch or lost supervisor leaves the lease and attempt unresolved. Recovery must refuse while any recorded owner PID is alive or cannot be checked. When all owners are absent, quarantine unfinished attempts as `unknown` and clear their execution leases in the same transaction. Never free a lease or retry work from process identity alone while the owner may still exist.
- **Reason:** managed worktrees already isolate task files, but the singleton lease and project-wide unresolved-attempt guard currently serialize all Writers. R7's planned parallel child-task workflow therefore cannot run as documented. It does not add a separate Guild entity or promise native Agy delegation.
- **Limit:** child path scopes remain review gates, not OS sandboxes. Parallel work uses independent TALOS processes and explicit child tasks; it does not imply that Agy can launch native subagents.

## ADR-034 — Keep GitHub authentication checks explicit and aggregate

- **Status:** implemented in T8.1; local CLI behavior acceptance pending.
- **Decision:** `doctor` may report only local `gh` availability and version. `talos github status` stays local unless the operator passes `--check-auth`; that flag runs `gh auth status --active --json hosts`, which may use configured credentials internally. Keep process time/output bounded, discard raw output, and expose only an aggregate state. Never invoke `gh auth token`, inspect credential environment variables, or store account/token metadata.
- **Reason:** readiness checks should not unexpectedly contact GitHub or reveal which account or credential source is configured. The auth status JSON contains more identity and credential metadata than TALOS needs.
- **Consequence:** aggregate status can be unknown on malformed, oversized, failed, or timed-out output. TALOS does not parse, store, or expose credential values, account names, or token sources. Repository access, push, PR, and CI require an explicit repository policy and command; authentication alone does not authorize those actions. Live behavior remains unaccepted until a user-provided repository is available.
- **References:** [GitHub CLI auth status](https://cli.github.com/manual/gh_auth_status), [GitHub CLI status implementation](https://github.com/cli/cli/blob/trunk/pkg/cmd/auth/status/status.go), and [Tokio process child](https://docs.rs/tokio/latest/tokio/process/struct.Child.html).

## ADR-035 — Require explicit policy and durable intent for GitHub publication

- **Status:** T8.2 implementation complete; local behavior and remote acceptance pending.
- **Decision:** use a schema-v3 `.talos/config.json` GitHub policy that names one `owner/repository`, one Git remote, base branch `master`, and a feature prefix rooted at `talos/`. Push and PR permissions default to false. Local branch preparation requires a clean primary checkout on `master`, validates the generated ref, and uses `git switch --create` from the configured base. Publication additionally requires per-invocation push and PR confirmations, verified fetch and push URLs, a matching remote base SHA, and a clean named feature branch. TALOS stores the exact commit and diff digest before pushing one explicit refspec with tag following disabled, then creates only a draft PR.
- **Reason:** branch policy must not infer a target or turn a locally selected task branch into an implicit push. The project's default branch remains `master` as requested. An interrupted network operation must not become an automatic duplicate push or PR.
- **Consequence:** push/PR remain opt-in and merge remains unavailable to TALOS. Append-only publication events record pending, confirmed, and uncertain states. Reconciliation reads the remote and permits only a matching recorded result; it stops for manual inspection when the result is absent, conflicting, or unverifiable. No remote behavior has been accepted until a user-provided repository is available.
- **References:** [Git branch validation](https://git-scm.com/docs/git-check-ref-format), [Git switch](https://git-scm.com/docs/git-switch), [Git push refspecs](https://git-scm.com/docs/git-push), and [GitHub CLI PR creation](https://cli.github.com/manual/gh_pr_create).

## ADR-036 — Bind CI evidence to an immutable published commit

- **Status:** implemented in T8.3; local compile/lint checked, live behavior pending.
- **Invariants:** CI evidence belongs only to a recorded open PR and the exact head commit already stored for that publication. Snapshot rows are bounded, append-only, and deduplicated by publication, commit, and summary digest. `pending`, `timeout`, `unknown`, and `no_checks` never count as passing. CI evidence never completes or merges a TALOS task.
- **Failure handling:** a remote branch or PR head that differs from the recorded commit produces an incomplete observation; it cannot be recorded as a pass. GitHub CLI errors, malformed/oversized output, and API uncertainty remain visibly unknown. `--wait` bounds only the CI polling window; pre/post identity checks are separate calls bounded to 60 seconds each. The final compact snapshot is saved once, with no poll-by-poll event rows.
- **Retention:** persist check names, workflow names, states/buckets, and timestamps only; omit descriptions, links, raw CLI output, account data, and credentials. Deduplicate identical summaries to limit database growth.
- **Reference:** [GitHub CLI `gh pr checks`](https://cli.github.com/manual/gh_pr_checks).

## ADR-037 — Keep merge and remote branch deletion outside TALOS

- **Status:** implemented in T8.4.
- **Decision:** TALOS can open draft pull requests and record their CI evidence. It has no merge or remote-branch deletion command. `talos github policy` reports `manual_only` merge and `unsupported` remote branch deletion in both human and JSON output.
- **Reason:** approval of code integration and cleanup stays with the user. A passing CI result is evidence, not authority to merge or remove remote refs.
- **Consequence:** TALOS never calls `gh pr merge`, GitHub's merge API, or a remote branch deletion command. Users perform merge and cleanup through their chosen GitHub workflow after review.

## ADR-038 — Generate task handoff packets from existing state

- **Status:** implemented in T6.6; live interactive-session continuity remains unverified.
- **Decision:** provide `talos task handoff <ID>` as a read-only, versioned new-context fallback. Generate it from current task, latest Writer, Critic, verification, dependency, and worktree rows. Cap and redact free-text fields, include only the latest attempt/review/verification summaries, and omit prompts, patches, transcripts, raw command output, and current Git diff. Do not persist a second report or transcript.
- **Reason:** when a CLI cannot preserve one conversation across interactive and headless modes, a later session still needs a small, inspectable summary without copying the full prompt history or adding duplicate database rows.
- **Consequence:** the packet recommends the next explicit CLI command from saved state but does not authorize the action, launch a process, or prove same-session continuity. The operator must inspect the working tree before a Writer action or retry. Its dependency list is capped; the report states when the cap omits rows. The TUI-to-headless acceptance probe remains pending until the workspace-trust decision is made.

## ADR-039 — Index task status for bounded queue selection

- **Status:** implemented in T9.2; migration and large-queue acceptance pending.
- **Decision:** schema v17 adds a non-unique `(status, id)` index on `tasks` for queue discovery. Auto/Semi select the first runnable queued task with one bounded SQLite query and use an existence query to distinguish an empty queue from a queue blocked on prerequisites. Task-specific commands use primary-key lookups instead of materializing every task.
- **Invariants:** the index is derived acceleration only; task rows, dependencies, worktree integration state, and task events remain authoritative. The migration changes no task data or state. It runs inside the existing schema transaction, so a failure leaves the database at its prior complete version. No task history is deleted or compacted.
- **Reason:** repeatedly loading every task and checking dependencies one by one makes a long-running project slower and increases memory use as completed history grows.
- **Acceptance:** migrate a populated v16 database, confirm v17 and all task/event/dependency rows remain intact, compare runnable selection for queued tasks with and without prerequisites, and fault-inject the migration transaction. These checks remain pending.

## ADR-040 — Reconcile paused task approvals with bounded memory

- **Status:** implemented in T9.2; transactional behavior acceptance pending.
- **Invariants:** resume holds an immediate SQLite transaction while it counts verifying tasks, captures at most 50 preview IDs, reopens every verifying task for a fresh Critic review, and appends exactly one task event per changed task. The task update and all task/project control events commit together. The complete count remains available even when the ID preview is truncated.
- **Decision:** perform the status change and task-event insertion with set-based SQL. Choose a timestamp greater than every existing task `updated_at_ms`, then use that unique timestamp to select exactly the rows changed by the set-based update. Do not materialize all reopened task IDs in Rust memory.
- **Failure handling:** reject count or timestamp overflow. Compare the update row count with the prior verifying count and the inserted event count with the update count. Any mismatch or SQL/serialization error aborts the transaction; the paused queue and prior task states remain intact.
- **Reason:** project history may contain a large number of tasks awaiting verification. Manual reconciliation must preserve per-task audit rows without allocating a full in-memory ID list or producing an unbounded event payload.
- **Acceptance:** reconcile zero, one, and many verifying tasks; verify each task moves to `reviewing` with one matching event, the preview stays capped at 50 with a full count, and injected transaction failures leave task state/events and pause state unchanged. These checks remain pending.

## ADR-041 — Bound Git revision capture before buffering

- **Status:** implemented in T9.2; behavior acceptance pending.
- **Invariants:** a Critic revision is either captured in full within the configured patch/path limits or rejected. TALOS never hashes, stores, reviews, or marks an over-limit revision as if it were complete. Path identity remains exact for every accepted revision.
- **Decision:** stream Git stdout into bounded buffers. Keep the existing 64 KiB patch limit; cap changed paths at 4096 entries, each at 1024 bytes; cap each Git path-list response at 128 KiB and serialized changed-path metadata at 256 KiB. For dirty-base, worktree-removal, and tracked-runtime-state probes, read only enough output to establish that the operation must be rejected. Do not accumulate every status row or path merely to return an error.
- **Failure handling:** when a capture exceeds its limit or cannot be parsed, terminate and reap the Git child where possible and fail the task operation without truncating the revision or persisting partial evidence. Git status parse failures remain errors rather than a false clean result.
- **Reason:** `Command::output()` allocated the full diff/path output before existing checks rejected oversized revisions. Large repositories must not bypass memory limits just because the final review payload has a cap.
- **Acceptance:** verify exact tracked/untracked paths including rename/copy and unusual names, confirm oversized patch/path output fails before full capture, check the child is reaped after limit failure, and confirm no partial review/evidence row is persisted. These checks remain pending.

## ADR-043 — Deliver the multi-CLI profiles in the original TALOS goal

- **Status:** accepted by the user on 2026-11-18; implementation not started.
- **Decision:** TALOS must add selectable profiles for Codex, OMP, Pi, Kilo, OpenCode, and Claude Code, in that order, in addition to the existing Agy adapter. Complete the profiles as provider-specific vertical slices after the current R6–R9 local closeout.
- **Reason:** the project goal and proposed architecture already describe multiple coding CLIs and per-vendor capability profiles. R1 researched alternatives and R4 added role fields, but the code still implements only Agy. Role selection must not be presented as multi-CLI support.
- **Contract:** a profile owns executable/version discovery, fixed argument construction, structured output parsing, public session identity/resume behavior when available, timeout/cancellation, and role-specific permission probes. Config may select only built-in profile IDs; it must not accept arbitrary shell commands or unrestricted argument strings. Writer and Critic may use different CLIs. Store the selected profile ID and observed version with each Writer attempt and Critic review so later configuration changes cannot rewrite history.
- **Enablement:** capability help is discovery evidence only. Enable Writer only after a disposable-project run proves the actual write boundary; enable Critic only after a separate process proves the actual read-only boundary. A missing credential, blocked probe, unsafe permission mode, or failed live run leaves that role disabled. Parser fixtures and unit tests do not satisfy live acceptance.
- **Limits:** do not inspect private CLI session databases or credential stores. Do not use dangerous permission bypasses. If public session resume is unavailable or incompatible, report it and use a fresh bounded task context. Native subagents, Guild entities, F1 project history, and F2 controlled-language ideas remain separate scope. Linux-native checks continue to wait for the user's server. Git remains local on `master` until the user creates a remote.

## ADR-044 — Hybrid Team Orchestration with Writer and Critic Teams

- **Status:** implemented and verified in R11.
- **Decision:** implement autonomous two-tier team orchestration where TALOS manages two distinct engineering teams:
  1. **Writer Team:** Led by Writer Lead with full task implementation authority, technical planning, subtask decomposition, worker assignment, progress supervision, and stuck worker recovery. Supported by up to `max_writer_workers` (default 2) Writer Workers bounded by explicit project-relative `allowed_paths`.
  2. **Critic Team:** Led by Critic Lead with strict read-only permissions, responsible for consolidated architecture review, evaluating worker findings, debating discrepancies with Writer Lead, and rendering the final authoritative verdict (`approve` or `request_changes`). Supported by up to `max_critic_workers` (default 1) specialized Critic Workers (e.g. security, algorithms, edge-cases).
- **Orchestration Modes:**
  - `pair` (default): Preserves the exact existing sequential Writer -> Critic -> Verify -> Complete workflow when `talos up --auto` is called without `--mode`.
  - `team`: Activated via `talos up --auto --mode team` or `config.json` `"orchestration": { "mode": "team" }`.
- **Delegation Invariants:**
  - `detect_delegation` checks whether the target CLI provides safe, confirmed headless programmatic subagent execution.
  - Because no supported CLI exposes safe headless native subagent delegation without interactive prompts or trust dialogs, `DelegationKind::Hybrid` deterministically falls back to `DelegationKind::External`, using managed isolated CLI processes with worktree or path bounds.
- **Worker Assistance & Recovery:**
  - When a Writer Worker experiences process failure, timeout, or missing deliverables, Writer Lead is invoked with the failure diagnostics.
  - Writer Lead can choose: `RetryWithGuidance` (bounded by `max_help_attempts`, default 2), `Takeover` (Lead executes the subtask directly), or `BlockSubtask` (mark failed/blocked).
- **Debate & Rebuttal Loop:**
  - If Critic Lead issues `request_changes`, Writer Lead receives the critique, diff, and verification results to produce a structured rebuttal and implementation fix plan.
  - Fix iterations are bounded by `max_review_rounds` (default 3) before marking the task blocked.
- **Durable State:**
  - SQLite schema v19 introduces the `team_subtasks` table with foreign key references to parent tasks, recording subtask status, path boundaries, worker role, assignment, and assistance attempts.
  - Backward compatibility is maintained for schema versions 1 through 18.
- **Verification:**
  - 137 automated unit and integration tests passing; verification profile clean.

## ADR-045 — Unified Terminal Orchestrator Cockpit on Ratatui and Crossterm

- **Status:** implemented and verified in v0.1.0 release.
- **Decision:** bare `talos` invocation (and explicit `talos tui`) launches a full-screen interactive Terminal Orchestrator Cockpit powered by `ratatui` and `crossterm`. Persistent multi-pane tmux workspaces remain available via `talos attach` and `talos --tmux`.
- **Architecture and Invariants:**
  1. **UI Event Loop & Background Supervisor:** The terminal UI runs asynchronously on Tokio without blocking the rendering loop. Dispatching a task spawns background execution while streaming live output to the TUI.
  2. **Multi-View Tabbed Interface:** The Cockpit provides four distinct operational views:
     - `Overview`: Task queue, worker/critic statuses, lease owner, and live event log.
     - `Dialogue & Review`: Conversation stream between Writer Lead, Critic Lead, and worker subtasks.
     - `Git Diff`: Real-time tracked and untracked file diffs for the current working revision.
     - `Audit & Checks`: Verification test results, timing, exit codes, and evidence logs.
  3. **In-TUI Slash Commands & Russian Aliases:** The interactive command bar accepts extensible commands (`/stop`, `/pause`, `/resume`, `/commit`, `/diff`, `/audit`, `/dialogue`, `/help`, etc.) alongside full Russian alias support (`/стоп`, `/пауза`, `/продолжить`, `/помощь`).
  4. **Terminal Keybinding Safety:** Hotkey `Ctrl+P` was removed to prevent collisions with native tmux prefixes or terminal emulators; navigation uses standard tabs, arrow keys, and slash commands.
  5. **Safety & Recovery:** Reconnection to active SQLite tasks on startup; dirty tree protection before auto-dispatch; clean terminal state restoration on exit via Crossterm raw mode teardown.
- **Verification:** verified live across macOS and Linux terminal environments; automated CLI parser test in `src/cli.rs`.

## ADR-046 — Intelligent Rate-Limit & Quota Detection with Adaptive Backoff and Auto-Resume

- **Status:** implemented and verified in R13.
- **Decision:** when external AI CLIs (Codex, Claude, Agy, OMP, Kilo, etc.) return rate limit or quota exhaustion errors (HTTP 429, `insufficient_quota`, `RESOURCE_EXHAUSTED`, `rate limit reached`, `overloaded`), TALOS must NOT treat this as an implementation defect, code syntax error, or waste code-repair help attempts.
- **Architecture and Invariants:**
  1. **Signature Pattern Matching:** `src/rate_limit.rs` classifies errors into `RequestLimit` (RPM/TPM/429), `QuotaExhausted` (credits/billing), or `Overloaded` (529/capacity) using known vendor signatures.
  2. **Intelligent Wait Extraction:** Extracts explicit wait periods from CLI outputs (e.g. `try again in 20s`, `resets in 45s`, `retry-after: 60`, `wait 5m`).
  3. **Adaptive Backoff Schedule:** When no explicit duration is given by the CLI, TALOS applies progressive backoff (request limits: 20s → 60s → 120s → 300s; quota exhaustion: 5m → 15m → 30m → 45m).
  4. **Seamless Auto-Resume:** Records `task.rate_limit_backoff` event in SQLite with duration and attempt count, prints clear countdown status, sleeps asynchronously with `tokio::time::sleep`, and automatically retries the worker or query attempt (capped at 5 retries).
- **Verification:** 6 dedicated unit tests covering OpenAI, Anthropic, Gemini, DeepSeek, and non-rate-limit code errors; integrated into Writer task execution, Critic review, and agent planning queries.

## ADR-047 — Autonomous Decisions Artifact, Session Timings, and Token Economy Budgeting

- **Status:** implemented and verified in R14.
- **Decision:** upon completing a task in Team or Pair mode, TALOS automatically generates a structured Markdown report (`TALOS_SUMMARY.md`) in the project root documenting session timings, autonomous architectural choices, passing tests, and git commit details.
- **Architecture and Invariants:**
  1. **Deliverable Artifact (`TALOS_SUMMARY.md`):** Documents task metadata, timestamps, elapsed session duration (active vs rate-limit pauses), autonomous architectural decisions made by Writer Lead / agents, passing project check names, and modified file paths.
  2. **Token Economy Truncation:** Large error outputs and compiler logs are truncated via `src/summary.rs::truncate_for_token_economy` to a bounded 4096-byte budget preserving head and tail diagnostic context, preventing token blowups in agent prompts.
  3. **Non-Intrusive Artifact Generation:** Written only on successful verification; failures remain recorded in SQLite without cluttering the project workspace with incomplete artifacts.
- **Verification:** unit tests for token truncation head/tail boundaries and small-string bypass; live verification across `run_team_task` and `run_pair_task`.

## ADR-048 — Dynamic CLI Adapter Selection (/cli) and Unified Feed Cockpit Redesign

- **Status:** implemented and verified in R15.
- **Decision:** provide immediate, ergonomic CLI adapter switching in both interactive TUI (`/cli <name>` with Russian aliases) and headless/batch commands (`--cli <name>` flag), and redesign the primary TUI view from fragmented cards into a responsive, live-streaming Unified Feed.
- **Architecture and Invariants:**
  1. **Dynamic Adapter Pass-Through:**
     - In TUI command line: `/cli` inspects active adapters and valid options; `/cli <name>` sets both Writer and Critic to `<name>` and persists to `.talos/config.json`; `/cli <writer> <critic>` sets distinct roles. Supported aliases: `/кли`, `/adapter`, `/адаптер`.
     - Global CLI flag: `--cli <name>` on `talos` overrides the active adapter and persists to `.talos/config.json`.
     - Subcommand: `talos config set cli <name>` supported.
  2. **Unified Feed Cockpit Architecture:**
     - The primary screen (`ActiveTab::Feed`, " 1: Unified Feed ") replaces the old fragmented overview.
     - Responsive layout: On wide viewports (>= 100 cols), provides a 34-col Cockpit Sidebar (AI Agents card, Active Task card, Queue & Repo card) alongside a prominent live Unified Feed. On narrow viewports, switches to a compact 7-row header summary.
     - Merged event stream: Interleaves supervisor logs (streaming planning, subtask breakdown, worker assignments, test runs, rate limit countdowns) and SQLite milestone events (`task.created`, `task.transition`, `writer.started`, `critic.review`, `rate_limit_backoff`, `worktree.created`, `worktree.integrated`).
     - Distinct color-coded badges: `[PHASE]`, `[WRITER]`, `[CRITIC]`, `[SUPERVISOR]`, `[TASK #N]`, `[VERIFY]`, `[RATE-LIMIT]`, `[WORKTREE]`, `[GIT]`, `[ERROR]`, `[SYS]`.
  3. **Auto-Scroll & Manual Navigation Engine:**
     - When `scroll_offset == 0`, view remains pinned to the bottom (live auto-scroll mode) as new logs arrive.
     - Scrolling up (`Up`, `PageUp`, `Home`) engages manual history inspection, updating the title indicator to `"Viewing earlier lines: -N | Press 'End' to resume"`.
     - Pressing `End` or scrolling down to the bottom snaps back to live stream auto-scroll.
- **Verification:** automated CLI parsing tests (`cli_flag_parses_global_adapter_override`), TUI slash command tests (`tui_cli_slash_commands_and_feed`), tab title & navigation tests (`active_tab_navigation_and_titles`), and full verification profile `./scripts/verify-project.sh` clean.
