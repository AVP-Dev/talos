# TALOS Backlog

This is the single project backlog. Task IDs remain stable across rounds. Mark work `done` only with an observed result; record user decisions separately.

Statuses: `done` — completed and verified; `in progress` — actively being worked; `ready` — may be started; `planned` — scheduled; `blocked` — stopped by a specific external condition; `deferred` — intentionally moved to a later round; `idea` — recorded for possible work after the current roadmap, with no round assigned.

## R0 — Bootstrap and research

| ID | Status | Task | Result / acceptance |
|---|---|---|---|
| T0.1 | done | Read and install Agent Starter Kit using its official method | Ran `init-project.sh --dry-run --update`, then `--update`; files were only added to the empty TALOS directory; copied scripts were SHA-256 checked; scanner passed. |
| T0.2 | done | Adapt Starter Kit guidance and structure for TALOS | `AGENTS.md` distinguishes recommendations from safety boundaries; `CLAUDE.md` is a relative symlink; project layout is documented. |
| T0.3 | done | Study pair-mailbox protocol, history, code, tests, and recovery audit | `docs/research/pair-mailbox-assessment.md` contains a transfer/decline matrix; original source untouched. |
| T0.4 | done | Inventory installed CLI versions and public flags | Versions/help recorded in `docs/research/cli-spike.md`; discovery is not live proof. |
| T0.5 | done | Establish a plan and handoff that does not depend on chat | `docs/plan.md`, `docs/tasks.md`, and `docs/state.md` are in place. |

## R1 — CLI spike (completed with deferred checks)

Dependency: the user approved the planned CLI spike. Research used isolated temporary projects and did not change source projects. Start with `agy`, compare with Codex, then test fallbacks where useful.

| ID | Status | Task | Exit criteria |
|---|---|---|---|
| T1.1 | done | Capture `--version` / `--help` and resolved executable paths | Versions and advertised flags recorded; help output is not functional evidence. |
| T1.2 | done | Make a minimal headless request without file tools in a temporary project | Antigravity returned a fixed response in plan/sandbox mode; outcome and token usage recorded in `cli-spike.md`. Other CLI attempts stopped before inference. |
| T1.3 | done | Verify JSON/JSONL format and extract final response/session ID | Antigravity returned parseable JSON with a response and conversation ID; Codex/Pi failure output did not count as success. |
| T1.4 | done | Continue after the headless process exits using its session ID | Antigravity resumed the same public conversation ID and returned the prior token; live proof recorded. |
| T1.5 | deferred | Test `headless → resume → interactive TUI → headless resume` | The `agy` TUI opened a workspace-trust prompt that was declined to avoid changing persistent user trust settings. Revisit as T6.6; never claim continuity before then. |
| T1.6 | deferred | Check native subagents and context transfer | Interfaces are advertised for some CLIs, but no live delegate probe completed; continue in T7.3. |
| T1.7 | deferred | Check Ctrl-C, SIGTERM, CLI exit, timeout, and restart | Verify at the process-supervision stage in T3.2/T6.3. |
| T1.8 | done | Inventory public approval/sandbox/Auto options | Modes and trust boundary recorded; no dangerous bypass was used. The T3.7 live unattended-write gate later passed for `agy 1.5.10` under the exact tested session policy. |
| T1.9 | done | Compare `agy`, Codex, and fallbacks; choose the MVP adapter | Provisional `agy` choice recorded in ADR-011; headless resume is live-tested, other capabilities remain explicitly limited. |

## R2 — Rust core and durable state

Dependency: R1 determines process invocation and session IDs. Check current Context7 documentation for library APIs before implementation.

| ID | Status | Task | Exit criteria |
|---|---|---|---|
| T2.1 | done | Initialize Rust binary crate and CLI skeleton | `--help` / `--version`; format, Clippy, 13 tests, and macOS release build pass. |
| T2.2 | done | Discover Git root and calculate `.talos/` runtime path | A nested Git invocation resolves to the repository root even with hostile `GIT_DIR`; invalid `.git` metadata fails clearly instead of falling back. |
| T2.3 | done | Design and create versioned SQLite schema | WAL, foreign keys, transactional migration, valid `user_version`; future schema is rejected without changing its schema or journal mode. |
| T2.4 | done | Implement task/event models, transitions, and append-only journal | Valid transitions atomically update state and append time, actor, reason, and transition data; invalid transitions add no event. |
| T2.5 | done | Add atomic supervisor lease with fencing token | Two concurrent database connections cannot both acquire an unexpired lease; tokens advance after expiry or release. Heartbeat and process-liveness reconciliation are R3/R6 work. |
| T2.6 | done | Implement `task add/list`, `status`, `logs`, `doctor`, and `up --observe` | At the R2 checkpoint, release-binary smoke passed; repeated observe was safe; auto/semi were disabled before runtime creation. R3 later enabled one-shot Auto; Semi remains disabled. |
| T2.7 | done | Create and run the active Starter Kit verification profile | `scripts/verification-profile.tsv` is active; the dispatcher passed all listed checks and the secret scanner passed. |

## R3 — One autonomous Writer (initial macOS release)

macOS is the currently verified release/runtime platform. Linux-specific implementation remains in scope for T6.5 and T9.3; Linux-native build, runtime, and sandbox checks may be deferred to the user's server if setting them up on macOS would slow development. This does not block R3. Preserve the Linux research for those later tasks.

Dependency: R2 and the adapter chosen in R1.

| ID | Status | Task | Exit criteria |
|---|---|---|---|
| T3.1 | done | Implement capability probe for the selected CLI | `doctor` resolves the executable path, version, and help-advertised features as typed data; filesystem scope remains separately marked not tested. |
| T3.2 | done | Launch processes with Tokio argv, stdio, and signal handling | Prompt is NDJSON on stdin, never an argv value; stdout/stderr drain concurrently into bounded buffers; timeout, Ctrl-C, SIGTERM, future cancellation, and surviving process-group members are cleaned up; fake-CLI tests confirm exit/reap behavior. |
| T3.3 | done | Send persisted tasks to Writer | Schema v2 transactionally stores attempt state, PID, prompt SHA-256, outcome, and public conversation ID. `task run <ID>` sends one queued task over stdin and leaves success in `reviewing`; fake-CLI integration, persistence smoke, and a live Antigravity run in a disposable Git project passed. |
| T3.4 | done | Handle exit, timeout, CLI error, and safe continuation | Failures remain blocked and never complete. Fake-CLI tests cover timeout, nonzero exit, Agy `ERROR`, missing/invalid session ID, malformed result, and unresolved launch errors. `task continue <ID> --confirm-project-inspected` accepts only the latest terminal Agy `ERROR` with exit 0 and a validated public conversation ID; it passes `--conversation`, persists the inspection attestation, waits for a matching resumed `init` ID before sending one follow-up, and does not replay the original prompt. A successful continuation remains `reviewing`. Unit, CLI, and live disposable-project checks passed. The live check used a local wrapper to fault-inject the terminal `ERROR` status after a real Agy turn; it proves TALOS resume behavior, not that Agy naturally emits this error. |
| T3.5 | done | Deliver one-shot `talos up --auto`, `task add`, `status`, and `logs` | One explicit `up --auto` invocation requires a clean Git worktree and processes at most one queued task. It reports the Writer response and a changed-path count with a preview capped at 50 paths. Fake and live Antigravity 1.5.10 checks verified one changed file, task visibility in a separate TALOS process, `reviewing` (never false completion), one-task limit with two queued tasks, and no duplicate Writer after repeat invocation. `up --semi` and its approval policy remain R5 work. |
| T3.6 | done | Recover an attempt after TALOS restarts | Schema v3 transactionally preserves v1/v2 data and stores operator resolution separately from the attempt's `unknown` outcome. `task recover` quarantines unfinished attempts under the current lease; it does not infer outcomes, signal, resume, or retry. `task resolve` requires both stopped-Writer and inspected-project attestations plus a non-empty reason; it records action/time/role/reason atomically and keeps the attempt `unknown`. It sets the task to `reviewing` (`accept-for-review`), `queued` (`authorize-retry`), or `failed` (`abandon`); retry execution still requires a later explicit task command. The CLI refuses resolution if a recorded process group exists or cannot be probed. Unknown attempts without a recorded PID rely entirely on operator attestation. Migration, lease/fencing, idempotency, live/unverifiable process-group refusal, all three actions, and CLI recovery/resolution passed isolated checks. Linux process identity/build/runtime and Agy Linux sandbox acceptance remain unverified for later server validation; no Docker/VM setup was used. |
| T3.7 | done | Verify Antigravity unattended write mode and sandbox in a disposable project | On `agy 1.5.10`, session-only settings allowed workspace writes and denied reads and writes under `.talos/` and `.git/`, an outside path, and terminal commands. Host checks confirmed the expected results; no user-wide settings changed. See `cli-spike.md`. |

## R4 — Independent Critic

Dependency: R3.

| ID | Status | Task | Exit criteria |
|---|---|---|---|
| T4.1 | done | Define review input/output contract and revision identity | `docs/critic-review-contract.md` defines strict JSON findings and approval rules. Review identity binds the full base commit and complete patch; patch, prompt, and response budgets are 64/96/64 KiB. Oversized context fails closed without truncation. |
| T4.2 | done | Enforce an actual Critic read-only boundary | Antigravity 1.5.10 attempted `write_file` in a disposable project and received `Permission denied` matching the session deny rule; the marker file was absent. Unit checks confirm command/MCP/URL and `.talos`/`.git` denials. |
| T4.3 | done | Run independent review through an available CLI | `task review` uses a fresh Antigravity process without the Writer conversation. A live approval and a live `request_changes` result were schema-validated and tied to exact revision IDs. Process/parse failures never approve. |
| T4.4 | done | Implement reject → fix → review again | A live Critic finding moved a deliberately defective task to `implementing`; a fake Writer fix created attempt 2 linked to review 1 and kept the original base. Database tests recorded the later approval and confirmed prior review rows cannot be updated or deleted. |
| T4.5 | done | Keep task open for unresolved findings or an active Writer | Active/unresolved Writer state blocks review; approval moves only to `verifying`, never `complete`. Database tests cover the global active-Writer gate and state transition. |
| T4.6 | done | Select verified Writer and Critic roles per project | `.talos/config.json` has independent role fields and `doctor` reports them. Both currently resolve only to Agy. This task delivered role selection, not additional CLI adapters; R10 now owns multi-CLI profiles. |

## R5 — Verification evidence and permissions

Dependency: R3/R4.

| ID | Status | Task | Exit criteria |
|---|---|---|---|
| T5.1 | done | Support verifiable acceptance criteria and project checks | Config v2 defines allowlisted executable argv, version argv, and project-relative working directory. A task snapshots named checks; unknown tools, shell paths, secret-like arguments, and unknown check IDs fail closed. `task evidence <ID>` retrieves each run's command, working directory, version, separate version/check durations and exit statuses, timeout/failure code, revision identity, and output hashes. Raw check output is bounded while streamed and is not persisted. |
| T5.2 | done | Compare tree facts and Git diff with the claimed result | Critic approval and verification bind the base commit, HEAD, sorted changed paths, and complete patch digest. TALOS checks the exact same facts before and after every saved check. |
| T5.3 | done | Keep Critic review findings-only and bind Writer fixes to reviewed revisions | The Critic cannot return an applicable patch; unknown `patch` fields are rejected. A requested fix is a new linked Writer attempt from the saved base, and the new full revision must receive a separate review. This preserves R4's findings-only authority boundary. |
| T5.4 | done | Design Auto/Semi action policy | Auto permits only allowlisted Writer launch and local project checks; Semi prompts before each Writer launch and requires a TTY. Critic patch application, Git mutation, and publish actions are hard denied. `task verify` is an explicit operator command. |
| T5.5 | done | Redact credentials and secrets from logs/reports | Common token, URL credential, cloud key, JWT, bearer, generic token assignment, and PEM patterns are rejected from persisted inputs or redacted from text/JSON output. Raw check stdout/stderr is neither logged nor stored; only its digest is retained. |
| T5.6 | done | Close tasks only with complete evidence | Completion is one transaction requiring the latest approved review, current `verifying` status, every saved check's passing version probe and command exit, and unchanged Git facts. Missing, failed, stale, or partial evidence leaves the task open. |
| T5.7 | done | Keep local TALOS runtime state out of Git | Git projects must already ignore `.talos/state.sqlite3`; tracked state or a missing ignore rule is refused before the runtime database opens. TALOS does not rewrite project ignore files. |

## R6 — tmux, manual takeover, and recovery

Dependency: R3 and R5.

| ID | Status | Task | Exit criteria |
|---|---|---|---|
| T6.1 | done | Create and reconnect tmux control session | `talos attach` creates one detached, project-scoped user shell or reuses it; it attaches from outside tmux and switches the current client from inside. Session identity is a stable hash of the canonical root. The command does not start a worker or database; foreground commands still work without tmux and attach gives a clear fallback error. A fake tmux test verifies idempotence, and a host smoke test verified a real isolated tmux 3.8 server. |
| T6.2 | done | Support durable pause/resume and manual takeover | SQLite pause state blocks new Writer attempts and continuations while preserving tasks. Resume compares Git facts, requires explicit reconciliation for changed/unavailable snapshots, reopens verification approvals for Critic review, and records an append-only project control journal. A pause does not interrupt an active Writer. |
| T6.3 | in progress | Recover after CLI/TALOS exit and SIGTERM/SIGKILL | Implemented `talos stop` as a durable cooperative Writer request; timeout/interrupt cleanup now covers Writer and check process groups while the leader is unreaped. Recovery can reclaim an unexpired task or worktree operation lease only when the recognized owner PID is absent and the numeric timestamp/PID suffix is valid, then advances the fencing token. Numeric PGIDs are never signalled after leader reap. Behavior tests, full profile, and crash-point acceptance remain pending; fail closed if cleanup or owner identity cannot be established. Normal CLI exit with a detached descendant intentionally leaves it alone because the old PGID cannot be distinguished from a reused one. |
| T6.4 | in progress | Check SSH reconnect on macOS and record deferred Linux boot recovery | Existing `attach` and local SQLite state provide the reconnect path. The macOS SSH disconnect/reconnect scenario is still unverified; Linux boot recovery and systemd user autostart remain deferred until Linux runtime support under T6.5/T9.3. |
| T6.5 | in progress | Implement Linux process identity and complete Linux server acceptance | Schema v9 stores the Linux `/proc` fingerprint `(boot_id, pid, pgrp, starttime_ticks)` when available; `task recover` reports `matches`, `different_process`, `unavailable`, or `not_recorded`. This is lifecycle evidence only and never authorizes signaling, retry, or resolution. macOS remains fail-closed without public process identity. Linux build/runtime/Agy sandbox checks are deferred to the user's server; no Docker/VM or heavy local Linux matrix is required. |
| T6.6 | in progress | Verify interactive handoff and provide a bounded new-context fallback | `task handoff <ID>` emits a read-only `talos-task-handoff-v1` packet from existing task, attempt, review, verification, dependency, and worktree rows. Text fields are redacted and capped; prompts, patches, transcripts, raw output, current Git diff, and duplicate stored reports are excluded. The operator must inspect the working tree before a Writer action or retry. This fallback does not prove same-session continuity. The live TUI→headless probe remains pending because the workspace-trust decision has not been made. |

## R7 — DAG, subagents, and worktrees

Dependency: R5/R6.

| ID | Status | Task | Exit criteria |
|---|---|---|---|
| T7.1 | in progress | Add task dependencies and DAG scheduler | Schema v10 adds append-only dependency edges; cycle and self-dependency checks reject invalid graphs. Explicit Writer attempts and `up --auto` select only tasks whose prerequisites are all complete. Auto/Semi now select the first eligible task in SQLite instead of loading every task and checking dependencies one by one. `task dependencies` reports prerequisite states through bounded cursor pages. Migration, cycle, ordering, and scheduler behavior checks remain pending. |
| T7.2 | in progress | Define bounded child-task contract | Schema v11 stores parent, up to 64 normalized project-relative allowed paths, and a 1–1800 second timeout. `task child` creates the child and parent prerequisite atomically; `task children` uses small cursor pages for large child sets. Writer prompts include the scope and budget; Critic review adds a deterministic blocker for any out-of-scope changed path. `task child-report` emits a compact versioned shape from existing records without duplicating transcripts or result rows. These scope checks are review gates, not an OS sandbox. Migration, validation, report, and out-of-scope behavior checks remain pending. |
| T7.3 | in progress | Detect native subagents per CLI | Doctor reports whether a public invocation interface is advertised or remains untested. Native delegation stays disabled unless the installed CLI, configured agent roster, and a harmless live invocation are verified. Manual child tasks remain usable regardless of native-delegation support. |
| T7.4 | in progress | Create/manage task worktrees only when useful | Local commit `2b672ea` adds explicit create/list/reconcile/remove commands and durable lifecycle records. Creation requires a clean Git tree and unused queued task; removal refuses tracked, untracked, or ignored files, leaves the branch, and sends the task back to the primary project. `task worktree list` uses bounded cursor pages. Lifecycle behavior acceptance remains pending. |
| T7.5 | in progress | Bound concurrency and integrate changes | Local commit `837b82c` adds schema v18 durable per-attempt Writer slots and atomic first-worktree routing. The database claim also matches the execution root selected by the caller to the current task route, so a concurrent worktree lifecycle change cannot write into the wrong tree. `max_concurrent_writers` defaults to one and may be set to two or three only when every overlapping Writer uses a different active managed worktree. Primary-tree Writers stay exclusive; ordinary supervisor operations are blocked until Writer slots close. Recovery checks each owning TALOS PID and fails closed for live or uncheckable owners. Worktree integration and user-commit finalization remain explicit. Migration, behavior, and three-agent acceptance remain pending; native Agy delegation is still disabled. |

## R8 — GitHub

Dependency: R5/R7.

| ID | Status | Task | Exit criteria |
|---|---|---|---|
| T8.1 | in progress | Check `gh` auth and GitHub capability without reading token values | `doctor` and `talos github status` report local `gh` availability/version without contacting GitHub. Only explicit `talos github status --check-auth` invokes `gh auth status --active --json hosts`; output is bounded and only aggregate state is exposed. The auth command has not been invoked during development. Repo-specific access checks remain deferred until the user creates a repository. |
| T8.2 | in progress | Create feature branch, push, and PR under an explicit policy | Schema-v3 policy pins repository, remote, `master`, and `talos/` prefix; push/PR default off. Local branch creation is network-free. `publish` verifies remote identity and exact commits, stores append-only intent/state before push, pushes one exact branch, creates a draft PR, and records its URL. `reconcile` checks ambiguous outcomes before any retry. Code compiles and passes Clippy; remote behavior is unverified because no repository/remote exists. |
| T8.3 | in progress | Wait for CI and attach evidence | `talos github ci <publication_id>` captures status for the exact recorded PR head commit; `--wait` polls within bounded limits. Schema v16 stores bounded, append-only, deduplicated check summaries and digest. Pending, timeout, unknown, and no-checks remain incomplete. No live repository/CI behavior has been exercised. |
| T8.4 | done | Keep merge under user control | TALOS can create draft PRs but has no merge or remote branch deletion command. `talos github policy` reports `manual_only` merge and `unsupported` remote branch deletion. |

## R9 — Hardening and delivery

Dependency: R6–R8.

| ID | Status | Task | Exit criteria |
|---|---|---|---|
| T9.1 | in progress | Run the end-to-end scenarios from the original request | `docs/acceptance-scenarios.md` maps all 15 scenarios and separates prior evidence, unverified behavior, and deferred infrastructure. Current-schema execution remains pending. |
| T9.2 | in progress | Run long-duration/load and fault-injection checks | `docs/verification.md` separates database growth, migration faults, process limits, worktree recovery, GitHub recovery, CI deduplication, backup/restore, and reproducible-build scenarios. Schema v17 adds the task-status index; schema v18 adds durable Writer execution slots and backfills active attempts conservatively. Auto/Semi select the first eligible task in SQLite and cap blocked-queue previews; task, dependency, child, worktree, event, and verification-evidence listings use bounded cursor pages; `task evidence` defaults to 5 runs, caps pages at 10, and previews at most 10 changed paths per run in JSON; Git status paths are streamed, with at most 50 output paths and a 256-byte prefix per path; dirty-tree checks, managed-worktree removal, the tracked `.talos/` guard, and both package-script clean-checks stop after one output byte; Critic revision capture streams bounded patch/path output, caps persisted changed-path JSON at 256 KiB, and rejects more than 4096 changed paths or 1024 bytes per path; GitHub remote discovery caps output at 64 KiB, 64 URLs, and 2 KiB per URL; Agy `--version`/`--help` probes read stdout and stderr concurrently, cap each stream at 64 KiB, and time out after 15 seconds; recovery selects unresolved attempts; resume reconciliation updates rows and appends audit events set-wise with a bounded ID preview; stop/latest-record paths use targeted queries. Migration, execution, and measurements remain pending. |
| T9.3 | in progress | Prepare macOS install/upgrade/uninstall and Linux packages | `docs/operator-runbook.md` documents Cargo install/update/uninstall without touching project state. `scripts/package-macos.sh` and `scripts/package-linux.sh` create native-target binaries and checksums for their respective systems. The Linux script is authored but not run. Install lifecycle acceptance remains pending; Linux build/package acceptance waits for T6.5 server access. |
| T9.4 | done | Finish English documentation and operator runbook | Reviewed README, roadmap/backlog/handoff, architecture, project structure, decisions, operator and agent-routing guides, Critic contract, and acceptance/verification docs against current commands and limits. Corrected stale Cargo install, `.talos/` exclusion, implementation-status, and platform wording. Remote/Linux claims remain explicitly pending. |
| T9.5 | in progress | Build release artifacts | Existing ARM64 macOS artifact `dist/talos-v0.1.0-aarch64-apple-darwin-b4ac9ef6b33c/` was built from `b4ac9ef6b33c` and its checksums passed, but it is stale relative to the current Rust changes. Rebuild from a clean committed source tree. The current release binary itself passed a narrow local task-persistence smoke; the fresh Agy Writer run failed. Reproducibility and install lifecycle remain pending; Linux artifacts wait for T6.5/T9.3 acceptance. |

## R10 — Multi-CLI adapter profiles
**Status:** started by explicit user direction. Implement the original multi-CLI goal in provider-specific vertical slices. R10 follows the R6–R9 code/release closeout; native Linux validation remains deferred to the user's server.

| ID | Status | Task | Exit criteria |
|---|---|---|---|
| T10.1 | in progress | Define provider-neutral profile contract and durable identity | Config uses stable profile IDs, never arbitrary shell commands. Shared interfaces cover bounded discovery, version identity, argument construction, structured events, optional public session resume, timeout/cancellation, and role-specific permission evidence. Add schema migration to record adapter ID and version for Writer attempts and Critic reviews; backfill existing rows as `agy` at the installed version if known, otherwise `agy` with unknown historical version. `doctor` must distinguish detected, implemented, role-verified, unsupported, and untested capabilities. |
| T10.2 | planned | Add Codex profile | `codex` can be selected for Writer and Critic only after its public JSONL protocol, task-specific session identity, process cleanup, actual workspace-write boundary, and actual read-only boundary are exercised in disposable projects. `--dangerously-bypass-approvals-and-sandbox` and equivalent unrestricted modes are never used. |
| T10.3 | planned | Add OMP profile | `omp` can be selected per role only after the installed runtime/version, bounded JSON or RPC protocol, stdin/stdout lifecycle, session behavior, cancellation, Writer boundary, and Critic read-only boundary are proven. Preserve OMP-specific session semantics; do not assume Pi compatibility. |
| T10.4 | planned | Add Pi profile | `pi` can be selected per role only after its JSON/RPC output, task/session identity, tool restrictions, process cleanup, Writer boundary, and Critic read-only boundary pass with an authenticated disposable run. Missing credentials leave the profile unverified and disabled; tests do not substitute for the live check. |
| T10.5 | planned | Add Kilo profile | First discover a documented isolated-state invocation without writing to the user's existing Kilo state. Then implement its public structured interface, permissions, session limits, and cleanup; enable each role only after live disposable Writer/Critic boundary checks. Current local `--version` and `--help` attempts failed on denied writes to user state, so the version is not known. |
| T10.6 | planned | Add OpenCode profile | `opencode` can be selected per role only after structured output, session identity, process cleanup, and a complete explicit permission policy pass live disposable Writer and Critic boundary checks. Broad `--auto` is not accepted as a safe boundary. |
| T10.7 | planned | Add Claude Code profile | `claude` can be selected per role only after authenticated headless JSON/stream-JSON behavior, task/session identity, process cleanup, and live Writer/Critic permission boundaries pass. An isolated unauthenticated probe does not count as acceptance. |
| T10.8 | planned | Verify cross-provider role combinations and update operator docs | Run at least one Writer/Critic pair with different providers, then exercise the supported pairings that have passed individual role gates. Verify the database and `doctor` preserve each selected profile/version; document unsupported resume, permission, and platform behavior. Keep native subagent delegation as a separate capability from selecting a CLI profile. |

## Required end-to-end scenarios from the original request

First launch/repeat `up`; Writer/Critic success and justified rejection; child task; independent parallel work; adding a task while work is running; manual takeover; CLI recovery; orchestrator crash; SSH disconnect/reconnect; Linux reboot autostart; PR without merge; duplicate-execution prevention; and rechecking original acceptance criteria before closeout. Keep separate evidence for each scenario; do not combine them into a misleading test.

## Unscheduled ideas — revisit after R10

| ID | Status | Idea | Note |
|---|---|---|---|
| F1 | idea | Add a local project-origin and history journal for Git and non-Git projects | Revisit after the current roadmap. Explore project registration/creation milestones, plan and decision history, and references to files or reports produced during work. Keep structured metadata local in SQLite; evaluate path, digest, size, and provenance references for larger files. Define privacy, retention, backup, export, archival, and token/context budgets before implementation. Keep rows and prompts bounded; do not store full transcripts or duplicate project trees by default. This is not a request for TALOS to generate or copy entire projects. Research: `docs/research/project-history-idea.md`. |
| F2 | idea | Evaluate typed agent handoffs and controlled technical language | Revisit after R10. This remains exploratory only; TALOS has not adopted ASD-STE100 or a Russian profile. Evaluate whether typed JSON contracts, stable terminology, or optional text checks improve Writer/Critic, child-task, and handoff clarity. Measure ambiguity, false positives, maintenance cost, and token/context overhead. It may not fit TALOS. Research: `docs/research/agent-language-idea.md`. |
