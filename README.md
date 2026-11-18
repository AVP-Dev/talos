<div align="center">
  <h1>TALOS</h1>
  <p><strong>A local terminal cockpit for AI coding agents</strong></p>
  <p>Assign a Writer and a Critic, run a Git task, and inspect the verified result.</p>
  <p>
    <a href="https://github.com/AVP-Dev/talos/releases/latest"><img src="https://img.shields.io/github/v/release/AVP-Dev/talos?style=for-the-badge&amp;label=release&amp;color=3977bd" alt="Latest release"></a>
    <a href="https://github.com/AVP-Dev/talos/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/AVP-Dev/talos/ci.yml?branch=master&amp;style=for-the-badge&amp;label=CI" alt="Continuous integration status"></a>
    <a href="#install"><img src="https://img.shields.io/badge/macOS%20%7C%20Linux-334155?style=for-the-badge" alt="macOS and Linux"></a>
    <a href="LICENSE"><img src="https://img.shields.io/badge/license-Apache--2.0-425a70?style=for-the-badge" alt="Apache 2.0 license"></a>
  </p>
  <p>
    <a href="#install">Install</a> ·
    <a href="#run-your-first-task">Quick start</a> ·
    <a href="docs/operator-runbook.md">Operator guide</a> ·
    <a href="docs/architecture.md">Architecture</a> ·
    <a href="https://talos.avpdev.com/">Website</a>
  </p>
</div>

---

<div align="center">
  <img src="docs/media/cockpit.png" width="960" alt="TALOS Unified Feed, with Writes and Reviews roles, the task queue and the command line in Agterm">
</div>

<p align="center"><sub>TALOS v0.1.1 cockpit in Agterm, shown in Pair mode.</sub></p>

## Install

**macOS with Homebrew**

```sh
brew install AVP-Dev/apps/talos
```

**macOS or Linux with a release archive** — download the archive for your CPU and its matching `.tar.gz.sha256` file from [GitHub Releases](https://github.com/AVP-Dev/talos/releases). Follow the [checksum and install steps](docs/operator-runbook.md#release-archives). Each release lists the binaries it ships.

You need Git, a project with a first commit, and at least one configured coding CLI. Sign in through that CLI if it requires an account. To check your TALOS installation and discover agents, run `talos doctor`. It also reports the executable path if a previous install shadows the new one.

## Run your first task

1. Run `talos` in your project, or enter the project path when prompted.
2. Check **Writes** and **Reviews**. Open `/config` or press **F2** to assign each role and model. Start in **Pair** mode.
3. Describe a change and press **Enter** to review the draft. Check the roles and project checks, then press **Enter** again to run that task. If Git changes block the run, **D** opens the diff and **F5** returns to the draft.
4. Open the result and press **D** to see its saved Git changes. Use `/history` or **F3** to reopen earlier results. Use `/new` or **F4** while idle to start a fresh task with fresh agent conversations and the same project settings.

<details>
<summary><strong>Watch the cockpit walkthrough</strong></summary>
<br>
<img src="docs/media/cockpit-demo.gif" width="960" alt="Native Agterm recording of TALOS role settings, task confirmation, saved Git changes and a new session">
</details>

The screenshots and animation capture the built TALOS interface in Agterm with a disposable Git project. Deterministic fixture CLIs stand in for Codex and Claude Code, and TALOS runs a real project check. See the [acceptance record](docs/acceptance-scenarios.md) for live provider and platform coverage.

## Choose a mode

| Mode | Workflow |
| --- | --- |
| **Pair** (default) | A Writer makes the change. A separate Critic reviews it. TALOS runs the project's saved checks, when configured, against the approved revision. |
| **Team** (opt-in, preview) | A Writer Lead can split work into parallel Git worktrees, integrate the changes, and respond to Critic findings. Parallel work depends on the plan, configured limits and project conditions. |

TALOS includes adapters for Codex (`codex`), Claude Code (`claude`), Antigravity (`agy`), OMP (`omp`), Pi (`pi`), Kilo (`kilo`), and OpenCode (`opencode`). Provider versions, account setup, permissions and available models vary. Adapter detection confirms local availability, and the [acceptance record](docs/acceptance-scenarios.md) describes which combinations have live evidence.

## Use TALOS from a shell

```sh
talos config show
talos task add "Add a test for empty parser input" --criteria "The test covers empty input and the existing suite passes."
talos task list
talos up --auto --task 1
```

Replace `1` with the ID returned by `talos task add`. Run `talos up --auto` without a task ID when you intend to process the runnable queue.

## Documentation

- [Operator guide](docs/operator-runbook.md) — install, configure checks, operate the queue and recover an interrupted attempt
- [Architecture](docs/architecture.md) — local state, execution stages and boundaries
- [Acceptance evidence](docs/acceptance-scenarios.md) · [Architecture decisions](docs/decisions.md) · [Release notes](docs/releases/v0.1.1.md)
- [Contribute](CONTRIBUTING.md) · [Report an issue](https://github.com/AVP-Dev/talos/issues)

---

<div align="center">
  <p>TALOS is a beta project by <a href="https://avpdev.com/">Aliaksei Patskevich (AVPDev)</a>.</p>
  <p><a href="https://github.com/AVP-Dev">GitHub</a> · <a href="https://t.me/AVP_Dev">Telegram</a> · <a href="LICENSE">Apache 2.0</a></p>
</div>
