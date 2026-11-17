# T3.6 Platform Process Identity Research

**Checked:** 2026-11-17

This note identifies public operating-system facts that may help TALOS label an in-flight Writer process after restart. A process match is lifecycle evidence only. It never proves whether Antigravity received the prompt, changed files, returned a result, or is safe to retry.

## Linux

The Linux kernel documents process information in `/proc/<pid>/stat`. Its fields include the process-group ID and process start time; the start time is measured in clock ticks since system boot. The kernel also documents `/proc/sys/kernel/random/boot_id` as a UUID that remains constant for one boot.

A candidate identity tuple is `(pid, process_group_id, boot_id, starttime_ticks)`. TALOS must classify the identity as unavailable when `/proc` is missing, restricted, malformed, or inconsistent. It must not treat an unavailable identity or a non-matching process as evidence that no project effect occurred.

The `stat` record includes a parenthesized command name before numeric fields. Any parser must isolate that prefix carefully before indexing fields; whitespace splitting the entire record is not safe.

## macOS

Apple's XNU source defines `proc_bsdinfo` fields for PID, process-group ID, and process start time in seconds and microseconds. The XNU `libproc.h` header also explicitly labels its process-information interfaces private and subject to change. TALOS must not depend on `proc_pidinfo` for durable identity under the project's public-interface boundary.

Apple documents `KERN_BOOTTIME` through `sysctl`, but a boot timestamp alone does not identify a process. This research did not confirm a supported, stable macOS public API that provides a durable process-start identity suitable for distinguishing PID reuse. Until one is confirmed, macOS recovery must report process identity as unavailable and keep the attempt unresolved. Do not infer identity by parsing `ps` output.

## Recovery policy

- Linux may report whether the recorded PID still has the same `(boot_id, starttime_ticks, process_group_id)` identity, after the collector and parser are verified.
- macOS reports identity unavailable unless TALOS later verifies a supported public API.
- A matching live process remains unresolved. TALOS must not signal it, resume its conversation, or infer an attempt outcome.
- A missing, reused, inaccessible, or unverified process identity also remains unresolved. An absent process does not prove that no files changed.
- Public conversation IDs are currently saved only with the final result. Even a future early-saved session ID will not prove the Writer stopped or that its changes are safe.
- A project baseline can show that project state differs from its pre-run snapshot. Equality or change alone does not establish completion or make replay safe; use it as review evidence only.

## Bounded parser and production integration — 2026-11-17–18

`src/process_identity.rs` contains a Linux `/proc/<pid>/stat` parser and collector. The parser uses the first opening and last closing parenthesis to isolate the command field, then reads the PID, process-group ID, and start-time ticks. It normalizes and validates the Linux boot UUID. Its fingerprint includes `(boot_id, pid, process_group_id, starttime_ticks)` so two PIDs in the same process group and clock tick cannot compare equal. A malformed, inaccessible, or inconsistent record returns an error; callers report it as unavailable and keep the attempt unresolved.

Schema v9 now stores the fingerprint when the Writer process is launched. `task recover` compares the stored fingerprint with the current `/proc` state and reports `matches`, `different_process`, `unavailable`, or `not_recorded`. The report is lifecycle evidence only. It does not authorize signaling, retry, resolution, or an inferred Writer outcome. On macOS, capture remains unavailable and the recovery policy remains fail-closed. The implementation uses no private macOS APIs and does not parse `ps`.

The new schema migration and Linux `/proc` production path have not yet been verified on Linux. Only `aarch64-apple-darwin` is available for the current local build. A Linux-only collector check captures and compares the current Linux process but has not run here. Do not claim Linux support until server-side build, process identity, runtime, and Agy sandbox acceptance pass. A matching process remains lifecycle evidence only.

## Sources

- Linux kernel `/proc` documentation: [process stat fields](https://docs.kernel.org/filesystems/proc.html).
- Linux man-pages: [`/proc/<pid>/stat`, field 22 `starttime`](https://man7.org/linux/man-pages/man5/proc_pid_stat.5.html).
- Linux kernel documentation: [`/proc/sys/kernel/random/boot_id`](https://kernel.org/doc/html/latest/admin-guide/sysctl/kernel.html).
- Apple XNU: [`proc_bsdinfo` process fields](https://github.com/apple-oss-distributions/xnu/blob/main/bsd/sys/proc_info.h).
- Apple XNU: [`libproc.h` private-interface notice and `proc_pidinfo`](https://github.com/apple-oss-distributions/xnu/blob/main/libsyscall/wrappers/libproc/libproc.h).
- Apple archived manual: [`sysctl(3)` and `KERN_BOOTTIME`](https://developer.apple.com/library/archive/documentation/System/Conceptual/ManPages_iPhoneOS/man3/sysctl.3.html).
