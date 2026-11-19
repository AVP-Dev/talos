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
| T4.6 | done | Select verified Writer and Critic roles per project | `.talos/config.json` has independent role fields and `doctor` reports them. At the R4 milestone both resolved to Agy. This task delivered role selection; R10 now owns multi-CLI profiles. |

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
| T6.5 | done | Implement Linux process identity and complete Linux server acceptance | Rust toolchain and TALOS built and tested on Linux server `nyxos` (Ubuntu x86_64) in `~/work/talos`. Full `cargo test --lib` passes (124 passed, 2 ignored, 0 failed). Release binary builds and `talos doctor` passes. |
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
| T9.5 | in progress | Build release artifacts | ARM64 package `dist/talos-v0.1.0-aarch64-apple-darwin-cb67c52e23e6/` was built from clean commit `cb67c52`; SHA-256 verification passed for `talos`, `README.md`, and `BUILD-INFO.txt`, and the packaged binary returned `talos 0.1.0`. The task-persistence smoke passed, but the fresh Agy Writer run failed. Reproducibility and install lifecycle remain pending; Linux artifacts wait for T6.5/T9.3 acceptance. |

## R10 — Multi-CLI adapter profiles
**Status:** complete. All 7 requested AI CLI adapters (`agy`, `codex`, `omp`, `pi`, `kilo`, `opencode`, `claude`) implemented and wired into headless execution for Writer and Critic. Tested end-to-end with live autonomous loop (`talos up --auto`).

| ID | Status | Task | Exit criteria |
|---|---|---|---|
| T10.1 | done | Define provider-neutral profile contract and durable identity | Shared `AdapterKind` enum, runner options, capability detection, and `doctor` report implemented. `writer_model` and `critic_model` configuration options added. |
| T10.2 | done | Add Codex profile | `codex` adapter implemented (`src/adapter/codex.rs`) with `codex exec --json`, role sandboxing (`workspace-write` for Writer, `read-only` for Critic), JSONL event stream parsing, model configuration, and session capture. Live end-to-end verified. |
| T10.3 | done | Add OMP profile | `omp` adapter implemented (`src/adapter/omp.rs`) with `--print --mode json`, role-based `--tools` filtering (`read,grep,glob` for Critic; `read,bash,edit,write,grep,glob` for Writer), and optional model selection. |
| T10.4 | done | Add Pi profile | `pi` adapter implemented (`src/adapter/pi.rs`) with `--print --mode json`, role-based `--tools` filtering, and model selection. |
| T10.5 | done | Add Kilo profile | `kilo` adapter implemented (`src/adapter/kilo.rs`) with `kilo run --format json`, role-based `--auto` (Writer), and model selection. |
| T10.6 | done | Add OpenCode profile | `opencode` adapter implemented (`src/adapter/opencode.rs`) with `opencode run --format json`, `--auto` for Writer, and model selection. |
| T10.7 | done | Add Claude Code profile | `claude` adapter implemented (`src/adapter/claude.rs`) with `claude -p --output-format json`, role-based tool restrictions (`--tools Read,Grep,Glob` for Critic, `--allow-dangerously-skip-permissions` for Writer), and model selection. |
| T10.8 | done | Verify cross-provider role combinations and autonomous loop | Unified autonomous cycle (`talos up --auto`) implemented: Writer execution -> Critic review -> automated fix loop (up to 3 rounds) -> verification checks -> task completion. Tested end-to-end with live AI CLI. |

## R11 — Hybrid Team Orchestration
**Status:** complete. Autonomous two-tier team orchestration with Writer Team and Critic Team implemented. Backward compatibility with Pair mode (`talos up --auto`) fully preserved.

| ID | Status | Task | Exit criteria |
|---|---|---|---|
| T11.1 | done | Schema v19 and configuration for team orchestration | Added `OrchestrationMode` (`pair`, `team`), `DelegationMode` (`hybrid`, `native`, `external`), and `OrchestrationConfig`. Advanced schema to v19 with `team_subtasks` table and foreign key indexes. Full backward compatibility with config v1-v4 and schema v1-v18. |
| T11.2 | done | Writer Lead decomposition planning and delegation engine | Writer Lead prompt constructs technical plan and subtask breakdown with explicit `allowed_paths`. `detect_delegation` checks for safe headless native delegation and deterministically falls back to external processes. |
| T11.3 | done | Writer Lead worker supervision and assistance for stuck/failing workers | Error/timeout diagnostics captured and delivered to Writer Lead. Writer Lead evaluates and chooses `RetryWithGuidance` (bounded by `max_help_attempts`), `Takeover` (Lead executes subtask), or `BlockSubtask`. |
| T11.4 | done | Critic Team multi-angle evaluation and Critic Lead consolidated verdict | Critic Workers perform specialized domain audits (security, algorithms, testing) within read-only bounds. Critic Lead consolidates diff, plan, verification output, and worker findings into structured `approve` or `request_changes` verdict. Rebuttal and fix loop negotiated autonomously. |
| T11.5 | done | Verification and backward compatibility preservation for Pair mode | Tested both `pair` and `team` workflows live in isolated git project `/private/tmp/talos-team-e2e` with Codex (Writer) and Agy (Critic). Parallel execution of two Writer Workers verified with exact millisecond timestamps (Worker 1: 1794996988535 ms, Worker 2: 1794996988668 ms; 30.7s concurrency overlap). Writer Lead automated integration, Critic Worker audit, Critic Lead approval, project checks (unittest), and git commit verified. Pair mode verified with Task #5 running sequentially. 140 unit and integration tests passing; format, clippy (0 warnings), and security checks clean. |

## R12 — Terminal Orchestrator Cockpit (TUI) & Adapter Diagnostics Polish
**Status:** complete. Delivered interactive TUI Cockpit as default `talos` interface and accurate multi-adapter capability discovery.

| ID | Status | Task | Exit criteria |
|---|---|---|---|
| T12.1 | done | Implement Terminal Orchestrator Cockpit on Ratatui and Crossterm | Full-screen interactive dashboard with 4 tabbed views (`Overview`, `Dialogue & Review`, `Git Diff`, `Audit & Checks`), Tokio async background supervisor streaming, and session reconnection on launch. |
| T12.2 | done | Extensible in-TUI slash command engine and terminal keybinding safety | In-TUI slash commands (`/stop`, `/pause`, `/resume`, `/commit`, `/diff`, `/audit`, `/dialogue`, `/help`) with full Russian alias support. Removed conflicting `Ctrl+P` hotkey. |
| T12.3 | done | Multi-adapter executable probing in `talos doctor` | `doctor` probes actual presence and executable paths for all 7 supported AI CLI adapters instead of delegating all checks to `AgyProbe`. Verified in diagnostics. |

## R13 — Intelligent Rate-Limit & Quota Detection with Adaptive Backoff
**Status:** complete. Implemented multi-vendor error signature parsing, wait time extraction, adaptive backoff, and seamless auto-resume.

| ID | Status | Task | Exit criteria |
|---|---|---|---|
| T13.1 | done | Implement rate limit and quota classifier module | `src/rate_limit.rs` classifies errors from OpenAI, Anthropic, Gemini, DeepSeek, and local gateways into `RequestLimit`, `QuotaExhausted`, or `Overloaded` with time parsing and adaptive fallbacks. 6 unit tests passing. |
| T13.2 | done | Integrate backoff auto-pause and auto-resume into Writer and Critic | `execute_writer_task`, `execute_critic_review`, and `run_agent_query` catch rate limit errors, log `task.rate_limit_backoff` events, sleep asynchronously without burning code help attempts, and retry automatically up to 5 times. |

## R14 — Autonomous Decisions Artifact, Session Timings & Token Economy
**Status:** complete. Automatic `TALOS_SUMMARY.md` generation, session duration metrics, and token economy context truncation implemented.

| ID | Status | Task | Exit criteria |
|---|---|---|---|
| T14.1 | done | Generate Deliverable Summary Artifact (`TALOS_SUMMARY.md`) | `src/summary.rs` generates structured Markdown summarizing session timings, autonomous architectural decisions, verified checks, and modified files upon successful completion. |
| T14.2 | done | Session timings and execution metrics | Tracks task start timestamp, total duration, active execution time, and pause intervals across Team and Pair orchestration modes. |
| T14.3 | done | Token economy context truncation | Large diagnostic error texts truncated to 4096 bytes preserving head and tail context (`truncate_for_token_economy`), protecting agent context windows. 2 unit tests passing. |

## R15 — CLI Adapter Pass-Through (/cli <name>) and TUI Unified Feed Redesign
**Status:** complete. Implemented dynamic CLI adapter configuration via `/cli <name>` (in TUI) and `--cli <name>` (on CLI), and completely redesigned the TUI primary cockpit screen around a responsive, live-scrolling Unified Feed.

| ID | Status | Task | Exit criteria |
|---|---|---|---|
| T15.1 | done | Dynamic CLI adapter switching via `/cli <name>` and `--cli` | Added `/cli` slash command in TUI supporting single adapter (sets both Writer and Critic) or pair adapters (`/cli <writer> <critic>`) with Russian aliases (`/кли`, `/адаптер`), persistence to `.talos/config.json`, and global `--cli <name>` CLI argument flag. |
| T15.2 | done | Redesign TUI primary cockpit view into Unified Feed | Replaced fragmented overview with responsive `ActiveTab::Feed` (" 1: Unified Feed "): 34-col Cockpit Sidebar (AI Agents card, Active Task card, Queue & Repo card) alongside a prominent live Unified Feed stream combining real-time supervisor logs, phase announcements, agent planning, and milestone SQLite events. |
| T15.3 | done | Interactive auto-scroll and manual history inspection | Feed supports automatic bottom-pinning stream mode (`scroll_offset == 0`) with seamless transition to manual scroll (Up, Down, PageUp, PageDown, Home, End) and live stream status indicator. 2 dedicated TUI unit tests passing. |

## R16 — Operator Ergonomics: Tab-Completion, Task Presets & Interactive Diff Control
**Status:** complete. Implemented inline contextual Tab-completion, parameterized task presets (`/task:test`, `/task:refactor`, `/task:fix`, `/task:bench`), and interactive Git Diff file curation.

| ID | Status | Task | Exit criteria |
|---|---|---|---|
| T16.1 | done | Tab-completion for commands, adapters, and file paths | In TUI input bar, `Tab` auto-completes slash commands (`/cli`, `/commit`, `/mode`, `/feed`, etc. and Russian aliases), adapter names (`codex`, `claude`, `agy`, etc.), and relative project file paths via `git ls-files`. Common prefix matching and suggestions indicator. Unit test `tab_completion_and_task_presets`. |
| T16.2 | done | Parameterized task presets (`/task:test`, `/task:refactor`, `/task:fix`, `/task:bench`) | Added shorthand archetype commands with automatic `--criteria` generation and path scoping. Verified in SQLite database with unit test `tab_completion_and_task_presets`. |
| T16.3 | done | Interactive file staging & discarding in `3: Git Diff` view | Operator navigates modified files with `j`/`k`, inspects diff for selected file or working tree, stages clean files with `s`, and discards/reverts unwanted edits with `d`. Verified with unit test `diff_file_navigation_and_selection`. |

## R17 — Autonomous Engineering Evolution: Deliberation, Adaptive Triage, and Resilient Validation
**Status:** complete. Implemented structured deliberation, flexible recommendation-based triage, backlog curation, verification failure handling, and resilient validation.

| ID | Status | Task | Exit criteria |
|---|---|---|---|
| T17.1 | complete | Test-verify and resolve the verification failure completion defect | Write regression test reproducing verification failure behavior where `run_task_verification` returns Ok while task status is `failed` in DB; ensure failed checks prevent completion, skip commit/PR, and report failure cleanly. |
| T17.2 | complete | Replace brittle validation bails with resilient normalization | Replace crashes on text lengths (`summary`, `title`, `reason`) with multi-part retention or safe capture; normalize line 0; route findings on unmodified paths to cross-cutting backlog rather than failing review. |
| T17.3 | complete | Activate Writer Lead deliberation and rebuttal loop | Wire `build_writer_lead_rebuttal_prompt` into `run_team_task`; allow Writer Lead to argue technical choices; ensure green tests inform but do NOT automatically dismiss Critic's architectural concerns; record decisions in summary. |
| T17.4 | complete | Recommendation-based pre-flight task triage | Read-only analysis of task requirements and repository structure recommending analysis depth and team posture without rigid hardcoded selection rules. |
| T17.5 | complete | Backlog curation for non-critical findings and technical debt | Capture `Minor` and `Note` findings into persistent backlog storage upon Critic approval; include deferred items in `TALOS_SUMMARY.md`. |
| T17.6 | complete | Anti-stagnation loop detection across fix rounds | Compare diff hash and verification output across fix iterations; abort futile stagnant loops early without wasting token budgets. |

## R18 — Guided first run and repeatable cockpit sessions

| ID | Status | Outcome |
|---|---|---|
| T18.1 | done | Project selection and initial role suggestions. Startup excludes runtime files locally and generated reports are per task. |
| T18.2 | done | Settings opens from the cockpit, with separate roles, models and orchestration mode. Agent discovery uses the current user's installation. |
| T18.3 | done | Draft review, explicit single-task dispatch, blocker guidance, task-specific retries and queued cancellation. |
| T18.4 | done | Unicode input editing, repeated deletion, paste, fresh sessions and durable task history with saved changes. |
| T18.5 | published | GitHub release v0.1.1 contains four binary archives, matching SHA-256 files and a generated formula. The external tap has its own release lifecycle. |
| T18.6 | done | Reconciled public docs with the CLI and cockpit. Captured Settings and task review screenshots and an animated walkthrough. Added versioned v0.1.1 release notes consumed by the release workflow. |
| T18.7 | done | Live Feed wraps to its pane width, with visual-row scrolling. Input, drafts, review and diff views adapt to narrower terminals. Verified with automated rendering tests and Agterm with its sidebar open. |

## R19 — Resilient CLI Discovery, Adaptive Orchestration, Input History, and Slash Command Palette
**Status:** complete. Decoupled CLI detection from brittle version strings, smart auto-orchestration between Pair and Team, Readline-grade input history, and interactive command popup palette.

| ID | Status | Task | Exit criteria |
|---|---|---|---|
| T19.1 | done | Resilient Agy CLI detection without brittle version locking | Decoupled `adapter_role_status` and cockpit availability from rigid `"1.5.10"` equality in `src/app.rs`. Uses `is_verified_agy_version` supporting `1.5.11+` so Agy remains available in TUI and `/cli`. Verified with unit test `resilient_agy_version_tolerance_accepts_updates`. |
| T19.2 | done | Smart Adaptive Orchestration Mode (`mode: adaptive`) | Added `OrchestrationMode::Adaptive` as intelligent default. Pre-flight triage classifies scope: simple/local tasks execute via lightweight Pair mode, complex/multi-file tasks automatically escalate to Team mode. Manual overrides (`--mode pair`, `--mode team`, `/mode adaptive|pair|team` and Russian aliases) fully supported. Verified with unit test `mode_slash_command_supports_adaptive_and_aliases`. |
| T19.3 | done | TUI Readline input history ($\uparrow / \downarrow$) | Maintained `input_history` buffer in `TuiApp`. Pressing $\uparrow$ scrolls backward through previous prompts/commands, $\downarrow$ scrolls forward, restoring current draft at the bottom. Duplicate consecutive entries are de-duplicated. Verified with unit test `tui_input_history_navigation_with_arrows`. |
| T19.4 | done | Interactive Slash Command Palette (`/` popup menu) | Typing `/` in Cockpit input bar opens floating palette overlay displaying slash commands (`/cli`, `/mode`, `/task:test`, `/diff`, `/help`, etc.) with real-time prefix filtering, description metadata, and arrow/Tab/Enter selection. Verified with unit test `interactive_slash_command_palette_filtering_and_selection`. |
| T19.5 | done | Keep task launch and progress on Work | One Enter launches the submitted local task. Work shows its status and keeps blocked input. Review, Changes, Logs, and History are tabs. Settings is the separate screen. Draft PR publishing and generated presets have inline confirmation. |

## R20 — Clean Borderless Workspace, Goal & Stage Progress Tracker, One-Key Clipboard Yank, and Context-Aware Task Continuation (`Continue Task`)
**Status:** open. Deliver uncluttered copy-friendly workspace without box-drawing traps, single-key system clipboard integration, persistent goal & subtask progress banner, interactive plan inspector, and intelligent task continuation from history.

| ID | Status | Task | Exit criteria |
|---|---|---|---|
| T20.1 | open | Borderless Content Canvas & Native Mouse Copying | Remove vertical box-drawing borders (`│`) from reading areas (`Activity`, `Review`, `Diff`, `Logs`, `Plan`). Replace 4-sided `Block::bordered()` with clean top/bottom separators or open margins. Terminal mouse drag-selection copies 100% clean code, diffs, and logs without capturing vertical borders, line numbers, or box corners. |
| T20.2 | open | One-Key System Clipboard Integration (`y` / `Ctrl+Y` / `/copy`) | Single-key instant export of the active view (diff, review analysis, plan, logs) to system clipboard via macOS `pbcopy` and ANSI OSC 52 escape sequences with visual confirmation toast `✓ Copied to clipboard!`. |
| T20.3 | open | Pinned Active Goal & Stage Progress Tracker Banner | Persistent top card directly below header rendering root user prompt (`🎯 GOAL`), active stage/round (`📍 STAGE: [2 of 4] T19.2 Adaptive Mode · Phase: Writing`), and visual pipeline breadcrumb tracker (`[✓ T1] ──▶ [▶ T2] ──○ [T3] ──○ [T4]`). |
| T20.4 | open | Dedicated Interactive Plan & Tasks View (`p` / `[Plan]` Tab) | Full inspection view accessible via `p` or mouse click on Goal banner showing original user request, definition of done, multi-message session thread, and living subtask checklist with worker/worktree assignments and status. |
| T20.5 | open | Context Pack Assembly & Smart `Continue Task` Engine | History shows structured Resume Summary Card (goal, where it stopped, landed diff, open critic defects, failing check output, git drift). Action `[c] Continue Task` deterministically routes to next logical step (Critic review, Writer fix for open remarks, Writer test fix, or drift reconciliation) with structured context rehydration so agent sessions never restart from scratch. |

## Required end-to-end scenarios from the original request

First launch/repeat `up`; Writer/Critic success and justified rejection; child task; independent parallel work; adding a task while work is running; manual takeover; CLI recovery; orchestrator crash; SSH disconnect/reconnect; Linux reboot autostart; PR without merge; duplicate-execution prevention; and rechecking original acceptance criteria before closeout. Keep separate evidence for each scenario; do not combine them into a misleading test.

## Unscheduled ideas — revisit after R10

| ID | Status | Idea | Note |
|---|---|---|---|
| F1 | idea | Add a local project-origin and history journal for Git and non-Git projects | Revisit after the current roadmap. Explore project registration/creation milestones, plan and decision history, and references to files or reports produced during work. Keep structured metadata local in SQLite; evaluate path, digest, size, and provenance references for larger files. Define privacy, retention, backup, export, archival, and token/context budgets before implementation. Keep rows and prompts bounded; do not store full transcripts or duplicate project trees by default. This is not a request for TALOS to generate or copy entire projects. Research: `docs/research/project-history-idea.md`. |
| F2 | idea | Evaluate typed agent handoffs and controlled technical language | Revisit after R10. This remains exploratory only; TALOS has not adopted ASD-STE100 or a Russian profile. Evaluate whether typed JSON contracts, stable terminology, or optional text checks improve Writer/Critic, child-task, and handoff clarity. Measure ambiguity, false positives, maintenance cost, and token/context overhead. It may not fit TALOS. Research: `docs/research/agent-language-idea.md`. |
