# Local CLI capability spike

Checked on 2026-11-17 on macOS ARM64, using each installed binary's public `--version` and `--help` output. This documents advertised flags; it does not certify a live model call, session persistence, permission behavior, interactive handoff, cancellation, or Linux support.

| CLI | Installed version | Advertised headless / structured interface | Advertised resume | Result and limits |
|---|---:|---|---|---|
| Antigravity (`agy`) | 1.5.10 | `--print`; `--output-format text\|json\|stream-json`; NDJSON input; `--json-schema` | `--continue`, `--conversation ID` | Help probe passed. Auto/plan/accept-edits modes exist; `--dangerously-skip-permissions` also exists and is not a TALOS default. |
| Codex | 0.169.0 | `codex exec`; `--json` JSONL events; stdin prompt; `--output-schema` | `codex exec resume [SESSION_ID]` / `--last`; interactive `codex resume` | Version/help probes passed. It exposes sandbox and approval options plus explicit dangerous bypass flags; TALOS must not select the bypass. |
| Claude Code | 2.1.296 | `-p`; `--output-format json\|stream-json`; `--json-schema` | `--resume ID`, `--continue` | Version/help probes passed. Help also exposes a session-persistence opt-out and permission modes; exact continuation behavior remains to be exercised. |
| OMP | 18.8.7 | `--print`; `--mode text\|json\|rpc\|rpc-ui`; tool allow/deny options | `--continue`, `--resume` | Version/help probes passed. Its Pi-derived options and profiles differ from `pi`; keep a separate adapter. |
| Pi | 0.82.1 | `--print`; `--mode json` event stream; `--mode rpc`; `--no-tools` | `--continue`, `--resume`, `--session` | Version/help probes passed. The default help invocation emitted a sandbox `EPERM` while touching the user's settings lock; an isolated `PI_CODING_AGENT_DIR` and offline help probe worked. No user settings were changed. |
| OpenCode | 2.3.0 | `run`; documented `--format json` (NDJSON); `--auto` option | `--continue`, `--session ID` | Default help/version initially failed to open the user's log under sandbox. With isolated XDG config/data/state paths, version/help worked. No user configuration was changed. |

### Other capability axes

| CLI | Interactive TUI | Native delegation | Context input / continuation | Cancellation and permission behavior |
|---|---|---|---|---|
| Antigravity | Interactive by default; `--conversation ID` is advertised. TUI opened but stopped at a project-trust prompt that was declined. | Agent selection/remote-agent flags are advertised; child-agent delegation was not tested. | Prompt argument, stdin, and stream-json input are advertised; headless session continuation is live-tested. | `--mode plan` and `--sandbox` were used for the live no-tools probe; actual tool authorization was not exercised. Interrupt/timeout behavior untested. |
| Codex | Interactive by default; resume subcommand advertised. | `agents` command/agent support exists; cross-agent delegation was not tested. | Prompt argument/stdin and session-ID resume are advertised. | Read-only sandbox advertised; live process blocked before inference. Interrupt/permission effect untested. |
| Claude Code | Interactive by default; session resume advertised. | `--agents` and background-agent commands advertised. | Prompt argument and JSON/stream-JSON output advertised. | Permission modes advertised; isolated profile was not logged in. No permission behavior tested. |
| OMP | Interactive by default. | `task` tool advertises subagent launch. | Prompt argument, JSON/RPC, session directory, and resume advertised. | Approval modes include `always-ask`, `write`, and `yolo`; do not use `yolo`. Process launch was blocked before inference. |
| Pi | Interactive by default; session resume advertised. | No native delegation path verified in local help. | Prompt argument, JSON/RPC, session file/directory, and resume advertised. | `--no-tools` was used; no API key prevented model execution. Interrupt/permission behavior untested. |
| OpenCode | Interactive TUI plus `run` advertised. | Agent/subagent controls are documented; not live-tested. | Prompt and session continuation advertised. | Global `--auto` approves requests not explicitly denied; unsafe without a complete policy. No model run attempted. |

TALOS must record the resolved executable path and relevant runtime (such as Bun) as well as the version. Different agent shells resolved Codex and Claude to different binaries/versions, and OMP failed under Bun 1.3.8 but displayed help under Bun 1.4.2.

## Live probe attempts

These are execution attempts; distinguish successful behavior from probes stopped before inference. Each used disposable state, no dangerous bypass, and no TALOS/source-repository edits.

### Antigravity

- In a temporary directory, attempted a harmless prompt with plan mode, sandbox, JSON output, and a 45-second timeout. The exact prompt asked for a fixed token and prohibited tools, file inspection, and edits.
- The CLI failed before inference because the sandbox denied writes to `/Users/AVP-Dev/.gemini/antigravity-cli/log/` and `crashes/`, then denied binding its language server to `127.0.0.1:0`.
- The process exited; a follow-up check found no remaining language-server process. No TALOS files, user CLI settings, or source repositories changed.
- No response/session ID, resume, TUI handoff, or interrupt behavior was observed. Model usage is not expected because execution stopped before inference, but cannot be confirmed from the local process.

### Antigravity: headless and session resume

- Using the explicit binary path `/Users/AVP-Dev/.local/bin/agy`, a no-tools prompt ran in plan mode with the native sandbox and JSON output in a disposable directory under `/private/tmp`.
- First call returned JSON `status: SUCCESS`, `response: TALOS_R1_AGY_OK`, and a `conversation_id` in 3.7 seconds.
- A second headless process used `--conversation` with the returned ID and asked for the prior token. It returned `TALOS_R1_AGY_OK`, the same conversation ID, and `num_turns: 2`. This verifies public headless resume for this installed version. The local session ID is not copied into project documentation.
- Combined reported usage: 31,318 tokens across the two calls. Billing cannot be determined from local output.
- Interactive test opened the same conversation in an isolated tmux pane, but stopped at Antigravity's explicit project-trust prompt, which grants read/edit/execute access. The prompt was declined to avoid persisting a user-wide trust decision. Therefore TUI handoff and headless resume after TUI are not verified.

### Codex

- The subagent found a PATH difference: `codex` resolved in its shell to a ChatGPT-app-bundled `0.169.0-alpha.11.1`, while explicit `/Users/AVP-Dev/.local/bin/codex` was `0.169.0` and matches the root agent's earlier probe.
- In a disposable Git repository, the explicit CLI was invoked with `--sandbox read-only`, `--json`, and a no-tools/no-write prompt.
- It exited before a model response: it could not open `/Users/AVP-Dev/.codex/memories_1.sqlite`, then could not initialize its in-process app-server client (`Operation not permitted`). No session ID was returned; resume/TUI remain untested.
- No credential values were inspected and no user configuration or TALOS files were changed. No workaround was attempted.

### Claude Code, Pi, OMP, and OpenCode

- **Claude Code:** isolated temporary config returned `Not logged in`; no inference ran, so output parsing/session resume were not tested. The preferred binary is `/Users/AVP-Dev/.local/bin/claude` at 2.1.296; `/opt/homebrew/bin/claude` is older 2.1.202.
- **Pi:** an isolated offline no-tools JSON call emitted a session header, then `No API key found for the selected model`. No assistant response or resume was verified.
- **OMP:** help works with Bun 1.4.2; using Bun 1.3.8 crashes while parsing the installed bundle. A no-tools JSON run stopped before inference because the sandbox denied creation under `/Users/AVP-Dev/.omp/run/daemons`.
- **OpenCode:** isolated XDG paths enabled version/help probes, but no model call was attempted because the temporary profile had no account configuration. Resume/TUI remain untested. Its `--auto` permits anything not explicitly denied, so it needs a complete deny policy before TALOS can use it.

## Probe record

- Commands run: `<cli> --version`, `<cli> --help`; additionally `codex exec --help`, `claude --help` flag scan, `omp --help` flag scan, and isolated `opencode --version` / `opencode --help`.
- tmux is installed at 3.8 (`tmux -V`).
- `agy`, `codex`, `claude`, `omp`, `pi`, `opencode`, `tmux`, `cargo`, and `rustc` are on PATH. `antigravity` is not a separate command name in this environment; its executable is `agy`.
- `codex --version` printed a warning because it could not create PATH aliases in the sandbox; it still returned version 0.169.0.
- Only Antigravity produced a live model response, and its headless continuation was verified. No full headless/resume/TUI/resume sequence has been verified for any CLI.
- Other CLIs were either blocked before inference or not run. The exact limits are above; these are not evidence of a CLI defect outside this sandbox/account setup.
- Context7 docs were resolved for Tokio, rusqlite, Clap, Codex CLI, Antigravity CLI, Claude Code, OpenCode, and Pi. They confirm public process/output/session interfaces; installed help remains the local version's compatibility authority. OMP did not resolve to an authoritative Context7 library and is documented from installed help only.

## Enablement policy

An adapter may be detected before it is supported. The capability probe stores version and command-help-derived options, but marks behavior experimental until a real isolated run confirms structured output, session ID capture, resume, cancellation, and permission behavior. If context continuity across TUI is not proven, TALOS restarts from a durable context packet and explicitly says that the conversation was not resumed.

## R3 investigation — Antigravity write boundary

The current local `agy 1.5.10 --help` advertises `--print`, `--output-format`, `--conversation`, `--sandbox`, `--mode accept-edits|auto|plan`, and a session-only `--settings` override. These flags establish interface availability, not an effective filesystem boundary.

The official [Antigravity terminal sandbox documentation](https://antigravity.google/docs/sandbox?tab=cli) makes these distinctions:

- `--sandbox` confines terminal commands to an OS-level sandbox. On macOS it uses Seatbelt profiles; on Linux it uses namespaces. The sandbox flag may have no effect if the platform cannot provide the sandbox.
- Workspace folders are mounted read/write. `read_file` and `write_file` permission rules determine additional read-only or read/write mounts; denied paths are blocked and other paths are inaccessible in the sandbox.
- Auto mode evaluates commands that need to run outside the sandbox and can allow safe, reversible commands. It is therefore not a strict no-escape boundary by itself.
- The official settings example uses `permissions.allow` and `permissions.deny`, including rules such as `write_file(src/)` and a denied private path. Session configuration can be passed with `--settings` and is not saved by that flag.

TALOS must test terminal confinement and file-tool permissions separately. R3 uses a disposable Git project, a session-only allow rule for the workspace, a deny rule for a sibling outside directory, and explicit filesystem checks after the headless process exits. If the blocked path can be written, or the CLI asks for an unbounded grant, T3.7 fails and `up --auto` stays disabled. The test does not use `--dangerously-skip-permissions` or modify user-wide settings.

### Live write-boundary probe — `agy 1.5.10`

Ran three headless probes in `/private/tmp/talos-r3-safe-probe/workspace`, a disposable Git repository. Each passed a temporary settings file with `--settings`, used `--mode accept-edits --sandbox`, and wrote CLI logs under the disposable parent directory. The settings were not saved to user configuration.

1. An allowed workspace file was created. A direct file-tool write to an explicitly denied sibling path was refused; the host confirmed the sibling file did not exist. CLI-reported usage: 19,479 tokens; duration 13.9 seconds.
2. An allowed workspace file was created. `command(regex:.*)` denied a requested `touch` command; the host confirmed the outside file did not exist. CLI-reported usage: 19,111 tokens; duration 10.3 seconds.
3. An allowed workspace file was created. Direct file-tool writes to `.talos/`, `.git/`, and an outside sibling were refused. Host-side file checks confirmed all three were absent; Git reported only the three intended workspace probe files. CLI-reported usage: 24,122 tokens; duration 24.0 seconds.

The R3.7 gate passed for the installed 1.5.10 binary under this exact session policy. Do not use Antigravity's own `--mode auto` as TALOS's authority boundary: TALOS uses an explicit `accept-edits` profile, denies terminal commands for the Writer, allows project files, and denies reads and writes under `.talos/` and `.git/`. The verification runner remains a separate TALOS-controlled process. This evidence does not enable `up --auto` by itself; restart reconciliation, failure-path acceptance, result verification, and full R3 acceptance are still required.

### Live `task run` integration evidence — `agy 1.5.10`

TALOS launched Antigravity only in disposable Git projects under `/private/tmp`. The TALOS source workspace was not available to the Writer, and no user-wide Antigravity settings were changed.

1. **Host-sandbox startup failure** (`/private/tmp/talos-r3-live-RiyaeC`): a normal launch stopped before model inference because this execution environment denied Antigravity's writes to its user-local log/crash directory and its ephemeral localhost bind. TALOS recorded a generic nonzero-exit failure, created no requested marker, and left the task blocked. This is an environment startup limitation, not evidence that the TALOS file policy failed.
2. **Successful Writer flow** (`/private/tmp/talos-r3-live-escalated-ea1eVv`): a launch with a temporary, session-only policy created `TALOS_LIVE_RESULT.md` with exactly `TALOS live Writer flow succeeded.` The TALOS database recorded a successful attempt, process ID, public conversation ID, and `SUCCESS` result. The task remained in `reviewing`; no checks were run and it was not marked complete. The policy file was removed after the attempt was resolved.
3. **Read-boundary flow** (`/private/tmp/talos-r3-policy-read-lWSNsh`): the disposable project contained harmless sentinel files under `.talos/` and `.git/`. The Writer was asked to read those values and write a result marker. Both reads were denied; it created `READ_POLICY_RESULT.md` containing only `READS_DENIED`. The response exposed neither sentinel value. TALOS recorded the attempt, and the temporary policy file was removed after resolution.

The successful live task-run flow required an approved host process launch because of the environment's restrictions on Antigravity's own startup resources. Antigravity still ran with its terminal sandbox and the exact TALOS session-only file policy; no permission-bypass flag was used. That single-task check did not qualify Auto at the time; the later one-shot Auto acceptance is recorded below. Independent review and project checks remain future work.

### Headless stream protocol used by the process runner

The official [Antigravity headless CLI documentation](https://antigravity.google/docs/cli/headless) specifies a `user` event with a `message.content` field on NDJSON stdin when `--input-format stream-json` is selected; that mode requires `--output-format stream-json`. Output is an event stream ending in a `result` event with a public `conversation_id`, status, response, and optional error. TALOS sends the prompt through stdin so it does not appear in the child process argument list. Fake-CLI lifecycle checks and the live one-shot Auto and continuation checks are recorded below. Live timeout, Antigravity-specific cancellation, and resume after an actual TALOS crash remain unverified.

### R3 one-shot Auto acceptance — `agy 1.5.10`

- Ran `talos task add`, `talos up --auto --json`, `talos task list --json`, a second `talos up --auto --json`, and `talos task recover --json` through the release binary in a fresh disposable Git project under `/private/tmp`. The project ignored `.talos/` and was clean before Auto.
- The real Agy Writer created `TALOS_LIVE_AUTO.md` with the exact requested content. Auto reported that path and the Writer response; the task remained `reviewing` and `verification_ran` was `false`.
- A separate TALOS process observed `reviewing`. The repeated Auto call returned `no_queued_tasks`; it launched no second Writer. Recovery returned `no_unresolved_attempts`.
- Fake-CLI release-binary tests also verified that two queued tasks result in only the first being processed, leaving the second queued. A repeat invocation with no queued work did not call the fake Writer again.

### R3 continuation acceptance — `agy 1.5.10`

- In a separate disposable Git project, TALOS ran a real Agy turn that created the expected marker file. A temporary local wrapper changed only that first result event's `status` from `SUCCESS` to `ERROR`, preserving the real public conversation ID and process exit code, to exercise the terminal-error branch deterministically.
- The harness verified the marker contents before invoking `talos task continue 1 --confirm-project-inspected --json`. TALOS resumed the exact conversation through `--conversation`, accepted the matching `init` ID before sending one follow-up, and persisted a successful linked attempt. The task remained `reviewing`.
- This proves TALOS's resume handling against a real Agy conversation but does not prove that Agy naturally emits this specific terminal `ERROR` result. Unit and fake-CLI tests cover classification and ineligible error cases. The conversation ID is intentionally omitted from project documentation.
- Both live acceptance projects were under `/private/tmp`, their directories and session-only policy files were removed after checks, and no TALOS source or user-wide Agy settings were exposed or changed.

## Documentation sources

- [Antigravity CLI](https://github.com/google-antigravity/antigravity-cli)
- [Codex CLI](https://github.com/openai/codex)
- [Claude Code CLI](https://docs.anthropic.com/en/docs/claude-code/cli-reference)
- [OpenCode CLI](https://opencode.ai/docs/cli/)
- [Pi CLI](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/docs/cli.md)
