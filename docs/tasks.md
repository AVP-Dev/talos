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
| T3.5 | done | Deliver one-shot `talos up --auto`, `task add`, `status`, and `logs` | One explicit `up --auto` invocation requires a clean Git worktree and processes at most one queued task. It reports the Writer response and Git-reported changed paths. Fake and live Antigravity 1.5.10 checks verified one changed file, task visibility in a separate TALOS process, `reviewing` (never false completion), one-task limit with two queued tasks, and no duplicate Writer after repeat invocation. `up --semi` and its approval policy remain R5 work. |
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
| T4.6 | done | Select verified Writer and Critic adapters per project | `.talos/config.json` selects roles independently. Antigravity 1.5.10 is the only accepted adapter for both roles; missing or unverified role choices are refused. `doctor` reports the selected roles. |

## R5 — Verification evidence and permissions

Dependency: R3/R4.

| ID | Status | Task | Exit criteria |
|---|---|---|---|
| T5.1 | planned | Support verifiable acceptance criteria and project checks | Command, CWD, version, and exit code recorded; unknown command is not skipped. |
| T5.2 | planned | Compare tree facts and Git diff with the claimed result | HEAD, dirty paths, and diff digest match the attempt being verified. |
| T5.3 | planned | Apply Critic patches only after digest verification | Changed patch/base is blocked before application. |
| T5.4 | planned | Design Auto/Semi action policy | Auto uses only the approved allowlist; Semi requests approval at policy boundaries; deny rules outrank allows. |
| T5.5 | planned | Redact credentials and secrets from logs/reports | Test secrets never appear in runtime logs, reports, database events, or final output. |
| T5.6 | planned | Close tasks only with complete evidence | Failed/unrun checks, open review, or invalid Git facts block completion. |
| T5.7 | planned | Keep local TALOS runtime state out of Git | Ensure `.talos/` is excluded in each Git project or fail with actionable setup guidance; do not silently rewrite tracked project files. Verify normal repositories and worktrees. |

## R6 — tmux, manual takeover, and recovery

Dependency: R3 and R5.

| ID | Status | Task | Exit criteria |
|---|---|---|---|
| T6.1 | planned | Create and reconnect tmux control session | Repeated launch creates no duplicate workers/panes; without tmux, foreground mode is clear. |
| T6.2 | planned | Support pause/resume/stop and manual takeover | Pause preserves tasks; manual edits are detected and reconciled before continuing. |
| T6.3 | planned | Recover after CLI/TALOS exit and SIGTERM/SIGKILL | Test separate failure points; transitions preserve the last confirmed write. Address stale 35-minute lease recovery after a hard crash. Review the narrow numeric process-group ID reuse window in runner cleanup and ensure cleanup cannot signal an unrelated group; if the platform cannot prove the target, fail closed. |
| T6.4 | planned | Check SSH reconnect on macOS and record deferred Linux boot recovery | macOS state and status remain visible after reattach. Linux boot recovery and systemd user autostart remain deferred until Linux runtime support is implemented under T6.5/T9.3. |
| T6.5 | planned | Implement Linux process identity and complete Linux server acceptance | Do not use private macOS `libproc` APIs or parse `ps`. macOS remains fail-closed when no supported public identity source is available. Implement the Linux `/proc` process identity path as part of the codebase. If Linux-target compilation, runtime, or Agy sandbox checks require cumbersome setup on macOS, defer them to the user's Linux server; no Docker/VM or heavy local Linux matrix is required. Keep Linux support unverified until those checks pass. |
| T6.6 | planned | Verify interactive handoff and return to headless resume | Test after the workspace trust decision in a disposable project; if same-session continuity is unsupported, use an explicit new context packet and report the difference. |

## R7 — DAG, subagents, and worktrees

Dependency: R5/R6.

| ID | Status | Task | Exit criteria |
|---|---|---|---|
| T7.1 | planned | Add task dependencies and DAG scheduler | Cycles are rejected; dependent work waits for prerequisites. |
| T7.2 | planned | Define bounded child-task contract | Each child has a parent, allowed scope, acceptance criteria, timeout, and report shape. |
| T7.3 | planned | Detect native subagents per CLI | Missing support disables only that delegation path. |
| T7.4 | planned | Create/manage task worktrees only when useful | Branch and lifecycle are recorded; dirty worktrees are not auto-deleted. |
| T7.5 | planned | Bound concurrency and integrate changes | Unique lease/worktree; conflicts remain reviewable; run real checks after integration. Acceptance includes one Writer, one independent Critic, and at least one additional bounded agent (three total) with separate scopes and workspaces. |

## R8 — GitHub

Dependency: R5/R7.

| ID | Status | Task | Exit criteria |
|---|---|---|---|
| T8.1 | planned | Check `gh` auth and GitHub capability without reading token values | Diagnostics show access without leaking credentials. |
| T8.2 | planned | Create feature branch, push, and PR under an explicit policy | Push is limited to a named feature branch; URL and diff hash are recorded. |
| T8.3 | planned | Wait for CI and attach evidence | Status comes from GitHub; timeout/error remains visibly incomplete. |
| T8.4 | planned | Keep merge under user control | No automatic merge or deletion of a branch with unverified changes. |

## R9 — Hardening and delivery

Dependency: R6–R8.

| ID | Status | Task | Exit criteria |
|---|---|---|---|
| T9.1 | planned | Run the end-to-end scenarios from the original request | Each of the 15 scenarios has a result and evidence; untested cases are explicit. |
| T9.2 | planned | Run long-duration/load and fault-injection checks | Memory/log limits, stuck children, and incomplete writes do not destroy working state. |
| T9.3 | planned | Prepare macOS install/upgrade/uninstall and Linux packages | Verify macOS install/upgrade/uninstall without deleting `.talos` unless explicitly requested. Complete reproducible Linux build, runtime, and package support after T6.5; use the user's server for native checks when local Linux setup would slow development, and do not claim verified Linux support before those checks pass. |
| T9.4 | planned | Finish English documentation and operator runbook | Commands and limits match the binary and locally supported CLI versions. |
| T9.5 | planned | Build release artifacts | Produce reproducible initial macOS artifacts. Add reproducible Linux artifacts only after Linux build/runtime/package support passes T6.5/T9.3. |

## Required end-to-end scenarios from the original request

First launch/repeat `up`; Writer/Critic success and justified rejection; child task; independent parallel work; adding a task while work is running; manual takeover; CLI recovery; orchestrator crash; SSH disconnect/reconnect; Linux reboot autostart; PR without merge; duplicate-execution prevention; and rechecking original acceptance criteria before closeout. Keep separate evidence for each scenario; do not combine them into a misleading test.

## Unscheduled ideas — revisit after R9

| ID | Status | Idea | Note |
|---|---|---|---|
| F1 | idea | Add a local project-origin and history journal for Git and non-Git projects | Revisit after the current roadmap. Explore project registration/creation milestones, plan and decision history, and references to files or reports produced during work. Keep structured metadata local in SQLite; evaluate path, digest, size, and provenance references for larger files. Define privacy, retention, backup, export, archival, and token/context budgets before implementation. Keep rows and prompts bounded; do not store full transcripts or duplicate project trees by default. This is not a request for TALOS to generate or copy entire projects. Research: `docs/research/project-history-idea.md`. |
| F2 | idea | Evaluate typed agent handoffs and controlled technical language | Revisit after R9. This is exploratory only; TALOS has not adopted ASD-STE100 or a Russian profile. Evaluate whether typed JSON contracts, stable terminology, or optional text checks improve Writer/Critic, child-task, and handoff clarity. Measure ambiguity, false positives, maintenance cost, and token/context overhead. It may not fit TALOS. Research: `docs/research/agent-language-idea.md`. |
