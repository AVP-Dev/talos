# TALOS Project Plan

**Status:** R1–R5 are complete for the locally verified macOS path. R5 adds project-check snapshots, exact Git-revision evidence, evidence-gated completion, deterministic secret handling, and an interactive Semi Writer gate. Linux-specific implementation remains in scope; Linux-native build/runtime/sandbox acceptance is deferred to the user's server under T6.5/T9.3, so Linux remains unverified. No Docker or VM setup is required.

## 1. Goal

TALOS is a local CLI-first orchestrator that enables multiple AI coding CLIs to work autonomously on development tasks. From a project directory, the user runs `talos up --auto`, submits a task, and returns later to work that has continued or recovered.

The system detects the project, stores its queue and context locally, launches suitable CLIs through their public interfaces, assigns a Writer and an independent Critic, verifies code and acceptance criteria, and recovers after terminal, SSH, or process loss. The goal is a practical executable, not an architecture experiment.

macOS is the current verified development and runtime platform. Linux process identity code is implemented under T6.5; continue any remaining Linux-specific runtime and packaging work in plan order. If Linux-target compilation, runtime, or Agy sandbox checks require cumbersome setup on macOS, defer those checks to the user's server under T6.5/T9.3. Do not add a Docker/VM requirement or claim verified Linux support before server acceptance passes. Linux validation is not an R3 acceptance dependency.

## 2. Design principles and boundaries

- **Local modular monolith:** one Rust executable and SQLite WAL. No required MCP server, Python runtime, Docker, Redis, message broker, Kubernetes, or network database.
- **Incremental delivery:** the first working slice uses one CLI. Other adapters, parallelism, SSH, and GitHub come after the foundation is proven.
- **Deterministic core:** Rust controls state, permissions, processes, queues, and evidence. Models propose technical decisions within granted authority.
- **Use pair-mailbox experience:** transfer lessons, not code: one Writer, an independent Critic, atomic write authority, content-verified patches, ground-truth tree checks, durable tasks, an event journal, a distinction between silence and process death, and recovery from observed state. See the detailed [pair-mailbox assessment matrix](research/pair-mailbox-assessment.md).
- **Use Agent Starter Kit as guidance:** ordinary practices are recommendations; explicit safety and data-preservation boundaries remain hard limits. Starter Kit is not a product dependency.
- **No false evidence:** CLI help, a successful model response, a mock, or an agent's claim alone does not prove session continuity or task completion.
- **Bounded autonomy:** Auto skips repeated prompts for routine, explicitly permitted in-project work. It does not disable CLI controls, grant unrestricted access, allow production operations, or merge automatically.
- **English-only system:** all code, prompts, CLI output, logs, reports, examples, and documentation are in English.
- **Development and platform support:** macOS is the current verified platform. Linux process identity code is implemented, but native Linux build/runtime/sandbox acceptance remains open under T6.5 and T9.3.

## 3. Proposed architecture

```text
talos CLI
  ├── project discovery and configuration checks
  ├── SQLite WAL: tasks, leases, attempts, events, and evidence
  ├── Tokio scheduler and supervisor
  ├── AI CLI adapters with per-vendor capability profiles
  ├── Writer → Critic → verification → report
  ├── Git and isolated worktrees for parallel tasks
  └── tmux as a persistent user shell
```

### Source of state

For a Git project, TALOS resolves `git rev-parse --show-toplevel` and stores its ignored `.talos/state.sqlite3` under the repository root. If Git metadata is absent, root discovery checks an existing `.talos` directory, common project markers, then the current directory. If Git metadata exists but Git cannot resolve it, TALOS reports the error instead of silently selecting a different root. Runtime state, logs, and temporary context are not committed.

Database entities: project/configuration; tasks with acceptance criteria and dependencies; execution attempts with lease/fencing token, PID, and CLI session ID; append-only events; verification results; Critic findings and patch digest; and worktree metadata. SQLite is authoritative. tmux and terminals are presentation only.

### Proposed task lifecycle

`queued → planning → implementing → reviewing → verifying → complete`.

A rejected review returns the task to `implementing`, with findings attached and the revision incremented. A process failure leaves a recoverable state. On macOS, where a supported public process-identity API has not been confirmed, TALOS quarantines unfinished attempts and requires an auditable operator attestation before recording any resolution authorization. Recovery never uses private `libproc` interfaces, parses `ps`, sends process signals, or retries automatically. Any later action requires its own explicit command. An ambiguous side effect is never repeated automatically.

### CLI surface: implemented and planned commands

```text
talos up --auto             # implemented in R3: one explicit call processes at most one eligible task
talos up --semi             # implemented in R5; prompts before one Writer launch
talos task add "..."        # add a task with a verifiable goal
talos task run <ID>         # implemented in R3 for the verified Writer
talos task review <ID>      # implemented in R4 for the independent Critic
talos task checks <ID> ...  # add immutable verification requirements
talos task verify <ID>      # run checks for the exact approved Git revision
talos task evidence <ID>    # inspect paged saved verification evidence
talos task continue <ID> --confirm-project-inspected  # implemented for one eligible terminal ERROR result
talos task recover          # implemented: quarantine ambiguous attempts as unknown
talos task resolve <ATTEMPT_ID> ...  # implemented: audited operator decision
talos status                # session, worker, queue, and capability status
talos logs [TASK_ID]        # task event history
talos attach                # implemented in T6.1: create/reuse this project's control shell
talos pause [--reason ...]  # implemented in T6.2: block new Writer attempts durably
talos resume [--reconcile]  # implemented in T6.2: compare Git facts before resuming
talos logs --project        # inspect append-only pause/reconciliation history
talos doctor                # diagnose CLI, tmux, Git, and database setup
talos stop                  # request graceful Writer/process-group shutdown
```

R3 Writer commands, R4 `task review`, and R5 verification commands are implemented for the verified macOS Antigravity 1.5.10 path. Task, dependency, child-contract, worktree, event, and verification-evidence lists expose bounded cursor pages. Auto/Semi blocked-queue output shows a capped preview and total count. T6.1 adds `attach`; T6.2 adds durable pause/resume and project event history; T6.3 now adds a cooperative `stop` request, process-group timeout/interrupt cleanup, and best-effort stale-lease reclaim. T6.3 behavior tests and full-profile verification remain pending. `task continue` is limited to one explicit follow-up for the latest terminal Agy `ERROR` result with exit status zero and a validated public conversation ID. It never handles `unknown`, timeout, nonzero exit, or malformed output. TALOS resumes that exact ID through the public `--conversation` interface, verifies the resumed `init` ID before sending the follow-up, and asks the operator to inspect current workspace state without replaying the original prompt. A successful follow-up remains `reviewing`. Critic approval moves a task only to `verifying`; it does not claim project checks or completion.

## 4. Roadmap and rounds

Each round ends with a usable, verifiable result, an updated backlog, and a session handoff. Proceed when the round's acceptance criteria are observable; do not wait for future features to demonstrate intermediate value.

| Round | Direction | Round result |
|---|---|---|
| R0 | Bootstrap and research | Starter Kit adapted, pair-mailbox lessons recorded, installed CLI versions captured. Complete. |
| R1 | CLI spike | Headless JSON and same-ID resume verified for Antigravity; TUI handoff and native delegation remain deferred. R1 is complete; R3 later verified the write boundary. |
| R2 | Core and durable state | Buildable Rust CLI, project discovery, SQLite WAL, schema migration, task queue/transitions, event journal, leases, status/doctor. Complete. |
| R3 | One autonomous Writer | One verified adapter completes one safe local task per explicit `up --auto` invocation from a clean temporary Git project. Complete for the initial macOS target. |
| R4 | Independent Critic | Read-only review, real blocker finding, denied write probe, linked Writer fix, immutable review history, and per-project role selection. Complete for verified macOS Antigravity 1.5.10. |
| R5 | Verification and permissions | Configured checks, exact Git facts, Auto/Semi policy, redaction, and evidence-gated completion. Complete on macOS; see R5 acceptance below. |
| R6 | Persistent runtime and recovery | In progress: T6.1 adds a project-scoped tmux control shell; T6.2 adds durable pause/resume and manual-change reconciliation; T6.3 adds cooperative stop and safer recovery; T6.6 adds a bounded new-context packet. Process/recovery and macOS SSH behavior acceptance remain open. T6.5 Linux implementation exists; native server acceptance is deferred until access is available. |
| R7 | Decomposition and parallel work | T7.1–T7.5 implementation includes schema-v18 durable Writer slots and opt-in concurrency up to three distinct managed worktrees. Migration and behavior acceptance remain open; native delegation stays disabled and the three-agent scenario is unverified. |
| R8 | GitHub workflow | In progress: T8.1 adds local `gh` capability reporting and an explicitly requested aggregate auth check. T8.2 implements policy-gated exact branch push, durable reconciliation, and draft PR creation. T8.3 records commit-bound CI snapshots with bounded polling. Remote behavior awaits the user's repository. No automatic merge. |
| R9 | Hardening and release | In progress: T9.1 has a 15-scenario inventory; T9.2 has a separate hardening matrix and bounded package-script clean checks; T9.3 has Cargo lifecycle guidance and native macOS/Linux package scripts; T9.4 documentation is complete; T9.5 has a current ARM64 macOS artifact from source commit `b4ac9ef`. Final-schema runtime scenarios, install lifecycle, reproducibility, binary smoke, SSH/TUI continuity, and Linux server acceptance remain open. |

### R0 — completed foundation

1. Read the author-provided Starter Kit and ran its `scripts/init-project.sh --update` in the empty TALOS directory after a dry run.
2. Adapted Starter Kit guidance as recommendations with separate non-negotiable boundaries; added project files for continuity across sessions.
3. Reviewed pair-mailbox protocol, code, tests, and recent history; left the source untouched.
4. Collected installed CLI versions and public help; the R1 live results and deferred capability checks are recorded in the CLI spike report.

## 5. Technical round details

### R1 — CLI spike (completed)

The spike used disposable test projects to evaluate Antigravity CLI (`agy`) first, followed by Codex and the available alternatives. The report records the version and only the behavior actually observed for each adapter.

The spike attempted to check headless execution; machine-readable output; session ID capture and continuation; TUI handoff; task/context transfer; native subagents; Ctrl-C/SIGTERM; permission/sandbox behavior; restart; and auth/network errors. It used harmless prompts in isolated state. Private session stores and persistent user trust settings were not changed. Blocked capabilities have later task IDs.

**Exit criteria:** a capability table of `supported / unsupported / not tested`, exact version/path, command, and concise evidence; primary and fallback CLI choices; known incompatibilities and maintenance cost. At least one candidate must pass a real headless structured-output and same-ID resume check for the sequential MVP. Defer interactive TUI, native delegation, cancellation, and write-mode checks only with explicit blockers and later task IDs. Auto writes stay disabled until R3 verifies native permission boundaries in a disposable project.

**Result at R1 close:** Antigravity 1.5.10 returned parseable JSON and resumed the same public conversation ID in a second headless process. The TUI stopped at a project-trust prompt that was declined. Alternative CLIs were blocked before inference or not run. Auto write behavior, interruption, TUI handoff, and native delegation were deferred at this checkpoint; see `docs/research/cli-spike.md` and R1 task records. R3 later verified Antigravity 1.5.10 as the initial Writer adapter under the tested session policy. TUI handoff and native delegation remain deferred.

### R2 — Minimal deterministic core (completed)

Create a Rust binary crate, CLI model, project root discovery, safe configuration defaults, SQLite WAL, and a versioned schema. Add task add/list/status, append-only lifecycle events, a transaction-protected supervisor lease with persistent fencing tokens, and diagnostics. `up --observe` inspects readiness without starting an agent. On Unix, private state paths must be owned by the current user and restricted to owner-only permissions.

**Result:** task persistence, transition validation, append-only events, atomic leases, unknown-schema refusal, project-root discovery, private state permissions, and the observe-only CLI are implemented and locally verified. Worker lifecycle, lease renewal, and agent execution remain in R3.

**Exit criteria:** repeat initialization is safe; queued tasks survive restart; a second supervisor cannot claim the lease; an unknown/future schema is not silently erased; status explains what is missing before agent launch.

### R3 — One verified Writer and executable task

Implement the first R1 adapter using its public interface: version/help discovery, capabilities, argument construction without shell strings, structured output, exit/error mapping, cancellation/timeout, and session IDs. The supervisor processes one task at a time. R3 `up --auto` is one-shot: each explicit invocation may process at most one eligible queued task in the current clean temporary Git project; it does not install a background service or poll indefinitely. `task add` persists work; the result reports actual output and project changes for review.

Before enabling writes, use the CLI's verified native isolation and confine filesystem access to the project. If safe unattended behavior requires a dangerous bypass, Auto stays disabled for that adapter; report the incompatibility instead of bypassing its controls.

**Exit criteria:** a live one-shot flow from a fresh temporary Git project to one changed file, correct stdout/exit handling, task visible after a TALOS restart, no duplicate execution on repeat invocation, and no false task completion. Full R3 recovery and task acceptance gates must pass before Auto is enabled. Semi mode and its approval policy are deferred to R5.

**Accepted result:** T3.1–T3.7 are complete. Schema v3 transactionally adds separate operator-resolution metadata and continuation source links. Recovery quarantines `launching`/`running` as `unknown` under the current fencing lease, preserves metadata, blocks new Writers, and never infers an outcome or retries automatically. `task resolve` requires stopped-Writer and inspected-project confirmations plus a reason; it records the choice atomically while keeping the attempt outcome `unknown`. The app refuses resolution while a recorded process group is live or cannot be probed. macOS process identity is unavailable; the liveness probe is not an identity check and cannot detect escaped descendants.

`task continue <ID> --confirm-project-inspected` accepts only the latest terminal Agy `ERROR` with exit status zero and a validated public conversation ID. It resumes through `--conversation`, waits for the matching `init` ID before sending one inspection-first follow-up, and leaves success `reviewing`. Fake-CLI tests cover failure mapping and rejection cases. A live disposable project used a wrapper to inject terminal `ERROR` after a real Agy 1.5.10 turn, then TALOS resumed that same public conversation; this verifies the continuation path but does not prove that Agy naturally emits this error status.

One-shot `up --auto` now requires a clean Git worktree and processes at most one queued task per invocation. It reports the response and changed-path count with a preview of at most 50 paths. Fake and live Agy 1.5.10 disposable-project acceptance verified a changed marker file, task visibility after a separate TALOS process, no duplicate execution, the one-task limit with two queued tasks, and no false completion. No project checks or Critic review run in R3; tasks stay `reviewing`. `up --semi` remains R5 work. Linux validation is unverified and does not hold R3 open; no Docker/VM setup was used.

### R4 — Writer/Critic

Add a separate Critic role, preferably using another vendor/CLI when available. Project configuration will select Writer and Critic adapters independently from the CLIs whose required interfaces and authority boundaries have been verified. The Critic receives the base/new revisions and acceptance criteria; its checkout or file permissions are actually read-only. Review output is structured as path/line/reason/severity findings or an approval with limits. A rejection starts a new Writer iteration. Role and authority persist independently of process restarts. T7.5 later verifies a bounded three-agent flow with one Writer, one independent Critic, and one additional scoped agent.

**Exit criteria:** a realistic defect found by Critic blocks closure; Critic cannot write; repeated Writer/Critic cycles preserve work.

**Result (completed):** T4.1–T4.6 are implemented. `task review <ID>` starts a fresh Antigravity 1.5.10 process without a Writer conversation ID or transcript. The review request binds acceptance criteria and a complete patch to the full base commit and a SHA-256 revision ID. The patch, serialized prompt, and structured response are capped at 64 KiB, 96 KiB, and 64 KiB; TALOS never truncates review context. A valid approval moves the task to `verifying`, a rejection moves it to `implementing`, and `complete` remains unavailable. Historical review rows are immutable. A live review approved a simple revision; another live review found a real empty-input defect, recorded a blocker, and moved the task to `implementing`. A fake Writer fix started a linked second attempt from the same base. A separate live write probe confirmed that Agy's `write_file` call was denied and the marker file was absent. `cargo test`, Clippy, release build, and the security scan passed at the R4 close checkpoint; exact current run results are in `docs/state.md`. CLI and model probes used disposable projects only. Linux-native acceptance remains unverified and stays under T6.5/T9.3.

### R5 — Ground truth, verification, and permissions

Project config v2 defines checks as validated argument arrays, version probes, and project-relative working directories. Tasks save an immutable snapshot of selected checks. Verification records exact commands, working directories, version/command exit codes, failure/timeout status, output hashes, and approved revision facts. Output is drained with a bounded capture and raw check stdout/stderr is not persisted. Git facts include base commit, HEAD, sorted changed paths, and patch digest; they must match both the Critic approval and the final post-check snapshot. The Critic remains findings-only and any unexpected patch field is rejected; Writer fixes are linked attempts that receive a new exact-revision review. Auto permits only allowlisted Writer launch and project checks; Semi requires an interactive approval before launching a Writer; hard-denied actions take precedence. A task completes only when all saved checks and all Git/review invariants pass.

**Result (completed locally on macOS):** T5.1–T5.7 are implemented. `task add --check`, `task checks`, `task verify`, and `task evidence` provide the check/evidence path. Verification uses a supervisor lease and schema-v6 immutable records. `complete` is committed in the same transaction as a passed run and requires the latest approval, current `verifying` state, every saved check passing, and identical revision facts. `task evidence` returns stored checks, versions, separate version/check durations, statuses, digests, and revision identity across later sessions. Semi prompts once for the one-shot Writer action and refuses non-interactive input. `.talos/state.sqlite3` is checked for Git ignore status before opening local state. No CI evidence is inferred. Linux-native acceptance remains deferred.

**Exit criteria:** a failed, missing, open, stale, or incomplete check blocks `complete`; no Critic patch is applied; dangerous operations remain denied; secrets and raw check output do not enter persisted logs or reports.

### R6 — Runtime and resumption

Use tmux for persistent sessions with a visible status and a user control shell. Distinguish stopping an agent, pausing the queue, and detaching a terminal. Recovery considers PID reuse, stale leases, SIGINT/SIGTERM, CLI crash, daemon crash, reboot, and repeat SSH attach. macOS is the currently verified runtime. Implement Linux paths in T6.5; defer native Linux checks to the user's server when setting up a local Linux target or runtime would slow macOS development. Linux boot recovery and optional systemd user autostart require an explicit install choice. Do not use text typed into another TUI as IPC.

**T6.1 result (complete):** `talos attach` creates or reuses one detached shell session per canonical project root. The session name is a truncated SHA-256 path identity, so projects with the same directory basename do not collide. Outside tmux, TALOS attaches to the session; inside tmux, it switches the current client. It never starts a TALOS worker, and task commands remain usable in the foreground if tmux is unavailable. `doctor` reports the installed tmux version without creating a session.

**T6.2 result (complete):** schema v7 persists the queue pause flag, reason, and optional Git revision snapshot; project control events are append-only. `pause` blocks new Writer attempts and continuations in the same SQLite transaction that creates them, and it preserves queued work. `resume` refuses while a Writer outcome is active or unresolved. Matching Git facts resume directly; changed or unavailable facts keep the queue paused until `--reconcile`. Reconciliation moves tasks from `verifying` back to `reviewing`, requiring a new Critic decision. `status` and `up --observe` show pause state; `logs --project` displays the control journal. A running Writer is not interrupted by pause.

**T6.3 implementation (in progress):** schema v8 stores an active Writer stop request and append-only lifecycle events. `talos stop` asks one active Writer at a time to shut down and waits up to 10 seconds for a terminal result; repeat the command to request the next active Writer. Timeout and interrupt handlers terminate the process group before reaping its leader; an armed guard sends SIGKILL if cleanup itself is cancelled. After leader reap, TALOS does not signal a numeric PGID. Recovery can reclaim an unexpired task or worktree operation lease only when its recognized owner ID has a valid positive PID and timestamp suffix and that PID is absent; R7 adds the same fail-closed liveness rule for each active Writer slot. Behavior tests and full-profile verification remain open. Detached descendants after a normal CLI exit are left alone as a fail-closed limitation.

**T6.4 status (in progress):** the project-scoped tmux shell and SQLite state support reconnecting after SSH loss without starting a second worker. The macOS SSH disconnect/reconnect scenario has not been observed in this batch. Linux boot recovery and systemd user autostart remain deferred to Linux server acceptance.

**T6.5 implementation (in progress):** schema v9 stores a Linux process identity fingerprint captured at Writer launch from `/proc/<pid>/stat` plus the boot ID. `task recover` reports whether the current identity matches the saved one, differs, is unavailable, or was not recorded. This comparison is lifecycle evidence only; it never signals a process, infers a Writer outcome, or authorizes resolution or retry. Linux-native build/runtime/sandbox acceptance remains deferred until server access is available; macOS identity remains unavailable and fail-closed.

**T6.6 implementation (in progress):** `task handoff <ID>` emits a read-only, versioned context packet from existing task, Writer, Critic, verification, dependency, and worktree rows. Free-text fields are redacted and capped; prompts, patches, transcripts, raw output, current Git diff, and duplicate stored reports are omitted. The operator must inspect the working tree before a Writer action or retry. The report recommends a next explicit command but neither authorizes it nor proves an interactive conversation can resume headlessly. The live TUI→headless probe remains pending because the workspace-trust prompt was declined during the original spike.

**Exit criteria:** state survives SSH loss and is visible on reattach; repeated `up` creates no duplicate workers or panes; ambiguous attempts stop in a safe recoverable state.

### R7 — Parallel development

Start with a dependency DAG and bounded child tasks. Use native subagents only when that CLI has a documented, verified interface. Parallel file writes require separate Git worktrees and bounded concurrency. Before integration, verify base revision, ownership separation, tests, and merge-conflict status. Record worktree lifecycle; never remove a dirty worktree automatically.

**T7.1 implementation (in progress):** schema v10 stores append-only task dependency edges. A recursive prerequisite walk rejects cycles; dependency edges can only be added while the dependent task is queued. Explicit Writer launch and `up --auto`/`--semi` both require every prerequisite to be `complete`. `task dependencies` displays current prerequisite statuses; Auto and Semi report queued tasks waiting on open prerequisites. Migration and behavior checks remain pending.

**T7.2 implementation (in progress):** schema v11 stores bounded child contracts with a parent, normalized relative path scope, and a 1–1800 second Writer timeout. `task child` creates the contract and a parent-waits-for-child dependency in one transaction. The Writer receives the scope and timeout; the Critic gets the scope as acceptance context and TALOS deterministically turns any out-of-scope diff into a blocker. `task child-report` derives a compact versioned report from existing task, review, and verification records, so reports and transcripts are not duplicated in SQLite. Path scope is a review gate, not an OS sandbox. Migration and behavior checks remain pending.

**T7.3 implementation (in progress):** `doctor` reports native subagent invocation only as advertised when the installed CLI help exposes its public invocation tool. The installed Agy CLI does not expose that tool in `--help`, and the configured roster/live invocation has not been verified; native delegation remains disabled. Manual child tasks are independent of this capability.

**T7.4 implementation (in progress):** schema v12 records one worktree lifecycle per task. Local commit `2b672ea` adds `task worktree create/list/reconcile/remove`. Creation requires a clean base and unused queued task; removal is explicit, refuses tracked, untracked, and ignored content, exposes no force mode, leaves the branch in place, and returns the queued task to the primary project. Lifecycle behavior acceptance remains pending.

**T7.5 implementation (in progress):** schema v13 stores integration facts/state; v14 records the first Writer-routing event so legacy worktrees with earlier Writer history keep using the primary tree. `Project` separates execution from the primary `.talos` state/config root. An active new worktree routes Writer execution there; review and verification are refused until explicit integration. Integration requires an unchanged clean primary base, checks child scope, applies and stages the exact revision, and records recoverable intent before the filesystem change. After integration, Critic and checks run against the primary tree. Passing checks keep the task `verifying` until the user commits and `task worktree finalize` confirms that the committed primary revision, latest Critic approval, and latest passing checks agree. Dependency scheduling treats a worktree prerequisite as ready only after integration is finalized. Schema v18 adds durable per-attempt Writer slots. `max_concurrent_writers` defaults to one and permits up to three only when every overlapping Writer uses a different active managed worktree; a primary-tree Writer remains exclusive. Each SQLite slot claim verifies the selected execution root against the current route, preventing a concurrent worktree lifecycle change from sending Writer output to the wrong tree. SQLite serializes slot claims against supervisor operations, and recovery refuses to quarantine a slot owned by a live or uncheckable TALOS process. The first route event is recorded atomically with attempt creation. A rejected post-integration review routes Writer fixes to the primary tree. Worktree behavior/migration checks and the three-agent acceptance remain open; native subagent delegation is disabled until verified.

**T8.1 implementation (in progress):** `doctor` reports local `gh` availability/version without checking authentication. `talos github status --check-auth` explicitly requests `gh auth status --active --json hosts`; output is bounded, raw stdout/stderr is discarded, and only an aggregate state is reported. The auth command has not been invoked during development. Repo identity, remote access, push, PR, and CI checks remain deferred until a repository exists and the user authorizes those effects.

**T8.2 implementation (in progress):** schema-v3 project config carries a credential-free GitHub publication policy pinned to `owner/repository`, a named remote, the `master` base, and a `talos/` feature prefix. Push and PR permissions are opt-in and default false. `talos github policy` reports local policy; `talos github branch create <slug>` creates a clean local feature branch from `master` without network access. `talos github publish` checks both remote URLs, base/head commits, clean branch state, and diff identity; writes durable intent before pushing one exact branch with tag following disabled; and creates a draft PR. `talos github reconcile` inspects ambiguous push/PR outcomes and never retries an unverified effect. A read-only review found and fixed tag-following and mixed-case PR URL handling. Local compile and Clippy pass. No remote operation has been run because the user has not created the repository/remote.

**T8.3 implementation (in progress):** `talos github ci <publication_id>` verifies that the remote branch and open PR still point to the recorded commit before and after reading `gh pr checks` JSON. `--wait` polls at a bounded interval/deadline, and each check request is capped by the remaining wait time. Schema v16 stores an append-only, deduplicated snapshot with check names, workflows, buckets, states, and timestamps only; raw output, descriptions, links, and credential metadata are omitted. `pending`, `timeout`, `unknown`, and `no_checks` are never represented as passing. This evidence does not change task state. Code compiles and passes Clippy, but live behavior and migration acceptance await the user's repository; tests were not run.

**T8.4 complete:** `talos github policy` reports that merge is `manual_only` and remote branch deletion is `unsupported`. TALOS has no merge or remote branch deletion command; publication creates draft PRs only.

**Exit criteria:** dependent tasks wait for complete and integrated prerequisites; duplicate child submission does not create a second worker; real checks run after integration; separate workspaces and scopes do not collide. The configured three-agent live acceptance remains open while Agy's public delegation capability is unverified.

### R8 — GitHub

T8.1–T8.4 implement local `gh` diagnostics, explicit auth checking, policy-gated one-branch push, draft PR creation, durable reconciliation, commit-bound CI snapshots, and a manual merge boundary. The CLI has not contacted GitHub in this workspace. The user will provide the repository before live access, push, PR, or CI acceptance is attempted.

**Exit criteria:** a user-provided repository completes the end-to-end PR/CI workflow and saves evidence. Merge and remote branch deletion remain user operations.

### R9 — Reliability and release

For macOS, run clean installation, upgrade, diagnostics, long-running and fault-injection runs, process-loss scenarios, and incomplete-write recovery without destroying the last good database. Implement Linux-specific paths incrementally; run native Linux build, runtime, and package acceptance on the user's server when local setup would slow development. Document SSH, manual takeover, access policy, CLI limitations, and backups. Produce binaries only after reproducible builds and packaging checks for the platform being released.

Current preparation includes the 15-scenario inventory, a distinct hardening matrix, a Cargo-based install/update/uninstall runbook, native macOS and Linux packaging scripts, a minimum Rust version, and build manifest/checksum formats. An ARM64 macOS artifact was generated from committed source and its listed checksums verified. Byte-for-byte reproducibility, binary smoke, and install lifecycle acceptance remain pending. The Linux script is not yet run; native Linux artifact generation waits for T6.5 server access and acceptance.

**Exit criteria:** clean install on supported systems; acceptance scenarios pass with evidence; documentation matches actual supported CLI versions; no unverified feature is enabled.

## 6. Dependency order

```text
R0 → R1 → R2 → R3 → R4 → R5
                         └──→ R6
R5 + R6 → R7 → R8 → R9
```

Implementation details may change based on experiments. Preserve invariants and acceptance checks; update `docs/decisions.md`, `docs/tasks.md`, and `docs/state.md` when scope changes materially.

## 7. Risks to test early

1. **CLI resume and TUI:** `--resume` does not guarantee context survives a mode switch. The TUI-to-headless sequence remains scheduled for T6.6.
2. **Autonomy and permissions:** CLIs interpret approval/sandbox settings differently. If safe unattended mode exists only through a dangerous bypass, do not use that CLI for Auto.
3. **Crash between model and database:** an effect may occur before its result is recorded. R3/R6 need lease renewal, reconciliation, and explicit unknown outcomes.
4. **Actual isolation:** Git worktrees separate files from other workers, but do not constrain OS or network access. Add a separate boundary.
5. **API usage and quotas:** multiple workers can compete for account limits. Add concurrency limits, budget visibility, and a clear pause state.
6. **Platform differences:** macOS is the verified runtime. Implement Linux-specific code in the planned rounds. Linux process identity, build, runtime, Agy sandbox, autostart, and packages require acceptance on the user's server when those checks need Linux; keep that verification lean and do not add Docker/VM as a project dependency.
7. **GitHub side effects:** push/PR need a configured policy; merge remains manual.

## 8. Current continuation point

R3–R5 are closed. R6–R9 scheduled code and documentation are present locally; their remaining open tasks are acceptance gates recorded in `docs/state.md` and `docs/tasks.md`. Complete macOS install/update/uninstall and final-schema behavior/migration/fault acceptance when verification work resumes. T6.4/T6.6 continuity, reproducible release checks, and binary smoke also remain open. T6.5/T9.3 Linux-native checks and artifacts wait for the user's server. T8.1–T8.3 live publication/CI acceptance waits for the user's repository; T8.4 keeps merge/deletion manual. Do not create a repository, add a remote, or invoke GitHub before it is provided.
