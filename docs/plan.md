# TALOS Project Plan

**Status:** R1–R4 are complete for the verified macOS path. R4 adds independent read-only Critic review, immutable review history, and a linked reject/fix/review cycle. A valid Critic approval moves a task to `verifying`; project checks and evidence-gated completion remain R5 work. `up --semi` remains deferred to R5. Linux-specific implementation remains in scope. Linux build, runtime, and Agy sandbox checks that require a Linux environment may be deferred to the user's server under T6.5/T9.3 if setting them up on macOS would slow development; no Docker or VM setup is required.

## 1. Goal

TALOS is a local CLI-first orchestrator that enables multiple AI coding CLIs to work autonomously on development tasks. From a project directory, the user runs `talos up --auto`, submits a task, and returns later to work that has continued or recovered.

The system detects the project, stores its queue and context locally, launches suitable CLIs through their public interfaces, assigns a Writer and an independent Critic, verifies code and acceptance criteria, and recovers after terminal, SSH, or process loss. The goal is a practical executable, not an architecture experiment.

macOS is the current verified development and runtime platform. Keep shared code portable and implement Linux-specific paths as their planned tasks are reached. If Linux-target compilation, runtime, or Agy sandbox checks require cumbersome setup on macOS, defer those checks to the user's Linux server under T6.5/T9.3. Do not add a Docker/VM requirement or claim verified Linux support before the server acceptance passes. Linux validation is not an R3 acceptance dependency.

## 2. Design principles and boundaries

- **Local modular monolith:** one Rust executable and SQLite WAL. No required MCP server, Python runtime, Docker, Redis, message broker, Kubernetes, or network database.
- **Incremental delivery:** the first working slice uses one CLI. Other adapters, parallelism, SSH, and GitHub come after the foundation is proven.
- **Deterministic core:** Rust controls state, permissions, processes, queues, and evidence. Models propose technical decisions within granted authority.
- **Use pair-mailbox experience:** transfer lessons, not code: one Writer, an independent Critic, atomic write authority, content-verified patches, ground-truth tree checks, durable tasks, an event journal, a distinction between silence and process death, and recovery from observed state. See the detailed [pair-mailbox assessment matrix](research/pair-mailbox-assessment.md).
- **Use Agent Starter Kit as guidance:** ordinary practices are recommendations; explicit safety and data-preservation boundaries remain hard limits. Starter Kit is not a product dependency.
- **No false evidence:** CLI help, a successful model response, a mock, or an agent's claim alone does not prove session continuity or task completion.
- **Bounded autonomy:** Auto skips repeated prompts for routine, explicitly permitted in-project work. It does not disable CLI controls, grant unrestricted access, allow production operations, or merge automatically.
- **English-only system:** all code, prompts, CLI output, logs, reports, examples, and documentation are in English.
- **Development and platform support:** macOS is the current verified platform. Linux-specific implementation is in scope; do not claim verified Linux support until the Linux build/runtime/sandbox acceptance in T6.5 and T9.3 passes.

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
talos up --semi             # planned for R5 policy work; not part of initial R3 Auto
talos task add "..."        # add a task with a verifiable goal
talos task run <ID>         # implemented in R3 for the verified Writer
talos task review <ID>      # implemented in R4 for the independent Critic
talos task continue <ID> --confirm-project-inspected  # implemented for one eligible terminal ERROR result
talos task recover          # implemented: quarantine ambiguous attempts as unknown
talos task resolve <ATTEMPT_ID> ...  # implemented: audited operator decision
talos status                # session, worker, queue, and capability status
talos logs [TASK_ID]        # task event history
talos attach                # planned for R6: return to the tmux session
talos pause | resume        # planned for R6: stop assigning work / continue
talos doctor                # diagnose CLI, tmux, Git, and database setup
talos stop                  # planned for R6: graceful worker shutdown
```

R3 Writer commands and the R4 `task review` command are implemented for the verified macOS Antigravity 1.5.10 path. `up --semi`, `attach`, `pause`, `resume`, and `stop` remain later policy/runtime work. `task continue` is limited to one explicit follow-up for the latest terminal Agy `ERROR` result with exit status zero and a validated public conversation ID. It never handles `unknown`, timeout, nonzero exit, or malformed output. TALOS resumes that exact ID through the public `--conversation` interface, verifies the resumed `init` ID before sending the follow-up, and asks the operator to inspect current workspace state without replaying the original prompt. A successful follow-up remains `reviewing`. Critic approval moves a task only to `verifying`; it does not claim project checks or completion.

## 4. Roadmap and rounds

Each round ends with a usable, verifiable result, an updated backlog, and a session handoff. Proceed when the round's acceptance criteria are observable; do not wait for future features to demonstrate intermediate value.

| Round | Direction | Round result |
|---|---|---|
| R0 | Bootstrap and research | Starter Kit adapted, pair-mailbox lessons recorded, installed CLI versions captured. Complete. |
| R1 | CLI spike | Headless JSON and same-ID resume verified for Antigravity; TUI handoff and native delegation remain deferred. R1 is complete; R3 later verified the write boundary. |
| R2 | Core and durable state | Buildable Rust CLI, project discovery, SQLite WAL, schema migration, task queue/transitions, event journal, leases, status/doctor. Complete. |
| R3 | One autonomous Writer | One verified adapter completes one safe local task per explicit `up --auto` invocation from a clean temporary Git project. Complete for the initial macOS target. |
| R4 | Independent Critic | Read-only review, real blocker finding, denied write probe, linked Writer fix, immutable review history, and per-project role selection. Complete for verified macOS Antigravity 1.5.10. |
| R5 | Verification and permissions | Acceptance criteria, real project checks, Git facts, patch digest, Auto/Semi allowlist, and evidence-gated completion. |
| R6 | Persistent runtime and recovery | tmux, reconnect/manual takeover, cancellation, CLI/TALOS crash, SSH disconnect, and optional Linux reboot autostart. |
| R7 | Decomposition and parallel work | DAG and subagent tasks, isolated worktrees, leases/concurrency limits, conflict checks, and safe integration. |
| R8 | GitHub workflow | Authorized branch, push and PR, CI monitoring, and report; no automatic merge. |
| R9 | Hardening and release | Resilience, long-running checks, macOS install/update/uninstall, complete operator documentation, and Linux build/runtime/package acceptance. |

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

One-shot `up --auto` now requires a clean Git worktree and processes at most one queued task per invocation. It reports the response and changed Git paths. Fake and live Agy 1.5.10 disposable-project acceptance verified a changed marker file, task visibility after a separate TALOS process, no duplicate execution, the one-task limit with two queued tasks, and no false completion. No project checks or Critic review run in R3; tasks stay `reviewing`. `up --semi` remains R5 work. Linux validation is unverified and does not hold R3 open; no Docker/VM setup was used.

### R4 — Writer/Critic

Add a separate Critic role, preferably using another vendor/CLI when available. Project configuration will select Writer and Critic adapters independently from the CLIs whose required interfaces and authority boundaries have been verified. The Critic receives the base/new revisions and acceptance criteria; its checkout or file permissions are actually read-only. Review output is structured as path/line/reason/severity findings or an approval with limits. A rejection starts a new Writer iteration. Role and authority persist independently of process restarts. T7.5 later verifies a bounded three-agent flow with one Writer, one independent Critic, and one additional scoped agent.

**Exit criteria:** a realistic defect found by Critic blocks closure; Critic cannot write; repeated Writer/Critic cycles preserve work.

**Result (completed):** T4.1–T4.6 are implemented. `task review <ID>` starts a fresh Antigravity 1.5.10 process without a Writer conversation ID or transcript. The review request binds acceptance criteria and a complete patch to the full base commit and a SHA-256 revision ID. The patch, serialized prompt, and structured response are capped at 64 KiB, 96 KiB, and 64 KiB; TALOS never truncates review context. A valid approval moves the task to `verifying`, a rejection moves it to `implementing`, and `complete` remains unavailable. Historical review rows are immutable. A live review approved a simple revision; another live review found a real empty-input defect, recorded a blocker, and moved the task to `implementing`. A fake Writer fix started a linked second attempt from the same base. A separate live write probe confirmed that Agy's `write_file` call was denied and the marker file was absent. `cargo test`, Clippy, release build, and the security scan passed at the R4 close checkpoint; exact current run results are in `docs/state.md`. CLI and model probes used disposable projects only. Linux-native acceptance remains unverified and stays under T6.5/T9.3.

### R5 — Ground truth, verification, and permissions

Include verifiable acceptance criteria and project commands. Record the exact commands, working directory, exit code, duration, and commit/diff identity; bound output and redact secrets. Collect Git facts such as HEAD, dirty paths, and diff digest. Verify a Critic patch against its content digest immediately before applying. Auto/Semi classify actions using an explicit policy.

**Exit criteria:** a failed test, missing file, open review, or changed digest blocks `complete`; CI is not claimed as local evidence unless its status was retrieved; dangerous operations are excluded from Auto defaults.

### R6 — Runtime and resumption

Use tmux for persistent sessions with a visible status and a user control shell. Distinguish stopping an agent, pausing the queue, and detaching a terminal. Recovery considers PID reuse, stale leases, SIGINT/SIGTERM, CLI crash, daemon crash, reboot, and repeat SSH attach. macOS is the currently verified runtime. Implement Linux paths in T6.5; defer native Linux checks to the user's server when setting up a local Linux target or runtime would slow macOS development. Linux boot recovery and optional systemd user autostart require an explicit install choice. Do not use text typed into another TUI as IPC.

**Exit criteria:** state survives SSH loss and is visible on reattach; repeated `up` creates no duplicate workers or panes; ambiguous attempts stop in a safe recoverable state.

### R7 — Parallel development

Start with a dependency DAG and bounded child tasks. Use native subagents only when that CLI has a documented, verified interface. Parallel file writes require separate Git worktrees and bounded concurrency. Before integration, verify base revision, ownership separation, tests, and merge-conflict status. Record worktree lifecycle; never remove a dirty worktree automatically.

**Exit criteria:** independent tasks run in parallel without write collisions; dependent tasks wait; duplicate child submission does not create a second worker; real checks run after integration.

### R8 — GitHub

Use `gh` or another supported official GitHub interface after a dedicated capability/security review. Create a feature branch and PR, include report/evidence, and wait for CI. Do not include local push/PR in a general allowlist without an explicit configured policy. TALOS never auto-merges.

**Exit criteria:** end-to-end PR workflow completes after CI and saves the link; merge remains a user decision.

### R9 — Reliability and release

For macOS, run clean installation, upgrade, diagnostics, long-running and fault-injection runs, process-loss scenarios, and incomplete-write recovery without destroying the last good database. Implement Linux-specific paths incrementally; run native Linux build, runtime, and package acceptance on the user's server when local setup would slow development. Document SSH, manual takeover, access policy, CLI limitations, and backups. Produce binaries only after reproducible builds and packaging checks for the platform being released.

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

R3 is closed. The next planned task is **T4.1 — define the independent Critic input/output contract and revision identity**. Linux-specific implementation and any deferred Linux server acceptance remain tracked in T6.5/T9.3 and do not reopen R3.
