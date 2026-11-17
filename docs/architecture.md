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

Each project has its own database at `<project-root>/.talos/state.sqlite3` and uses SQLite WAL; there is no global shared queue or project catalogue. Root discovery uses the Git root when available, then an existing `.talos` directory, common project markers, or the current directory, so TALOS can create local state for a non-Git project too. Schema v6 stores task titles and acceptance criteria, task status, append-only lifecycle events, a supervisor lease, Writer-attempt metadata and base revisions, immutable Critic review records, immutable per-task check requirements, verification runs, and per-check evidence. Each run captures its base commit, HEAD, changed paths, patch digest, exact command arrays, working directory, separate version/check durations and exit statuses, and output hashes. Raw stdout/stderr, Writer/Critic transcripts, and full review patches are not stored. `talos task evidence` reads verification history. TALOS does not yet keep a separate project plan, a project-creation history, or a snapshot of project files. See unscheduled ideas F1 and F2 in `docs/tasks.md` and their research notes.

Project role selection is stored at `.talos/config.json`, with `writer` and `critic` fields. Both roles currently default to `agy`; an explicit unknown or unverified adapter is rejected. The database stores review summaries and validated result JSON, but never the full patch or CLI transcript. R4 caps the diff at 64 KiB, serialized prompt at 96 KiB, and response at 64 KiB. These limits are context guards, not a long-term retention policy. F1 tracks future archival, retention, backup, export, and storage/token-budget decisions.

Prompts and CLI transcripts may contain proprietary project information. Runtime data stays local and must be ignored by Git; TALOS currently relies on the project to exclude `.talos/`, and Auto refuses dirty worktrees. On Unix, TALOS requires the current user to own its runtime directory and database, and enforces private directory/file mode bits. Diagnostics report process metadata and bounded error summaries, not credentials or full environment values.

## Authority and task lifecycle

Initial task states are `queued → planning → implementing → reviewing → verifying → complete`, with `blocked` and `failed` exits. The R2 core validates transitions and records each transition's time, actor, reason, and state change in an append-only journal. R4 adds `task review <ID>`: the Critic receives the full Git base, a SHA-256 identity over the full bounded patch, and task criteria in a fresh Antigravity process. A valid rejection returns the task to `implementing`; a valid approval moves it to `verifying`, never `complete`. Review fails closed on process/JSON errors, stale diffs, mismatched revisions, or exceeded context limits. R5 lets tasks snapshot named checks from config v2 and exposes `task verify` and `task evidence`. The verifier takes a lease, runs exact allowlisted argv arrays, hashes bounded output, compares Git facts before and after checks, and marks `complete` transactionally only if the latest approval and every saved check remain valid. The Critic cannot provide an applicable patch; unknown patch fields are rejected.

R3 Auto is one explicit call that processes at most one queued task from a clean Git worktree, using the verified Antigravity 1.5.10 session-only write policy. It does not loop or run checks/review; success remains `reviewing`. R5 Semi is an interactive one-shot Writer launch that prompts once and refuses non-TTY input. `task verify` is an explicit operator command. Neither preset may silently bypass a CLI's security controls or permit secret, production, destructive Git, or publish operations.

## Process supervision

Tokio starts children with argument arrays, never through a shell string. Process output is drained concurrently to avoid pipe deadlocks. Each attempt records start time, PID when available, CLI/session ID as soon as a public `init` event arrives, exit code, and timeout/termination reason. SIGINT/SIGTERM triggers graceful child shutdown followed by bounded cleanup and reaping. `task recover` quarantines unfinished attempts as `unknown`; `task resolve` requires explicit stopped-Writer/project-inspected attestations and never changes the recorded attempt outcome or starts a process. It changes task status to `reviewing`, `queued`, or `failed` for `accept-for-review`, `authorize-retry`, or `abandon`; retry execution requires a later explicit task command. `task continue` is a separate explicit action limited to the latest terminal Agy `ERROR` with exit 0 and a valid session ID; it verifies the resumed `init` ID before sending one follow-up. A process-group probe reports liveness only, not identity.

tmux is an optional terminal persistence layer. `talos attach` creates or reuses one detached user shell per canonical project root, then attaches or switches the current tmux client. The session name is derived from a truncated SHA-256 path hash; no project path or task metadata is stored in tmux. This control shell does not start a TALOS worker. Task commands remain available in the foreground if tmux is missing. R6.2 adds SQLite-backed queue pause state and append-only project control events. A pause atomically blocks new Writer attempts and continuations; it does not stop a running child. Resume checks a Git revision snapshot and requires explicit reconciliation when project facts differ or are unavailable. Reconciliation reopens `verifying` tasks to `reviewing` so the current code receives a fresh Critic review. R6.3 adds a durable operator stop request polled by the active Writer supervisor; the request closes in the same transaction that finalizes or quarantines the attempt. The SQLite queue remains authoritative if tmux is unavailable or its server restarts.

## Git and parallelism

Git worktrees provide task isolation, not an OS security boundary. One Writer runs at a time. R4 adds a separate read-only Critic process and project-local `writer`/`critic` selection in `.talos/config.json`; Antigravity 1.5.10 is currently the only adapter accepted for both roles. On Unix, timeout, interrupt, or explicit stop signals the process group while the leader PID remains unreaped; the guard sends SIGKILL if cleanup is cancelled. After reap, TALOS does not signal that numeric PGID because it may have been reused. This avoids signalling an unrelated process at the cost of leaving descendants that outlive a normally exiting CLI. Stale supervisor leases are reclaimed early only when a TALOS owner PID can be parsed and is absent; ambiguous or live owners remain protected by the lease. Parallel DAG workers require explicit file ownership, independent worktrees, bounded concurrency, merge conflict checks, and real verification before integration. R7 acceptance includes a Writer, an independent Critic, and at least one additional bounded agent. No branch is merged to the user's trunk automatically.

Schema v9 can store a Linux process fingerprint containing the boot ID, PID, process-group ID, and `/proc/<pid>/stat` start ticks. `task recover` compares the saved and current fingerprint and reports `matches`, `different_process`, `unavailable`, or `not_recorded`. The comparison is lifecycle evidence only: it does not establish a Writer outcome, permit signaling, or remove operator attestations. macOS continues to report identity as unavailable and does not use private `libproc` APIs or parse `ps`.

## Phased delivery

1. Bootstrap: apply and adapt the Agent Starter Kit, inspect pair-mailbox, and measure local CLI capabilities.
2. CLI spike: confirm headless invocation, structured output, session resume, interactive handoff, cancellation, and permission behavior for each adapter.
3. Minimal core: one proven adapter, durable task queue, process supervision, `up --auto`, task submission, status, and restart reconciliation.
4. Writer/Critic: independent read-only review, sealed proposal path, evidence-based completion.
5. Runtime: tmux, Auto/Semi policies, manual takeover, SSH reconnection, and persistent recovery.
6. Parallel work and GitHub: DAG/worktrees, push and PR creation under explicit policy, CI observation, no auto-merge.
7. Hardening: Linux/macOS matrix, failure injection, installation, and runbook.

Only measured features are marked supported. Unimplemented phases remain explicit in `docs/state.md` and the CLI help.
