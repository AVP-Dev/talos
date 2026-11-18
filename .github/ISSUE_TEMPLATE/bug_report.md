---
name: Bug Report
about: Report an issue or unexpected behavior in TALOS
title: '[BUG] '
labels: bug
assignees: ''
---

**Describe the bug**
A clear and concise description of what happened.

**Environment Information**
- OS: [e.g. macOS 14.5 Apple Silicon / Ubuntu 24.04 x86_64]
- TALOS version: [e.g. 0.1.0 (from `talos --version`)]
- Active AI CLIs: [e.g. Codex v0.169.0, Antigravity 1.5.11, Claude Code]
- Orchestration Mode: [`pair` or `team`]

**Steps to Reproduce**
1. Run `talos task add "..." --criteria "..."`
2. Run `talos up --auto`
3. Error or unexpected behavior observed

**Expected Behavior**
What should have occurred instead.

**Diagnostics and Logs**
Please paste output from:
```bash
talos doctor
talos logs --project --limit 20
```
