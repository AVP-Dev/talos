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

- **Status:** accepted by the user.
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
- **Decision:** continue day-to-day implementation and launch work on macOS while implementing Linux-specific paths as their planned tasks are reached. If Linux-target compilation, runtime, or sandbox checks require cumbersome setup on macOS, defer those checks to the user's Linux server. Do not add Docker, a VM, or a heavy local Linux test matrix.
- **Reason:** avoid delaying implementation and macOS launch for platform-specific test infrastructure while still building Linux support into TALOS.
- **Consequence:** macOS remains the verified platform. Linux code can be implemented before server validation, but Linux support must remain marked unverified until the deferred build/runtime/sandbox acceptance passes. This supersedes ADR-016's macOS-only implementation scope and clarifies ADR-020.

## ADR-024 — Use `master` as the default branch

- **Status:** accepted by the user.
- **Decision:** create and maintain `master` as TALOS's default branch.
- **Reason:** this is the user's explicit branch naming preference.
- **Consequence:** initialize the local repository and any later hosted repository with `master` as the default branch unless the user changes this direction.
