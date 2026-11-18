# TALOS Development, Optimization & Test Harness Backlog

> This document tracks strategic architectural ideas, optimization opportunities, and test harness enhancements identified during TALOS development.

## 1. Architecture & Feature Enhancements

### IDEA-01: Low-Latency IPC Streaming for TUI Feed
- **Observation:** Currently, the background supervisor (`talos up --auto`) spawned by the TUI redirects stdout/stderr into `.talos/supervisor.log`, and the TUI polls the log file every 250ms via standard file reading.
- **Proposal:** Replace disk file polling with an in-memory Unix domain socket or shared Tokio broadcast ring buffer between supervisor daemon and the Ratatui Cockpit.
- **Benefits:** Sub-millisecond stream rendering, zero disk I/O churn, and live character-by-character streaming of model responses and planner thoughts.

### IDEA-02: Live Token & Cost Budget Meter in TUI Cockpit
- **Observation:** Users working with multiple CLIs (OpenAI Codex, Anthropic Claude, Gemini Antigravity) want clear visibility of session costs and token consumption.
- **Proposal:** Introduce a live gauge widget in the AI Agents Cockpit Sidebar displaying estimated token consumption against model context limits (e.g., `[████████░░] 82% 105k / 128k tokens`) and accumulated session cost ($).
- **Benefits:** Prevents unexpected context window overflows and gives transparent cost visibility.

### IDEA-03: Interactive Hunk & Patch Staging in TUI Git Diff Tab
- **Observation:** When reviewing agent contributions in Tab 3 (`Git Diff`), the operator currently inspects the unified diff read-only, but may want to selectively accept or reject specific files or hunks.
- **Proposal:** Add interactive commands in Tab 3: `/stage <path>`, `/unstage <path>`, `/discard <path>`, allowing fine-grained operator curation before triggering verification checks.
- **Benefits:** Greater operator control when an agent produces a good implementation alongside minor unwanted modifications.

### IDEA-04: Multi-Model Ensemble Reviews
- **Observation:** In Team Mode, Critic Workers are spawned sequentially or in parallel under the Critic profile.
- **Proposal:** Allow Critic Workers to be sharded across multiple *different* AI CLI vendors simultaneously (e.g. Claude audits security, Codex audits algorithms, Antigravity audits test completeness), with Critic Lead synthesizing all three perspectives into the final verdict.
- **Benefits:** Eliminates single-vendor blind spots and leverages specialized model strengths.

---

## 2. Optimization & Performance

### OPT-01: SQLite WAL Vacuum & Event Journal Pruning
- **Observation:** For long-running projects with hundreds of tasks, SQLite WAL and `task_events` accumulate thousands of rows.
- **Proposal:** Introduce periodic automatic truncation of old task events older than 30 days (while preserving task definitions, attempts, and verification evidence), accompanied by an explicit `talos vacuum` command.
- **Benefits:** Keeps database size under 500 KB and ensures cursor pagination queries remain instantaneous.

### OPT-02: In-Memory Worktree Git Object Sharing
- **Observation:** Creating Git worktrees currently creates linked working directories on disk (`.talos-worktrees/`).
- **Proposal:** Evaluate whether transient subtasks can use ephemeral worktrees or in-memory Git object indices (`git write-tree`) for rapid parallel subtask diffing, writing to disk only upon integration.
- **Benefits:** Speeds up worker spawn and cleanup times from hundreds of milliseconds to under 20ms.

---

## 3. Test Harness & Verification Improvements

### HARN-01: Dedicated Process-Signal Test Binary
- **Observation:** `cargo test --locked --all-targets` runs 154 tests concurrently. Several process-group signal tests (`ctrl_c_and_sigterm_interrupt_the_supervisor_and_reap_the_writer`, `operator_stop_request_terminates_and_reaps_the_writer_group`, `timeout_terminates_and_reaps_the_process_group`) spawn actual subprocess groups and send `SIGTERM`/`SIGKILL`. Under heavy test runner thread contention, OS process group scheduling can occasionally introduce test runner jitter.
- **Proposal:** Move process-signal integration tests into a dedicated integration test file under `tests/process_signals.rs` with `#[serial]`, keeping `cargo test --lib` blazing fast (<1.5s) and completely deterministic.
- **Benefits:** Eliminates risk of test port or PID reuse contention in CI environments.

### HARN-02: Deterministic Mock Adapter Matrix for Offline Verification
- **Observation:** Real CLI end-to-end testing requires installed and authenticated CLIs (`codex`, `agy`, etc.).
- **Proposal:** Add a test-only `MockAdapter` module that simulates CLI responses, JSON streaming, process exits, and rate limits without network or credential dependencies.
- **Benefits:** Enables running complete 10-step Team and Pair scenarios on unauthenticated headless CI servers in seconds.
