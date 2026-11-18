# Project structure and ownership

This is the current and intended structure. Files are added when the corresponding round needs them; the tree is a guide, not a requirement to create empty modules.

```text
AGENTS.md                  session entry point, boundaries, engineering guidance
README.md                  overview and links to durable context
Cargo.toml                 package metadata and Rust minimum version
Cargo.lock                 locked dependency resolution
src/
  main.rs                   binary entrypoint
  lib.rs                    library exports
  app.rs                    command execution, reports, and orchestration entry points
  cli.rs                    Clap argument and subcommand definitions
  config.rs                 project configuration and GitHub policy
  db.rs                     SQLite schema, migrations, task state, events, and leases
  adapter/                  7 provider profiles (agy, codex, claude, omp, pi, kilo, opencode)
  checks.rs                 project check execution and evidence
  github.rs                 GitHub CLI probes and policy-gated operations
  policy.rs                 Writer authority and execution-mode rules
  process_group.rs          Unix child process-group control
  process_identity.rs       platform process identity, including Linux procfs
  project.rs                project root discovery and private runtime path
  rate_limit.rs             intelligent rate-limit/quota detection & adaptive backoff
  review.rs                 structured Critic review contracts
  revision.rs               Git revision facts and review snapshots
  secrets.rs                credential detection and output redaction
  summary.rs                deliverable task summary artifact & token economy truncation
  supervisor.rs             Writer/Critic lifecycle and recovery coordination
  team.rs                   two-tier team orchestration (Writer Team & Critic Team)
  tmux.rs                   optional persistent terminal control session
  tui.rs                    unified terminal orchestrator cockpit (Ratatui/Crossterm)
  worktree.rs               explicit task-worktree lifecycle and integration
  inline test modules      unit and behavior tests colocated with modules
scripts/                    verification profile, scanner, and macOS/Linux packagers
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
dist/                      ignored local release artifacts
```

These paths express intended ownership, not a mandate to build every module now. Split code along actual responsibilities as each round needs it. SQLite schema, task-state transitions, and permission policy are shared contracts: update `docs/architecture.md` and `docs/design-manifest.md` when changing them. Adapter code must not write directly into another adapter's state. Process arguments use argument arrays, never concatenated shell strings.

`.talos/` contains local state, logs, leases, and runtime metadata; it must be ignored and must not be committed. TALOS checks the ignore rule before opening the database and refuses tracked or unignored state; it does not edit project ignore files. Explicit task worktrees are stored in a private sibling `.talos-worktrees/<project-hash>` directory so they stay outside the source checkout and TALOS's protected state path. `dist/` contains ignored local package output. Never put credentials in project configuration or logs.
