use std::{
    env,
    process::Command as ProcessCommand,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde_json::json;

use crate::{
    adapter::agy::AgyProbe,
    cli::{Cli, Command, ResolveAction, TaskCommand},
    db::{
        Database, QueueSummary, TaskEvent, TaskRecord, WriterResolutionAction,
        WriterResolutionRecord,
    },
    project::{DiscoveryMethod, Project},
    supervisor::{
        VERIFIED_AGY_WRITE_VERSION, WriterContinuation, continue_writer_attempt,
        run_writer_attempt, validate_project_scope,
    },
};

const WRITER_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const SUPERVISOR_LEASE: Duration = Duration::from_secs(35 * 60);

#[derive(Serialize)]
struct StatusReport {
    project_root: String,
    database_path: String,
    schema_version: i64,
    journal_mode: String,
    queue: QueueSummary,
    supervisor: &'static str,
}

#[derive(Serialize)]
struct DoctorReport {
    project_root: String,
    discovery: crate::project::DiscoveryMethod,
    database_path: String,
    schema_version: i64,
    journal_mode: String,
    agent_execution: String,
    antigravity: AgyProbe,
    git: &'static str,
}

pub async fn run(cli: Cli) -> Result<()> {
    let working_directory = env::current_dir().context("Cannot read current directory")?;
    let project = Project::discover(&working_directory)?;

    if matches!(
        &cli.command,
        Command::Task(arguments)
            if matches!(&arguments.command, TaskCommand::Run { .. } | TaskCommand::Continue { .. })
    ) {
        validate_project_scope(&project)?;
    }

    if let Command::Up(arguments) = &cli.command
        && arguments.semi
    {
        bail!(
            "Semi execution is not available in R3; its approval policy is planned for R5. Use `up --auto` for one queued task or `task run <ID>` for an explicit task."
        );
    }

    let mut database = Database::open(&project)?;
    match cli.command {
        Command::Up(arguments) if arguments.auto => {
            run_one_shot_auto(cli.json, &project, &mut database).await?;
        }
        Command::Up(_) => {
            let queue = database.queue_summary()?;
            if cli.json {
                print_json(&json!({
                    "mode": "observe",
                    "project_root": project.root,
                    "database_path": database.path(),
                    "schema_version": database.schema_version()?,
                    "queue": queue,
                    "worker_started": false,
                }))?;
            } else {
                println!("TALOS is ready in observe mode.");
                println!("Project: {}", project.root.display());
                println!("Database: {}", database.path().display());
                println!("Queue: {} total, {} queued", queue.total, queue.queued);
                println!("No agent or worker was started.");
            }
        }
        Command::Task(arguments) => match arguments.command {
            TaskCommand::Add { title, criteria } => {
                if title.trim().is_empty() {
                    bail!("Task title cannot be empty");
                }
                let task = database.add_task(&title, criteria.as_deref().unwrap_or(""))?;
                if cli.json {
                    print_json(&task)?;
                } else {
                    println!("Added task #{} ({}).", task.id, task.status.as_str());
                }
            }
            TaskCommand::List => {
                let tasks = database.list_tasks()?;
                if cli.json {
                    print_json(&tasks)?;
                } else if tasks.is_empty() {
                    println!("No tasks in the queue.");
                } else {
                    for task in tasks {
                        print_task(&task);
                    }
                }
            }
            TaskCommand::Run { task_id } => {
                run_queued_task(cli.json, &project, &mut database, task_id).await?;
            }
            TaskCommand::Continue {
                task_id,
                confirm_project_inspected,
            } => {
                continue_task(
                    cli.json,
                    &project,
                    &mut database,
                    task_id,
                    confirm_project_inspected,
                )
                .await?;
            }
            TaskCommand::Recover => {
                recover_writer_attempts(cli.json, &mut database)?;
            }
            TaskCommand::Resolve {
                attempt_id,
                action,
                confirm_writer_stopped,
                confirm_project_inspected,
                reason,
            } => {
                resolve_writer_attempt(
                    cli.json,
                    &mut database,
                    attempt_id,
                    action,
                    confirm_writer_stopped,
                    confirm_project_inspected,
                    &reason,
                )?;
            }
        },
        Command::Status => {
            let report = StatusReport {
                project_root: project.root.display().to_string(),
                database_path: database.path().display().to_string(),
                schema_version: database.schema_version()?,
                journal_mode: database.journal_mode()?,
                queue: database.queue_summary()?,
                supervisor: "not running",
            };
            if cli.json {
                print_json(&report)?;
            } else {
                println!("Project: {}", report.project_root);
                println!(
                    "Database: {} (schema {}, {})",
                    report.database_path,
                    report.schema_version,
                    report.journal_mode.to_uppercase()
                );
                println!(
                    "Queue: {} total, {} queued, {} active, {} complete, {} blocked/failed",
                    report.queue.total,
                    report.queue.queued,
                    report.queue.active,
                    report.queue.complete,
                    report.queue.blocked_or_failed
                );
                println!("Supervisor: {}", report.supervisor);
            }
        }
        Command::Logs(arguments) => {
            let events = database.task_events(arguments.task_id)?;
            if cli.json {
                print_json(&events)?;
            } else if events.is_empty() {
                println!("No matching events.");
            } else {
                for event in events {
                    print_event(&event);
                }
            }
        }
        Command::Doctor => {
            let git = if matches!(&project.discovery, crate::project::DiscoveryMethod::Git) {
                "project Git root detected"
            } else {
                "no Git root detected; local project discovery is active"
            };
            let antigravity = AgyProbe::inspect(&project.root);
            let agent_execution = if antigravity.version.as_deref()
                == Some(VERIFIED_AGY_WRITE_VERSION)
                && antigravity.status.as_str() == "available"
                && has_required_writer_capabilities(&antigravity)
            {
                "one-shot Auto and explicit single-task Writer runs are enabled for the tested Agy version; tasks still require independent review and verification, while Semi is deferred to R5".to_owned()
            } else {
                format!(
                    "Writer and Auto execution are unavailable until Agy {} passes the R3 write-boundary probe; Semi is deferred to R5",
                    VERIFIED_AGY_WRITE_VERSION
                )
            };
            let report = DoctorReport {
                project_root: project.root.display().to_string(),
                discovery: project.discovery,
                database_path: database.path().display().to_string(),
                schema_version: database.schema_version()?,
                journal_mode: database.journal_mode()?,
                agent_execution,
                antigravity,
                git,
            };
            if cli.json {
                print_json(&report)?;
            } else {
                println!(
                    "Project root: {} ({:?})",
                    report.project_root, report.discovery
                );
                println!("Database: {}", report.database_path);
                println!("Schema: {}", report.schema_version);
                println!("Journal mode: {}", report.journal_mode.to_uppercase());
                println!("Git: {}", report.git);
                println!("Agent execution: {}", report.agent_execution);
                println!("Antigravity CLI: {}", report.antigravity.status.as_str());
                if let Some(executable) = &report.antigravity.executable {
                    println!("  Executable: {}", executable.display());
                }
                if let Some(version) = &report.antigravity.version {
                    println!("  Version: {version}");
                }
                println!(
                    "  Headless: {}",
                    report.antigravity.capabilities.headless_print.as_str()
                );
                println!(
                    "  JSON output: {}",
                    report
                        .antigravity
                        .capabilities
                        .structured_json_output
                        .as_str()
                );
                println!(
                    "  Session resume: {}",
                    report
                        .antigravity
                        .capabilities
                        .public_session_resume
                        .as_str()
                );
                println!(
                    "  Terminal sandbox: {}",
                    report.antigravity.capabilities.terminal_sandbox.as_str()
                );
                println!(
                    "  Auto mode: {}",
                    report.antigravity.capabilities.auto_mode.as_str()
                );
                println!(
                    "  Filesystem write scope: {}",
                    report
                        .antigravity
                        .capabilities
                        .filesystem_write_scope
                        .as_str()
                );
                println!(
                    "  Safe unattended write: {}",
                    report
                        .antigravity
                        .capabilities
                        .safe_unattended_write
                        .as_str()
                );
                if let Some(note) = &report.antigravity.note {
                    println!("  Note: {note}");
                }
            }
        }
    }
    Ok(())
}

async fn run_queued_task(
    json_output: bool,
    project: &Project,
    database: &mut Database,
    task_id: i64,
) -> Result<()> {
    let report = execute_writer_task(json_output, project, database, task_id, None).await?;
    print_writer_report(json_output, &report, None)?;
    ensure_writer_succeeded(&report)
}

async fn continue_task(
    json_output: bool,
    project: &Project,
    database: &mut Database,
    task_id: i64,
    project_inspected_attested: bool,
) -> Result<()> {
    if !project_inspected_attested {
        bail!("Writer continuation requires --confirm-project-inspected");
    }
    let source = database
        .latest_continuable_attempt(task_id)?
        .with_context(|| {
            format!("Task #{task_id} has no eligible terminal Writer error to continue")
        })?;
    let conversation_id = source
        .conversation_id
        .context("Eligible Writer attempt has no saved conversation ID")?;
    let continuation = WriterContinuation {
        source_attempt_id: source.id,
        conversation_id,
        project_inspected_attested,
    };
    let report =
        execute_writer_task(json_output, project, database, task_id, Some(continuation)).await?;
    print_writer_report(json_output, &report, None)?;
    ensure_writer_succeeded(&report)
}

async fn execute_writer_task(
    _json_output: bool,
    project: &Project,
    database: &mut Database,
    task_id: i64,
    continuation: Option<WriterContinuation>,
) -> Result<crate::supervisor::WriterExecutionReport> {
    let probe = AgyProbe::inspect(&project.root);
    if probe.status.as_str() != "available" {
        bail!(
            "Antigravity CLI is unavailable: {}",
            probe.note.as_deref().unwrap_or("capability probe failed")
        );
    }
    let version = probe
        .version
        .clone()
        .context("Antigravity did not report a version")?;
    if version != VERIFIED_AGY_WRITE_VERSION {
        bail!(
            "Writer execution is enabled only for the boundary-tested Antigravity version {VERIFIED_AGY_WRITE_VERSION}; found {version}"
        );
    }
    if !has_required_writer_capabilities(&probe) {
        bail!("Antigravity no longer advertises the required headless, JSON, or sandbox flags");
    }
    let executable = probe
        .executable
        .context("Antigravity executable path is unavailable")?;
    let now = now_ms()?;
    let operation = if continuation.is_some() {
        "continue"
    } else {
        "run"
    };
    let owner_id = format!("talos-task-{operation}-{}-{now}", std::process::id());
    let lease_duration_ms = i64::try_from(SUPERVISOR_LEASE.as_millis())
        .context("Supervisor lease duration is out of range")?;
    let lease = database
        .acquire_supervisor_lease(&owner_id, now, lease_duration_ms)?
        .context("Another TALOS supervisor currently owns the project lease")?;

    let execution = if let Some(continuation) = continuation {
        continue_writer_attempt(
            project,
            database,
            task_id,
            executable,
            &version,
            WRITER_TIMEOUT,
            continuation,
        )
        .await
    } else {
        run_writer_attempt(
            project,
            database,
            task_id,
            executable,
            &version,
            WRITER_TIMEOUT,
        )
        .await
    };
    let lease_released = database.release_supervisor_lease(&lease)?;
    if !lease_released {
        bail!("TALOS lost its supervisor lease while running the Writer");
    }
    execution
}

fn print_writer_report(
    json_output: bool,
    report: &crate::supervisor::WriterExecutionReport,
    changed_files: Option<&[String]>,
) -> Result<()> {
    let task_status = if report.outcome == crate::db::WriterAttemptOutcome::Succeeded {
        "reviewing"
    } else {
        "blocked"
    };
    let output = json!({
        "task_id": report.attempt.task_id,
        "task_status": task_status,
        "attempt": &report.attempt,
        "writer_response": &report.response,
        "changed_files": changed_files,
        "review_required": true,
        "verification_ran": false,
    });
    if json_output {
        print_json(&output)?;
    } else {
        println!(
            "Writer attempt #{} ended in {}. Task #{} is now {task_status}.",
            report.attempt.id, report.attempt.state, report.attempt.task_id
        );
        if task_status == "reviewing" {
            println!("Independent review and project verification are still required.");
        }
        if let Some(response) = &report.response {
            println!("Writer response: {response}");
        }
        if let Some(changed_files) = changed_files {
            if changed_files.is_empty() {
                println!("Git reports no changed files.");
            } else {
                println!("Changed files:");
                for path in changed_files {
                    println!("  {path}");
                }
            }
        }
    }
    Ok(())
}

fn ensure_writer_succeeded(report: &crate::supervisor::WriterExecutionReport) -> Result<()> {
    if report.outcome != crate::db::WriterAttemptOutcome::Succeeded {
        bail!(
            "Writer attempt #{} did not produce a reviewable result ({})",
            report.attempt.id,
            report
                .attempt
                .error_code
                .as_deref()
                .unwrap_or("unknown_error")
        );
    }
    Ok(())
}

async fn run_one_shot_auto(
    json_output: bool,
    project: &Project,
    database: &mut Database,
) -> Result<()> {
    if !matches!(&project.discovery, DiscoveryMethod::Git) {
        bail!("`up --auto` requires a Git project so TALOS can report actual changed files");
    }
    validate_project_scope(project)?;
    let task = database
        .list_tasks()?
        .into_iter()
        .find(|task| task.status == crate::db::TaskStatus::Queued);
    let Some(task) = task else {
        if json_output {
            print_json(&json!({
                "mode": "auto",
                "result": "no_queued_tasks",
                "writer_launched": false,
                "automatic_retry": false,
            }))?;
        } else {
            println!("No queued tasks; no Writer was started.");
        }
        return Ok(());
    };

    let before = git_status_paths(project)?;
    if !before.is_empty() {
        bail!(
            "`up --auto` requires a clean Git worktree; inspect or commit these existing paths first: {}",
            before.join(", ")
        );
    }

    let execution = execute_writer_task(json_output, project, database, task.id, None).await;
    let changed_files = git_status_paths(project)?;
    let report = execution?;
    print_writer_report(json_output, &report, Some(&changed_files))?;
    ensure_writer_succeeded(&report)
}

fn git_status_paths(project: &Project) -> Result<Vec<String>> {
    let mut command = ProcessCommand::new("git");
    command
        .args(["status", "--porcelain=v1", "-z", "--untracked-files=all"])
        .current_dir(&project.root)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0");
    for (key, _) in env::vars_os().filter(|(key, _)| key.to_string_lossy().starts_with("GIT_")) {
        command.env_remove(key);
    }
    let output = command
        .output()
        .context("Cannot inspect Git changes after the Writer run")?;
    if !output.status.success() {
        bail!(
            "Git status failed while inspecting Writer changes (exit code {})",
            output.status.code().unwrap_or(-1)
        );
    }

    let entries = output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
        .collect::<Vec<_>>();
    let mut paths = Vec::with_capacity(entries.len());
    let mut index = 0;
    while index < entries.len() {
        let entry = entries[index];
        if entry.len() < 4 || entry[2] != b' ' {
            bail!("Git returned an invalid porcelain status entry");
        }
        paths.push(String::from_utf8_lossy(&entry[3..]).into_owned());
        let status = &entry[..2];
        if status.contains(&b'R') || status.contains(&b'C') {
            index += 1;
            if index >= entries.len() {
                bail!("Git returned an incomplete rename status entry");
            }
            paths.push(String::from_utf8_lossy(entries[index]).into_owned());
        }
        index += 1;
    }
    Ok(paths)
}

fn recover_writer_attempts(json_output: bool, database: &mut Database) -> Result<()> {
    let now = now_ms()?;
    let owner_id = format!("talos-task-recover-{}-{now}", std::process::id());
    let lease_duration_ms = i64::try_from(SUPERVISOR_LEASE.as_millis())
        .context("Supervisor lease duration is out of range")?;
    let lease = database
        .acquire_supervisor_lease(&owner_id, now, lease_duration_ms)?
        .context("Another TALOS supervisor currently owns the project lease")?;

    let quarantine_result = database.quarantine_unfinished_writer_attempts(&lease);
    let lease_released = database.release_supervisor_lease(&lease)?;
    if !lease_released {
        bail!("TALOS lost its supervisor lease during Writer recovery");
    }
    let quarantined_count = quarantine_result?;
    let unresolved_attempts = database
        .writer_attempts(None)?
        .into_iter()
        .filter(|attempt| {
            matches!(attempt.state.as_str(), "launching" | "running")
                || (attempt.state == "unknown" && attempt.operator_resolution.is_none())
        })
        .collect::<Vec<_>>();

    if json_output {
        print_json(&json!({
            "recovery": if unresolved_attempts.is_empty() {
                "no_unresolved_attempts"
            } else {
                "requires_operator_inspection"
            },
            "newly_quarantined": quarantined_count,
            "unresolved_attempts": unresolved_attempts,
            "writer_launched": false,
            "automatic_retry": false,
        }))?;
    } else if unresolved_attempts.is_empty() {
        println!("No unresolved Writer attempts require inspection.");
        println!("No agent was started.");
    } else {
        if quarantined_count > 0 {
            println!("Quarantined {quarantined_count} unfinished Writer attempt(s).");
        } else {
            println!(
                "{} unresolved Writer attempt(s) remain quarantined.",
                unresolved_attempts.len()
            );
        }
        println!("No agent was started.");
        for attempt in &unresolved_attempts {
            let recorded_pid = attempt
                .process_id
                .map(|pid| pid.to_string())
                .unwrap_or_else(|| "not recorded".to_owned());
            println!(
                "Attempt #{} for task #{} is {} (recorded PID: {}).",
                attempt.id, attempt.task_id, attempt.state, recorded_pid
            );
        }
        println!(
            "An unresolved Writer may still be running or may have changed project files. TALOS did not inspect or signal processes, resume a conversation, or retry an attempt. Inspect process and project state before deciding how to proceed."
        );
    }
    Ok(())
}

fn resolve_writer_attempt(
    json_output: bool,
    database: &mut Database,
    attempt_id: i64,
    action: ResolveAction,
    confirm_writer_stopped: bool,
    confirm_project_inspected: bool,
    reason: &str,
) -> Result<()> {
    if !confirm_writer_stopped || !confirm_project_inspected {
        bail!(
            "Writer resolution requires --confirm-writer-stopped and --confirm-project-inspected"
        );
    }
    let now = now_ms()?;
    let owner_id = format!("talos-task-resolve-{}-{now}", std::process::id());
    let lease_duration_ms = i64::try_from(SUPERVISOR_LEASE.as_millis())
        .context("Supervisor lease duration is out of range")?;
    let lease = database
        .acquire_supervisor_lease(&owner_id, now, lease_duration_ms)?
        .context("Another TALOS supervisor currently owns the project lease")?;

    let resolution = (|| {
        let attempt = database
            .writer_attempts(None)?
            .into_iter()
            .find(|attempt| attempt.id == attempt_id)
            .with_context(|| format!("Writer attempt #{attempt_id} does not exist"))?;
        if attempt.state != "unknown" || attempt.operator_resolution.is_some() {
            bail!("Writer attempt #{attempt_id} is not an unresolved unknown attempt");
        }
        if let Some(process_id) = attempt.process_id {
            match crate::supervisor::writer_process_group_running(process_id) {
                Ok(false) => {}
                Ok(true) => bail!(
                    "Writer process group for attempt #{attempt_id} still exists; resolution was refused"
                ),
                Err(error) => {
                    return Err(error).context(format!(
                        "Cannot verify the Writer process group for attempt #{attempt_id}; resolution was refused"
                    ));
                }
            }
        }
        database.resolve_unknown_writer_attempt(
            &lease,
            attempt_id,
            match action {
                ResolveAction::AcceptForReview => WriterResolutionAction::AcceptForReview,
                ResolveAction::AuthorizeRetry => WriterResolutionAction::AuthorizeRetry,
                ResolveAction::Abandon => WriterResolutionAction::Abandon,
            },
            confirm_writer_stopped,
            confirm_project_inspected,
            reason,
        )
    })();
    let released = database.release_supervisor_lease(&lease);
    let released = released?;
    if !released {
        bail!("TALOS lost its supervisor lease during Writer resolution");
    }
    let resolution = resolution?;

    if json_output {
        print_json(&resolution)?;
    } else {
        print_resolution(&resolution);
    }
    Ok(())
}

fn print_resolution(resolution: &WriterResolutionRecord) {
    println!(
        "Recorded operator resolution '{}' for Writer attempt #{}.",
        match resolution.action {
            WriterResolutionAction::AcceptForReview => "accept-for-review",
            WriterResolutionAction::AuthorizeRetry => "authorize-retry",
            WriterResolutionAction::Abandon => "abandon",
        },
        resolution.attempt_id
    );
    println!(
        "Task #{} is now {}. The Writer attempt outcome remains unknown.",
        resolution.task_id,
        resolution.task_status.as_str()
    );
    if resolution.action == WriterResolutionAction::AuthorizeRetry {
        println!("No retry was launched; run the task explicitly when ready.");
    }
}

fn has_required_writer_capabilities(probe: &AgyProbe) -> bool {
    probe.capabilities.headless_print.as_str() == "advertised"
        && probe.capabilities.structured_json_output.as_str() == "advertised"
        && probe.capabilities.terminal_sandbox.as_str() == "advertised"
}

fn now_ms() -> Result<i64> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("System clock is earlier than the Unix epoch")?;
    i64::try_from(duration.as_millis()).context("System clock timestamp is out of range")
}

fn print_task(task: &TaskRecord) {
    println!("#{} [{}] {}", task.id, task.status.as_str(), task.title);
    if !task.acceptance_criteria.is_empty() {
        println!("  Acceptance criteria: {}", task.acceptance_criteria);
    }
}

fn print_event(event: &TaskEvent) {
    println!(
        "{} task #{} {} by {}: {}",
        event.occurred_at_ms, event.task_id, event.event_type, event.actor, event.reason
    );
}

fn print_json(value: &impl Serialize) -> Result<()> {
    println!(
        "{}",
        serde_json::to_string_pretty(value).context("Cannot serialize JSON output")?
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        sync::atomic::{AtomicU64, Ordering},
    };

    use super::{
        ResolveAction, SUPERVISOR_LEASE, now_ms, recover_writer_attempts, resolve_writer_attempt,
    };
    use crate::{
        db::{Database, TaskStatus},
        project::{DiscoveryMethod, Project},
    };

    static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

    fn temp_project() -> Project {
        let sequence = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "talos-recovery-test-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir_all(&root).expect("create temporary project");
        Project {
            root,
            discovery: DiscoveryMethod::CurrentDirectory,
        }
    }

    #[test]
    fn recovery_requires_the_supervisor_lease() {
        let project = temp_project();
        let mut database = Database::open(&project).expect("initialize database");
        let task = database
            .add_task("Do not recover under another owner", "")
            .expect("add task");
        let attempt = database
            .begin_writer_attempt(task.id, &"56".repeat(32))
            .expect("begin Writer attempt");
        let now = now_ms().expect("read current time");
        let lease_duration_ms = i64::try_from(SUPERVISOR_LEASE.as_millis())
            .expect("lease duration fits in SQLite integer");
        let held_lease = database
            .acquire_supervisor_lease("another-supervisor", now, lease_duration_ms)
            .expect("acquire competing lease")
            .expect("competing owner gets lease");

        assert!(recover_writer_attempts(false, &mut database).is_err());
        assert_eq!(
            database
                .writer_attempts(Some(task.id))
                .expect("read unchanged attempt")[0]
                .state,
            "launching"
        );
        assert_eq!(
            database.list_tasks().expect("read unchanged task")[0].status,
            TaskStatus::Implementing
        );

        assert!(
            database
                .release_supervisor_lease(&held_lease)
                .expect("release competing lease")
        );
        recover_writer_attempts(false, &mut database).expect("recover after lease release");
        let recovered = database
            .writer_attempts(Some(task.id))
            .expect("read quarantined attempt");
        assert_eq!(recovered[0].id, attempt.id);
        assert_eq!(recovered[0].state, "unknown");
        assert_eq!(
            database.list_tasks().expect("read recovered task")[0].status,
            TaskStatus::Blocked
        );
        fs::remove_dir_all(&project.root).expect("remove temporary project");
    }

    fn quarantine_attempt(database: &mut Database, attempt_id: i64, owner: &str) {
        let now = now_ms().expect("read current time");
        let lease_duration_ms = i64::try_from(SUPERVISOR_LEASE.as_millis())
            .expect("lease duration fits in SQLite integer");
        let lease = database
            .acquire_supervisor_lease(owner, now, lease_duration_ms)
            .expect("acquire preparation lease")
            .expect("preparation lease is available");
        database
            .quarantine_unfinished_writer_attempts(&lease)
            .expect("quarantine attempt");
        assert_eq!(
            database
                .writer_attempts(None)
                .expect("read quarantined attempts")
                .into_iter()
                .find(|attempt| attempt.id == attempt_id)
                .map(|attempt| attempt.state),
            Some("unknown".to_owned())
        );
        assert!(
            database
                .release_supervisor_lease(&lease)
                .expect("release preparation lease")
        );
    }

    #[test]
    fn app_resolution_requires_both_attestations_and_never_claims_writer_success() {
        let project = temp_project();
        let mut database = Database::open(&project).expect("initialize database");
        let task = database
            .add_task("Resolve a pre-PID crash", "")
            .expect("add task");
        let attempt_id = database
            .begin_writer_attempt(task.id, &"56".repeat(32))
            .expect("begin attempt without a recorded PID")
            .id;
        quarantine_attempt(&mut database, attempt_id, "prepare-no-pid-resolution");

        assert!(
            resolve_writer_attempt(
                false,
                &mut database,
                attempt_id,
                ResolveAction::AcceptForReview,
                false,
                true,
                "The Writer and diff were inspected",
            )
            .is_err()
        );
        assert!(
            database
                .has_unresolved_writer_attempts()
                .expect("missing attestation keeps queue blocked")
        );

        resolve_writer_attempt(
            true,
            &mut database,
            attempt_id,
            ResolveAction::AcceptForReview,
            true,
            true,
            "The Writer and diff were inspected",
        )
        .expect("both explicit attestations allow review disposition");
        let attempt = database
            .writer_attempts(Some(task.id))
            .expect("read attempt")
            .remove(0);
        assert_eq!(attempt.state, "unknown");
        assert_eq!(
            attempt.operator_resolution.as_deref(),
            Some("accept_for_review")
        );
        assert_eq!(
            database.list_tasks().expect("read task status")[0].status,
            TaskStatus::Reviewing
        );
        assert!(
            !database
                .has_unresolved_writer_attempts()
                .expect("committed resolution releases queue guard")
        );
        fs::remove_dir_all(&project.root).expect("remove temporary project");
    }

    #[cfg(unix)]
    #[test]
    fn app_resolution_refuses_live_or_unverifiable_process_groups() {
        let project = temp_project();
        let mut database = Database::open(&project).expect("initialize database");
        let live_task = database
            .add_task("Refuse a live group", "")
            .expect("add live-group task");
        let live_attempt = database
            .begin_writer_attempt(live_task.id, &"67".repeat(32))
            .expect("begin live-group attempt");
        let own_group = u32::try_from(unsafe { libc::getpgrp() })
            .expect("current process group fits in PID type");
        database
            .mark_writer_attempt_running(live_attempt.id, own_group)
            .expect("record live process group ID");
        let live_attempt_id = live_attempt.id;
        quarantine_attempt(&mut database, live_attempt_id, "prepare-live-group");
        assert!(
            resolve_writer_attempt(
                false,
                &mut database,
                live_attempt_id,
                ResolveAction::Abandon,
                true,
                true,
                "I inspected the process and project diff",
            )
            .is_err()
        );
        assert_eq!(
            database
                .writer_attempts(Some(live_task.id))
                .expect("read live-group attempt")[0]
                .operator_resolution,
            None
        );

        let unavailable_project = temp_project();
        let mut unavailable_database =
            Database::open(&unavailable_project).expect("initialize unavailable group database");
        let unavailable_task = unavailable_database
            .add_task("Refuse an unverifiable group", "")
            .expect("add unavailable-group task");
        let unavailable_attempt = unavailable_database
            .begin_writer_attempt(unavailable_task.id, &"78".repeat(32))
            .expect("begin unavailable-group attempt");
        unavailable_database
            .mark_writer_attempt_running(unavailable_attempt.id, u32::MAX)
            .expect("record out-of-range process group ID");
        let unavailable_attempt_id = unavailable_attempt.id;
        quarantine_attempt(
            &mut unavailable_database,
            unavailable_attempt_id,
            "prepare-unavailable-group",
        );
        assert!(
            resolve_writer_attempt(
                false,
                &mut unavailable_database,
                unavailable_attempt_id,
                ResolveAction::Abandon,
                true,
                true,
                "I inspected the process and project diff",
            )
            .is_err()
        );
        assert_eq!(
            unavailable_database
                .writer_attempts(Some(unavailable_task.id))
                .expect("read unavailable-group attempt")[0]
                .operator_resolution,
            None
        );
        assert!(
            unavailable_database
                .has_unresolved_writer_attempts()
                .expect("refused resolutions keep the global guard")
        );
        fs::remove_dir_all(&project.root).expect("remove temporary project");
        fs::remove_dir_all(&unavailable_project.root).expect("remove unavailable group project");
    }
}
