# TALOS Operator Runbook

## Platform support

macOS is the currently verified development and runtime platform. TALOS contains Linux-specific process identity code, but Linux build, runtime, boot, and Agy sandbox acceptance remain unverified until the user's server is available. Do not interpret a successful macOS compile as Linux acceptance.

## Install, update, and remove

Rust 1.94 or newer is required to build from source. From a trusted TALOS checkout, install the binary with Cargo:

```sh
cargo install --locked --path .
```

Update from the checkout with:

```sh
cargo install --locked --path .
```

Installing from `--path` rebuilds and installs the local package. `--force` is available if Cargo reports a conflicting binary from another package.

Remove the installed binary with:

```sh
cargo uninstall talos
```

These commands manage Cargo's executable installation root. They do not remove project-local `.talos/` databases or backups. Cargo chooses the root from `--root`, `CARGO_INSTALL_ROOT`, Cargo configuration, `CARGO_HOME`, or `$HOME/.cargo`, in that order. Use the same root option when uninstalling a custom-root install. See the official [Cargo install](https://doc.rust-lang.org/cargo/commands/cargo-install.html) and [Cargo uninstall](https://doc.rust-lang.org/cargo/commands/cargo-uninstall.html) references.

On macOS, `scripts/package-macos.sh` builds the native ARM64 artifact by default. Pass `x86_64-apple-darwin` to build for Intel when that target is installed. The script requires a clean committed worktree and writes the binary, README, build manifest, and SHA-256 checksums under the ignored `dist/` directory. It records the compiler and lockfile digest so another build can be compared. Byte-for-byte reproducibility has not yet been verified. The current artifact is unsigned and not notarized; it is a local build output, not a ready-to-publish macOS distribution.

On Linux, `scripts/package-linux.sh` builds for the host's native GNU target when it is `x86_64-unknown-linux-gnu` or `aarch64-unknown-linux-gnu`. It requires a clean committed checkout, Cargo, Rust, Git, `install`, and `sha256sum`. It writes the binary, README, build manifest, and checksums under ignored `dist/`. The script has been authored on macOS but has not been run; native Linux build/runtime acceptance and package generation remain pending on the user's server. It does not cross-compile from macOS.

Before an update, back up the project database. A newer TALOS can migrate it forward; an older binary rejects a database schema newer than it supports. Do not downgrade by replacing the executable after a migration unless you have restored a compatible backup.

## First launch and normal task flow

Run readiness checks without starting a Writer:

```sh
talos --version
talos doctor
talos up --observe
```

Add a task with acceptance criteria, then start one explicit Writer attempt. Review, run the configured checks, and inspect the saved evidence:

```sh
talos task add "Describe the change" --criteria "State the observable result"
talos task run 1
talos task review 1
talos task verify 1
talos task evidence 1
```

An approved review moves a task to `verifying`; only passing checks and any managed-worktree finalization complete it. `up --auto` processes at most one eligible task per invocation. `up --semi` requires an interactive terminal and one approval prompt.

## Pause, stop, and recovery

`talos pause` prevents new Writer launches and continuations. It does not terminate a process already running. `talos stop` requests cooperative termination of the active Writer and reports whether termination was confirmed. If it times out or TALOS was killed, inspect the process and project changes before resolving the attempt.

Use `talos task recover` to quarantine interrupted attempts. Recovery reports `unknown`; it does not infer success, signal a process, resume a session, or retry work. `task resolve` requires explicit operator attestations. `authorize-retry` only returns the task to the queue; a separate later command must launch a Writer.

`talos attach` creates or reconnects to a project-scoped tmux shell. After an SSH disconnect, reconnect to the host and run `talos attach` again. The complete macOS SSH reconnect scenario is not yet accepted. Linux reboot and optional systemd user autostart remain deferred to server validation.

If an interactive CLI cannot resume the same conversation in headless mode, create a fresh-context packet from durable task records with:

```sh
talos --json task handoff 1
```

The packet is generated on demand and does not write a second history record. It omits prompts, patches, transcripts, raw command output, and the current Git diff. Inspect the working tree before a Writer action or retry. It is a new context summary; it does not establish interactive session continuity.

## Git worktrees

Task worktrees are separate checkouts, not OS sandboxes. TALOS preserves a worktree that contains changes. Integration stages the exact recorded patch in the primary checkout, then requires a Critic review and project checks. The user creates the final commit and runs `task worktree finalize`; TALOS does not create that commit. Review `git status` and the patch before removing or cleaning any worktree.

## Local state and backups

Each project stores its database in `<project>/.talos/state.sqlite3`. The directory and file are private on Unix and `.talos/` must be ignored by Git. The database contains task titles and criteria, lifecycle history, Writer/Critic metadata, verification hashes, worktree records, GitHub publication state, and compact CI summaries. It does not contain full Writer/Critic transcripts or raw check output.

Use SQLite's backup API or CLI `.backup` command to create a consistent backup while the database may be open:

```sh
sqlite3 .talos/state.sqlite3 ".backup '/absolute/secure/path/talos-state.sqlite3'"
```

Protect backups as project data; keep them encrypted and outside the project checkout. Verify the backup digest and retain at least one offline copy. The current product has no automatic retention or pruning command. Task and event history is append-only, so database size grows with project activity. F1 in `docs/tasks.md` tracks a separate archival and retention design.

## GitHub workflow

`doctor` and `talos github status` are local by default. `talos github status --check-auth` explicitly asks `gh` to inspect its active authentication; the CLI may use configured credentials internally. TALOS reports only an aggregate status.

Configure a schema-v3 `github` policy in `.talos/config.json` only after creating the hosted repository and a matching remote. Push and PR flags default to false. `talos github publish` additionally requires `--confirm-push` and `--confirm-pr`; it creates a draft PR only. Use `talos github reconcile <ID>` after an uncertain push or PR result. `talos github ci <ID> --wait` polls CI for the exact recorded PR commit and saves a compact local snapshot. Pending, timed out, unknown, and no-check results are not passing evidence. TALOS has no merge or remote-branch deletion command; review and merge through the user's chosen GitHub workflow.

No GitHub auth check, push, PR, or CI request has been performed from this workspace. Live acceptance is pending the user's repository.

## Current verification limits

Earlier R8/R9 code changes passed formatting, Clippy, and `cargo check`. The task-handoff changes pass `cargo fmt --all` and `cargo check --locked --all-features --lib --bin talos` on macOS. Their behavior tests were not run. Automated tests and Linux-native checks remain pending under the current development direction. The exact current status and next action are in [`docs/state.md`](state.md), and scenario evidence is tracked in [`docs/acceptance-scenarios.md`](acceptance-scenarios.md).
