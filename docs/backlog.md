# TALOS Development, Usability & Optimization Backlog

> Pragmatic, high-value ideas for TALOS development, developer ergonomics (DX/UX), and engine optimization.

---

## 1. Ergonomics & Usability (Удобство и UX/DX)

### IDEA-01: Tab-Completion & Argument Hints in TUI Command Bar
- **Value**: High
- **Context**: Currently, the TUI command bar requires full manual typing for commands (`/cli codex`, `/commit <msg>`, `/mode team`) and project file paths.
- **Proposal**: Implement Crossterm/Ratatui inline completion on `Tab`:
  - Slash command completion (`/c<Tab>` -> `/cli`, `/commit`, `/critic`).
  - Adapter argument completion (`/cli c<Tab>` -> `codex`, `claude`).
  - Relative file path completion based on repository tree (`tests/t<Tab>` -> `tests/test_calc.py`).
- **Impact**: Makes TUI interaction as fast and fluid as modern shells (Fish/Zsh), eliminating typos.

### IDEA-02: Task Presets & Archetypes (`/task:test`, `/task:refactor`, `/task:fix`)
- **Value**: High
- **Context**: Developers frequently submit recurring patterns: adding test suites, fixing defects, or optimizing existing functions.
- **Proposal**: Add parameterized task shortcuts:
  - `/task:test <file>`: Automatically constructs criteria *"Add comprehensive unit tests for <file> without altering production logic. Ensure 100% test pass."*
  - `/task:refactor <file>`: Automatically sets `--criteria "Refactor <file> to improve readability and structure. All existing checks must pass."*
  - `/task:bench <func>`: Creates benchmark tasks.
- **Impact**: Saves time on prompt crafting and guarantees strict, deterministic acceptance criteria.

### IDEA-03: Interactive Hunk & File Staging in TUI Git Diff View
- **Value**: High
- **Context**: An AI agent may implement a requested feature correctly, but accidentally touch whitespace or leave temporary debug statements in another file.
- **Proposal**: In Tab 3 (`Git Diff`), allow arrow-key navigation through modified files with single-key actions:
  - `s`: Stage file.
  - `d`: Discard / revert file changes immediately (`git checkout -- <file>`).
  - `c`: Commit staged files with prompt.
- **Impact**: Operator can curate the agent's work directly in TUI without switching to an external terminal.

---

## 2. Real-Time Performance & Streaming (Оптимизация и скорость)

### OPT-01: In-Memory Low-Latency IPC Streaming (PTY / Tokio Channel)
- **Value**: High
- **Context**: Currently, the background supervisor (`talos up --auto`) redirects stdout/stderr into `.talos/supervisor.log`, and TUI reads it every 250ms from disk.
- **Proposal**: Connect the supervisor and Cockpit via an in-memory Tokio broadcast channel or local Unix domain socket.
- **Impact**: Sub-millisecond stream rendering, zero disk I/O churn, and live character-by-character streaming ("typewriter effect") of agent planning and thoughts.

### OPT-02: AST & Interface Slicing for Token Economy
- **Value**: High
- **Context**: Sending full 2,000-line dependency files to AI CLIs consumes prompt budgets and dilutes attention.
- **Proposal**: When a task targets a specific file, generate a compact symbol outline (AST function signatures, public types, and docstrings) for referenced files rather than their full implementation.
- **Impact**: Reduces token usage by 50–70% and speeds up agent response times by 2–3x.

### OPT-03: Live Token Consumption & Session Cost Meter in Cockpit
- **Value**: Medium-High
- **Context**: Operators running multiple concurrent LLMs need visibility into quota consumption.
- **Proposal**: Add a compact gauge widget in the AI Agents Cockpit Sidebar:
  `[████████░░] 82% (105k / 128k tokens) | Est. Cost: $0.42`
- **Impact**: Prevents sudden context overflows and provides real-time economic transparency.

---

## 3. Test Harness & Quality Assurance (Харнес и надежность)

### HARN-01: Dedicated Process-Signal Test Isolation (`tests/process_signals.rs`)
- **Value**: High
- **Context**: Tests that spawn child process groups and test `SIGTERM`/`SIGKILL` run concurrently with the 152-test unit test suite. Under heavy CPU thread contention, process scheduling can introduce test runner jitter.
- **Proposal**: Separate process-signal integration tests into `tests/process_signals.rs` with `#[serial]`.
- **Impact**: Keeps `cargo test --lib` blazing fast (<1.5s) and 100% deterministic on all platforms.

### HARN-02: Deterministic MockAdapter Matrix for Offline CI Verification
- **Value**: High
- **Context**: Testing multi-round Team delegation and Critic rejections currently requires installed, authenticated AI CLIs or network calls.
- **Proposal**: Implement a test-only `MockAdapter` capable of replaying scripted NDJSON streams, emulating rate limits, and simulating process exits.
- **Impact**: Enables verifying complete 10-step Team orchestration and worktree integration in CI without API keys or external tools.
