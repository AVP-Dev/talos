# Project structure and ownership

This is the current and intended structure. Files are added when the corresponding round needs them; the tree is a guide, not a requirement to create empty modules.

```text
AGENTS.md                  session entry point, boundaries, engineering guidance
README.md                  overview and links to durable context
src/
  main.rs                  CLI entrypoint and top-level errors
  lib.rs                   library exports
  app.rs                   CLI command handling and reports
  cli.rs                   clap definitions
  project.rs               project root discovery and private runtime path
  db.rs                    SQLite schema, migrations, tasks, events, and leases
  adapter/                 public CLI discovery and per-vendor contracts
  process.rs               Tokio child lifecycle, output, cancellation
  supervisor.rs            orchestration, retry rules, shutdown
  revision.rs               Git revision facts and review snapshots
  worktree.rs               explicit task-worktree lifecycle operations
  evidence.rs              verification evidence and completion gate
tests/                     behavioral tests, temporary DBs/repos/processes
scripts/                   verification dispatcher and security scanner
docs/
  plan.md                  product direction and delivery rounds
  tasks.md                 status-tracked implementation backlog
  state.md                 current handoff/checkpoint for new sessions
  decisions.md             decision log and user-approved direction
  architecture.md          target boundaries and data flow
  design-manifest.md       invariants, transitions, failures, acceptance
  research/                Starter Kit, pair-mailbox, and CLI records
.talos/                    project-local runtime state (must be Git-ignored)
../.talos-worktrees/       private sibling directory for managed task worktrees
```

These paths express intended ownership, not a mandate to build every module now. Split code along actual responsibilities as each round needs it. SQLite schema, task-state transitions, and permission policy are shared contracts: update `docs/architecture.md` and `docs/design-manifest.md` when changing them. Adapter code must not write directly into another adapter's state. Process arguments use argument arrays, never concatenated shell strings.

`.talos/` contains local state, logs, leases, and runtime metadata; it must be ignored and must not be committed. Explicit task worktrees are stored in a private sibling `.talos-worktrees/<project-hash>` directory so they stay outside the source checkout and TALOS's protected state path. TALOS currently relies on the project to exclude `.talos/`; one-shot `up --auto` refuses a dirty worktree and reports unignored `.talos/` files. T5.7 tracks actionable exclusion handling. Never put credentials in project configuration or logs.
