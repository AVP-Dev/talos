# TALOS — Development Guide

This file adapts Agent Starter Kit to TALOS. It helps contributors understand the project and continue work; it is not a collection of mandatory rituals. User intent and the current project state take precedence over generic process templates.

## Project language

English is the only language used inside TALOS: source code, comments, prompts, system instructions, CLI output, logs, errors, reports, README files, and all documentation. Keep product strings and examples in English. Human conversation may follow the user's language, but project artifacts remain English.

## Start every session here

Before working, read:

1. [`README.md`](README.md) — project purpose and entry points.
2. [`docs/state.md`](docs/state.md) — current checkpoint, verified facts, and next action.
3. [`docs/plan.md`](docs/plan.md) and [`docs/tasks.md`](docs/tasks.md) — delivery rounds, dependencies, and task status.
4. [`docs/decisions.md`](docs/decisions.md) — accepted decisions and their rationale.

Continue from the current open task. Do not rely on the previous chat as the only source of project state.

## Purpose and technology

TALOS is a local CLI-first orchestrator for installed AI coding CLIs. It persists tasks, supervises processes, coordinates Writer/Critic, verifies results, and recovers work.

Preferred stack: Rust stable, Tokio, SQLite WAL through `rusqlite`, `serde` / `serde_json`, `clap`, `tracing`, Git CLI, and tmux. Treat this as the starting point from the user's request, not a reason to add every component in advance. Add dependencies and platforms when a concrete delivery round needs them.

The product must work without Agent Starter Kit, MCP, a Python runtime, Docker, Redis, a message broker, PostgreSQL, Kubernetes, or a separate local server. Starter Kit content supports TALOS development only; it is not a runtime dependency.

## Hard boundaries

- Do not modify the original Agent Starter Kit or pair-mailbox repositories.
- Do not publish code, create a PR, or merge into the primary branch without the user's explicit instruction.
- Do not run production operations, change secrets, or print credentials/environment values in logs.
- Do not enable dangerous permission bypasses or unrestricted sandbox modes by default.
- Do not use `git reset --hard` or other destructive commands as a general recovery method.
- Do not treat a Git worktree as an OS security sandbox.
- Do not present CLI help, mocks, model claims, or log entries as proof that a capability or task succeeded.
- Do not depend on private CLI session stores, undocumented interfaces, or ANSI/TUI parsing.

Handle ordinary reversible implementation choices within the agreed round; do not ask for confirmation on every routine edit.

## Recommended working practices

- Work in small vertical slices and leave a usable result after each round.
- Before changing a schema, authority model, recovery path, or security boundary, briefly write down invariants and failure cases.
- Check current library and CLI documentation when implementation depends on their APIs or behavior; the locally installed version and public interface matter.
- Separate CLI, storage, queue, adapters, process supervision, and Git/tmux integration by real responsibility. Avoid abstractions with no second use case.
- Keep state transitions and invariants in deterministic Rust code. Models make technical decisions only within granted authority.
- Account for retries, cancellation, timeouts, and parent-process loss. Inspect completed effects before retrying.
- Keep one Writer per checkout and use an independent read-only Critic. Enable parallel edits only for independent tasks with explicit file ownership.
- Record limitations. If a CLI capability or check cannot be verified in the environment, do not mark it complete.
- Follow the round plan; do not begin product implementation while the project is waiting for plan review or the user has not started that round.

## Verification

For Rust changes, use the checks relevant to the current slice: `cargo fmt --all -- --check`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo test --all-targets`, `cargo build --release`, and `./scripts/security-scan.sh`. Record exact commands and outcomes in `docs/state.md`.

`scripts/verification-profile.tsv` must contain real project commands. Do not activate a profile until the crate and those commands exist and have been checked.

## Cross-session source of truth

- `docs/plan.md` — product direction, delivery rounds, dependencies, and exit criteria.
- `docs/tasks.md` — one backlog with stable IDs, status, dependencies, and outcomes.
- `docs/decisions.md` — accepted architecture and user direction with rationale.
- `docs/state.md` — the handoff: current round, complete/incomplete work, checks, risks, and the next exact action.

Update `docs/tasks.md` and `docs/state.md` after each completed work batch and before ending a session. The next agent must be able to continue from project files without access to the previous conversation.
