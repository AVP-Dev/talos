# TALOS Operator Runbook

## Platform Support

TALOS supports both **macOS** and **Linux**:
- **macOS:** Tested and verified on Apple Silicon (`aarch64-apple-darwin`) and Intel (`x86_64-apple-darwin`).
- **Linux:** Tested and verified on Ubuntu x86_64 (`x86_64-unknown-linux-gnu`) with native process identity fingerprinting via `/proc`.

## Installation and Updates

### 1. One-Line Installer (macOS & Linux)

The recommended installation method downloads the pre-built native binary from the official release assets, places it in `~/.local/bin/talos`, and strips macOS quarantine attributes:

```sh
curl -fsSL https://talos.avpdev.com/install.sh | bash
```

To install directly from GitHub without the custom domain:

```sh
curl -fsSL https://raw.githubusercontent.com/AVP-Dev/talos/master/install.sh | bash
```

### 2. Homebrew (macOS)

Install via the official `AVP-Dev/homebrew-apps` tap:

```sh
brew install AVP-Dev/apps/talos
```

To update:

```sh
brew update && brew upgrade talos
```

### 3. From Source (Cargo)

Rust 1.94 or newer is required to compile from source:

```sh
cargo install --locked --path .
```

To remove the installed binary:

```sh
cargo uninstall talos
```

### 4. GitHub Releases & Pre-built Tarballs

Pre-compiled tarballs and SHA-256 checksums are attached to every GitHub release (`https://github.com/AVP-Dev/talos/releases`):
- `talos-v0.1.0-aarch64-apple-darwin.tar.gz` (macOS Apple Silicon)
- `talos-v0.1.0-x86_64-unknown-linux-gnu.tar.gz` (Ubuntu/Debian Linux x86_64)

## Interactive 3-Pane Workspace (`tmux`)

When executed inside your project repository without subcommands, TALOS opens the **Terminal Orchestrator Cockpit (TUI)**:

```sh
# Inside your project repository:
talos
# Or explicitly:
talos tui
```

### Terminal Orchestrator Cockpit (TUI)

```text
┌─ TALOS ────────────────────────────────────────────── [Team / Auto] ── [● Running] ─┐
│ Project: pair-mailbox                      Queue: 1 active, 2 queued, 5 complete    │
├─────────────────────────────────────┬───────────────────────────────────────────────┤
│ 🖊  Writer Team (Codex)              │ 🛡  Critic Team (AGY)                         │
│ Adapter Profile: codex              │ Adapter Profile: agy                          │
│ Current Task: #18 Fix eval parser   │ Status: ● Auditing & Reviewing                │
│ Status: ● implementing              │ Checks Configured: 2 verification checks      │
│ Worktrees: 1 active                 │                                               │
├─────────────────────────────────────┴───────────────────────────────────────────────┤
│ 📜 Activity & Supervision Log (Auto-updates)                                        │
│ [Task #18] [writer] child_task_created: Worker 1 started in task-18-w1              │
│ [Task #18] [git] worktree_integrated: integrated patch into primary tree            │
│ [Task #18] [critic] review_completed: approved revision R3                          │
├─────────────────────────────────────────────────────────────────────────────────────┤
│ > Describe next task (Enter: Enqueue & Run | Ctrl+P: Pause/Resume | Ctrl+C/q: Exit) █
└─────────────────────────────────────────────────────────────────────────────────────┘
```

- **Interactive Task Input:** Enter your task directly in the bottom input bar. TALOS automatically persists it into SQLite and runs the autonomous supervisor loop.
- **Session Recovery & Reconnect:** If your connection drops, laptop sleeps, or SSH disconnects, background tasks keep executing. Re-opening `talos` reconnects to the active session and displays the in-progress state.
- **Safety Guards:** Detects uncommitted changes in your repository before executing tasks to prevent merge conflicts or code loss.
- **Controls:** `Ctrl+P` pauses or resumes the queue; `Ctrl+C` or `q` exits the interface without stopping background workers.

### Manual Multi-Pane Mode (`talos attach`)

If you prefer a 3-pane tmux workspace with dedicated shell panes:
```sh
talos attach
```

## Configuration Management (`talos config`)

Manage `.talos/config.json` directly from the CLI without editing JSON files by hand:

```sh
# View current project configuration:
talos config

# Set Writer adapter (agy, codex, claude, omp, pi, kilo, opencode):
talos config set writer codex

# Set Critic adapter:
talos config set critic claude

# Set orchestration mode (team or pair):
talos config set mode team

# Set max concurrent worktree writers (1 to 3):
talos config set max-writers 2

# Configure automated verification test checks:
talos config set checks '[{"id":"unit","argv":["pytest"]}]'
```

## Normal Task Workflow

1. **Verify environment and installed CLIs:**
   ```sh
   talos doctor
   ```
   *Note: TALOS automatically appends `.talos/` to `.gitignore` on first launch if not already ignored.*

2. **Queue a task with verifiable criteria:**
   ```sh
   talos task add "Add safe_eval function" --criteria "calc.py defines safe_eval(expr). Disallow eval/exec. Unit tests pass."
   ```

3. **Run autonomously to completion:**
   ```sh
   # Team Mode (parallel worktrees + lead integration):
   talos up --auto --mode team

   # Pair Mode (sequential Writer -> Critic -> Verify pipeline):
   talos up --auto
   ```

4. **Inspect tasks, evidence, and logs:**
   ```sh
   talos task list
   talos task evidence 1
   talos logs --project
   ```

## Pause, Stop, and Recovery

- **Pause dispatch:** `talos pause --reason "Operator review"` atomically prevents new Writer launches while preserving active tasks.
- **Resume dispatch:** `talos resume` resumes the queue. If manual code edits occurred, run `talos resume --reconcile` to reopen approvals for fresh Critic review.
- **Stop running process:** `talos stop` requests cooperative termination of the active Writer process group (bounded TERM-to-KILL sequence).
- **Crash quarantine:** `talos task recover` quarantines orphaned `launching` or `running` attempts as `unknown`.
- **Operator resolution:** `talos task resolve <ATTEMPT_ID> --action authorize-retry --confirm-writer-stopped --confirm-project-inspected --reason "..."` resets the task to `queued` for retry.

## Evolution & Post-v0.1.0 Enhancements

The initial `v0.1.0` release established:
- 7 AI CLI adapters (`agy`, `codex`, `claude`, `omp`, `pi`, `kilo`, `opencode`).
- Dual orchestration modes (Team Mode and Pair Mode).
- Git worktree isolation with path containment review gates.
- Automated verification gates requiring exit code 0 on project checks.

Key enhancements introduced in the current pre-release cycle:
1. **Zero-friction `.gitignore` auto-injection:** Eliminates manual `.gitignore` errors by automatically appending `.talos/` on first launch.
2. **Interactive 3-pane workspace:** Launching `talos` without arguments opens a split tmux workspace with live Writer/Critic monitors and a focused console.
3. **Dynamic `talos config` CLI:** Built-in subcommands (`talos config` and `talos config set <KEY> <VAL>`) to manage roles and settings cleanly.
4. **Automated multi-platform CI/CD releases:** GitHub Actions builds native release tarballs and checksums for both macOS and Ubuntu Linux on every release dispatch.
5. **Standardized Homebrew tap:** Migrated to `AVP-Dev/apps/talos` with formula documentation.
