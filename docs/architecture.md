# TALOS Architecture

## Product Boundary

TALOS is a local modular monolith. A single Rust executable coordinates external AI CLI processes. It does not implement model inference, expose a local network service, or require an agent-development kit at runtime.

```text
clap CLI ── task/control API ── SQLite WAL
                 │
                 ├── queue and state transitions
                 ├── 7 adapter capability discovery (agy, codex, claude, omp, pi, kilo, opencode)
                 ├── dual orchestration engines (Team Mode & Pair Mode)
                 ├── Tokio process supervisor
                 ├── verification and independent review gates
                 ├── Git worktree / 3-pane tmux integration
                 └── policy-gated GitHub publication / CI evidence
```

Adapters are replaceable modules selected by role from implemented and role-verified profiles. Each owns CLI-specific discovery, fixed argument construction, structured output, session behavior, and permission checks; shared orchestration does not assume all CLIs have the same session model, output format, approval policy, or subagent support.

**Supported Adapter Profiles:**
TALOS ships with seven built-in adapter profiles (`src/adapter/`):
1. `agy` — Antigravity CLI (`agy 1.5.11+`): native headless protocol with stream parsing, strict session isolation, and token accounting.
2. `codex` — OpenAI Codex CLI (`v0.169.0+`): headless JSON invocation, write-safety enforcement, and prompt streaming.
3. `claude` — Claude Code (`claude --print`): non-interactive headless execution, role-bound permissions, and structured review audits.
4. `omp` — Oh-My-Prompt / OMP runner: headless execution with model selection and process supervision.
5. `pi` — Pi coding agent: headless task dispatch and verification integration.
6. `kilo` — Kilo CLI: structured command runner with local worktree bounds.
7. `opencode` — OpenCode CLI: headless agent execution with strict filesystem boundaries.

Each adapter can be assigned to either the **Writer** role or the **Critic** role, with optional model overrides configured via `.talos/config.json` or `talos config set`.

## Dual Orchestration Modes

TALOS provides two complementary orchestration modes:

1. **Pair Mode (`pair`, default):**
   Sequential, evidence-bound pipeline: Writer → Critic → Automated Verification → Completion.
   - One primary Writer attempt at a time (or bounded worktree attempts).
   - Independent read-only Critic review gate.
   - Deterministic automated verification gate before marking tasks complete.

2. **Team Mode (`--mode team`):**
   Two-tier team orchestration designed for multi-task and parallel workflows:
   - **Writer Lead:** Autonomously plans and decomposes parent tasks into child subtasks, manages parallel Writer Workers in isolated Git worktrees, monitors progress, detects stalled or failed workers (e.g. via `RETRY WITH GUIDANCE` or takeover), and integrates validated worktree patches into the primary branch.
   - **Critic Team:** Performs multi-angle audits (functional correctness, security, architectural consistency), while the **Critic Lead** aggregates findings and issues authoritative verdicts (`approve` or `request_changes` with actionable rebuttal guidance).
   - **Fix Loops:** Automated feedback cycles where Critic findings are directly routed back to the Writer Lead or Workers for targeted resolution.

## Persistent State

Each project has its own database at `<project-root>/.talos/state.sqlite3` and uses SQLite WAL; there is no global shared queue or project catalogue. Root discovery uses the Git root when available, then an existing `.talos` directory, common project markers, or the current directory, so TALOS can create local state for a non-Git project too.

Schema v19 stores:
- Task titles, acceptance criteria, statuses, and append-only lifecycle events.
- Supervisor leases and durable per-attempt Writer execution leases.
- Writer attempt metadata, adapter IDs, observed versions, and base revisions.
- Immutable Critic review records, findings, and verdicts.
- Dependency DAG edges and bounded child-task contracts.
- Team-mode subtasks, worker assignments, lead decisions, and parent lookups.
- Task-worktree lifecycle and patch integration records.
- Immutable per-task check requirements, verification runs, and per-check evidence.
- GitHub publication intent, branch transitions, and deduplicated CI snapshots.

`talos task list` returns cursor-paged summaries without acceptance criteria; `talos task show` reads one full task record. Each verification run captures its base commit, HEAD, changed paths, patch digest, exact command arrays, working directory, separate version/check durations and exit statuses, and output hashes. GitHub CI snapshots are bound to a recorded open PR's immutable head commit and contain only check names, workflow names, buckets, states, and timestamps. Raw stdout/stderr, Writer/Critic transcripts, full review patches, GitHub descriptions/links, and credentials are not stored. `talos task evidence` reads verification history. `talos task handoff` builds a bounded read-only context packet from existing rows on demand; it writes no duplicate report or transcript and cannot establish session continuity.

Project role selection is stored at `.talos/config.json`, which can be inspected or modified via `talos config` and `talos config set`.

Prompts and CLI transcripts may contain proprietary project information. Runtime data stays local and is ignored by Git; TALOS automatically ensures `.talos/` is appended to `.gitignore` on first run to protect the database and task state. On Unix, TALOS requires the current user to own its runtime directory and database, and enforces private directory/file mode bits (`0700`/`0600`). Diagnostics report process metadata and bounded error summaries, not credentials or full environment values.

## Authority and Task Lifecycle

Task states transition through:
`queued → planning → implementing → reviewing → verifying → complete`, with `blocked` and `failed` exits.

The core validates transitions and records each transition's time, actor, reason, and state change in an append-only journal:
- `task review <ID>`: the Critic receives the full Git base, a SHA-256 identity over the full bounded patch, and task criteria in a fresh read-only process. A valid rejection returns the task to `implementing`; a valid approval moves it to `verifying`, never `complete`. Review fails closed on process/JSON errors, stale diffs, mismatched revisions, or exceeded context limits.
- `task verify <ID>`: runs exact allowlisted argv arrays from `.talos/config.json`, hashes bounded output, compares Git facts before and after checks, and marks `complete` transactionally only if the latest approval and every saved check remain valid. The Critic cannot provide an applicable patch; unknown patch fields are rejected.
- `up --auto`: processes runnable queued tasks to completion, supporting both Pair mode (default) and Team mode (`--mode team`).

## Process Supervision & Interactive Workspace

Tokio starts children with argument arrays, never through a shell string. Process output is drained concurrently to avoid pipe deadlocks. SIGINT/SIGTERM triggers bounded child cleanup and reaping. `task recover` quarantines unfinished attempts as `unknown`; `task resolve` requires explicit stopped-Writer/project-inspected attestations and never changes the recorded attempt outcome or starts a process.

**Interactive 3-Pane Tmux Workspace:**
When executed without arguments (`talos`) or via `talos attach`, TALOS checks whether tmux is available. Outside tmux, it creates or reconnects to an interactive 3-pane layout:
```text
┌───────────────────────────────────────┬───────────────────────────────────────┐
│ 🤖 Pane 1: Writer Stream (top-left)   │ 🕵️ Pane 2: Critic Stream (top-right)  │
│ Live streaming agent output, file     │ Live audit diff, reviewer analysis,   │
│ edits, and unit test execution.       │ defect notes, and verdicts.           │
├───────────────────────────────────────┴───────────────────────────────────────┤
│ 🎮 Pane 3: Console (bottom, focused)                                          │
│ Interactive operator prompt for queueing tasks and controlling execution.     │
│ talos> _                                                                      │
└───────────────────────────────────────────────────────────────────────────────┘
```
The session persists across laptop lid closure or SSH disconnection. Re-running `talos` reattaches immediately to the active workspace.

## Git and Worktree Isolation

Git worktrees provide clean task isolation. Writer concurrency defaults to one and can be configured up to three in `.talos/config.json`. SQLite atomically admits each Writer attempt only when every overlapping attempt uses a distinct active managed worktree. A primary-tree Writer remains exclusive.

Parallel DAG workers use explicit file ownership, independent worktrees, bounded concurrency, merge conflict checks, and real verification before integration. No branch is merged to the user's trunk automatically.
