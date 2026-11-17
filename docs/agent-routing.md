# Agent routing and session continuity

This document describes how a TALOS development session or future product run can be handed to another agent. It is a practical reference, not a mandatory delegation ritual.

## Continue work in a new development session

Read [`state.md`](state.md), [`plan.md`](plan.md), [`tasks.md`](tasks.md), and [`decisions.md`](decisions.md) before making changes. These files are the durable project context. The chat transcript is optional background.

Pick the next unfinished task in `tasks.md`. If another agent is already working on the same task or files, coordinate ownership before editing. When taking over, preserve its uncommitted changes and update the handoff rather than restarting its work.

At the end of a bounded work period, write down:

- task ID and status;
- files changed and any intentionally uncommitted work;
- commands run and their actual outcomes;
- decisions made and limitations found;
- the single next recommended action.

No agent should need access to a private session store or another product to resume from this handoff.

## Future TALOS runtime roles

Writer and Critic are product roles with different permissions. The Writer owns implementation changes. The Critic independently checks the same revision and cannot edit the Writer's checkout. A model's agreement does not grant permissions or prove a result.

Delegate bounded child tasks only when the selected CLI supports a documented subagent interface and the work can be independently reviewed. Include the parent task, goal, allowed paths/actions, dependencies, acceptance criteria, timeout, and expected report shape. The lead verifies returned facts and remains responsible for integration.

Use parallel execution only for independent work with explicit file ownership and separate Git worktrees. Start sequentially by default. A worktree separates files; it does not restrict OS access, network access, or secrets.

## Recovery context packet

If a CLI cannot resume its recorded session, reconstruct only from durable facts:

1. Task ID, current state and acceptance criteria.
2. Last acknowledged checkpoint and unfinished actions.
3. Repository root, branch, current commit, dirty paths, and worktree owner.
4. Relevant verification results, review findings, and pending decisions.
5. Exact next action and its permission boundary.

Do not claim that this packet resumes model conversation history; it starts a new agent session from a checked project snapshot.
