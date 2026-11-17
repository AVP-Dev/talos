# Local project history idea

**Status:** Unscheduled. Revisit after the current R0–R9 roadmap is complete. Backlog ID: F1.

## Current TALOS behavior

Each project gets a local SQLite database at `.talos/state.sqlite3`, including projects without Git metadata. The current schema stores task titles and acceptance criteria, task states, append-only task events, and Writer-attempt metadata. `talos logs` displays task events. A Writer response is printed after a run but its full prompt and transcript are not stored. TALOS has no separate project-registration event, project plan store, or history of project files.

## Direction to evaluate later

- Treat project creation/history as a local journal: registration time and origin, milestone events, approved plan versions, decisions, and links to files or reports produced during work.
- Keep searchable structured records and provenance metadata in SQLite. For larger outputs, evaluate keeping the file in the project or a private `.talos` area while SQLite stores its relative path, digest, size, media type, producer task/attempt, and source revision. Avoid copying the source tree or storing full CLI transcripts by default.
- Decide retention, deletion, backup, and export behavior before implementation. `.talos/` is private local runtime state and must remain ignored by Git.
- Keep this per-project. A cross-project portfolio database is a separate product decision and is not implied by F1.
- In product text, describe the feature as a project history/journal. Use “artifact” only when it means a concrete file or file collection produced by a run; TALOS should not generate or duplicate project content merely to create history.

## Research notes

SQLite is designed for local application data and may serve as an application file format. Its storage guidance compares BLOBs in the database with external files and warns that performance depends on file size, page size, hardware, and workload; there is no universal cutoff to adopt without measuring TALOS's usage. GitHub Actions uses “artifact” for files or file collections produced by a workflow and treats retention as an explicit setting. SLSA provenance offers a useful pattern for recording output digests and how an output was produced, though a local TALOS history feature does not need to implement signed supply-chain attestations.

## Sources

- SQLite, [Appropriate Uses For SQLite](https://www.sqlite.org/whentouse.html)
- SQLite, [Internal Versus External BLOBs](https://www.sqlite.org/intern-v-extern-blob.html)
- GitHub, [Workflow artifacts](https://docs.github.com/en/actions/concepts/workflows-and-actions/workflow-artifacts)
- SLSA, [Provenance v1.2](https://slsa.dev/spec/v1.2/provenance)
