# End-to-End Acceptance Scenarios

T9.1 tracks the original operator workflow. “Prior evidence” refers to a result recorded during an earlier round; it is not a claim that the current schema-v17 tree was re-run. Re-run affected scenarios during final acceptance and record the exact command, outcome, platform, and evidence path. Do not combine unrelated scenarios into one result.

| ID | Scenario | Current record | Evidence / remaining work |
|---|---|---|---|
| E2E-01 | First observe launch and repeat `up --observe` | Prior evidence | `docs/state.md` R2 release-binary smoke. Re-run on final release. |
| E2E-02 | Run one queued task with `up --auto`; repeat without duplicate work | Prior evidence | `docs/state.md` R3 closure acceptance: one-shot Auto and repeated invocation reported no queued task. Re-run on final release. |
| E2E-03 | Successful Writer result followed by Critic approval | Prior evidence | `docs/state.md` R3 live Auto and R4 live `task review` acceptance. Re-run after final changes. |
| E2E-04 | Critic identifies a justified defect and a Writer fix is reviewed | Prior evidence | `docs/state.md` R4 deliberate-defect review and linked fake Writer fix. Re-run on final release. |
| E2E-05 | Child task contract, path scope, and parent dependency | Not run | T7.2 implementation exists; migration and runtime acceptance remain pending. |
| E2E-06 | Independent parallel work in separate task worktrees | Not verified | T7.4/T7.5 code exists. Native Agy delegation is disabled until its public invocation interface and roster are verified; three-agent acceptance remains pending. |
| E2E-07 | Add a task while another Writer is active | Not run | Verify queue persistence and lease behavior while the first process remains active. |
| E2E-08 | Manual takeover through pause/resume and persistent control shell | Partial prior evidence | `docs/state.md` R6.2 records pause/resume smoke; T6.4 SSH disconnect/reconnect is still untested. |
| E2E-09 | Recover and resolve an interrupted CLI attempt | Prior evidence | `docs/state.md` R3 closure acceptance records injected launch persistence failure, quarantine, and explicit resolution. Re-run on final schema. |
| E2E-10 | TALOS process crash with no inferred success or retry | Partial prior evidence | `docs/state.md` recovery notes record recovery tests and crash-shaped database states. A live forced crash/restart acceptance remains pending. |
| E2E-11 | SSH disconnect and reconnect to the macOS control session | Not run | tmux session behavior was checked separately; the full SSH disconnect/reconnect scenario remains T6.4. |
| E2E-12 | Linux reboot and worker/autostart recovery | Deferred | Requires the user's Linux server. No Linux host was contacted. |
| E2E-13 | Open a PR, collect CI evidence, and leave merge to the user | Deferred | T8.1–T8.3 remote behavior awaits a user-created repository. TALOS has no merge command. |
| E2E-14 | Prevent duplicate execution under repeated or concurrent requests | Partial prior evidence | R3 repeated Auto and lease tests are recorded; rerun concurrency acceptance against the final schema and worktree routing. |
| E2E-15 | Recheck the original acceptance criteria before closeout | Not run | Create a final evidence-backed review after E2E-01 through E2E-14 are resolved or explicitly deferred. |

## Current closeout state

T9.1 is in progress. This inventory separates prior evidence from current unverified behavior. No scenario was executed while preparing this inventory. Linux and GitHub-hosted scenarios remain deferred until the required user infrastructure exists.
