# TALOS Architecture

TALOS is a local Rust orchestrator for installed AI coding CLIs. One executable owns project discovery, the cockpit and command-line interface, the task queue, process supervision, and review and verification gates. SQLite WAL is the durable state store. No separate TALOS server is required.

```text
Cockpit or shell → Task queue and project configuration
                          ↓
        Writer → Critic review → Project checks → Result
                          ↓
              SQLite evidence and local reports
```

## From draft to result

The cockpit maintains a draft until the operator confirms it. Confirmation dispatches the selected task ID. A separate shell command, `talos up --auto`, processes the runnable queue. A new cockpit session changes the working view while retaining project settings and history. The next task uses new provider conversations.

- **Pair** runs a Writer and a separate Critic in sequence. The Critic receives a bounded Git patch and must return structured findings or approval tied to that revision.
- **Team** can add a planning lead, bounded parallel Writers in managed worktrees and additional review. The orchestration uses triage, a bounded correction and rebuttal loop, and repeated-diff detection. Availability of workers and worktree integration depends on the task and project.
- **Verification** compares Git facts around the saved project checks. Invalid review output, stale revisions and failed checks block completion. When no checks are configured, revision and review evidence still apply.

## Adapter and permission boundaries

Adapter modules implement discovery, process arguments, output parsing and provider session handling for `agy`, `codex`, `claude`, `omp`, `pi`, `kilo` and `opencode`.

The adapters use provider-specific tool restrictions and execution modes. Their permissions and session capabilities differ. A local executable or a `doctor` detection result does not establish authentication, an end-to-end run, or equivalent permissions across adapters. Antigravity's headless role paths have explicit version and capability checks.

Git worktrees isolate checkouts and conflict scopes. Their files and running processes still operate under the host and provider's security boundaries. Run TALOS in repositories you trust and review the selected provider permissions before granting write access.

## State, evidence and recovery

Each project owns its `.talos/` directory. Git projects use a local exclude rule when needed. SQLite stores task state, dependencies, attempt and lease records, review results, verification results, worktree integration and GitHub publication evidence. The runtime directory also holds local configuration, supervisor logs and per-task reports. Logs and reports can include repository paths, task text, and agent output.

Writer attempts use durable claims. An ambiguous process outcome can be quarantined as `unknown` and resolved with an explicit operator decision. Project state persists across cockpit restarts. Same-conversation continuation is a separate capability with adapter- and state-specific eligibility.

TALOS can create local Git commits from verified changes. Optional GitHub publication and CI evidence use project policy and recorded revisions. Merging a GitHub pull request is an operator action.

See [the review contract](critic-review-contract.md) for evidence and response limits, [the acceptance record](acceptance-scenarios.md) for observed behavior, and [architecture decisions](decisions.md) and [the project plan](plan.md) for schema and implementation history.
