use std::{
    env,
    io::{self, IsTerminal, Write},
    process::Command as ProcessCommand,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde_json::json;

use crate::{
    adapter::agy::AgyProbe,
    checks::{CheckExecution, ProjectCheck, run_project_check, validate_selected_checks},
    cli::{Cli, Command, ResolveAction, TaskCommand},
    config::{AdapterKind, AgentRole, ProjectConfig},
    db::{
        Database, ProjectControlEvent, QueueSummary, ResumeProjectOutcome, StopRequestOutcome,
        TaskEvent, TaskRecord, WriterResolutionAction, WriterResolutionRecord,
    },
    policy::{self, Action as PolicyAction, Authorization, ExecutionMode},
    project::{DiscoveryMethod, Project},
    revision::{RevisionSnapshot, git_head},
    secrets::{contains_sensitive, redact_sensitive},
    supervisor::{
        VERIFIED_AGY_CRITIC_VERSION, VERIFIED_AGY_WRITE_VERSION, WriterContinuation,
        continue_writer_attempt, run_critic_review, run_writer_attempt, validate_project_scope,
    },
    tmux::{TmuxClient, TmuxProbe},
};

const WRITER_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const SUPERVISOR_LEASE: Duration = Duration::from_secs(35 * 60);
const VERIFICATION_BUDGET: Duration = Duration::from_secs(25 * 60);

#[derive(Serialize)]
struct StatusReport {
    project_root: String,
    database_path: String,
    schema_version: i64,
    journal_mode: String,
    queue: QueueSummary,
    queue_paused: bool,
    pause_reason: Option<String>,
    stop_requested_attempt_id: Option<i64>,
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
    writer_adapter: &'static str,
    critic_adapter: &'static str,
    project_checks: Vec<String>,
    tmux: TmuxProbe,
    antigravity: AgyProbe,
    git: &'static str,
}

pub async fn run(cli: Cli) -> Result<()> {
    let working_directory = env::current_dir().context("Cannot read current directory")?;
    let project = Project::discover(&working_directory)?;

    if matches!(
        &cli.command,
        Command::Task(arguments)
            if matches!(
                &arguments.command,
                TaskCommand::Run { .. } | TaskCommand::Continue { .. } | TaskCommand::Review { .. }
                    | TaskCommand::Verify { .. }
            )
    ) {
        validate_project_scope(&project)?;
    }

    if matches!(&cli.command, Command::Attach) {
        let tmux = TmuxClient::default();
        let probe = tmux.probe();
        if !probe.available {
            bail!("tmux is unavailable; use TALOS foreground commands or install tmux");
        }
        let session_name = tmux.ensure_project_session(&project.root)?;
        if cli.json {
            print_json(&json!({
                "mode": "tmux_attach",
                "project_root": project.root,
                "session_name": session_name,
                "worker_started": false,
            }))?;
        } else {
            println!("TALOS control session: {session_name}");
        }
        tmux.attach_project_session(&session_name)?;
        return Ok(());
    }

    ensure_talos_ignored(&project)?;

    let mut database = Database::open(&project)?;
    match cli.command {
        Command::Up(arguments) if arguments.auto => {
            ensure_queue_running(&database)?;
            run_one_shot_auto(cli.json, &project, &mut database).await?;
        }
        Command::Up(arguments) if arguments.semi => {
            ensure_queue_running(&database)?;
            run_one_shot_semi(cli.json, &project, &mut database).await?;
        }
        Command::Up(_) => {
            let queue = database.queue_summary()?;
            let control = database.project_control_state()?;
            if cli.json {
                print_json(&json!({
                    "mode": "observe",
                    "project_root": project.root,
                    "database_path": database.path(),
                    "schema_version": database.schema_version()?,
                    "queue": queue,
                    "queue_paused": control.queue_paused,
                    "pause_reason": control.pause_reason,
                    "stop_requested_attempt_id": control.stop_requested_attempt_id,
                    "worker_started": false,
                }))?;
            } else {
                println!("TALOS is ready in observe mode.");
                println!(
                    "Project: {}",
                    redact_sensitive(&project.root.display().to_string())
                );
                println!(
                    "Database: {}",
                    redact_sensitive(&database.path().display().to_string())
                );
                println!("Queue: {} total, {} queued", queue.total, queue.queued);
                if control.queue_paused {
                    println!(
                        "Queue is PAUSED: {}",
                        redact_sensitive(
                            control
                                .pause_reason
                                .as_deref()
                                .unwrap_or("no reason recorded"),
                        )
                    );
                }
                if let Some(attempt_id) = control.stop_requested_attempt_id {
                    println!("Writer stop requested for attempt #{attempt_id}.");
                }
                println!("No agent or worker was started.");
            }
        }
        Command::Task(arguments) => match arguments.command {
            TaskCommand::Add {
                title,
                criteria,
                checks,
            } => {
                if title.trim().is_empty() {
                    bail!("Task title cannot be empty");
                }
                let acceptance_criteria = criteria.as_deref().unwrap_or("");
                if contains_sensitive(&title) || contains_sensitive(acceptance_criteria) {
                    bail!(
                        "Task title or acceptance criteria contains secret-like text; remove credentials before saving the task"
                    );
                }
                let config = ProjectConfig::load(&project)?;
                let check_specs = validate_selected_checks(&config.checks, &checks)?;
                let task =
                    database.add_task_with_checks(&title, acceptance_criteria, &check_specs)?;
                if cli.json {
                    print_json(&task)?;
                } else {
                    println!("Added task #{} ({}).", task.id, task.status.as_str());
                }
            }
            TaskCommand::Checks { task_id, checks } => {
                let config = ProjectConfig::load(&project)?;
                let check_specs = validate_selected_checks(&config.checks, &checks)?;
                database.add_task_verification_checks(task_id, &check_specs)?;
                if cli.json {
                    print_json(&json!({
                        "task_id": task_id,
                        "check_ids": check_specs.iter().map(|check| check.id.as_str()).collect::<Vec<_>>(),
                        "requirements_saved": true,
                    }))?;
                } else {
                    println!(
                        "Saved {} verification check(s) for task #{}.",
                        check_specs.len(),
                        task_id
                    );
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
            TaskCommand::Review { task_id } => {
                run_task_review(cli.json, &project, &mut database, task_id).await?;
            }
            TaskCommand::Verify { task_id } => {
                run_task_verification(cli.json, &project, &mut database, task_id).await?;
            }
            TaskCommand::Evidence { task_id } => {
                if !database.list_tasks()?.iter().any(|task| task.id == task_id) {
                    bail!("Task #{task_id} does not exist");
                }
                let history = database.verification_history(task_id)?;
                if cli.json {
                    print_json(&history)?;
                } else if history.is_empty() {
                    println!("Task #{task_id} has no project verification history.");
                } else {
                    for evidence in history {
                        println!(
                            "Run #{} {} (review #{}, revision {}).",
                            evidence.run.id,
                            evidence.run.status,
                            evidence.run.critic_review_id,
                            redact_sensitive(&evidence.run.revision_id),
                        );
                        println!(
                            "  HEAD: {}; patch SHA-256: {}",
                            redact_sensitive(&evidence.head_commit),
                            evidence.patch_sha256
                        );
                        for check in evidence.checks {
                            let argv = serde_json::to_string(&check.argv)?;
                            println!(
                                "  Check {}: exit={}, duration={}, version exit={}, version duration={}, passed={}",
                                redact_sensitive(&check.check_id),
                                check
                                    .exit_code
                                    .map_or_else(|| "none".to_owned(), |code| code.to_string()),
                                check.duration_ms.map_or_else(
                                    || "unknown".to_owned(),
                                    |millis| format!("{millis}ms"),
                                ),
                                check
                                    .version_exit_code
                                    .map_or_else(|| "none".to_owned(), |code| code.to_string()),
                                check.version_duration_ms.map_or_else(
                                    || "unknown".to_owned(),
                                    |millis| format!("{millis}ms"),
                                ),
                                check.passed(),
                            );
                            println!("    argv: {}", redact_sensitive(&argv));
                            println!("    cwd: {}", redact_sensitive(&check.working_directory));
                            println!(
                                "    stdout sha256: {}; stderr sha256: {}",
                                check.stdout_sha256, check.stderr_sha256
                            );
                        }
                    }
                }
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
            let control = database.project_control_state()?;
            let report = StatusReport {
                project_root: project.root.display().to_string(),
                database_path: database.path().display().to_string(),
                schema_version: database.schema_version()?,
                journal_mode: database.journal_mode()?,
                queue: database.queue_summary()?,
                queue_paused: control.queue_paused,
                pause_reason: control.pause_reason,
                stop_requested_attempt_id: control.stop_requested_attempt_id,
                supervisor: "not running",
            };
            if cli.json {
                print_json(&report)?;
            } else {
                println!("Project: {}", redact_sensitive(&report.project_root));
                println!(
                    "Database: {} (schema {}, {})",
                    redact_sensitive(&report.database_path),
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
                if report.queue_paused {
                    println!(
                        "Queue: PAUSED ({})",
                        redact_sensitive(
                            report
                                .pause_reason
                                .as_deref()
                                .unwrap_or("no reason recorded"),
                        )
                    );
                } else {
                    println!("Queue: running");
                }
                if let Some(attempt_id) = report.stop_requested_attempt_id {
                    println!("Writer stop requested for attempt #{attempt_id}.");
                }
                println!("Supervisor: {}", report.supervisor);
            }
        }
        Command::Logs(arguments) => {
            if arguments.project {
                let events = database.project_control_events()?;
                if cli.json {
                    print_json(&events)?;
                } else if events.is_empty() {
                    println!("No project control events.");
                } else {
                    for event in events {
                        print_project_control_event(&event);
                    }
                }
            } else {
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
        }
        Command::Pause(arguments) => {
            let reason = arguments
                .reason
                .as_deref()
                .unwrap_or("Operator requested a queue pause");
            let revision_id = current_project_revision_id(&project);
            let control = database.pause_project(reason, revision_id.as_deref())?;
            if cli.json {
                print_json(&json!({
                    "queue_paused": control.queue_paused,
                    "paused_at_ms": control.paused_at_ms,
                    "pause_reason": control.pause_reason,
                    "pause_revision_id": control.pause_revision_id,
                    "active_writer_interrupted": false,
                }))?;
            } else {
                println!("Project queue paused.");
                println!("New Writer attempts and continuations are blocked.");
                println!("An already-running Writer may continue; inspect it separately.");
                if control.pause_revision_id.is_none() {
                    println!("No safe Git snapshot was recorded; resume will require --reconcile.");
                }
            }
        }
        Command::Resume(arguments) => {
            let revision_id = current_project_revision_id(&project);
            match database.resume_project(revision_id.as_deref(), arguments.reconcile)? {
                ResumeProjectOutcome::AlreadyRunning => {
                    if cli.json {
                        print_json(&json!({ "queue_paused": false, "already_running": true }))?;
                    } else {
                        println!("Project queue is already running.");
                    }
                }
                ResumeProjectOutcome::ReconciliationRequired {
                    pause_revision_id,
                    current_revision_id,
                } => {
                    if cli.json {
                        print_json(&json!({
                            "queue_paused": true,
                            "reconciliation_required": true,
                            "pause_revision_id": pause_revision_id,
                            "current_revision_id": current_revision_id,
                        }))?;
                    } else {
                        println!(
                            "Project changes differ from the pause snapshot or no safe snapshot was available."
                        );
                        println!(
                            "Inspect `git diff` and `git status`, then run `talos resume --reconcile` to accept the current state."
                        );
                        println!("The queue remains paused until reconciliation completes.");
                    }
                }
                ResumeProjectOutcome::Resumed {
                    reconciled_manual_changes,
                    reopened_task_ids,
                } => {
                    if cli.json {
                        print_json(&json!({
                            "queue_paused": false,
                            "reconciled_manual_changes": reconciled_manual_changes,
                            "reopened_task_ids": reopened_task_ids,
                        }))?;
                    } else {
                        if reconciled_manual_changes {
                            println!(
                                "Manual project changes reconciled; affected tasks require a fresh Critic review."
                            );
                            if reopened_task_ids.is_empty() {
                                println!("No verifying tasks needed to be reopened.");
                            } else {
                                println!("Reopened tasks: {}", format_task_ids(&reopened_task_ids));
                            }
                        }
                        println!("Project queue resumed.");
                    }
                }
            }
        }
        Command::Stop => {
            stop_writer(cli.json, &mut database).await?;
        }
        Command::Doctor => {
            let git = if matches!(&project.discovery, crate::project::DiscoveryMethod::Git) {
                "project Git root detected"
            } else {
                "no Git root detected; local project discovery is active"
            };
            let antigravity = AgyProbe::inspect(&project.root);
            let project_config = ProjectConfig::load(&project)?;
            let agent_execution = if antigravity.version.as_deref()
                == Some(VERIFIED_AGY_WRITE_VERSION)
                && antigravity.status.as_str() == "available"
                && has_required_writer_capabilities(&antigravity)
            {
                "one-shot Auto and explicit Writer runs are enabled for the tested Agy version; Semi asks before starting a Writer, and approved project checks are available after Critic review".to_owned()
            } else {
                format!(
                    "Writer and Auto execution are unavailable until Agy {} passes the R3 write-boundary probe",
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
                writer_adapter: project_config.adapter_for(AgentRole::Writer).as_str(),
                critic_adapter: project_config.adapter_for(AgentRole::Critic).as_str(),
                project_checks: project_config
                    .checks
                    .iter()
                    .map(|check| check.id.clone())
                    .collect(),
                tmux: TmuxClient::default().probe(),
                antigravity,
                git,
            };
            if cli.json {
                print_json(&report)?;
            } else {
                println!(
                    "Project root: {} ({:?})",
                    redact_sensitive(&report.project_root),
                    report.discovery
                );
                println!("Database: {}", redact_sensitive(&report.database_path));
                println!("Schema: {}", report.schema_version);
                println!("Journal mode: {}", report.journal_mode.to_uppercase());
                println!("Git: {}", report.git);
                println!("Agent execution: {}", report.agent_execution);
                println!("Writer adapter: {}", report.writer_adapter);
                println!("Critic adapter: {}", report.critic_adapter);
                if report.project_checks.is_empty() {
                    println!("Project checks: none configured");
                } else {
                    println!("Project checks: {}", report.project_checks.join(", "));
                }
                match (&report.tmux.available, &report.tmux.version) {
                    (true, Some(version)) => println!("tmux: {version}"),
                    (true, None) => println!("tmux: available, version unknown"),
                    (false, _) => println!("tmux: unavailable; foreground mode remains available"),
                }
                println!("Antigravity CLI: {}", report.antigravity.status.as_str());
                if let Some(executable) = &report.antigravity.executable {
                    println!(
                        "  Executable: {}",
                        redact_sensitive(&executable.display().to_string())
                    );
                }
                if let Some(version) = &report.antigravity.version {
                    println!("  Version: {}", redact_sensitive(version));
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
                    println!("  Note: {}", redact_sensitive(note));
                }
            }
        }
        Command::Attach => bail!("Attach command was not dispatched before runtime initialization"),
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

fn ensure_queue_running(database: &Database) -> Result<()> {
    let control = database.project_control_state()?;
    if control.queue_paused {
        bail!(
            "TALOS queue is paused{}; inspect project changes and run `talos resume`",
            control
                .pause_reason
                .as_deref()
                .map(|reason| format!(" ({})", redact_sensitive(reason)))
                .unwrap_or_default()
        );
    }
    Ok(())
}

fn current_project_revision_id(project: &Project) -> Option<String> {
    let head = git_head(&project.root).ok().flatten()?;
    RevisionSnapshot::capture(&project.root, &head)
        .ok()
        .map(|snapshot| snapshot.revision_id)
}

async fn stop_writer(json_output: bool, database: &mut Database) -> Result<()> {
    let attempt_id = match database.request_writer_stop()? {
        StopRequestOutcome::NoActiveWriter => {
            if json_output {
                print_json(&json!({
                    "stop_requested": false,
                    "result": "no_active_writer",
                }))?;
            } else {
                println!("No active Writer attempt; nothing to stop.");
            }
            return Ok(());
        }
        StopRequestOutcome::Requested { attempt_id } => attempt_id,
    };

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let attempt = database
            .writer_attempts(None)?
            .into_iter()
            .find(|attempt| attempt.id == attempt_id)
            .with_context(|| format!("Writer attempt #{attempt_id} disappeared while stopping"))?;
        if !matches!(attempt.state.as_str(), "launching" | "running") {
            let stop_confirmed =
                attempt.error_code.as_deref() == Some("writer_stopped_by_operator");
            if json_output {
                print_json(&json!({
                    "stop_requested": true,
                    "stop_confirmed": stop_confirmed,
                    "attempt_id": attempt_id,
                    "attempt_state": attempt.state,
                }))?;
            } else if stop_confirmed {
                println!("Writer attempt #{attempt_id} stopped after the request.");
            } else {
                println!(
                    "Writer attempt #{attempt_id} finished as {} before the stop request took effect.",
                    attempt.state
                );
            }
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            if json_output {
                print_json(&json!({
                    "stop_requested": true,
                    "stop_confirmed": false,
                    "attempt_id": attempt_id,
                    "attempt_state": attempt.state,
                }))?;
            } else {
                println!("Stop request recorded for Writer attempt #{attempt_id}.");
                println!(
                    "The Writer has not confirmed shutdown yet; inspect `talos status` and `talos task recover` before any retry."
                );
            }
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
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

async fn run_task_review(
    json_output: bool,
    project: &Project,
    database: &mut Database,
    task_id: i64,
) -> Result<()> {
    let report = execute_critic_review(project, database, task_id).await?;
    print_critic_report(json_output, &report)?;
    Ok(())
}

#[derive(Serialize)]
struct VerificationReport {
    run_id: i64,
    task_id: i64,
    critic_review_id: i64,
    revision_id: String,
    status: String,
    task_status: &'static str,
    checks: Vec<CheckExecution>,
}

async fn run_task_verification(
    json_output: bool,
    project: &Project,
    database: &mut Database,
    task_id: i64,
) -> Result<()> {
    let task = database
        .list_tasks()?
        .into_iter()
        .find(|task| task.id == task_id)
        .with_context(|| format!("Task #{task_id} does not exist"))?;
    if task.status != crate::db::TaskStatus::Verifying {
        bail!("Task #{task_id} must have an approved Critic review before verification");
    }
    let checks = database.task_verification_checks(task_id)?;
    if checks.is_empty() {
        bail!("Task #{task_id} has no saved project checks; it cannot be completed");
    }
    if policy::authorize(ExecutionMode::Auto, PolicyAction::ProjectCheck, true)
        != Authorization::Allow
    {
        bail!("Project-check policy denied the saved verification action");
    }
    let review = database
        .latest_critic_review(task_id)?
        .with_context(|| format!("Task #{task_id} has no Critic review"))?;
    if review.decision != "approve" {
        bail!("Task #{task_id} does not have an approved Critic review");
    }
    let before = RevisionSnapshot::capture(project.root.as_path(), &review.base_revision)?;
    let facts = before.facts();
    let now = now_ms()?;
    let owner_id = format!("talos-task-verify-{}-{now}", std::process::id());
    let lease_duration_ms = i64::try_from(SUPERVISOR_LEASE.as_millis())
        .context("Supervisor lease duration is out of range")?;
    let lease = database
        .acquire_supervisor_lease(&owner_id, now, lease_duration_ms)?
        .context("Another TALOS supervisor currently owns the project lease")?;

    let verification = async {
        let run = database.begin_verification_run(task_id, review.id, &facts, &lease)?;
        let started = tokio::time::Instant::now();
        let mut results = Vec::with_capacity(checks.len());
        let mut interrupted = false;
        for check in &checks {
            let elapsed = started.elapsed();
            let remaining = VERIFICATION_BUDGET.saturating_sub(elapsed);
            let result = if interrupted {
                failed_check_execution(check, "interrupted")
            } else if remaining.is_zero() {
                failed_check_execution(check, "verification_budget_exhausted")
            } else {
                match run_project_check(&project.root, check, remaining).await {
                    Ok(result) => result,
                    Err(_) => failed_check_execution(check, "check_runner_failed"),
                }
            };
            interrupted |= result.failure_code.as_deref() == Some("interrupted");
            database.record_verification_result(run.id, &result, &lease)?;
            results.push(result);
        }
        let snapshot_matches = RevisionSnapshot::capture(&project.root, &facts.base_commit)
            .map(|snapshot| snapshot.facts() == facts)
            .unwrap_or(false);
        let completed = database.finish_verification_run(run.id, snapshot_matches, &lease)?;
        Ok::<_, anyhow::Error>((completed, results))
    }
    .await;
    let lease_released = database.release_supervisor_lease(&lease)?;
    if !lease_released {
        bail!("TALOS lost its supervisor lease while running project checks");
    }
    let (run, checks) = verification?;
    let report = VerificationReport {
        run_id: run.id,
        task_id: run.task_id,
        critic_review_id: run.critic_review_id,
        revision_id: run.revision_id,
        status: run.status.clone(),
        task_status: if run.status == "passed" {
            "complete"
        } else {
            "verifying"
        },
        checks,
    };
    if json_output {
        print_json(&report)?;
    } else {
        println!("Verification run #{} {}.", report.run_id, report.status);
        println!("Task #{} is {}.", report.task_id, report.task_status);
        for check in &report.checks {
            println!(
                "Check {}: version={} version_exit={} version_ms={} exit={} duration_ms={}{}",
                check.check_id,
                check.tool_version.as_deref().unwrap_or("unavailable"),
                check
                    .version_exit_code
                    .map_or_else(|| "none".to_owned(), |code| code.to_string()),
                check
                    .version_duration_ms
                    .map_or_else(|| "none".to_owned(), |millis| millis.to_string()),
                check
                    .exit_code
                    .map_or_else(|| "none".to_owned(), |code| code.to_string()),
                check
                    .duration_ms
                    .map_or_else(|| "none".to_owned(), |millis| millis.to_string()),
                if check.passed() {
                    " (passed)"
                } else {
                    " (failed)"
                },
            );
        }
    }
    if run.status != "passed" {
        bail!("Verification did not pass for the approved revision; task remains open");
    }
    Ok(())
}

fn failed_check_execution(check: &ProjectCheck, failure_code: &str) -> CheckExecution {
    const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    CheckExecution {
        check_id: check.id.clone(),
        argv: check.argv.clone(),
        version_argv: check.version_argv.clone(),
        working_directory: check.working_directory.clone(),
        tool_version: None,
        version_duration_ms: None,
        version_exit_code: None,
        duration_ms: None,
        exit_code: None,
        timed_out: false,
        failure_code: Some(failure_code.to_owned()),
        stdout_sha256: EMPTY_SHA256.to_owned(),
        stderr_sha256: EMPTY_SHA256.to_owned(),
    }
}

async fn execute_critic_review(
    project: &Project,
    database: &mut Database,
    task_id: i64,
) -> Result<crate::supervisor::CriticReviewReport> {
    let config = ProjectConfig::load(project)?;
    match config.adapter_for(AgentRole::Critic) {
        AdapterKind::Agy => {}
    }
    let probe = AgyProbe::inspect(&project.root);
    if probe.status.as_str() != "available" {
        bail!(
            "Configured Antigravity Critic is unavailable: {}",
            probe.note.as_deref().unwrap_or("capability probe failed")
        );
    }
    let version = probe
        .version
        .clone()
        .context("Antigravity did not report a version")?;
    if version != VERIFIED_AGY_CRITIC_VERSION {
        bail!(
            "Critic execution is enabled only for the read-only-boundary-tested Antigravity version {VERIFIED_AGY_CRITIC_VERSION}; found {version}"
        );
    }
    if !has_required_writer_capabilities(&probe) {
        bail!("Antigravity no longer advertises the required headless, JSON, or sandbox flags");
    }
    let executable = probe
        .executable
        .context("Antigravity executable path is unavailable")?;
    let now = now_ms()?;
    let owner_id = format!("talos-task-review-{task_id}-{}-{now}", std::process::id());
    let lease_duration_ms = i64::try_from(SUPERVISOR_LEASE.as_millis())
        .context("Supervisor lease duration is out of range")?;
    let lease = database
        .acquire_supervisor_lease(&owner_id, now, lease_duration_ms)?
        .context("Another TALOS supervisor currently owns the project lease")?;
    let review = run_critic_review(
        project,
        database,
        task_id,
        executable,
        &version,
        WRITER_TIMEOUT,
    )
    .await;
    let lease_released = database.release_supervisor_lease(&lease)?;
    if !lease_released {
        bail!("TALOS lost its supervisor lease while running the Critic");
    }
    review
}

async fn execute_writer_task(
    _json_output: bool,
    project: &Project,
    database: &mut Database,
    task_id: i64,
    continuation: Option<WriterContinuation>,
) -> Result<crate::supervisor::WriterExecutionReport> {
    let config = ProjectConfig::load(project)?;
    match config.adapter_for(AgentRole::Writer) {
        AdapterKind::Agy => {}
    }
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

fn print_critic_report(
    json_output: bool,
    report: &crate::supervisor::CriticReviewReport,
) -> Result<()> {
    let task_status = if report.review.decision == "approve" {
        "verifying"
    } else {
        "implementing"
    };
    let output = json!({
        "task_id": report.review.task_id,
        "review_id": report.review.id,
        "writer_attempt_id": report.review.writer_attempt_id,
        "base_revision": report.review.base_revision,
        "revision_id": report.review.revision_id,
        "decision": report.review.decision,
        "task_status": task_status,
        "summary": report.review.summary,
        "findings": report.findings,
        "verification_ran": false,
    });
    if json_output {
        print_json(&output)?;
    } else {
        println!(
            "Critic review #{} {} for revision {}.",
            report.review.id, report.review.decision, report.review.revision_id
        );
        println!("Task #{} is now {task_status}.", report.review.task_id);
        println!("{}", redact_sensitive(&report.review.summary));
        for finding in &report.findings {
            let line = finding
                .line
                .map(|line| format!(":{line}"))
                .unwrap_or_default();
            println!(
                "{:?} {}{} — {}: {}",
                finding.severity,
                redact_sensitive(&finding.path),
                line,
                redact_sensitive(&finding.title),
                redact_sensitive(&finding.reason)
            );
        }
        if report.review.decision == "approve" {
            println!("Project verification has not run; the task is not complete.");
        }
    }
    Ok(())
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
            println!("Writer response: {}", redact_sensitive(response));
        }
        if let Some(changed_files) = changed_files {
            if changed_files.is_empty() {
                println!("Git reports no changed files.");
            } else {
                println!("Changed files:");
                for path in changed_files {
                    println!("  {}", redact_sensitive(path));
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
    if policy::authorize(ExecutionMode::Auto, PolicyAction::WriterLaunch, true)
        != Authorization::Allow
    {
        bail!("Auto policy denied the configured Writer action");
    }
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

async fn run_one_shot_semi(
    json_output: bool,
    project: &Project,
    database: &mut Database,
) -> Result<()> {
    if policy::authorize(ExecutionMode::Semi, PolicyAction::WriterLaunch, true)
        != Authorization::RequireOperatorApproval
    {
        bail!("Semi policy denied the configured Writer action");
    }
    if !matches!(&project.discovery, DiscoveryMethod::Git) {
        bail!("`up --semi` requires a Git project so TALOS can report actual changed files");
    }
    validate_project_scope(project)?;
    let task = database
        .list_tasks()?
        .into_iter()
        .find(|task| task.status == crate::db::TaskStatus::Queued);
    let Some(task) = task else {
        if json_output {
            print_json(&json!({
                "mode": "semi",
                "result": "no_queued_tasks",
                "writer_launched": false,
            }))?;
        } else {
            println!("No queued tasks; no Writer was started.");
        }
        return Ok(());
    };
    let before = git_status_paths(project)?;
    if !before.is_empty() {
        bail!(
            "`up --semi` requires a clean Git worktree; inspect or commit these existing paths first: {}",
            before.join(", ")
        );
    }
    request_operator_approval(&format!(
        "Start the configured Writer for task #{} ({})?",
        task.id, task.title
    ))?;
    let execution = execute_writer_task(json_output, project, database, task.id, None).await;
    let changed_files = git_status_paths(project)?;
    let report = execution?;
    print_writer_report(json_output, &report, Some(&changed_files))?;
    ensure_writer_succeeded(&report)
}

fn request_operator_approval(action: &str) -> Result<()> {
    if !io::stdin().is_terminal() {
        bail!("Semi mode requires an interactive terminal; no action was approved");
    }
    eprint!("{} [y/N] ", redact_sensitive(action));
    io::stderr()
        .flush()
        .context("Cannot print approval prompt")?;
    let mut answer = String::new();
    io::stdin()
        .read_line(&mut answer)
        .context("Cannot read operator approval")?;
    if !matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
        bail!("Operator did not approve the action");
    }
    Ok(())
}

fn ensure_talos_ignored(project: &Project) -> Result<()> {
    if !matches!(&project.discovery, DiscoveryMethod::Git) {
        return Ok(());
    }
    let tracked = git_command(project, &["ls-files", "--", ".talos"])?;
    if !tracked.status.success() {
        bail!("Cannot inspect whether .talos is tracked by Git");
    }
    if !tracked.stdout.is_empty() {
        bail!(
            ".talos contains tracked files; remove them from Git before TALOS creates local runtime state"
        );
    }
    let ignored = git_command(
        project,
        &[
            "check-ignore",
            "--no-index",
            "-q",
            "--",
            ".talos/state.sqlite3",
        ],
    )?;
    match ignored.status.code() {
        Some(0) => Ok(()),
        Some(1) => bail!(
            "TALOS runtime path .talos/state.sqlite3 is not ignored by Git; add .talos/ to the project .gitignore and retry"
        ),
        _ => bail!("Git could not determine whether .talos/state.sqlite3 is ignored"),
    }
}

fn git_command(project: &Project, arguments: &[&str]) -> Result<std::process::Output> {
    let mut command = ProcessCommand::new("git");
    command
        .args(arguments)
        .current_dir(&project.root)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0");
    for (key, _) in env::vars_os().filter(|(key, _)| key.to_string_lossy().starts_with("GIT_")) {
        command.env_remove(key);
    }
    command
        .output()
        .context("Cannot run Git while checking project runtime exclusions")
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
    let lease = acquire_recovery_lease(database, &owner_id, now, lease_duration_ms)?;

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

fn acquire_recovery_lease(
    database: &mut Database,
    owner_id: &str,
    now: i64,
    duration_ms: i64,
) -> Result<crate::db::SupervisorLease> {
    if let Some(lease) = database.acquire_supervisor_lease(owner_id, now, duration_ms)? {
        return Ok(lease);
    }

    #[cfg(unix)]
    {
        let current = database
            .current_supervisor_lease()?
            .context("Another TALOS supervisor currently owns the project lease")?;
        let owner_pid = talos_lease_owner_pid(&current.owner_id)
            .context("Another TALOS supervisor currently owns the project lease")?;
        if process_id_is_alive(owner_pid)? {
            bail!(
                "Another TALOS supervisor process is still alive; recovery will not steal its lease"
            );
        }
        if !database.expire_supervisor_lease_if_matches(&current, now)? {
            bail!("The supervisor lease changed while checking its owner; retry recovery");
        }
        return database
            .acquire_supervisor_lease(owner_id, now, duration_ms)?
            .context("Another TALOS supervisor acquired the project lease during recovery");
    }

    #[cfg(not(unix))]
    {
        bail!("Another TALOS supervisor currently owns the project lease")
    }
}

fn talos_lease_owner_pid(owner_id: &str) -> Option<u32> {
    if !owner_id.starts_with("talos-task-") {
        return None;
    }
    let process_id = owner_id.rsplit('-').nth(1)?.parse::<u32>().ok()?;
    (process_id > 0).then_some(process_id)
}

#[cfg(unix)]
fn process_id_is_alive(process_id: u32) -> Result<bool> {
    let process_id = i32::try_from(process_id).context("Supervisor process ID is out of range")?;
    if unsafe { libc::kill(process_id, 0) } == 0 {
        return Ok(true);
    }
    match std::io::Error::last_os_error().raw_os_error() {
        Some(libc::ESRCH) => Ok(false),
        Some(libc::EPERM) => Ok(true),
        _ => bail!("Cannot determine whether the prior TALOS supervisor process is alive"),
    }
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
    println!(
        "#{} [{}] {}",
        task.id,
        task.status.as_str(),
        redact_sensitive(&task.title)
    );
    if !task.acceptance_criteria.is_empty() {
        println!(
            "  Acceptance criteria: {}",
            redact_sensitive(&task.acceptance_criteria)
        );
    }
}

fn print_event(event: &TaskEvent) {
    println!(
        "{} task #{} {} by {}: {}",
        event.occurred_at_ms,
        event.task_id,
        redact_sensitive(&event.event_type),
        redact_sensitive(&event.actor),
        redact_sensitive(&event.reason)
    );
}

fn print_project_control_event(event: &ProjectControlEvent) {
    println!(
        "{} project {}: {}",
        event.occurred_at_ms,
        redact_sensitive(&event.event_type),
        redact_sensitive(&event.reason)
    );
}

fn format_task_ids(task_ids: &[i64]) -> String {
    task_ids
        .iter()
        .map(|task_id| format!("#{task_id}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn print_json(value: &impl Serialize) -> Result<()> {
    let mut value = serde_json::to_value(value).context("Cannot serialize JSON output")?;
    redact_json_value(&mut value);
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}

fn redact_json_value(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::String(text) => *text = redact_sensitive(text),
        serde_json::Value::Array(values) => {
            for value in values {
                redact_json_value(value);
            }
        }
        serde_json::Value::Object(values) => {
            for value in values.values_mut() {
                redact_json_value(value);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        process::Command,
        sync::atomic::{AtomicU64, Ordering},
    };

    use super::{
        ResolveAction, SUPERVISOR_LEASE, acquire_recovery_lease, ensure_talos_ignored, now_ms,
        recover_writer_attempts, resolve_writer_attempt, run_task_verification,
    };
    use crate::{
        checks::ProjectCheck,
        db::{CriticReviewInput, Database, TaskStatus, WriterAttemptOutcome},
        project::{DiscoveryMethod, Project},
        revision::{RevisionSnapshot, clean_base_commit},
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

    fn init_git(root: &std::path::Path) {
        let output = Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(root)
            .output()
            .expect("run git init");
        assert!(output.status.success(), "git init succeeds");
    }

    fn git(root: &std::path::Path, arguments: &[&str]) -> String {
        let output = Command::new("git")
            .args(arguments)
            .current_dir(root)
            .output()
            .expect("run Git command");
        assert!(
            output.status.success(),
            "Git command succeeds: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout)
            .expect("Git output is UTF-8")
            .trim()
            .to_owned()
    }

    #[test]
    fn git_project_requires_talos_ignore_before_creating_runtime_state() {
        let project = temp_project();
        init_git(&project.root);
        let git_project = Project {
            root: project.root.clone(),
            discovery: DiscoveryMethod::Git,
        };
        let error = ensure_talos_ignored(&git_project).expect_err("missing ignore must fail");
        assert!(format!("{error:#}").contains("add .talos/ to the project .gitignore"));
        assert!(!project.root.join(".talos").exists());

        fs::write(project.root.join(".gitignore"), ".talos/\n").expect("add ignore rule");
        ensure_talos_ignored(&git_project).expect("ignored TALOS state is allowed");
        assert!(!project.root.join(".talos").exists());
        fs::remove_dir_all(&project.root).expect("remove temporary Git project");
    }

    #[test]
    fn git_project_refuses_already_tracked_talos_state() {
        let project = temp_project();
        init_git(&project.root);
        fs::write(project.root.join(".gitignore"), ".talos/\n").expect("add ignore rule");
        fs::create_dir_all(project.root.join(".talos")).expect("create tracked TALOS folder");
        fs::write(project.root.join(".talos/tracked.txt"), "tracked").expect("write tracked file");
        let staged = Command::new("git")
            .args(["add", "-f", ".talos/tracked.txt"])
            .current_dir(&project.root)
            .status()
            .expect("stage tracked file");
        assert!(staged.success());
        let git_project = Project {
            root: project.root.clone(),
            discovery: DiscoveryMethod::Git,
        };
        let error = ensure_talos_ignored(&git_project).expect_err("tracked runtime must fail");
        assert!(format!("{error:#}").contains("contains tracked files"));
        fs::remove_dir_all(&project.root).expect("remove temporary Git project");
    }

    #[tokio::test]
    async fn verification_runs_saved_command_and_completes_only_matching_approved_revision() {
        let project = temp_project();
        fs::write(project.root.join(".gitignore"), ".talos/\n").expect("ignore TALOS runtime");
        fs::create_dir_all(project.root.join("src")).expect("create source directory");
        fs::write(
            project.root.join("src/lib.rs"),
            "pub fn value() -> u8 { 1 }\n",
        )
        .expect("write baseline source");
        init_git(&project.root);
        git(&project.root, &["add", ".gitignore", "src/lib.rs"]);
        git(
            &project.root,
            &[
                "-c",
                "user.name=TALOS",
                "-c",
                "user.email=talos@example.invalid",
                "commit",
                "-m",
                "baseline",
            ],
        );
        let base = clean_base_commit(&project.root).expect("capture clean Git base");
        fs::write(
            project.root.join("src/lib.rs"),
            "pub fn value() -> u8 { 2 }\n",
        )
        .expect("write task change");

        let mut database = Database::open(&project).expect("initialize task database");
        let check = ProjectCheck {
            id: "tool-version".to_owned(),
            argv: vec!["cargo".to_owned(), "--version".to_owned()],
            version_argv: vec!["cargo".to_owned(), "--version".to_owned()],
            working_directory: ".".to_owned(),
        };
        let task = database
            .add_task_with_checks("Verify task", "The configured check exits zero", &[check])
            .expect("create task with one required check");
        let writer = database
            .begin_writer_attempt(task.id, &"f".repeat(64))
            .expect("begin Writer attempt");
        database
            .record_writer_base_revision(writer.id, &base)
            .expect("record Writer base");
        database
            .finish_writer_attempt(
                writer.id,
                WriterAttemptOutcome::Succeeded,
                None,
                Some(0),
                Some("SUCCESS"),
                None,
            )
            .expect("finish Writer attempt");
        let snapshot =
            RevisionSnapshot::capture(&project.root, &base).expect("capture review revision");
        let revision_json = serde_json::to_string(&snapshot.changed_paths)
            .expect("serialize changed project paths");
        let critic_result = format!(
            r#"{{"decision":"approve","revision_id":"{}"}}"#,
            snapshot.revision_id
        );
        database
            .record_critic_review(CriticReviewInput {
                task_id: task.id,
                writer_attempt_id: writer.id,
                base_revision: &snapshot.base_commit,
                head_commit: &snapshot.head_commit,
                revision_id: &snapshot.revision_id,
                patch_sha256: &snapshot.patch_sha256,
                changed_paths_json: &revision_json,
                decision: "approve",
                summary: "The project check is saved for verification.",
                result_json: &critic_result,
                adapter: "agy",
                adapter_version: "1.5.10",
            })
            .expect("record approval for exact revision");

        run_task_verification(false, &project, &mut database, task.id)
            .await
            .expect("run saved project check");
        assert_eq!(
            database.list_tasks().expect("read completed task")[0].status,
            TaskStatus::Complete
        );
        let events = database
            .task_events(Some(task.id))
            .expect("read verification events");
        assert!(
            events
                .iter()
                .any(|event| event.event_type == "verification.started")
        );
        assert!(
            events
                .iter()
                .any(|event| event.event_type == "verification.finished")
        );
        drop(database);
        fs::remove_dir_all(&project.root).expect("remove temporary verification project");
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

    #[cfg(unix)]
    #[test]
    fn recovery_reclaims_a_lease_when_its_recorded_supervisor_pid_is_gone() {
        let project = temp_project();
        let mut database = Database::open(&project).expect("initialize database");
        let mut owner_process = Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("start temporary lease owner process");
        let now = now_ms().expect("read time");
        let duration_ms = i64::try_from(SUPERVISOR_LEASE.as_millis())
            .expect("lease duration fits in SQLite integer");
        let old_owner_id = format!("talos-task-run-{}-{now}", owner_process.id());
        let old_lease = database
            .acquire_supervisor_lease(&old_owner_id, now, duration_ms)
            .expect("acquire old supervisor lease")
            .expect("old lease is available");
        owner_process.kill().expect("kill temporary lease owner");
        owner_process.wait().expect("reap temporary lease owner");

        let recovery_now = now_ms().expect("read recovery time");
        let new_owner_id = format!("talos-task-recover-{}-{recovery_now}", std::process::id());
        let recovered =
            acquire_recovery_lease(&mut database, &new_owner_id, recovery_now, duration_ms)
                .expect("reclaim a lease only after its owner process is absent");
        assert_ne!(recovered.owner_id, old_lease.owner_id);
        assert_eq!(recovered.fencing_token, old_lease.fencing_token + 1);
        assert!(database.release_supervisor_lease(&recovered).unwrap());
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
