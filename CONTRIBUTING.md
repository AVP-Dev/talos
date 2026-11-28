# Contributing to TALOS

Thank you for your interest in contributing to TALOS! We welcome contributions, bug reports, and architectural feedback.

## Guiding Principles

1. **English Only:** All source code, commit messages, comments, pull requests, issue descriptions, and documentation must be in English.
2. **Determinism & Safety:** TALOS coordinates AI processes through deterministic Rust state machines and SQLite WAL. Avoid nondeterministic heuristics or private CLI session parsers.
3. **Verification First:** Every pull request must pass the full test suite and quality gates before merge.

## Development Workflow

### Prerequisites

- Rust stable (1.100 or newer)
- Git (2.38+ for worktree support)
- SQLite 3 (bundled with `rusqlite`)

### Local Build and Testing

Run the standard check suite before opening a PR:

```bash
# 1. Format code
cargo fmt --all -- --check

# 2. Run clippy linter
cargo clippy --locked --all-targets --all-features -- -D warnings

# 3. Run all unit and integration tests
cargo test --locked --all-targets

# 4. Security scan
./scripts/security-scan.sh
```

## Pull Request Guidelines

- Keep two long-lived branches: `master` for the verified release baseline and `dev` for ongoing development and testing.
- Work on `dev`; promote a verified batch through a `dev` → `master` pull request. Synchronize `dev` with `master` afterward. Create stable release tags only after the applicable acceptance checks pass.
- Keep changes scoped to a single logical slice.
- Include corresponding unit tests for new functionality or bug fixes.
- Fill out the PR template with verification evidence.

## Reporting Issues

- For bug reports, use the [Bug Report template](.github/ISSUE_TEMPLATE/bug_report.md) and include output from `talos doctor` and `talos logs`.
- For feature requests, describe the target CLI adapter or orchestration flow and why it benefits developers.
