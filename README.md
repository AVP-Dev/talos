<div align="center">
  <h1>TALOS</h1>
  <p><strong>Local CLI-first orchestrator for AI coding agents with Team and Pair orchestration</strong></p>

  <p>
    <a href="https://github.com/AVP-Dev/talos/releases/latest"><img src="https://img.shields.io/github/v/release/AVP-Dev/talos?label=Release&style=for-the-badge&color=blue" alt="Release" /></a>
    <img src="https://img.shields.io/badge/Status-Beta%20%2F%20Preview-yellow?style=for-the-badge" alt="Status: Beta" />
    <img src="https://img.shields.io/badge/Platform-macOS%20%7C%20Linux-black?style=for-the-badge&logo=apple" alt="macOS and Linux" />
    <img src="https://img.shields.io/badge/Rust-1.100%2B-orange?style=for-the-badge&logo=rust" alt="Rust" />
    <img src="https://img.shields.io/badge/Tests-166%20passed-brightgreen?style=for-the-badge" alt="Tests" />
  </p>

  <p>
    <a href="https://talos.avpdev.com" target="_blank" rel="noopener noreferrer">🌐 Website & Installer</a> &nbsp;&bull;&nbsp;
    <a href="https://github.com/AVP-Dev/talos/releases" target="_blank" rel="noopener noreferrer">📦 Releases</a> &nbsp;&bull;&nbsp;
    <a href="./docs/operator-runbook.md">📖 Operator Runbook</a> &nbsp;&bull;&nbsp;
    <a href="./docs/architecture.md">⚙️ Architecture</a>
  </p>
</div>

---

> [!NOTE]
> **Beta / Early Access:** TALOS is currently in active preview. Core multi-CLI orchestration, worktree isolation, automated verification gates, and Team/Pair recovery loops are functional and verified end-to-end. Community feedback and issue reports are welcome!

## ⚡ What is TALOS?

**TALOS** is a local, CLI-first orchestrator for installed AI coding CLIs. It stores tasks, execution attempts, events, and subtasks in a durable local SQLite database, supervises AI CLI processes across 7 supported adapters (`agy`, `codex`, `omp`, `pi`, `kilo`, `opencode`, `claude`), coordinates Writer and Critic teams, verifies results against automated project checks, and commits verified code directly to Git.

**Zero babysitting:** give TALOS a task once, and it orchestrates planning, parallel implementation in isolated Git worktrees, independent review, defect correction, and verification checks end-to-end.

---

## 🚀 Core Highlights

- **Dual Orchestration Modes:**
  - **Team Mode (`--mode team`):** Two-tier team collaboration. Writer Lead autonomously plans, decomposes subtasks, supervises parallel Writer Workers in isolated Git worktrees, assists failing workers, and integrates code. Critic Team performs multi-angle audits, and Critic Lead renders the authoritative verdict with automated rebuttal and fix loops.
  - **Pair Mode (`pair`, default):** Sequential Writer → Critic → Verify → Complete pipeline.
- **Reasoned Deliberation & Rebuttal:** Writer Lead can provide structured technical justification or code fixes upon review feedback. Passing tests inform discussion but do not automatically excuse architectural flaws.
- **Adaptive Task Triage:** Pre-flight advisory triage evaluates task complexity and recommends depth without rigid workflow lock-in.
- **Backlog Curation:** Deferred non-critical observations (`Minor`, `Note`, cross-cutting findings) are preserved in SQLite and surfaced in per-task reports under `.talos/reports/`.
- **Anti-Stagnation Protection:** Automatically detects identical diffs across fix iterations and halts futile loops early to conserve tokens.
- **7 AI CLI Adapters:** Native headless runners for `codex`, `agy` (Antigravity), `claude` (Claude Code), `omp`, `pi`, `kilo`, and `opencode`.
- **Deterministic Rust Core:** SQLite WAL (`.talos/state.sqlite3`, schema v19). No Docker, no Kubernetes, no Python runtime, and no separate local server required.
- **Git Worktree Isolation:** Parallel workers run in isolated checkouts with strict path containment enforcement.
- **Automated Verification Gate:** Runs project test suites (`pytest`, `cargo test`, `npm test`) and requires exit 0 before task completion. Failed checks cleanly abort completion and re-route tasks for fixes.

---

## 📦 Installation

### macOS (Homebrew)

```sh
brew install AVP-Dev/apps/talos
```

To update a Homebrew installation, run `brew update && brew upgrade AVP-Dev/apps/talos`.
If your shell still picks an earlier copy, run `type -a talos` to see the search order.
`talos doctor` reports the version and path of the running binary.

### Linux (release installer)

```sh
curl -fsSL https://talos.avpdev.com/install.sh | bash
```

The installer places the binary in `~/.local/bin/talos`.

### From Source (Cargo)

```sh
cargo install --locked --path .
```

---

## Quick Start

1. Open a terminal in an existing Git project and run `talos`. From another folder, enter the project path when prompted.
2. TALOS suggests installed agent CLIs for **Writes code** and **Reviews changes**. Press **F2** or enter `/config` to change either role, its model, or the mode.
3. Describe your task and press **Enter** to review it. Check the roles and project checks, then press **Enter** again to run it. A blocked draft explains what to fix and keeps the task description.
4. Open the result in **History** with **F3**. Enter `/new` or press **F4** to start a fresh session in the same window. Project settings and earlier results remain available.

**Editing:** Use the arrow keys and Backspace to edit, **Ctrl+W** or **Alt+Backspace** to delete a word, and **Esc** to clear the input. The terminal can paste multiline descriptions.

The [Operator Runbook](docs/operator-runbook.md) covers terminal commands, setting up project checks, and recovery.

---

## 🛠 Command Reference

| Command | Description |
| :--- | :--- |
| `talos` | Launch the interactive Terminal Orchestrator Cockpit (`tui`) |
| `talos attach` | Create or reconnect to the persistent tmux control shell |
| `talos config` | View current configuration (`writer`, `critic`, `mode`, checks) |
| `talos config set <KEY> <VAL>` | Configure roles (`writer`, `critic`, `mode`, `max-writers`) |
| `talos doctor` | Probe installed AI CLIs, Git, tmux, and dependencies |
| `talos task add "<TITLE>" --criteria "..."` | Queue a task with verification criteria |
| `talos task list` | Display active, queued, and completed tasks |
| `talos up --auto [--mode team]` | Run autonomous execution loop to completion |
| `talos status` | Inspect current worker, worktree, and queue status |
| `talos logs [--project]` | View persistent structured event audit log |
| `talos task recover` | Safely quarantine abandoned or interrupted attempts |
| `talos pause` / `talos resume` | Operator control to safely pause or resume dispatch |

---

## 📖 Documentation

- [Operator Runbook](docs/operator-runbook.md) — Day-to-day operations and recovery workflows
- [Architecture & Invariants](docs/architecture.md) — System design, state machine, and data flow
- [Acceptance Scenarios](docs/acceptance-scenarios.md) — Verified end-to-end acceptance tests
- [Design Decisions (ADRs)](docs/decisions.md) — Accepted architectural choices

---

## 🤝 Support & Contribution

TALOS is an open-source project created by **Aliaksei Patskevich (AVPDev)**.

If you have architectural suggestions, feature requests, or bug reports, feel free to [Open an Issue](https://github.com/AVP-Dev/talos/issues) or reach out directly:

<p align="center">
  <a href="https://avpdev.com/"><b>Aliaksei Patskevich (AVPDev)</b></a>
  <br />
  <sub>
    <b>AI Solutions Architect</b> • Code, Design & AI
    <br />
    <a href="https://github.com/AVP-Dev">GitHub</a> &bull; <a href="https://t.me/AVP_Dev">Telegram</a>
  </sub>
</p>

<p align="center">
  <img src="https://img.shields.io/badge/Rust-DEA584?style=flat-square&logo=rust&logoColor=black" alt="Rust" />
  <img src="https://img.shields.io/badge/SQLite-003B57?style=flat-square&logo=sqlite&logoColor=white" alt="SQLite" />
  <img src="https://img.shields.io/badge/Git-F05032?style=flat-square&logo=git&logoColor=white" alt="Git" />
  <img src="https://img.shields.io/badge/Homebrew-FBB040?style=flat-square&logo=homebrew&logoColor=black" alt="Homebrew" />
  <img src="https://img.shields.io/badge/macOS-000000?style=flat-square&logo=apple&logoColor=white" alt="macOS" />
  <img src="https://img.shields.io/badge/Linux-FCC624?style=flat-square&logo=linux&logoColor=black" alt="Linux" />
</p>
