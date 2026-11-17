# Agent Starter Kit integration record

## Source and integration method

- Read-only source: `/Users/AVP-Dev/Downloads/Github/Templates/agent-starter-kit`.
- Source revision: `957e97f9d96680cd3958b1ef3c472d179460b3c6` (clean `main`, tracking `origin/main`) on 2026-11-17.
- Read the source README, AGENTS rules, onboarding protocol, architecture, project-structure and verification docs, and the `scripts/init-project.sh` implementation.
- The author-provided integration is `scripts/init-project.sh --update <existing project>`: it adds only missing files. A dry run was performed first, then the script was run against this initially empty TALOS directory. No Starter Kit files were edited.

## Installed components

- `AGENTS.md` and a relative `CLAUDE.md -> AGENTS.md` symlink.
- `.gitignore` baseline; `docs/` project standards, onboarding, architecture, verification, routing, design manifest template, and supporting references.
- `scripts/verify-project.sh` and `scripts/security-scan.sh`.
- The shipped `scripts/verification-profile.tsv` is deliberately not copied by the installer. TALOS will create its active profile only after the Rust commands exist and have been run.

## What the kit does not install

The Starter Kit is a development-process standard, not an AI CLI installer or runtime. The init script does not rewrite Codex, Claude Code, Antigravity, Pi/OMP, or OpenCode settings. That leaves the user's native permission policies intact and prevents TALOS product runtime from depending on this kit. No CLI configuration was changed.

## TALOS adaptation

The generic template was replaced or completed with the Rust/Tokio/SQLite WAL stack, process and task invariants, repository boundaries, measured CLI capability rules, and initial DESIGN manifest. Per the user's preference, ordinary engineering practices remain recommendations; security, data preservation, and repository boundaries are explicit hard limits. Web UI theme standards remain as upstream reference only; TALOS is a CLI product. The specific pair-mailbox study and CLI probe records are separate documents under `docs/research/`.

## Verification record

- Dry-run reported only additions because the target was empty.
- Installer completed with no existing-file conflict.
- `CLAUDE.md` is a relative symlink, so it remains valid in a checkout.
- The original Starter Kit stayed clean at the recorded revision.
- Run the copied secret scanner and compare installed files against this record during the bootstrap gate. The Rust verification profile will be activated after implementation starts.
