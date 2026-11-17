# TALOS

TALOS is a Rust CLI-first orchestrator for local AI coding CLIs. It stores tasks, Writer attempts, and events in a durable local SQLite database, supervises one verified Antigravity Writer at a time, and preserves ambiguous work for operator review.

> **Current status:** R4 is complete for the verified macOS path. Linux-specific implementation remains in scope. If Linux build, runtime, or sandbox checks require cumbersome setup on macOS, defer those checks to the user's Linux server under T6.5/T9.3; do not claim Linux support until they pass. No Docker or VM test environment is required. Antigravity 1.5.10 is the only Writer- and Critic-verified adapter. Writer success leaves the task in `reviewing`; Critic approval moves it to `verifying`. TALOS does not run project checks or close tasks until R5. `up --auto` runs at most one task per explicit call; `up --semi` remains disabled until R5.

## Start here

Any agent or new session should read, in order:

1. [Current handoff](docs/state.md)
2. [Roadmap](docs/plan.md)
3. [Backlog and acceptance criteria](docs/tasks.md)
4. [Architecture decisions](docs/decisions.md)

This keeps progress independent of chat history, model vendor, or which agent continues the work.

## Available first workflow

```sh
talos up --observe
talos task add "Describe the change" --criteria "Describe how to verify it"
talos task list
talos up --auto
talos status
```

`up --auto` requires a clean Git worktree with `.talos/` ignored and processes the first queued task once. It reports the Writer response and changed Git paths. A successful attempt remains `reviewing`. Use the independent Critic for a structured review:

```sh
talos task review <ID>
```

The Critic uses a fresh read-only process. A rejection moves the task to `implementing`; run the task again to start a linked Writer fix, then review the new revision. Approval moves the task to `verifying`, not `complete`; run and inspect project checks manually until R5 adds the verification gate.

For an explicit task instead of Auto, use `talos task run <ID>`. If Agy returns a terminal `ERROR` with exit code zero and a valid public conversation ID, inspect the project and continue only with `talos task continue <ID> --confirm-project-inspected`. TALOS resumes that exact conversation, checks the matching `init` ID before sending one follow-up, and leaves success in `reviewing`.

After an interrupted or crashed Writer run, use `talos task recover` to quarantine persisted `launching` and `running` attempts as `unknown`. TALOS will not infer their outcome, signal, resume, or retry them. Inspect both process and project state before resolving an attempt:

```sh
talos task resolve <ATTEMPT_ID> --action accept-for-review \
  --confirm-writer-stopped --confirm-project-inspected \
  --reason "Inspected the process and project changes"
```

Available actions are `accept-for-review`, `authorize-retry`, and `abandon`. They set the task to `reviewing`, `queued`, or `failed`, respectively, while the attempt outcome remains `unknown`. `authorize-retry` does not launch a Writer; a later explicit `task run` or `up --auto` is required. An unresolved attempt with a recorded process group is not resolvable while that group exists or cannot be probed. The process-group probe does not identify the Writer or detect escaped descendants, so the operator attestations remain authoritative.

Runtime state is written to `.talos/state.sqlite3`; ensure `.talos/` is ignored by Git. TALOS does not add ignore rules yet, and `up --auto` refuses a dirty worktree. On Unix, TALOS restricts the runtime directory to mode `0700` and the database file to `0600`. Observe mode never launches an agent or worker. The explicit Writer command is verified only for Antigravity 1.5.10; its session-only policy denies reads and writes under `.talos/` and `.git/`, terminal commands, MCP tools, and URL tools. It never uses a dangerous permission bypass. TALOS does not merge to the trunk automatically and runs without Agent Starter Kit at runtime.

`.talos/config.json` can select `writer` and `critic` independently. Both currently default to `agy`; TALOS refuses adapters that have not passed the required role-specific checks. Review inputs are bounded to a 64 KiB patch and a 96 KiB prompt, and results to 64 KiB. TALOS does not save full CLI transcripts or review patches in SQLite.

## Project documents

- [Plan and rounds](docs/plan.md)
- [Task backlog](docs/tasks.md)
- [Current status and session handoff](docs/state.md)
- [Architecture](docs/architecture.md)
- [Design manifest](docs/design-manifest.md)
- [Decisions](docs/decisions.md)
- [Starter Kit integration](docs/research/starter-kit.md)
- [pair-mailbox assessment matrix](docs/research/pair-mailbox-assessment.md)
- [CLI research and unverified limits](docs/research/cli-spike.md)

TALOS targets Rust stable. macOS is the verified development and runtime platform. Implement Linux-specific paths as their planned tasks are reached; defer checks that need a Linux environment to the user's server when local setup would slow macOS work. Do not describe Linux as verified before that acceptance passes. Semi mode, independent Critic review, automated project checks, and evidence-gated completion are later work.
