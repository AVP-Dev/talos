# Design Manifest Template

Use this short template when a task changes an important interface, state model, worker, or external integration. It is a thinking aid, not paperwork for trivial edits.

## Goal

What concrete behavior should change, and for whom?

## Invariants

What must remain true in every valid state?

## State transitions

Which states exist? Which transitions are allowed, and which actor may perform each one?

## Failure and retry behavior

What happens on timeout, cancellation, duplicate request, partial result, process crash, database error, or lost connection? Which operations are safe to retry?

## Authority boundary

What can each agent or subsystem read, write, execute, or publish? Which actions require a user decision?

## Acceptance evidence

Which positive, negative, recovery, and regression checks demonstrate completion? What real-world action or external dependency remains unverified?
