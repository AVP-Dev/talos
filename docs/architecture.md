# TALOS architecture

## Product boundary

TALOS is a local modular monolith. A single Rust executable coordinates external AI CLI processes. It does not implement model inference, expose a local network service, or require an agent-development kit at runtime.

```text
clap CLI ── task/control API ── SQLite WAL
                 │
                 ├── queue and state transitions
                 ├── adapter capability discovery
                 ├── Tokio process supervisor
                 ├── verification and independent review gates
                 └── Git worktree / tmux integration
```

Adapters are replaceable modules selected by role from the adapters that have been implemented and verified. Each owns CLI-specific discovery, command arguments, structured output, and session behavior; shared orchestration must not assume all CLIs have the same session model, output format, approval policy, or subagent support. The current tree has an Agy probe in `src/adapter/agy.rs` and its headless process protocol in `src/adapter/agy_process.rs`. Extract a shared low-level process runner only when a second real adapter demonstrates the common contract.

## Persistent state

Each project has its own database at `<project-root>/.talos/state.sqlite3` and uses SQLite WAL; there is no global shared queue. The canonical project root is discovered through Git, an existing `.talos` directory, common project markers, or the current directory. Schema v3 stores tasks, lifecycle events, a singleton supervisor lease with a monotonically increasing fencing token, and durable Writer-attempt metadata including PID, prompt digest, early public session ID, result, resolution, and continuation linkage. It does not store prompts or transcripts. Linux process identity, checkpoints, verification evidence, and worker lease renewal remain later work.

Prompts and CLI transcripts may contain proprietary project information. Runtime data stays local and must be ignored by Git; TALOS currently relies on the project to exclude `.talos/`, and Auto refuses dirty worktrees. On Unix, TALOS requires the current user to own its runtime directory and database, and enforces private directory/file mode bits. Diagnostics report process metadata and bounded error summaries, not credentials or full environment values.

## Authority and task lifecycle

Initial task states are `queued → planning → implementing → reviewing → verifying → complete`, with `blocked` and `failed` exits. The R2 core validates transitions and records each transition's time, actor, reason, and state change in an append-only journal. Worker ownership, read-only Critic enforcement, and evidence-gated completion are implemented in later rounds.

R3 Auto is one explicit call that processes at most one queued task from a clean Git worktree, using the verified Antigravity 1.5.10 session-only write policy. It does not loop or run checks/review; success remains `reviewing`. Semi and its policy-boundary approvals are deferred to R5. Neither preset may silently bypass a CLI's security controls or permit secret, production, destructive Git, or publish operations.

## Process supervision

Tokio starts children with argument arrays, never through a shell string. Process output is drained concurrently to avoid pipe deadlocks. Each attempt records start time, PID when available, CLI/session ID as soon as a public `init` event arrives, exit code, and timeout/termination reason. SIGINT/SIGTERM triggers graceful child shutdown followed by bounded cleanup and reaping. `task recover` quarantines unfinished attempts as `unknown`; `task resolve` requires explicit stopped-Writer/project-inspected attestations and never changes the recorded attempt outcome or starts a process. It changes task status to `reviewing`, `queued`, or `failed` for `accept-for-review`, `authorize-retry`, or `abandon`; retry execution requires a later explicit task command. `task continue` is a separate explicit action limited to the latest terminal Agy `ERROR` with exit 0 and a valid session ID; it verifies the resumed `init` ID before sending one follow-up. A process-group probe reports liveness only, not identity.

tmux is an optional terminal persistence layer. It keeps a human control shell and supervised workers visible across terminal/SSH disconnects. The SQLite queue remains authoritative if tmux is unavailable or its server restarts.

## Git and parallelism

Git worktrees provide task isolation, not an OS security boundary. R3 supports one Writer at a time; no Critic or role-selection configuration is implemented yet. R4 will add an independent read-only Critic and project-level selection between verified Writer/Critic adapters. Parallel DAG workers require explicit file ownership, independent worktrees, bounded concurrency, merge conflict checks, and real verification before integration. R7 acceptance includes a Writer, an independent Critic, and at least one additional bounded agent. No branch is merged to the user's trunk automatically.

## Phased delivery

1. Bootstrap: apply and adapt the Agent Starter Kit, inspect pair-mailbox, and measure local CLI capabilities.
2. CLI spike: confirm headless invocation, structured output, session resume, interactive handoff, cancellation, and permission behavior for each adapter.
3. Minimal core: one proven adapter, durable task queue, process supervision, `up --auto`, task submission, status, and restart reconciliation.
4. Writer/Critic: independent read-only review, sealed proposal path, evidence-based completion.
5. Runtime: tmux, Auto/Semi policies, manual takeover, SSH reconnection, and persistent recovery.
6. Parallel work and GitHub: DAG/worktrees, push and PR creation under explicit policy, CI observation, no auto-merge.
7. Hardening: Linux/macOS matrix, failure injection, installation, and runbook.

Only measured features are marked supported. Unimplemented phases remain explicit in `docs/state.md` and the CLI help.
