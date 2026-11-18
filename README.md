# TALOS

TALOS is a Rust CLI-first orchestrator for local AI coding CLIs. It stores tasks, Writer attempts, and events in a durable local SQLite database, supervises one verified Antigravity Writer at a time, and preserves ambiguous work for operator review.

> **Current status:** R6.3 and R6.5 code is implemented; process-recovery, macOS SSH reconnect, and Linux server acceptance remain open. T6.6 adds a read-only task context packet, while interactive TUI-to-headless continuity remains unverified. R7.1–R7.5 code is implemented; migration, behavior, and three-agent acceptance remain open. Native Agy subagent delegation stays disabled until its public invocation interface and configured roster pass a live probe. R8.1 diagnostics, R8.2 policy-gated publication, and R8.3 commit-bound CI evidence are implemented locally; behavior acceptance and a user-provided GitHub repository remain pending. R9 has an end-to-end acceptance inventory, operator runbook, and one ARM64 macOS artifact; scenario execution, install lifecycle, and reproducibility checks remain open. No GitHub authentication or remote operation has been invoked. Linux-specific checks wait for the user's server; Linux support remains unverified. No Docker or VM is required. Antigravity 1.5.10 is the only Writer- and Critic-verified adapter. Critic approval moves a task to `verifying`; managed-worktree tasks also require explicit integration, post-integration checks, a user-created commit, and finalization. `up --auto` runs at most one task per explicit call. `up --semi` prompts before a Writer launch and requires an interactive terminal.

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
talos task depends 12 --on 7
talos task dependencies 12
talos task child 12 "Add parser support" --criteria "Parser handles the documented format" --allow src/parser.rs --timeout-seconds 900
talos task children 12
talos task child-report 13
talos task handoff 13
talos task worktree create 13
talos task run 13
talos task worktree integrate 13
talos task review 13
talos task verify 13
talos task worktree finalize 13
talos task list
talos up --auto
talos status
talos attach
talos pause --reason "Operator takeover"
talos stop
talos logs --project
talos resume
```

`talos task list` returns 25 compact task summaries by default (maximum 100 per page) and omits acceptance criteria. In JSON, use `has_more` and `next_after` to request the next page with `talos task list --after <ID>`. Use `talos task show <ID>` when you need the full criteria for one task.

`talos task dependencies`, `talos task children`, and `talos task worktree list` also use cursor pages. Dependencies and worktrees default to 25 rows (maximum 100); child contracts default to 5 rows (maximum 10) because each contract can contain many allowed paths.

`talos logs` and `talos logs --project` return the latest 25 events by default (maximum 100 per page). Use `--before <EVENT_ID>` to continue with older events; JSON includes `has_more` and `next_before`. Event history remains stored locally.

`talos task evidence <ID>` returns the latest 25 verification runs (maximum 100 per page). Use `--before <RUN_ID>` to continue with older runs; JSON includes `has_more` and `next_before`.

`talos resume --reconcile` reopens all verification approvals affected by inspected manual changes. Its output and audit event show the full task count and at most 50 task IDs.

When `up --auto` or `up --semi` finds no runnable task, it reports the total waiting count and a bounded preview of 25 tasks with up to 10 blocked prerequisites each. Use `talos task list` and `talos task dependencies <ID>` to inspect more.

Writer output reports the changed-path count and shows at most 50 paths, with each displayed name capped at a 256-byte prefix. Use `git status --short` to inspect the full set when paths are omitted or shortened.

`talos attach` creates or reconnects to one persistent tmux shell for the current project. It does not start a worker. Run the existing TALOS commands in that shell, or keep using them directly in the foreground when tmux is unavailable.

`talos pause` durably blocks new Writer attempts and continuations while preserving queued tasks. It does not stop an already-running Writer. Before resuming, TALOS compares the saved Git snapshot with the current project. If the snapshot changed or could not be captured, inspect `git diff` and `git status`, then run `talos resume --reconcile`; tasks already awaiting verification return to Critic review. `talos logs --project` shows the append-only pause and reconciliation history.

`talos stop` records an operator stop request for the active Writer and waits up to 10 seconds for confirmation. The Writer polls the durable request and terminates its process group with a bounded TERM-to-KILL sequence. A timeout means shutdown was not confirmed; inspect `talos status`, the process, and project changes, then use `talos task recover` before any retry. A hard kill of TALOS cannot send a graceful request; recovery keeps the Writer outcome unknown and requires operator inspection. Project checks also terminate their process group on timeout or interrupt. After any child leader has been reaped, TALOS does not signal its numeric process-group ID because the ID may have been reused; descendants detached from a normally exiting CLI may therefore outlive that CLI.

`up --auto` requires a clean primary Git worktree with `.talos/` ignored and processes the first queued task once. If the selected task has an active managed worktree, the Writer runs there while TALOS keeps SQLite and project configuration in the primary project. A successful attempt remains `reviewing`. For a task without a managed worktree, use the independent Critic for a structured review:

```sh
talos task review <ID>
```

Tasks can be linked into a directed acyclic graph. `task depends <TASK> --on <PREREQUISITE>` adds an append-only edge; self-dependencies and cycles are refused. Explicit runs and Auto both wait until all prerequisites have status `complete`. Use `task dependencies <TASK>` to inspect prerequisite states.

`task child <PARENT> <TITLE> --criteria ... --allow <PATH>` creates a bounded child contract with up to 64 project-relative paths and a Writer timeout of 1–1800 seconds. The parent automatically waits for each child to complete. TALOS includes the path scope in the Writer and Critic inputs and deterministically blocks Critic approval if the child diff changes an out-of-scope path. This is a review gate, not an OS sandbox. `task child-report <ID>` returns the compact `talos-child-task-report-v1` shape from existing task, review, and verification rows; TALOS does not store a second copy of the report or a full Writer transcript.

`task handoff <ID>` prints a read-only `talos-task-handoff-v1` context packet derived from current task, Writer, Critic, verification, dependency, and worktree records. It bounds and redacts free-text fields and omits prompts, patches, transcripts, raw command output, and the current Git diff. Inspect the working tree before a Writer action or retry. The packet recommends a next explicit command from recorded state; it does not launch an agent, authorize that action, or prove that an interactive CLI session can resume in headless mode. It is a compact new-context fallback when session continuity is unavailable.

`talos doctor` reports native subagent invocation as advertised only when the installed CLI help exposes its public invocation tool. Otherwise the capability remains untested and native delegation stays disabled until the configured agent roster and a harmless invocation are verified. Manual child tasks remain available.

`talos doctor` checks whether GitHub CLI (`gh`) is locally available and reports its version; it does not check authentication. `talos github status` also avoids a network call by default. Use `talos github status --check-auth` to explicitly check the active GitHub CLI authentication. During that check, `gh` may use its configured credentials. TALOS retains and reports only an aggregate state, discards raw CLI output, never invokes `gh auth token`, and does not parse, store, or expose credential values, login names, or token sources. TALOS does not create the hosted repository or configure the remote. `publish` and `ci` require the user's explicit repository policy; live commands remain unverified until the repository exists.

`talos github policy` shows the local publication policy. A schema-v3 `.talos/config.json` may include a `github` object with an `owner/repository`, remote name, `master` base, `talos/` feature prefix, and explicit `allow_push` / `allow_pull_request` flags. Both external-action flags default to false. It reports that merge is `manual_only` and remote branch deletion is `unsupported`. TALOS has no merge or remote branch deletion command. `talos github branch create <slug>` creates and switches to a clean local feature branch from `master`; it does not contact GitHub or push.

`talos github publish --branch talos/<slug> --title "..." --body "..." --confirm-push --confirm-pr` requires both policy permissions and explicit per-invocation confirmations. It verifies the fetch and push URLs, remote `master`, the clean local branch, and the exact commit/diff before writing a durable publication record and pushing only that branch. It opens a draft PR and does not merge. If a push or PR result is uncertain, inspect the publication ID and run `talos github reconcile <ID>`; this command only records a matching remote result. To create a PR during reconciliation, pass `--create-pr --title "..." --body "..."`. A conflicting or unverifiable remote result stops for manual inspection. The implementation has only been compile/lint checked locally; no repository or remote operation has been exercised.

`talos github ci <ID>` records one CI snapshot for the exact commit in a recorded open PR. Add `--wait` to poll every 15 seconds for up to 15 minutes; `--interval-seconds` and `--timeout-seconds` adjust the CI polling window. Bounded identity checks run before and after polling and can add time outside that window. The snapshot is one of `pass`, `fail`, `pending`, `timeout`, `unknown`, or `no_checks`; only `pass` means every returned check passed or was skipped and at least one check passed. Evidence is attached to the publication in SQLite, deduplicated by commit and snapshot digest, and omits raw output, descriptions, links, and credentials. Pending or incomplete results do not complete a TALOS task. The implementation follows the documented [`gh pr checks`](https://cli.github.com/manual/gh_pr_checks) JSON fields and exit code for pending checks; live behavior remains unverified until a repository is available.

Example policy fields (keep your existing `writer`, `critic`, and `checks` entries):

```json
{
  "schema_version": 3,
  "writer": "agy",
  "critic": "agy",
  "checks": [],
  "github": {
    "repository": "OWNER/REPOSITORY",
    "remote": "origin",
    "base_branch": "master",
    "feature_branch_prefix": "talos/",
    "allow_push": false,
    "allow_pull_request": false
  }
}
```

`task worktree create <ID>` explicitly creates a private linked worktree for an unused queued task, under a sibling `.talos-worktrees/<project-hash>` directory. TALOS records its branch, base commit, path, lifecycle, and first Writer-routing event in SQLite. When an active worktree is selected, `task run` routes Writer attempts there; Critic review and verification are refused until the operator integrates the patch. `task worktree integrate <ID>` requires the primary tree to be clean and still at the recorded base, checks the child path scope, then applies and stages the exact saved patch. It does not commit. Critic review and project checks then use the primary tree. A rejection routes the next Writer fix to that primary tree. After the approved revision passes checks, commit it manually in the primary project and run `task worktree finalize <ID>`; finalization checks the commit, latest review, and latest passing check evidence before marking the task complete. Prerequisites remain blocked until this finalization succeeds. `task worktree reconcile <ID>` inspects interrupted creation or integration and may reapply an exact recorded patch only when the primary tree is still clean at its saved base. `task worktree remove <ID>` only removes an active, unused queued worktree after checking that it has no tracked, untracked, or ignored files; it never force-removes the worktree or deletes its branch. The task then runs in the primary project. Integrated Writer worktrees remain dirty and are preserved for manual cleanup. Migrated worktrees with earlier Writer history keep their existing primary-tree route.

The Critic uses a fresh read-only process. A rejection moves the task to `implementing`; run the task again to start a linked Writer fix, then review the new revision. Approval moves the task to `verifying`. For a managed-worktree task, checks must run after integration in the primary project; passing checks leave the task `verifying` until the user commits and explicitly finalizes the integration. Configure named project checks in `.talos/config.json` and save them to a task:

```sh
talos task add "Describe the change" --criteria "Describe how to verify it" --check unit
talos task checks <ID> --check unit
talos task verify <ID>
talos task evidence <ID>
```

Checks use argument arrays from a fixed executable allowlist; TALOS does not invoke a shell. The task stores an immutable snapshot of each selected command and working directory. Verification records exact Git facts before and after checks, separate command/version durations and exits, and stdout/stderr hashes. It does not save raw check output. A failed or stale check leaves the task open.

For an explicit task instead of Auto, use `talos task run <ID>`. If Agy returns a terminal `ERROR` with exit code zero and a valid public conversation ID, inspect the project and continue only with `talos task continue <ID> --confirm-project-inspected`. TALOS resumes that exact conversation, checks the matching `init` ID before sending one follow-up, and leaves success in `reviewing`.

After an interrupted or crashed Writer run, use `talos task recover` to quarantine persisted `launching` and `running` attempts as `unknown`. On Linux, TALOS stores a `/proc` process fingerprint and reports whether it still matches; on macOS, the check remains unavailable. This is lifecycle evidence only. TALOS will not infer an attempt outcome, signal, resume, or retry it. Inspect both process and project state before resolving an attempt:

```sh
talos task resolve <ATTEMPT_ID> --action accept-for-review \
  --confirm-writer-stopped --confirm-project-inspected \
  --reason "Inspected the process and project changes"
```

Available actions are `accept-for-review`, `authorize-retry`, and `abandon`. They set the task to `reviewing`, `queued`, or `failed`, respectively, while the attempt outcome remains `unknown`. `authorize-retry` does not launch a Writer; a later explicit `task run` or `up --auto` is required. An unresolved attempt with a recorded process group is not resolvable while that group exists or cannot be probed. The process-group probe does not identify the Writer or detect escaped descendants, so the operator attestations remain authoritative.

Runtime state is written to `.talos/state.sqlite3`; Git projects must already ignore that file. TALOS refuses a tracked database or missing ignore rule and does not edit project ignore files. `up --auto` also refuses a dirty worktree. On Unix, TALOS restricts the runtime directory to mode `0700` and the database file to `0600`. Observe mode never launches an agent or worker. The explicit Writer command is verified only for Antigravity 1.5.10; its session-only policy denies reads and writes under `.talos/` and `.git/`, terminal commands, MCP tools, and URL tools. It never uses a dangerous permission bypass. TALOS does not merge to the trunk automatically and runs without Agent Starter Kit at runtime.

`.talos/config.json` can select `writer` and `critic` independently and, with schema version 2, define local project checks. Both roles currently default to `agy`; TALOS refuses adapters that have not passed the required role-specific checks. Critic revision capture streams Git output into bounded buffers: patches are limited to 64 KiB and changed paths to 4096 entries of at most 1024 bytes each. The serialized prompt is limited to 96 KiB and the structured result to 64 KiB. TALOS does not save full CLI transcripts, review patches, or check output in SQLite. Use `task evidence <ID>` to inspect persisted verification metadata and output hashes.

Example check configuration:

```json
{
  "schema_version": 2,
  "writer": "agy",
  "critic": "agy",
  "checks": [
    {
      "id": "unit",
      "argv": ["cargo", "test", "--locked", "--all-targets"],
      "version_argv": ["cargo", "--version"],
      "working_directory": "."
    }
  ]
}
```

## Project documents

- [Operator runbook](docs/operator-runbook.md)
- [Plan and rounds](docs/plan.md)
- [Verification policy and hardening matrix](docs/verification.md)
- [Task backlog](docs/tasks.md)
- [Current status and session handoff](docs/state.md)
- [End-to-end acceptance scenarios](docs/acceptance-scenarios.md)
- [Architecture](docs/architecture.md)
- [Design manifest](docs/design-manifest.md)
- [Project structure and ownership](docs/project-structure.md)
- [Agent routing and session continuity](docs/agent-routing.md)
- [Critic review contract](docs/critic-review-contract.md)
- [Glossary](docs/glossary.md)
- [Onboarding protocol](docs/onboarding-protocol.md)
- [Decisions](docs/decisions.md)
- [Starter Kit integration](docs/research/starter-kit.md)
- [pair-mailbox assessment matrix](docs/research/pair-mailbox-assessment.md)
- [CLI research and unverified limits](docs/research/cli-spike.md)
- [Project history idea](docs/research/project-history-idea.md)
- [Agent handoff idea](docs/research/agent-language-idea.md)

TALOS targets Rust stable 1.94 or newer. macOS is the verified development and runtime platform. Linux process identity code is implemented; defer native Linux build, runtime, boot, sandbox, and packaging checks to the user's server. Do not describe Linux as verified before that acceptance passes. R5 added interactive Semi approval for Writer launch, automated project checks, and evidence-gated completion.
