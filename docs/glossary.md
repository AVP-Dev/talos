# TALOS Glossary

| Term | Meaning in TALOS |
|---|---|
| Task | A record of the user's goal, acceptance criteria, dependencies, and current state. |
| Round | A verifiable TALOS delivery phase in `docs/plan.md`; not a pair-programming iteration. |
| Writer | The CLI agent granted the only write authority for a specific checkout/task. |
| Critic | An independent reviewer with read-only code access whose findings can return work to the Writer. |
| Adapter | A TALOS module for a specific CLI's public command interface and measured capabilities. |
| Capability | A claimed or verified property of a CLI; version/help output alone does not establish live support. |
| Lease | Temporary authority for a worker to execute a task, with a generation token that prevents stale owners. |
| Context packet | Verified information used to continue a task when public CLI session resume is unavailable. |
| Ground truth | Observable facts from the working tree, process, and executed checks; distinct from model text. |
| Auto mode | Executes pre-approved routine actions without repeated prompts; does not remove security boundaries. |
| Semi mode | Pauses for approval at policy-defined boundaries. |
| Worktree | A separate Git working tree/branch for an independent task; not an OS sandbox. |
| Supervisor | Deterministic Rust component that schedules tasks and monitors processes, leases, and recovery. |
