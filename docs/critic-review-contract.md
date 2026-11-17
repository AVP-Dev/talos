# Critic review contract

## Purpose and authority

The Critic reviews one immutable view of a task's changes in a separate, fresh CLI process. It receives the task and acceptance criteria, the base commit, a bounded patch, and the patch's revision ID. It does not receive the Writer transcript or conversation ID. It has read-only workspace access and cannot run commands, use MCP/URL tools, or modify files. The Critic returns findings or an approval; it never supplies a patch for TALOS to apply.

The Critic is a separate role and process. It may use the same CLI vendor as the Writer when no other locally verified, safely isolated adapter is available. Sharing a vendor does not share conversation state.

## Revision identity

- For a Git project, the base is the full `HEAD` commit captured before the first Writer attempt. A new task's first Writer run must start from a clean worktree, including the index and untracked files.
- A rejection/fix/review cycle keeps the original base commit. Each new review includes the current diff from that base, so prior Writer changes remain in scope.
- The patch is generated without rename detection and includes staged and unstaged tracked changes plus non-ignored untracked files. `.git/` and `.talos/` are excluded. The complete patch is capped at 64 KiB, the serialized prompt at 96 KiB, and the structured response at 64 KiB. If a limit is exceeded, review fails closed without truncating context or recording approval.
- `revision_id` is `sha256:<lowercase hex>` over the UTF-8 base commit, a NUL separator, and the complete patch bytes. The patch is never truncated. TALOS recomputes this ID after the Critic exits; a mismatch makes the result stale and unusable.
- Critic review requires a Git base commit. TALOS may still store and run tasks in a non-Git project, but R4 cannot certify an exact diff for such a project.

## Review input

The prompt contains a JSON request with:

```json
{
  "schema_version": 1,
  "task_id": 12,
  "writer_attempt_id": 19,
  "task": "Task title",
  "acceptance_criteria": "Verifiable criteria",
  "base_commit": "full Git object ID",
  "revision_id": "sha256:...",
  "changed_paths": ["src/example.rs"],
  "patch": "complete bounded unified/binary patch"
}
```

The Critic must judge only the supplied revision and acceptance criteria. It must not infer that tests ran, make changes, or claim an approval for a different revision.

## Review output

The Critic must return a single JSON object matching this shape, with no surrounding prose:

```json
{
  "schema_version": 1,
  "revision_id": "sha256:...",
  "decision": "approve",
  "summary": "The change satisfies the stated criteria.",
  "findings": []
}
```

Each finding has `severity` (`blocker`, `major`, `minor`, or `note`), a repository-relative `path`, an optional positive line number, a short `title`, and a concrete `reason`. Paths must name a file in `changed_paths` and may not be absolute or escape the project root.

Deterministic acceptance rules:

- A process error, nonzero exit, truncated output, malformed JSON, unknown schema version, revision mismatch, invalid finding, or stale working tree is a failed review, never approval.
- Persist only the validated structured result and bounded task/revision metadata. Do not persist the full prompt, patch, or CLI transcript in SQLite.
- The response schema rejects unknown fields, including `patch`; Critic output is findings-only and TALOS has no Critic patch-application route.
- `request_changes` requires at least one `blocker` or `major` finding.
- `approve` is accepted only when no `blocker` or `major` findings remain. `minor` and `note` findings are recorded as non-blocking observations.
- Findings are immutable historical records tied to one revision. A later fix and review create a new record; they do not edit or delete earlier findings.
- A rejection moves the task back to `implementing`; a valid approval moves it to `verifying`. R5 completion requires separately recorded project-check evidence for this exact approved Git revision.

## Planned command flow

```text
task run <ID>       # first Writer attempt, from a clean Git worktree
task review <ID>    # independent read-only review of the exact revision
task run <ID>       # after request_changes, start a Writer fix iteration
task review <ID>    # review the new full revision; retain earlier findings
```

Role selection is project-local in `.talos/config.json`:

```json
{
  "schema_version": 1,
  "writer": "agy",
  "critic": "agy"
}
```

If the file is absent, both roles default to `agy`. Only adapters with verified invocation and authority boundaries are accepted. In the current R4 environment Antigravity 1.5.10 is the only supported adapter for both roles; another CLI remains refused until it passes its own checks.
