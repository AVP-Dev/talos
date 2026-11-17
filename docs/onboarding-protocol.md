# TALOS Project Onboarding Protocol

This document adapts the Agent Starter Kit onboarding guidance to TALOS. It supports safe project inspection and durable handoff; it does not create or run the TALOS product.

## TALOS-specific context

The project goal and preferred stack are already specified in `docs/plan.md` and the original product request. Do not ask the user to restate them. Read `docs/state.md` and the backlog first, continue the current round, and clarify only a decision that cannot be resolved from project files and affects safety or compatibility.

English is the only language for all project artifacts, documentation, prompts, logs, and product output.

## Project classification

- **Greenfield:** no source code or manifests exist. Use the user-approved goals and stack; do not invent additional infrastructure.
- **Brownfield:** source, manifests, CI, or tests exist. Prefer observed repository facts over stale notes, while preserving explicit user decisions and safety boundaries.

## Safe setup

The Starter Kit's `scripts/init-project.sh` adds missing files and preserves existing project files. Its options are:

```sh
./scripts/init-project.sh --dry-run /path/to/project
./scripts/init-project.sh --yes /path/to/new-project
./scripts/init-project.sh --update /path/to/existing-project
```

The script is structural setup only; it does not deeply inspect a project or choose architecture. It does not install or rewrite AI CLIs. Inspect the target before running it. Do not replace existing project documentation, tests, configuration, secrets, or manual settings silently.

## Onboarding steps

1. Confirm the project root and read the current handoff, plan, tasks, and decisions.
2. Inspect repository status, manifests, lockfiles, source layout, checks, and current documentation.
3. Record verified facts and unknowns; do not infer framework or CLI capabilities from filenames alone.
4. Add or adapt guidance without deleting project-specific information.
5. Build the active verification profile only from commands that exist and have been run.
6. Run applicable checks and record their exact commands and outcomes in `docs/state.md`.
7. Update task status and name the next action so another agent can continue without conversation history.

## Protected operations

Do not overwrite or delete user files, modify secrets, alter database schemas, change access controls, or run destructive operations as part of routine onboarding. Preserve local changes. Do not run `git reset --hard`, publish changes, or modify an external system without explicit user direction.

## Completion record

Record the project classification, discovered stack, created/preserved files, checks run, unresolved limits, and next action. Keep the report and all project files in English.
