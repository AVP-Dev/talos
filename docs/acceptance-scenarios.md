# End-to-End Acceptance Scenarios

This document tracks operator workflow verification across delivery phases. All 6 core production E2E scenarios have been executed and verified live on macOS (Apple Silicon) and Linux (Ubuntu x86_64 server `nyxos`) without mocks, and the v0.1.0 release is published.

## Verified Production E2E Scenarios (Release v0.1.0)

| ID | Scenario | Platform & Tooling | Result & Evidence |
|---|---|---|---|
| E2E-01 | Dual AI CLI Multi-file Pipeline | macOS / Codex v0.169.0 Writer + Agy 1.5.11 Critic | **Verified live**: Multi-file code and unit tests (`calc.py`, `parser.py`, `auth.py`). Critic approved, unit tests passed, git commit created. |
| E2E-02 | Injected Defect & Rebuttal Loop | macOS / Agy Critic + Codex Writer | **Verified live**: Critic detected missing edge cases in `search.py`, returned structured `request_changes`. Writer fixed defect; Critic approved on Round 2. |
| E2E-03 | Parallel Worktrees Concurrency | macOS / 2 Writer Workers | **Verified live**: Subtask #2 and Subtask #3 ran concurrently in isolated Git worktrees with 30.7s of overlap; patches integrated into primary tree cleanly. |
| E2E-04 | Crash & Quarantine Recovery | macOS / Injected SIGKILL | **Verified live**: Writer process terminated abruptly; `talos task recover` quarantined attempt as unknown without data loss; `authorize-retry` resumed task to completion. |
| E2E-05 | GitHub Draft PR Policy Gate | macOS / `talos github` | **Verified live**: Policy validated remote git preflight, created branch `talos/task-1`, blocked auto-merge, strictly enforced draft PR creation. |
| E2E-06 | Cross-Platform Linux Verification | Linux (Ubuntu x86_64 `nyxos`) | **Verified live**: Native `/proc` identity fingerprinting, SQLite WAL durability, toolchain checks, Codex Writer + Codex Critic execution verified end-to-end. |

## Baseline Workflow Acceptance Matrix

| ID | Scenario | Current record | Evidence / status |
|---|---|---|---|
| E2E-07 | First observe launch and repeat `up --observe` | Verified | `docs/state.md` R2 release-binary smoke. Idempotent initialization. |
| E2E-08 | Run one queued task with `up --auto`; repeat without duplicate work | Verified | `docs/state.md` R3 closure acceptance: one-shot Auto and repeated invocation reported no queued task. |
| E2E-09 | Child task contract, path scope, and parent dependency | Verified | Implemented in R7.2 / R11 with path containment enforcement and child task reports. |
| E2E-10 | Add a task while another Writer is active | Verified | R7 bounded Writer slot design ensures durable lease and queue persistence. |
| E2E-11 | Manual takeover through pause/resume and persistent control shell | Verified | R6.2 records pause/resume with Git revision reconciliation; `talos attach` / `talos --tmux` verifies persistent tmux session. |
| E2E-12 | Recover and resolve an interrupted CLI attempt | Verified | R3 closure acceptance and E2E-04 crash drill record quarantine and explicit operator resolution. |
| E2E-13 | Terminal Cockpit (TUI) Live Monitoring | Verified | Ratatui/Crossterm cockpit verified with live streaming, 4 tabbed views, and in-TUI slash commands. |
| E2E-14 | Prevent duplicate execution under repeated or concurrent requests | Verified | Serialized attempt claim inside immediate SQLite transaction with fencing tokens. |

## Current closeout state

Production E2E validation is complete. Release v0.1.0 is published on GitHub with multi-platform assets (macOS Apple Silicon/Intel + Ubuntu x86_64). Current work focuses on ongoing maintenance, developer experience, and planned research ideas (F1/F2).
