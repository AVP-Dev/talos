use std::{
    env,
    io::{self, IsTerminal, Read, Write},
    path::{Path, PathBuf},
    process::Command as ProcessCommand,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde_json::json;
use tokio::time::{Instant, sleep};

use sha2::{Digest, Sha256};

use crate::{
    adapter::agy::AgyProbe,
    checks::{CheckExecution, ProjectCheck, run_project_check, validate_selected_checks},
    cli::{
        Cli, Command, ConfigCommand, GithubBranchCommand, GithubCommand, ResolveAction,
        TaskCommand, TaskWorktreeCommand,
    },
    config::{AdapterKind, AgentRole, GithubPolicy, ProjectConfig},
    db::{
        Database, GithubCiCheckRecord, GithubCiSnapshotInput, GithubPublicationInput,
        GithubPublicationRecord, ProjectControlEvent, QueueSummary, QueueWaitSummary,
        ResumeProjectOutcome, StopRequestOutcome, TaskEvent, TaskRecord, TaskSummaryRecord,
        WriterResolutionAction, WriterResolutionRecord,
    },
    github::{self, GithubProbe},
    policy::{self, Action as PolicyAction, Authorization, ExecutionMode},
    project::{DiscoveryMethod, Project},
    revision::{RevisionSnapshot, clean_base_commit, git_head, git_output_has_any_bytes},
    secrets::{contains_sensitive, redact_sensitive},
    supervisor::{
        WriterContinuation, WriterRunOptions, continue_writer_attempt_with_options,
        run_critic_review, run_writer_attempt_with_options, validate_project_scope,
    },
    tmux::{TmuxClient, TmuxProbe},
    worktree,
};

const WRITER_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const SUPERVISOR_LEASE: Duration = Duration::from_secs(35 * 60);
const VERIFICATION_BUDGET: Duration = Duration::from_secs(25 * 60);
const MAX_CHANGED_PATHS_PREVIEW: usize = 50;
const MAX_CHANGED_PATH_PREVIEW_BYTES: usize = 256;

pub(crate) struct GitStatusPaths {
    pub total_count: usize,
    pub preview: Vec<String>,
    pub preview_values_truncated: bool,
}

impl GitStatusPaths {
    pub fn is_empty(&self) -> bool {
        self.total_count == 0
    }

    pub(crate) fn display_preview(&self) -> String {
        let mut output = self
            .preview
            .iter()
            .map(|path| redact_sensitive(path))
            .collect::<Vec<_>>()
            .join(", ");
        if self.preview.len() < self.total_count {
            if !output.is_empty() {
                output.push_str(", ");
            }
            output.push_str(&format!(
                "and {} more",
                self.total_count - self.preview.len()
            ));
        }
        if self.preview_values_truncated {
            output.push_str(" (some path names are shortened)");
        }
        format!("{} path(s): {output}", self.total_count)
    }
}

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
    talos_executable: PathBuf,
    talos_version: &'static str,
    project_root: String,
    discovery: crate::project::DiscoveryMethod,
    database_path: String,
    schema_version: i64,
    journal_mode: String,
    agent_execution: String,
    writer_adapter: &'static str,
    critic_adapter: &'static str,
    orchestration_mode: &'static str,
    delegation: &'static str,
    adapter_profiles: Vec<AdapterCapabilityReport>,
    project_checks: Vec<String>,
    tmux: TmuxProbe,
    antigravity: AgyProbe,
    github: GithubProbe,
    git: &'static str,
}

#[derive(Serialize)]
struct AdapterCapabilityReport {
    id: &'static str,
    detection: &'static str,
    implementation: &'static str,
    writer: &'static str,
    critic: &'static str,
}

struct GithubPublishRequest<'a> {
    branch: &'a str,
    title: &'a str,
    body: &'a str,
    confirm_push: bool,
    confirm_pr: bool,
}

#[derive(Clone, Copy)]
struct GithubReconcileOptions<'a> {
    create_pr: bool,
    title: Option<&'a str>,
    body: Option<&'a str>,
}

pub async fn run(cli: Cli) -> Result<()> {
    let implicit_workspace = cli.command.is_none() && !cli.tmux;
    let command = match cli.command {
        Some(cmd) => cmd,
        None => {
            if cli.json {
                Command::Status
            } else if cli.tmux
                || (io::stdout().is_terminal() && TmuxClient::default().probe().available)
            {
                Command::Attach
            } else {
                Command::Tui
            }
        }
    };

    if let Command::Github(arguments) = &command {
        match &arguments.command {
            GithubCommand::Status { check_auth } => {
                run_github_status(cli.json, *check_auth).await?;
                return Ok(());
            }
            GithubCommand::Policy
            | GithubCommand::Branch { .. }
            | GithubCommand::Publish { .. }
            | GithubCommand::Reconcile { .. }
            | GithubCommand::Ci { .. } => {}
        }
    }

    let working_directory = env::current_dir().context("Cannot read current directory")?;
    let project = Project::discover(&working_directory)?;
    if implicit_workspace
        && matches!(&command, Command::Attach)
        && !matches!(project.discovery, DiscoveryMethod::Git)
    {
        return crate::tui::run_tui(project).await;
    }

    if let Some(cli_name) = &cli.cli {
        let adapter = crate::adapter::AdapterId::parse(cli_name).ok_or_else(|| {
            anyhow::anyhow!(
                "Unknown CLI adapter '{cli_name}'. Valid adapters: agy, codex, claude, omp, pi, kilo, opencode"
            )
        })?;
        if let Ok(mut cfg) = ProjectConfig::load(&project) {
            cfg.writer = adapter;
            cfg.critic = adapter;
            let _ = cfg.save(&project);
        }
    }

    if let Command::Github(arguments) = &command {
        match &arguments.command {
            GithubCommand::Policy => {
                ensure_talos_ignored(&project)?;
                run_github_policy(cli.json, &project)?;
                return Ok(());
            }
            GithubCommand::Branch {
                command: GithubBranchCommand::Create { slug },
            } => {
                ensure_talos_ignored(&project)?;
                create_github_branch(cli.json, &project, slug)?;
                return Ok(());
            }
            GithubCommand::Status { .. } => {
                bail!("GitHub status was not dispatched before project discovery");
            }
            GithubCommand::Publish { .. }
            | GithubCommand::Reconcile { .. }
            | GithubCommand::Ci { .. } => {}
        }
    }

    if matches!(
        &command,
        Command::Task(arguments)
            if matches!(
                &arguments.command,
                TaskCommand::Run { .. } | TaskCommand::Continue { .. } | TaskCommand::Review { .. }
                    | TaskCommand::Verify { .. }
            )
    ) {
        validate_project_scope(&project)?;
    }

    if matches!(&command, Command::Attach) {
        let tmux = TmuxClient::default();
        let probe = tmux.probe();
        if !probe.available {
            bail!("tmux is unavailable; use TALOS foreground commands or install tmux");
        }
        let root = project.root.canonicalize()?;
        let expected_session = crate::tmux::project_session_name(&root);
        if !tmux.has_session(&expected_session)? {
            // Prepare durable state before the compact pane starts. Do not
            // introduce a manual Writer while a managed task is active.
            ensure_talos_ignored(&project)?;
            let database = Database::open(&project)?;
            if database.queue_summary()?.active > 0 {
                bail!(
                    "A managed task is active; finish or reconcile it before opening a new native workspace"
                );
            }
        }
        if !tmux.has_session(&expected_session)?
            && !crate::config::config_path(&project.state_root).exists()
            && io::stdin().is_terminal()
            && !cli.json
            && !crate::workspace::run_settings(&project)?
        {
            return Ok(());
        }
        let config = ProjectConfig::load(&project)?;
        let session_name = tmux.ensure_pair_workspace(&project.root, &config)?;
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

    if matches!(&command, Command::Tui) {
        return crate::tui::run_tui(project).await;
    }
    if matches!(&command, Command::WorkspaceStatus) {
        return tokio::task::spawn_blocking(move || crate::workspace::run_status(project)).await?;
    }
    if matches!(&command, Command::Settings) {
        crate::workspace::run_settings(&project)?;
        return Ok(());
    }
    if let Command::RestartPane { role, confirm } = &command {
        if !confirm {
            bail!("Explicit confirmation is required to replace a pane");
        }
        crate::workspace::restart_role(&project, *role)?;
        println!("Selected native pane restarted");
        return Ok(());
    }

    ensure_talos_ignored(&project)?;

    let mut database = Database::open(&project)?;
    match command {
        Command::Up(arguments) if arguments.auto => {
            ensure_queue_running(&database)?;
            run_one_shot_auto(
                cli.json,
                &project,
                &mut database,
                arguments.mode,
                arguments.task,
                arguments.expected_config.as_deref(),
            )
            .await?;
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
            TaskCommand::Depends {
                task_id,
                prerequisite_task_id,
            } => {
                let dependency_added =
                    database.add_task_dependency(task_id, prerequisite_task_id)?;
                if cli.json {
                    print_json(&json!({
                        "task_id": task_id,
                        "prerequisite_task_id": prerequisite_task_id,
                        "dependency_added": dependency_added,
                    }))?;
                } else {
                    if dependency_added {
                        println!(
                            "Task #{task_id} now waits for task #{prerequisite_task_id} to complete."
                        );
                    } else {
                        println!("Task #{task_id} already waits for task #{prerequisite_task_id}.");
                    }
                }
            }
            TaskCommand::Dependencies {
                task_id,
                limit,
                after,
            } => {
                if database.get_task(task_id)?.is_none() {
                    bail!("Task #{task_id} does not exist");
                }
                let page = database.task_dependencies_page(task_id, after, limit)?;
                if cli.json {
                    print_json(&page)?;
                } else if page.dependencies.is_empty() && after == 0 {
                    println!("Task #{task_id} has no prerequisite tasks.");
                } else if page.dependencies.is_empty() {
                    println!("No more prerequisites for task #{task_id}.");
                } else {
                    for dependency in &page.dependencies {
                        println!(
                            "Task #{} requires task #{} ({}).",
                            dependency.task_id,
                            dependency.prerequisite_task_id,
                            dependency.prerequisite_status.as_str(),
                        );
                    }
                    if let Some(next_after) = page.next_after {
                        println!(
                            "More prerequisites are available: `talos task dependencies {task_id} --after {next_after} --limit {limit}`."
                        );
                    }
                }
            }
            TaskCommand::Child {
                parent_task_id,
                title,
                criteria,
                allowed_paths,
                timeout_seconds,
            } => {
                let timeout_ms = i64::try_from(timeout_seconds)
                    .context("Child timeout seconds are out of range")?
                    .checked_mul(1_000)
                    .context("Child timeout is out of range")?;
                let (task, contract) = database.add_child_task(
                    parent_task_id,
                    &title,
                    &criteria,
                    &allowed_paths,
                    timeout_ms,
                )?;
                if cli.json {
                    print_json(&json!({ "task": task, "contract": contract }))?;
                } else {
                    println!(
                        "Created child task #{} under parent #{} ({} allowed path(s), {} second timeout).",
                        task.id,
                        parent_task_id,
                        contract.allowed_paths.len(),
                        contract.timeout_ms / 1_000,
                    );
                    println!("Parent task #{parent_task_id} now waits for this child to complete.");
                }
            }
            TaskCommand::Children {
                parent_task_id,
                limit,
                after,
            } => {
                if database.get_task(parent_task_id)?.is_none() {
                    bail!("Parent task #{parent_task_id} does not exist");
                }
                let page = database.children_for_parent_page(parent_task_id, after, limit)?;
                if cli.json {
                    print_json(&page)?;
                } else if page.children.is_empty() && after == 0 {
                    println!("Task #{parent_task_id} has no child tasks.");
                } else if page.children.is_empty() {
                    println!("No more child tasks for task #{parent_task_id}.");
                } else {
                    for child in &page.children {
                        println!(
                            "Child #{}: allowed paths [{}], timeout {} seconds.",
                            child.child_task_id,
                            child.allowed_paths.join(", "),
                            child.timeout_ms / 1_000,
                        );
                    }
                    if let Some(next_after) = page.next_after {
                        println!(
                            "More child tasks are available: `talos task children {parent_task_id} --after {next_after} --limit {limit}`."
                        );
                    }
                }
            }
            TaskCommand::Handoff { task_id } => {
                print_task_handoff(cli.json, &database, task_id)?;
            }
            TaskCommand::ChildReport { task_id } => {
                print_child_task_report(cli.json, &database, task_id)?;
            }
            TaskCommand::Worktree { command } => match command {
                TaskWorktreeCommand::Create { task_id } => {
                    let record =
                        with_worktree_supervisor_lease(&mut database, "create", |database| {
                            worktree::create(&project, database, task_id)
                        })?;
                    print_task_worktree(cli.json, "created", &record, &database)?;
                }
                TaskWorktreeCommand::List { limit, after } => {
                    let page = worktree::list(&database, after, limit)?;
                    if cli.json {
                        print_json(&page)?;
                    } else if page.worktrees.is_empty() && after == 0 {
                        println!("No TALOS-managed task worktrees.");
                    } else if page.worktrees.is_empty() {
                        println!("No more TALOS-managed task worktrees.");
                    } else {
                        for record in &page.worktrees {
                            print_task_worktree(false, "", record, &database)?;
                        }
                        if let Some(next_after) = page.next_after {
                            println!(
                                "More worktrees are available: `talos task worktree list --after {next_after} --limit {limit}`."
                            );
                        }
                    }
                }
                TaskWorktreeCommand::Reconcile { task_id } => {
                    let record =
                        with_worktree_supervisor_lease(&mut database, "reconcile", |database| {
                            worktree::reconcile(&project, database, task_id)
                        })?;
                    print_task_worktree(cli.json, "reconciled", &record, &database)?;
                }
                TaskWorktreeCommand::Remove { task_id } => {
                    let record =
                        with_worktree_supervisor_lease(&mut database, "remove", |database| {
                            worktree::remove(&project, database, task_id)
                        })?;
                    print_task_worktree(cli.json, "retired", &record, &database)?;
                }
                TaskWorktreeCommand::Integrate { task_id } => {
                    let integration =
                        with_worktree_supervisor_lease(&mut database, "integrate", |database| {
                            worktree::integrate(&project, database, task_id)
                        })?;
                    print_task_worktree_integration(cli.json, "pending_review", &integration)?;
                }
                TaskWorktreeCommand::Finalize { task_id } => {
                    let integration =
                        with_worktree_supervisor_lease(&mut database, "finalize", |database| {
                            worktree::finalize(&project, database, task_id)
                        })?;
                    print_task_worktree_integration(cli.json, "integrated", &integration)?;
                }
            },
            TaskCommand::List { limit, after } => {
                let page = database.task_list_page(after, limit)?;
                if cli.json {
                    print_json(&page)?;
                } else if page.tasks.is_empty() {
                    if after == 0 {
                        println!("No tasks in the queue.");
                    } else {
                        println!("No tasks after ID {after}.");
                    }
                } else {
                    for task in &page.tasks {
                        print_task_summary(task);
                    }
                    if let Some(next_after) = page.next_after {
                        println!(
                            "More tasks are available: `talos task list --after {next_after}`."
                        );
                    }
                }
            }
            TaskCommand::Show { task_id } => {
                let task = database
                    .get_task(task_id)?
                    .with_context(|| format!("Task #{task_id} does not exist"))?;
                if cli.json {
                    print_json(&task)?;
                } else {
                    print_task(&task);
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
            TaskCommand::Evidence {
                task_id,
                limit,
                before,
            } => {
                if database.get_task(task_id)?.is_none() {
                    bail!("Task #{task_id} does not exist");
                }
                let page = database.verification_history_page(task_id, before, limit)?;
                if cli.json {
                    print_json(&verification_evidence_json(&page)?)?;
                } else if page.runs.is_empty() {
                    if before.is_some() {
                        println!("No older verification runs for task #{task_id}.");
                    } else {
                        println!("Task #{task_id} has no project verification history.");
                    }
                } else {
                    for evidence in &page.runs {
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
                        for check in &evidence.checks {
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
                    if let Some(next_before) = page.next_before {
                        println!(
                            "More evidence is available: `talos task evidence {task_id} --before {next_before} --limit {limit}`."
                        );
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
                let page =
                    database.project_control_events_page(arguments.before, arguments.limit)?;
                if cli.json {
                    print_json(&page)?;
                } else if page.events.is_empty() {
                    println!("No project control events.");
                } else {
                    for event in page.events {
                        print_project_control_event(&event);
                    }
                    if let Some(next_before) = page.next_before {
                        println!(
                            "More events are available: `talos logs --project --before {next_before}`."
                        );
                    }
                }
            } else {
                let page = database.task_events_page(
                    arguments.task_id,
                    arguments.before,
                    arguments.limit,
                )?;
                if cli.json {
                    print_json(&page)?;
                } else if page.events.is_empty() {
                    println!("No matching events.");
                } else {
                    for event in page.events {
                        print_event(&event);
                    }
                    if let Some(next_before) = page.next_before {
                        let task_filter = arguments
                            .task_id
                            .map(|task_id| format!(" {task_id}"))
                            .unwrap_or_default();
                        println!(
                            "More events are available: `talos logs{task_filter} --before {next_before}`."
                        );
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
                    reopened_task_count,
                    reopened_task_ids_preview,
                } => {
                    if cli.json {
                        print_json(&json!({
                            "queue_paused": false,
                            "reconciled_manual_changes": reconciled_manual_changes,
                            "reopened_task_count": reopened_task_count,
                            "reopened_task_ids_preview": reopened_task_ids_preview,
                            "reopened_task_ids_truncated": reopened_task_count > reopened_task_ids_preview.len(),
                        }))?;
                    } else {
                        if reconciled_manual_changes {
                            println!(
                                "Manual project changes reconciled; affected tasks require a fresh Critic review."
                            );
                            if reopened_task_count == 0 {
                                println!("No verifying tasks needed to be reopened.");
                            } else {
                                println!(
                                    "Reopened {} task(s); first IDs: {}",
                                    reopened_task_count,
                                    format_task_ids(&reopened_task_ids_preview)
                                );
                                if reopened_task_count > reopened_task_ids_preview.len() {
                                    println!(
                                        "ID list is truncated; use `talos task list` for task summaries."
                                    );
                                }
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
            let adapter_profiles = adapter_capability_reports(&antigravity, &project.root);
            let writer_adapter = project_config.adapter_for(AgentRole::Writer);
            let critic_adapter = project_config.adapter_for(AgentRole::Critic);
            let agent_execution = format!(
                "Writer {}: {}; Critic {}: {}",
                writer_adapter.as_str(),
                adapter_role_status(
                    writer_adapter,
                    AgentRole::Writer,
                    &antigravity,
                    &project.root
                ),
                critic_adapter.as_str(),
                adapter_role_status(
                    critic_adapter,
                    AgentRole::Critic,
                    &antigravity,
                    &project.root
                ),
            );
            let report = DoctorReport {
                talos_executable: env::current_exe()
                    .context("Cannot determine the current TALOS binary")?,
                talos_version: env!("CARGO_PKG_VERSION"),
                project_root: project.root.display().to_string(),
                discovery: project.discovery,
                database_path: database.path().display().to_string(),
                schema_version: database.schema_version()?,
                journal_mode: database.journal_mode()?,
                agent_execution,
                writer_adapter: writer_adapter.as_str(),
                critic_adapter: critic_adapter.as_str(),
                orchestration_mode: project_config.orchestration.mode.as_str(),
                delegation: project_config.orchestration.delegation.as_str(),
                adapter_profiles,
                project_checks: project_config
                    .checks
                    .iter()
                    .map(|check| check.id.clone())
                    .collect(),
                tmux: TmuxClient::default().probe(),
                antigravity,
                github: github::probe_local().await,
                git,
            };
            if cli.json {
                print_json(&report)?;
            } else {
                println!(
                    "TALOS {} · {}",
                    report.talos_version,
                    redact_sensitive(&report.talos_executable.display().to_string())
                );
                println!(
                    "Multiple installations? Use 'type -a talos' to see which copy your shell picks."
                );
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
                println!("Orchestration mode: {}", report.orchestration_mode);
                println!("Delegation mode: {}", report.delegation);
                println!("Adapter profiles:");
                for profile in &report.adapter_profiles {
                    println!(
                        "  {}: detected={}, implementation={}, Writer={}, Critic={}",
                        profile.id,
                        profile.detection,
                        profile.implementation,
                        profile.writer,
                        profile.critic
                    );
                }
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
                match (report.github.status, &report.github.version) {
                    ("available", Some(version)) => {
                        println!("GitHub CLI: {version}; authentication not checked")
                    }
                    ("unavailable", _) => println!("GitHub CLI: unavailable"),
                    ("timeout", _) => println!("GitHub CLI: version probe timed out"),
                    _ => println!("GitHub CLI: local probe failed"),
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
                println!(
                    "  Native subagents: {}",
                    report.antigravity.capabilities.native_subagents.as_str()
                );
                if let Some(note) = &report.antigravity.note {
                    println!("  Note: {}", redact_sensitive(note));
                }
            }
        }
        Command::Attach => bail!("Attach command was not dispatched before runtime initialization"),
        Command::Github(arguments) => match arguments.command {
            GithubCommand::Publish {
                branch,
                title,
                body,
                confirm_push,
                confirm_pr,
            } => {
                run_github_publish(
                    cli.json,
                    &project,
                    &mut database,
                    GithubPublishRequest {
                        branch: &branch,
                        title: &title,
                        body: &body,
                        confirm_push,
                        confirm_pr,
                    },
                )
                .await?;
            }
            GithubCommand::Reconcile {
                publication_id,
                create_pr,
                title,
                body,
            } => {
                run_github_reconcile(
                    cli.json,
                    &project,
                    &mut database,
                    publication_id,
                    GithubReconcileOptions {
                        create_pr,
                        title: title.as_deref(),
                        body: body.as_deref(),
                    },
                )
                .await?;
            }
            GithubCommand::Ci {
                publication_id,
                wait,
                timeout_seconds,
                interval_seconds,
            } => {
                run_github_ci(
                    cli.json,
                    &project,
                    &mut database,
                    publication_id,
                    wait,
                    timeout_seconds,
                    interval_seconds,
                )
                .await?;
            }
            _ => bail!("GitHub command was not dispatched before runtime initialization"),
        },
        Command::Config(arguments) => {
            let mut config = ProjectConfig::load(&project)?;
            match arguments.command {
                None | Some(ConfigCommand::Show) => {
                    if cli.json {
                        print_json(&config)?;
                    } else {
                        println!("TALOS Project Configuration (.talos/config.json):");
                        println!(
                            "  Writer:         {} (generates code and tests in worktrees)",
                            config.writer.as_str()
                        );
                        if let Some(model) = &config.writer_model {
                            println!("  Writer Model:   {model}");
                        }
                        println!(
                            "  Critic:         {} (independent read-only auditor)",
                            config.critic.as_str()
                        );
                        if let Some(model) = &config.critic_model {
                            println!("  Critic Model:   {model}");
                        }
                        println!(
                            "  Mode:           {} ({})",
                            config.orchestration.mode.as_str(),
                            if config.orchestration.mode == crate::config::OrchestrationMode::Team {
                                "two-tier team with parallel worktrees and lead integration"
                            } else {
                                "sequential Writer -> Critic -> Verify pipeline"
                            }
                        );
                        println!("  Max Writers:    {}", config.max_concurrent_writers);
                        println!("  Checks:         {} configured", config.checks.len());
                    }
                }
                Some(ConfigCommand::Set { key, value }) => {
                    match key.to_ascii_lowercase().as_str() {
                        "cli" => {
                            let adapter = crate::adapter::AdapterId::parse(&value).ok_or_else(|| {
                                anyhow::anyhow!(
                                    "Unknown CLI adapter '{value}'. Supported adapters: agy, codex, claude, omp, pi, kilo, opencode"
                                )
                            })?;
                            config.writer = adapter;
                            config.critic = adapter;
                        }
                        "writer" => {
                            let adapter = crate::adapter::AdapterId::parse(&value).ok_or_else(|| {
                                anyhow::anyhow!(
                                    "Unknown writer adapter '{value}'. Supported adapters: agy, codex, claude, omp, pi, kilo, opencode"
                                )
                            })?;
                            config.writer = adapter;
                        }
                        "critic" => {
                            let adapter = crate::adapter::AdapterId::parse(&value).ok_or_else(|| {
                                anyhow::anyhow!(
                                    "Unknown critic adapter '{value}'. Supported adapters: agy, codex, claude, omp, pi, kilo, opencode"
                                )
                            })?;
                            config.critic = adapter;
                        }
                        "mode" => match value.to_ascii_lowercase().as_str() {
                            "adaptive" => {
                                config.orchestration.mode =
                                    crate::config::OrchestrationMode::Adaptive
                            }
                            "team" => {
                                config.orchestration.mode = crate::config::OrchestrationMode::Team
                            }
                            "pair" => {
                                config.orchestration.mode = crate::config::OrchestrationMode::Pair
                            }
                            _ => bail!(
                                "Unknown mode '{value}'. Supported modes: adaptive, team, pair"
                            ),
                        },
                        "max-writers" | "max_writers" => {
                            let n: usize = value
                                .parse()
                                .context("max-writers must be a number between 1 and 3")?;
                            if !(1..=3).contains(&n) {
                                bail!("max-writers must be between 1 and 3");
                            }
                            config.max_concurrent_writers = n;
                            config.orchestration.max_writer_workers = n;
                        }
                        "checks" => {
                            let checks: Vec<crate::checks::ProjectCheck> = serde_json::from_str(
                                &value,
                            )
                            .context("checks value must be a valid JSON array of project checks")?;
                            crate::checks::validate_project_checks(&checks)?;
                            config.checks = checks;
                        }
                        _ => bail!(
                            "Unknown config key '{key}'. Supported keys: cli, writer, critic, mode, max-writers, checks"
                        ),
                    }
                    config.save(&project)?;
                    if cli.json {
                        print_json(&json!({
                            "updated_key": key,
                            "updated_value": value,
                            "status": "ok"
                        }))?;
                    } else {
                        println!("Updated configuration: {key} = {value}");
                    }
                }
            }
        }
        Command::Tui
        | Command::WorkspaceStatus
        | Command::Settings
        | Command::RestartPane { .. } => {
            unreachable!("Terminal UI handled before database open")
        }
    }
    Ok(())
}

async fn run_github_status(json_output: bool, check_auth: bool) -> Result<()> {
    let report = github::status_report(check_auth).await;
    if json_output {
        print_json(&report)?;
        return Ok(());
    }

    match (report.gh.status, &report.gh.version) {
        ("available", Some(version)) => println!("GitHub CLI: {version}"),
        ("unavailable", _) => println!("GitHub CLI: unavailable"),
        ("timeout", _) => println!("GitHub CLI: version probe timed out"),
        _ => println!("GitHub CLI: local probe failed"),
    }
    match report.auth_status {
        "not_checked" => println!("Authentication: not checked; pass --check-auth to query GitHub"),
        "gh_unavailable" => println!("Authentication: not checked because gh is unavailable"),
        "authenticated" => println!("Authentication: active account is authenticated"),
        "not_authenticated" => println!("Authentication: no active authenticated account"),
        "needs_attention" => println!("Authentication: active account needs attention"),
        "timeout" => println!("Authentication: status check timed out"),
        _ => println!("Authentication: status unknown"),
    }
    Ok(())
}

fn run_github_policy(json_output: bool, project: &Project) -> Result<()> {
    let policy = ProjectConfig::load(project)?.github;
    if json_output {
        print_json(&json!({
            "configured": policy.is_some(),
            "policy": policy,
            "external_actions_enabled": policy.as_ref().is_some_and(|policy| {
                policy.allow_push && policy.allow_pull_request
            }),
            "merge": "manual_only",
            "remote_branch_deletion": "unsupported",
        }))?;
    } else if let Some(policy) = policy {
        println!("GitHub repository: {}", policy.repository);
        println!("Git remote: {}", policy.remote);
        println!("Base branch: {}", policy.base_branch);
        println!("Feature prefix: {}", policy.feature_branch_prefix);
        println!("Push allowed: {}", policy.allow_push);
        println!(
            "Pull request creation allowed: {}",
            policy.allow_pull_request
        );
    } else {
        println!("No GitHub publication policy is configured.");
        println!(
            "Publication stays disabled until .talos/config.json contains a validated schema-v3 GitHub policy."
        );
    }
    if !json_output {
        println!("Merge: manual only");
        println!("Remote branch deletion: unsupported");
    }
    Ok(())
}

fn create_github_branch(json_output: bool, project: &Project, slug: &str) -> Result<()> {
    let mut database = Database::open(project)?;
    with_worktree_supervisor_lease(&mut database, "github-branch-create", |_| {
        create_github_branch_under_lease(json_output, project, slug)
    })
}

fn create_github_branch_under_lease(
    json_output: bool,
    project: &Project,
    slug: &str,
) -> Result<()> {
    if !matches!(&project.discovery, DiscoveryMethod::Git) {
        bail!("GitHub feature branches require a Git project");
    }
    if slug.is_empty()
        || slug.len() > 48
        || !slug
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        || !slug.as_bytes()[0].is_ascii_alphanumeric()
        || !slug.as_bytes()[slug.len() - 1].is_ascii_alphanumeric()
        || slug.contains("--")
    {
        bail!("Feature branch slug must be 1–48 lowercase letters, digits, or single hyphens");
    }
    let config = ProjectConfig::load(project)?;
    let policy: GithubPolicy = config
        .github
        .context("No GitHub publication policy is configured; local branch creation is disabled")?;
    let branch = format!("{}{}", policy.feature_branch_prefix, slug);
    let dirty_paths = git_status_paths(project)?;
    if !dirty_paths.is_empty() {
        bail!(
            "Cannot create a GitHub feature branch with a dirty primary worktree; inspect or commit these paths first: {}",
            dirty_paths.display_preview()
        );
    }
    let current_branch = git_command(project, &["branch", "--show-current"])?;
    if !current_branch.status.success() {
        bail!("Cannot read the current Git branch");
    }
    let current_branch = std::str::from_utf8(&current_branch.stdout)
        .context("Current Git branch name is not valid UTF-8")?
        .trim();
    if current_branch != policy.base_branch {
        bail!(
            "Create feature branches only from the configured base branch `{}`; current branch is `{}`",
            policy.base_branch,
            current_branch
        );
    }
    let branch_check = git_command(project, &["check-ref-format", "--branch", &branch])?;
    if !branch_check.status.success() {
        bail!("Configured feature branch name is not a valid Git branch");
    }
    let base_commit = git_command(
        project,
        &[
            "rev-parse",
            "--verify",
            &format!("refs/heads/{}", policy.base_branch),
        ],
    )?;
    if !base_commit.status.success() {
        bail!(
            "Configured base branch `{}` does not exist locally",
            policy.base_branch
        );
    }
    let existing_branch = git_command(
        project,
        &[
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
    )?;
    match existing_branch.status.code() {
        Some(1) => {}
        Some(0) => bail!("Feature branch `{branch}` already exists"),
        _ => bail!("Cannot determine whether feature branch `{branch}` already exists"),
    }
    let switched = git_command(
        project,
        &["switch", "--create", &branch, &policy.base_branch],
    )?;
    if !switched.status.success() {
        bail!("Git could not create and switch to feature branch `{branch}`");
    }
    let base_commit = std::str::from_utf8(&base_commit.stdout)
        .context("Base branch commit is not valid UTF-8")?
        .trim();
    if json_output {
        print_json(&json!({
            "branch": branch,
            "base_branch": policy.base_branch,
            "base_commit": base_commit,
            "local_only": true,
            "pushed": false,
        }))?;
    } else {
        println!(
            "Created local feature branch `{branch}` from `{}`.",
            policy.base_branch
        );
        println!("The branch has not been pushed.");
    }
    Ok(())
}

async fn run_github_publish(
    json_output: bool,
    project: &Project,
    database: &mut Database,
    request: GithubPublishRequest<'_>,
) -> Result<()> {
    if !request.confirm_push || !request.confirm_pr {
        bail!("Publishing requires explicit --confirm-push and --confirm-pr flags");
    }
    let policy = ProjectConfig::load(project)?
        .github
        .context("No GitHub publication policy is configured")?;
    if !policy.allow_push || !policy.allow_pull_request {
        bail!(
            "GitHub publication is disabled by the local policy; both push and PR permissions must be enabled"
        );
    }
    validate_pull_request_text(request.title, request.body)?;

    let lease = acquire_github_supervisor_lease(database, "publish")?;
    let publication =
        run_github_publish_under_lease(json_output, project, database, &policy, request).await;
    let released = database.release_supervisor_lease(&lease)?;
    if !released {
        bail!("TALOS lost its supervisor lease during GitHub publication");
    }
    publication
}

async fn run_github_publish_under_lease(
    json_output: bool,
    project: &Project,
    database: &mut Database,
    policy: &GithubPolicy,
    request: GithubPublishRequest<'_>,
) -> Result<()> {
    let GithubPublishRequest {
        branch,
        title,
        body,
        ..
    } = request;
    if !matches!(&project.discovery, DiscoveryMethod::Git) {
        bail!("GitHub publication requires a Git project");
    }
    if !branch.starts_with(&policy.feature_branch_prefix) || branch == policy.base_branch {
        bail!(
            "GitHub publication is limited to a named feature branch under the configured prefix"
        );
    }
    let branch_check = git_command(project, &["check-ref-format", "--branch", branch])?;
    if !branch_check.status.success() {
        bail!("GitHub publication branch is not a valid Git branch name");
    }
    let current_branch = git_command(project, &["branch", "--show-current"])?;
    if !current_branch.status.success() {
        bail!("Cannot read the current Git branch before GitHub publication");
    }
    let current_branch = std::str::from_utf8(&current_branch.stdout)
        .context("Current Git branch name is not valid UTF-8")?
        .trim();
    if current_branch != branch {
        bail!("Current Git branch does not match the explicitly named publication branch");
    }
    let dirty_paths = git_status_paths(project)?;
    if !dirty_paths.is_empty() {
        bail!(
            "GitHub publication requires a clean feature branch; inspect or commit these paths first: {}",
            dirty_paths.display_preview()
        );
    }

    let base_ref = format!("refs/heads/{}", policy.base_branch);
    let base_commit = git_command(project, &["rev-parse", "--verify", &base_ref])?;
    if !base_commit.status.success() {
        bail!(
            "Configured GitHub base branch `{}` is missing locally",
            policy.base_branch
        );
    }
    let base_commit = std::str::from_utf8(&base_commit.stdout)
        .context("Base branch commit is not valid UTF-8")?
        .trim()
        .to_owned();
    let head_commit = git_head(&project.root)?
        .context("GitHub publication requires at least one committed feature change")?;
    if head_commit == base_commit {
        bail!(
            "GitHub publication branch has no commits beyond `{}`",
            policy.base_branch
        );
    }
    let ancestry = git_command(
        project,
        &["merge-base", "--is-ancestor", &policy.base_branch, "HEAD"],
    )?;
    if !ancestry.status.success() {
        bail!("Feature branch must descend from the configured `master` base");
    }
    let revision = RevisionSnapshot::capture(&project.root, &base_commit)?;
    if revision.head_commit != head_commit || revision.changed_paths.is_empty() {
        bail!(
            "Feature branch revision changed during publication preflight or has no diff from master"
        );
    }
    if database
        .latest_github_publication_for_branch(&policy.repository, branch)?
        .is_some()
    {
        bail!(
            "This repository/branch already has a durable publication record; inspect it with `talos github reconcile` instead of publishing again"
        );
    }

    github::validate_remote_identity(&project.root, &policy.remote, &policy.repository)?;
    if !github::repository_default_branch_matches(&policy.repository, &policy.base_branch).await {
        bail!(
            "GitHub repository is unavailable, unauthenticated, or does not use the configured base branch"
        );
    }
    let remote_base = github::remote_ref_sha(&project.root, &policy.remote, &policy.base_branch)
        .await?
        .context("Configured GitHub base branch is missing on the remote")?;
    if remote_base != base_commit {
        bail!(
            "Local master does not match the remote master; fetch and reconcile the feature branch before publishing"
        );
    }
    if github::remote_ref_sha(&project.root, &policy.remote, branch)
        .await?
        .is_some()
    {
        bail!("The named remote feature branch already exists; TALOS will not update it");
    }

    let mut publication = database.begin_github_publication(&GithubPublicationInput {
        repository: &policy.repository,
        remote_name: &policy.remote,
        base_branch: &policy.base_branch,
        head_branch: branch,
        base_commit: &base_commit,
        head_commit: &head_commit,
        diff_sha256: &revision.patch_sha256,
    })?;
    let push_outcome =
        github::push_process_result(&project.root, &policy.remote, branch, &head_commit).await;
    if push_outcome != "pushed" {
        publication = database.transition_github_publication(
            publication.id,
            "push_unknown",
            if push_outcome == "timed_out" {
                "The explicit single-branch push timed out; remote state must be reconciled before retry"
            } else {
                "The explicit single-branch push did not confirm its outcome; remote state must be reconciled before retry"
            },
            None,
        )?;
        print_github_publication(json_output, &publication)?;
        return Ok(());
    }

    publication = database.transition_github_publication(
        publication.id,
        "pushed",
        "Git confirmed the exact recorded commit was pushed to the named feature branch",
        None,
    )?;
    publication = database.transition_github_publication(
        publication.id,
        "pr_pending",
        "TALOS recorded draft pull request intent before invoking GitHub CLI",
        None,
    )?;
    let remote_head = github::remote_ref_sha(&project.root, &policy.remote, branch).await?;
    if remote_head.as_deref() != Some(head_commit.as_str()) {
        publication = database.transition_github_publication(
            publication.id,
            "needs_attention",
            "Remote feature branch changed after push and before pull request creation",
            None,
        )?;
        print_github_publication(json_output, &publication)?;
        return Ok(());
    }
    if let Some((_, url)) = github::create_draft_pull_request(
        &policy.repository,
        &policy.base_branch,
        branch,
        title,
        body,
    )
    .await
    {
        publication = database.transition_github_publication(
            publication.id,
            "open",
            "GitHub CLI returned a matching draft pull request URL",
            Some(&url),
        )?;
    } else {
        publication = database.transition_github_publication(
            publication.id,
            "pr_unknown",
            "Draft pull request command did not confirm its outcome; inspect GitHub before retry",
            None,
        )?;
    }
    print_github_publication(json_output, &publication)?;
    Ok(())
}

async fn run_github_reconcile(
    json_output: bool,
    project: &Project,
    database: &mut Database,
    publication_id: i64,
    options: GithubReconcileOptions<'_>,
) -> Result<()> {
    let record = database
        .github_publication(publication_id)?
        .with_context(|| format!("GitHub publication #{publication_id} does not exist"))?;
    if options.create_pr {
        let title = options.title.context("`--create-pr` requires `--title`")?;
        let body = options.body.context("`--create-pr` requires `--body`")?;
        validate_pull_request_text(title, body)?;
    } else if options.title.is_some() || options.body.is_some() {
        bail!("`--title` and `--body` can only be used with `--create-pr`");
    }
    if options.create_pr
        && !matches!(
            record.state.as_str(),
            "push_pending" | "push_unknown" | "pushed"
        )
    {
        bail!(
            "`--create-pr` is only available when reconciling a push that has not entered PR creation; omit it to inspect an uncertain PR result"
        );
    }
    let policy = ProjectConfig::load(project)?
        .github
        .context("No GitHub publication policy is configured")?;
    if !record.repository.eq_ignore_ascii_case(&policy.repository)
        || record.remote_name != policy.remote
        || record.base_branch != policy.base_branch
    {
        bail!("Recorded GitHub publication does not match the current local policy");
    }
    if options.create_pr && !policy.allow_pull_request {
        bail!("Draft pull request creation is disabled by the current GitHub policy");
    }
    let lease = acquire_github_supervisor_lease(database, "reconcile")?;
    let outcome =
        run_github_reconcile_under_lease(json_output, project, database, &policy, record, options)
            .await;
    let released = database.release_supervisor_lease(&lease)?;
    if !released {
        bail!("TALOS lost its supervisor lease during GitHub publication reconciliation");
    }
    outcome
}

async fn run_github_reconcile_under_lease(
    json_output: bool,
    project: &Project,
    database: &mut Database,
    policy: &GithubPolicy,
    mut record: GithubPublicationRecord,
    options: GithubReconcileOptions<'_>,
) -> Result<()> {
    github::validate_remote_identity(&project.root, &record.remote_name, &record.repository)?;
    if !github::repository_default_branch_matches(&record.repository, &record.base_branch).await {
        bail!("Cannot verify the recorded GitHub repository and base branch");
    }
    match record.state.as_str() {
        "push_pending" | "push_unknown" => {
            let remote_head =
                github::remote_ref_sha(&project.root, &record.remote_name, &record.head_branch)
                    .await?;
            if remote_head.as_deref() == Some(record.head_commit.as_str()) {
                record = database.transition_github_publication(
                    record.id,
                    "pushed",
                    "Explicit reconciliation found the exact recorded commit on the remote branch",
                    None,
                )?;
            } else {
                record = database.transition_github_publication(
                    record.id,
                    "needs_attention",
                    "Remote branch is absent or differs from the recorded commit; automatic retry is unsafe",
                    None,
                )?;
                print_github_publication(json_output, &record)?;
                return Ok(());
            }
        }
        "pr_pending" | "pr_unknown" => {
            let remote_head =
                github::remote_ref_sha(&project.root, &record.remote_name, &record.head_branch)
                    .await?;
            let pull_request = github::find_matching_open_pull_request(
                &record.repository,
                &record.head_branch,
                &record.base_branch,
                &record.head_commit,
            )
            .await;
            if remote_head.as_deref() != Some(record.head_commit.as_str()) {
                record = database.transition_github_publication(
                    record.id,
                    "needs_attention",
                    "Remote feature branch no longer matches the recorded commit; inspect the PR manually",
                    None,
                )?;
                print_github_publication(json_output, &record)?;
                return Ok(());
            }
            match pull_request {
                github::PullRequestInspection::MatchingOpen { url, .. } => {
                    record = database.transition_github_publication(
                        record.id,
                        "open",
                        "Explicit reconciliation found one open PR for the recorded branch, base, and commit",
                        Some(&url),
                    )?;
                }
                _ => {
                    record = database.transition_github_publication(
                        record.id,
                        "needs_attention",
                        "GitHub did not confirm one matching open pull request; inspect the remote before continuing",
                        None,
                    )?;
                    print_github_publication(json_output, &record)?;
                    return Ok(());
                }
            }
        }
        "pushed" => {}
        "open" | "needs_attention" => {
            print_github_publication(json_output, &record)?;
            return Ok(());
        }
        _ => bail!("GitHub publication has an unsupported state and needs manual inspection"),
    }

    if options.create_pr {
        if !policy.allow_pull_request {
            bail!("Draft pull request creation is disabled by the current GitHub policy");
        }
        let remote_head =
            github::remote_ref_sha(&project.root, &record.remote_name, &record.head_branch).await?;
        if remote_head.as_deref() != Some(record.head_commit.as_str()) {
            record = database.transition_github_publication(
                record.id,
                "needs_attention",
                "Remote feature branch no longer matches the recorded commit; PR creation stopped",
                None,
            )?;
            print_github_publication(json_output, &record)?;
            return Ok(());
        }
        match github::find_matching_open_pull_request(
            &record.repository,
            &record.head_branch,
            &record.base_branch,
            &record.head_commit,
        )
        .await
        {
            github::PullRequestInspection::None => {}
            github::PullRequestInspection::MatchingOpen { url, .. } => {
                record = database.transition_github_publication(
                    record.id,
                    "pr_pending",
                    "Operator reconciliation found a PR created after push completion",
                    None,
                )?;
                record = database.transition_github_publication(
                    record.id,
                    "open",
                    "Operator reconciliation recorded the matching open pull request",
                    Some(&url),
                )?;
                print_github_publication(json_output, &record)?;
                return Ok(());
            }
            github::PullRequestInspection::OtherOrAmbiguous => {
                record = database.transition_github_publication(
                    record.id,
                    "needs_attention",
                    "A conflicting or non-matching pull request exists for this branch",
                    None,
                )?;
                print_github_publication(json_output, &record)?;
                return Ok(());
            }
            github::PullRequestInspection::Unknown => {
                bail!("Cannot determine whether a pull request already exists; no PR was created");
            }
        }
        let title = options.title.context("`--create-pr` requires `--title`")?;
        let body = options.body.context("`--create-pr` requires `--body`")?;
        record = database.transition_github_publication(
            record.id,
            "pr_pending",
            "Operator explicitly authorized draft PR creation during reconciliation",
            None,
        )?;
        if let Some((_, url)) = github::create_draft_pull_request(
            &record.repository,
            &record.base_branch,
            &record.head_branch,
            title,
            body,
        )
        .await
        {
            record = database.transition_github_publication(
                record.id,
                "open",
                "GitHub CLI returned a matching draft pull request URL during reconciliation",
                Some(&url),
            )?;
        } else {
            record = database.transition_github_publication(
                record.id,
                "pr_unknown",
                "Draft pull request command did not confirm its outcome; inspect GitHub before retry",
                None,
            )?;
        }
    }
    print_github_publication(json_output, &record)?;
    Ok(())
}

async fn run_github_ci(
    json_output: bool,
    project: &Project,
    database: &mut Database,
    publication_id: i64,
    wait_for_checks: bool,
    timeout_seconds: u64,
    interval_seconds: u64,
) -> Result<()> {
    if wait_for_checks
        && (!(1..=3_600).contains(&timeout_seconds) || !(5..=120).contains(&interval_seconds))
    {
        bail!("CI wait requires a timeout of 1–3600 seconds and an interval of 5–120 seconds");
    }
    let publication = database
        .github_publication(publication_id)?
        .with_context(|| format!("GitHub publication #{publication_id} does not exist"))?;
    if publication.state != "open" {
        bail!("GitHub CI status requires a recorded open pull request");
    }
    let policy = ProjectConfig::load(project)?
        .github
        .context("No GitHub publication policy is configured")?;
    if !publication
        .repository
        .eq_ignore_ascii_case(&policy.repository)
        || publication.remote_name != policy.remote
        || publication.base_branch != policy.base_branch
    {
        bail!("Recorded GitHub publication does not match the current local policy");
    }
    github::validate_remote_identity(
        &project.root,
        &publication.remote_name,
        &publication.repository,
    )?;

    let verified_pr_number = verify_github_publication_pr_head(&project.root, &publication).await;

    let deadline = Instant::now() + Duration::from_secs(timeout_seconds.max(1));
    let mut result = "unknown";
    let mut checks = Vec::new();
    if let Some(pull_request_number) = verified_pr_number {
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if wait_for_checks && remaining.is_zero() {
                result = "timeout";
                break;
            }
            let query_timeout = if wait_for_checks {
                remaining
            } else {
                Duration::from_secs(60)
            };
            match github::pull_request_checks(
                &publication.repository,
                pull_request_number,
                query_timeout,
            )
            .await
            {
                Some(observed) if github_ci_checks_are_safe(&observed) => {
                    checks = observed;
                    result = classify_github_ci_checks(&checks);
                }
                _ => {
                    result = if wait_for_checks && Instant::now() >= deadline {
                        "timeout"
                    } else {
                        "unknown"
                    };
                    checks.clear();
                }
            }
            if !wait_for_checks || result != "pending" {
                break;
            }
            let now = Instant::now();
            if now >= deadline {
                result = "timeout";
                break;
            }
            sleep(Duration::from_secs(interval_seconds).min(deadline - now)).await;
        }
    }
    if verified_pr_number.is_some()
        && verify_github_publication_pr_head(&project.root, &publication).await
            != verified_pr_number
    {
        result = "unknown";
        checks.clear();
    }
    let snapshot = database.record_github_ci_snapshot(&GithubCiSnapshotInput {
        publication_id,
        head_commit: &publication.head_commit,
        result,
        checks: &checks,
    })?;
    print_github_ci_snapshot(json_output, &snapshot, wait_for_checks)
}

async fn verify_github_publication_pr_head(
    project_root: &std::path::Path,
    publication: &GithubPublicationRecord,
) -> Option<u32> {
    let recorded_pr_number = publication
        .pull_request_url
        .as_deref()
        .and_then(|url| url.rsplit("/pull/").next())
        .and_then(|number| number.parse::<u32>().ok());
    if !github::remote_ref_sha(
        project_root,
        &publication.remote_name,
        &publication.head_branch,
    )
    .await
    .is_ok_and(|head| head.as_deref() == Some(publication.head_commit.as_str()))
    {
        return None;
    }
    match github::find_matching_open_pull_request(
        &publication.repository,
        &publication.head_branch,
        &publication.base_branch,
        &publication.head_commit,
    )
    .await
    {
        github::PullRequestInspection::MatchingOpen { number, .. }
            if Some(number) == recorded_pr_number =>
        {
            Some(number)
        }
        _ => None,
    }
}

fn github_ci_checks_are_safe(checks: &[GithubCiCheckRecord]) -> bool {
    checks.len() <= 128
        && checks.iter().all(|check| {
            matches!(
                check.bucket.as_str(),
                "pass" | "fail" | "pending" | "skipping" | "cancel"
            ) && !check.name.trim().is_empty()
                && check.name.len() <= 256
                && !contains_sensitive(&check.name)
                && !check.state.trim().is_empty()
                && check.state.len() <= 64
                && !contains_sensitive(&check.state)
                && check.workflow.as_ref().is_none_or(|workflow| {
                    !workflow.trim().is_empty()
                        && workflow.len() <= 256
                        && !contains_sensitive(workflow)
                })
                && check
                    .started_at
                    .as_ref()
                    .is_none_or(|timestamp| !timestamp.trim().is_empty() && timestamp.len() <= 64)
                && check
                    .completed_at
                    .as_ref()
                    .is_none_or(|timestamp| !timestamp.trim().is_empty() && timestamp.len() <= 64)
        })
}

fn classify_github_ci_checks(checks: &[GithubCiCheckRecord]) -> &'static str {
    if checks.is_empty() {
        return "no_checks";
    }
    if checks
        .iter()
        .any(|check| matches!(check.bucket.as_str(), "fail" | "cancel"))
    {
        return "fail";
    }
    if checks.iter().any(|check| check.bucket == "pending") {
        return "pending";
    }
    if checks
        .iter()
        .any(|check| !matches!(check.bucket.as_str(), "pass" | "skipping"))
    {
        return "unknown";
    }
    if checks.iter().any(|check| check.bucket == "pass") {
        "pass"
    } else {
        "no_checks"
    }
}

fn print_github_ci_snapshot(
    json_output: bool,
    snapshot: &crate::db::GithubCiSnapshotRecord,
    wait_for_checks: bool,
) -> Result<()> {
    if json_output {
        print_json(snapshot)?;
    } else {
        println!(
            "GitHub CI snapshot #{}: {} ({} check(s), commit {})",
            snapshot.id,
            snapshot.result,
            snapshot.checks.len(),
            snapshot.head_commit
        );
        if snapshot.result == "pending" {
            println!(
                "Checks are still running; use `talos github ci {} --wait` to continue polling.",
                snapshot.publication_id
            );
        } else if matches!(
            snapshot.result.as_str(),
            "unknown" | "timeout" | "no_checks"
        ) {
            println!("This snapshot is incomplete and does not count as passing CI.");
        }
        if !wait_for_checks && snapshot.result == "pending" {
            println!("The status was queried once; no polling was requested.");
        }
    }
    Ok(())
}

fn acquire_github_supervisor_lease(
    database: &mut Database,
    action: &str,
) -> Result<crate::db::SupervisorLease> {
    let now = now_ms()?;
    let owner_id = format!("talos-task-github-{action}-{}-{now}", std::process::id());
    let duration_ms = i64::try_from(SUPERVISOR_LEASE.as_millis())
        .context("Supervisor lease duration is out of range")?;
    database
        .acquire_supervisor_lease(&owner_id, now, duration_ms)?
        .context("Another TALOS supervisor currently owns the project lease")
}

fn validate_pull_request_text(title: &str, body: &str) -> Result<()> {
    if title.trim().is_empty() || title.len() > 200 || contains_sensitive(title) {
        bail!(
            "Pull request title must be non-empty, at most 200 bytes, and contain no secret-like text"
        );
    }
    if body.trim().is_empty() || body.len() > 16_000 || contains_sensitive(body) {
        bail!(
            "Pull request body must be non-empty, at most 16000 bytes, and contain no secret-like text"
        );
    }
    Ok(())
}

fn print_github_publication(json_output: bool, record: &GithubPublicationRecord) -> Result<()> {
    if json_output {
        print_json(record)?;
        return Ok(());
    } else {
        println!(
            "GitHub publication #{}: {} ({}, {})",
            record.id, record.state, record.repository, record.head_branch
        );
        println!("Diff: {}", record.diff_sha256);
        if let Some(url) = &record.pull_request_url {
            println!("Draft PR: {url}");
        } else if matches!(
            record.state.as_str(),
            "push_unknown" | "pr_pending" | "pr_unknown"
        ) {
            println!(
                "Inspect the remote, then run `talos github reconcile {}` before any retry.",
                record.id
            );
        } else if record.state == "needs_attention" {
            println!(
                "Inspect GitHub and the local branch manually; TALOS will not retry this publication."
            );
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
    let execution_project = execution_project_for_task(project, database, task_id)?;
    let report = execute_writer_task(
        json_output,
        &execution_project,
        database,
        task_id,
        None,
        None,
    )
    .await?;
    print_writer_report(json_output, &report, None)?;
    ensure_writer_succeeded(&report)
}

fn execution_project_for_task(
    project: &Project,
    database: &Database,
    task_id: i64,
) -> Result<Project> {
    worktree::execution_project(project, database, task_id)
}

fn ensure_execution_root_current(
    execution_project: &Project,
    database: &Database,
    task_id: i64,
) -> Result<()> {
    let primary_project = execution_project.primary_project();
    let current = worktree::execution_project(&primary_project, database, task_id)?;
    if current.root != execution_project.root {
        bail!(
            "Task #{task_id} execution worktree changed before the Writer attempt was claimed; retry after inspecting the task worktree"
        );
    }
    Ok(())
}

fn ensure_task_worktree_integrated_before_review(database: &Database, task_id: i64) -> Result<()> {
    if let Some(worktree) = database.task_worktree(task_id)?
        && worktree.state == crate::db::TaskWorktreeState::Active
        && worktree.writer_routed_at_ms.is_some()
        && database.task_worktree_integration(task_id)?.is_none()
    {
        bail!(
            "Task #{task_id} has an active worktree; run its Writer, then `talos task worktree integrate {task_id}` before Critic review or verification"
        );
    }
    Ok(())
}

fn with_worktree_supervisor_lease<T>(
    database: &mut Database,
    operation: &str,
    action: impl FnOnce(&mut Database) -> Result<T>,
) -> Result<T> {
    let now = now_ms()?;
    let owner_id = format!("talos-worktree-{operation}-{}-{now}", std::process::id());
    let lease_duration_ms = i64::try_from(SUPERVISOR_LEASE.as_millis())
        .context("Supervisor lease duration is out of range")?;
    let lease = database
        .acquire_supervisor_lease(&owner_id, now, lease_duration_ms)?
        .context("Another TALOS supervisor currently owns the project lease")?;
    let result = action(database);
    if !database.release_supervisor_lease(&lease)? {
        bail!("TALOS lost its supervisor lease during task worktree {operation}");
    }
    result
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

pub(crate) fn current_project_revision_id(project: &Project) -> Option<String> {
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
            .get_writer_attempt(attempt_id)?
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
    let execution_project = execution_project_for_task(project, database, task_id)?;
    let report = execute_writer_task(
        json_output,
        &execution_project,
        database,
        task_id,
        Some(continuation),
        None,
    )
    .await?;
    print_writer_report(json_output, &report, None)?;
    ensure_writer_succeeded(&report)
}

async fn run_task_review(
    json_output: bool,
    project: &Project,
    database: &mut Database,
    task_id: i64,
) -> Result<()> {
    ensure_task_worktree_integrated_before_review(database, task_id)?;
    let execution_project = execution_project_for_task(project, database, task_id)?;
    validate_project_scope(&execution_project)?;
    let report = execute_critic_review(&execution_project, database, task_id, None).await?;
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
    task_status: String,
    checks: Vec<CheckExecution>,
}

fn print_task_worktree(
    json_output: bool,
    action: &str,
    record: &crate::db::TaskWorktreeRecord,
    database: &Database,
) -> Result<()> {
    let integration = database.task_worktree_integration(record.task_id)?;
    if json_output {
        print_json(&json!({
            "action": if action.is_empty() { None } else { Some(action) },
            "worktree": record,
            "integration": integration,
        }))?;
    } else {
        if !action.is_empty() {
            println!("Task worktree {action}.");
        }
        println!(
            "Task #{}: {} on branch {}.",
            record.task_id,
            record.state.as_str(),
            redact_sensitive(&record.branch_name),
        );
        println!(
            "  Path: {}; base: {}",
            redact_sensitive(&record.worktree_path),
            record.base_commit
        );
        if let Some(integration) = integration {
            println!("  Integration: {}", integration.state);
        }
    }
    Ok(())
}

fn print_task_worktree_integration(
    json_output: bool,
    action: &str,
    record: &crate::db::TaskWorktreeIntegrationRecord,
) -> Result<()> {
    if json_output {
        print_json(&json!({ "action": action, "integration": record }))?;
    } else {
        println!(
            "Task #{} worktree integration is {}.",
            record.task_id, record.state
        );
        println!(
            "  Base: {}; source revision: {}",
            record.base_commit, record.revision_id
        );
        if let Some(commit) = &record.integration_commit {
            println!("  Primary commit: {commit}");
        } else if record.state == "pending" {
            println!(
                "  Review and verification must pass after integration, then commit the changes in the primary project and run `talos task worktree finalize {}`.",
                record.task_id
            );
        }
    }
    Ok(())
}

fn print_child_task_report(json_output: bool, database: &Database, task_id: i64) -> Result<()> {
    let task = database
        .get_task(task_id)?
        .with_context(|| format!("Task #{task_id} does not exist"))?;
    let contract = database
        .child_task_contract(task_id)?
        .with_context(|| format!("Task #{task_id} has no bounded child-task contract"))?;
    let latest_attempt = database.latest_writer_attempt(task_id)?.map(|attempt| {
        json!({
            "attempt_id": attempt.id,
            "state": attempt.state,
            "error_code": attempt.error_code,
            "exit_code": attempt.exit_code,
        })
    });
    let latest_review = database.latest_critic_review(task_id)?.map(|review| {
        json!({
            "review_id": review.id,
            "decision": review.decision,
            "summary": review.summary,
            "revision_id": review.revision_id,
        })
    });
    let latest_verification = database
        .latest_verification_summary(task_id)?
        .map(|evidence| {
            json!({
                "run_id": evidence.run.id,
                "status": evidence.run.status,
                "revision_id": evidence.run.revision_id,
                "check_count": evidence.check_count,
            })
        });
    let report = json!({
        "schema_version": "talos-child-task-report-v1",
        "parent_task_id": contract.parent_task_id,
        "child_task_id": task.id,
        "task_status": task.status.as_str(),
        "acceptance_criteria": task.acceptance_criteria,
        "allowed_paths": contract.allowed_paths,
        "timeout_ms": contract.timeout_ms,
        "latest_writer_attempt": latest_attempt,
        "latest_critic_review": latest_review,
        "latest_verification": latest_verification,
        "ready_for_parent": task.status == crate::db::TaskStatus::Complete,
    });
    if json_output {
        print_json(&report)?;
    } else {
        println!(
            "Child task #{} report ({}) for parent #{}.",
            task.id,
            task.status.as_str(),
            contract.parent_task_id
        );
        println!("Allowed paths: {}", contract.allowed_paths.join(", "));
        println!(
            "Acceptance criteria: {}",
            redact_sensitive(&task.acceptance_criteria)
        );
        println!(
            "Ready for parent: {}.",
            if task.status == crate::db::TaskStatus::Complete {
                "yes"
            } else {
                "no; review and verification are still required"
            }
        );
    }
    Ok(())
}

fn print_task_handoff(json_output: bool, database: &Database, task_id: i64) -> Result<()> {
    const MAX_DEPENDENCIES: usize = 32;

    let task = database
        .get_task(task_id)?
        .with_context(|| format!("Task #{task_id} does not exist"))?;
    let (title, title_truncated) = bounded_handoff_text(&task.title);
    let (acceptance_criteria, criteria_truncated) = bounded_handoff_text(&task.acceptance_criteria);

    let attempt = database.latest_writer_attempt(task_id)?;
    let latest_attempt = attempt.as_ref().map(|attempt| {
        json!({
            "attempt_id": attempt.id,
            "state": attempt.state,
            "error_code": attempt.error_code,
            "exit_code": attempt.exit_code,
            "base_revision": attempt.base_revision,
            "source_review_id": attempt.source_review_id,
            "operator_resolution": attempt.operator_resolution,
        })
    });
    let review = database.latest_critic_review(task_id)?;
    let latest_review = review.as_ref().map(|review| {
        let (summary, summary_truncated) = bounded_handoff_text(&review.summary);
        json!({
            "review_id": review.id,
            "writer_attempt_id": review.writer_attempt_id,
            "decision": review.decision,
            "summary": summary,
            "summary_truncated": summary_truncated,
            "revision_id": review.revision_id,
        })
    });
    let verification = database.latest_verification_summary(task_id)?;
    let latest_verification = verification.as_ref().map(|evidence| {
        json!({
            "run_id": evidence.run.id,
            "critic_review_id": evidence.run.critic_review_id,
            "status": evidence.run.status,
            "revision_id": evidence.run.revision_id,
            "check_count": evidence.check_count,
        })
    });
    let (dependency_count, dependencies) =
        database.task_dependency_summary(task_id, MAX_DEPENDENCIES)?;
    let worktree = database.task_worktree(task_id)?;
    let integration = database.task_worktree_integration(task_id)?;
    let integration_state = integration.as_ref().map(|record| record.state.as_str());
    let worktree_is_active = worktree.as_ref().is_some_and(|record| {
        record.state.as_str() == "active" && record.writer_routed_at_ms.is_some()
    });
    let latest_verification_status = verification
        .as_ref()
        .map(|evidence| evidence.run.status.as_str());
    let next_action = task_handoff_next_action(
        task_id,
        task.status.as_str(),
        worktree_is_active,
        integration_state,
        latest_verification_status,
    );
    let worktree_summary = worktree.as_ref().map(|record| {
        json!({
            "state": record.state.as_str(),
            "branch": record.branch_name,
            "base_commit": record.base_commit,
            "integration_state": integration_state,
            "integration_commit": integration.as_ref().and_then(|entry| entry.integration_commit.as_deref()),
        })
    });
    let report = json!({
        "schema_version": "talos-task-handoff-v1",
        "task": {
            "task_id": task.id,
            "title": title,
            "title_truncated": title_truncated,
            "status": task.status.as_str(),
            "acceptance_criteria": acceptance_criteria,
            "acceptance_criteria_truncated": criteria_truncated,
            "updated_at_ms": task.updated_at_ms,
        },
        "recommended_next_action": next_action,
        "latest_writer_attempt": latest_attempt,
        "latest_critic_review": latest_review,
        "latest_verification": latest_verification,
        "dependencies": dependencies,
        "dependency_count": dependency_count,
        "dependencies_truncated": dependency_count > MAX_DEPENDENCIES,
        "worktree": worktree_summary,
        "limits": [
            "This report is read-only and summarizes recorded state at generation time.",
            "It does not prove continuity of an interactive CLI session or authorize an action.",
            "Prompts, patches, transcripts, raw command output, and the current Git diff are not included.",
            "Inspect the current working tree before a Writer action or retry.",
        ],
    });

    if json_output {
        print_json(&report)?;
    } else {
        println!(
            "Task #{} handoff packet ({}, read-only).",
            task_id,
            task.status.as_str()
        );
        println!("Title: {title}");
        println!("Acceptance criteria: {acceptance_criteria}");
        println!("Recommended next action: {next_action}");
        if let Some(attempt) = attempt.as_ref() {
            println!("Latest Writer attempt: #{} {}.", attempt.id, attempt.state);
        }
        if let Some(review) = review.as_ref() {
            let (summary, truncated) = bounded_handoff_text(&review.summary);
            println!(
                "Latest Critic review: #{} {} — {}{}",
                review.id,
                review.decision,
                summary,
                if truncated { " [truncated]" } else { "" }
            );
        }
        if let Some(evidence) = verification.as_ref() {
            println!(
                "Latest verification: #{} {} ({} check(s)).",
                evidence.run.id, evidence.run.status, evidence.check_count
            );
        }
        if dependency_count > MAX_DEPENDENCIES {
            println!(
                "Dependencies: showing {MAX_DEPENDENCIES} of {dependency_count}; request `talos task dependencies {task_id}` for the full list."
            );
        }
        println!(
            "No prompts, patches, transcripts, raw command output, or current Git diff are included."
        );
    }
    Ok(())
}

fn bounded_handoff_text(value: &str) -> (String, bool) {
    const MAX_BYTES: usize = 4 * 1024;
    const SUFFIX: &str = " [truncated]";
    let redacted = redact_sensitive(value);
    if redacted.len() <= MAX_BYTES {
        return (redacted, false);
    }
    let mut end = MAX_BYTES.saturating_sub(SUFFIX.len()).min(redacted.len());
    while !redacted.is_char_boundary(end) {
        end -= 1;
    }
    (format!("{}{SUFFIX}", &redacted[..end]), true)
}

fn task_handoff_next_action(
    task_id: i64,
    task_status: &str,
    has_worktree: bool,
    integration_state: Option<&str>,
    verification_status: Option<&str>,
) -> String {
    match task_status {
        "queued" => format!(
            "Inspect current Git changes and prerequisites with `talos task dependencies {task_id}`; run `talos task run {task_id}` only after both are clear."
        ),
        "implementing" => format!(
            "Inspect the latest Writer outcome and project changes before another `talos task run {task_id}`."
        ),
        "reviewing" if has_worktree && integration_state.is_none() => format!(
            "Integrate the managed worktree with `talos task worktree integrate {task_id}`, then review the integrated revision."
        ),
        "reviewing" if integration_state == Some("applying") => format!(
            "Inspect both worktrees, then reconcile the interrupted integration with `talos task worktree reconcile {task_id}` before review."
        ),
        "reviewing" if integration_state == Some("needs_attention") => {
            "Inspect the recorded worktree integration mismatch before continuing.".to_owned()
        }
        "reviewing" => format!("Review the latest revision with `talos task review {task_id}`."),
        "verifying"
            if integration_state == Some("applying")
                || integration_state == Some("needs_attention") =>
        {
            "Inspect and reconcile the managed worktree integration before verification.".to_owned()
        }
        "verifying"
            if has_worktree
                && integration_state == Some("pending")
                && verification_status == Some("pass") =>
        {
            format!("Commit the verified primary-tree changes, then run `talos task worktree finalize {task_id}`.")
        }
        "verifying" => format!("Run the saved project checks with `talos task verify {task_id}`."),
        "complete" => "No further task action is required.".to_owned(),
        "planning" => "Inspect the task and its history to choose the next explicit action.".to_owned(),
        "blocked" | "failed" => "Inspect `talos logs --project` and `talos status`; use `talos task recover` only when a Writer attempt was interrupted.".to_owned(),
        _ => "Inspect the task status and history before taking an action.".to_owned(),
    }
}

async fn run_task_verification(
    json_output: bool,
    project: &Project,
    database: &mut Database,
    task_id: i64,
) -> Result<()> {
    let task = database
        .get_task(task_id)?
        .with_context(|| format!("Task #{task_id} does not exist"))?;
    if task.status != crate::db::TaskStatus::Verifying {
        bail!("Task #{task_id} must have an approved Critic review before verification");
    }
    ensure_task_worktree_integrated_before_review(database, task_id)?;
    let execution_project = execution_project_for_task(project, database, task_id)?;
    validate_project_scope(&execution_project)?;
    let checks = database.task_verification_checks(task_id)?;
    if !checks.is_empty()
        && policy::authorize(ExecutionMode::Auto, PolicyAction::ProjectCheck, true)
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
    let before =
        RevisionSnapshot::capture(execution_project.root.as_path(), &review.base_revision)?;
    let facts = before.facts();
    let now = now_ms()?;
    let owner_id = format!("talos-task-verify-{}-{now}", std::process::id());
    let lease_duration_ms = i64::try_from(SUPERVISOR_LEASE.as_millis())
        .context("Supervisor lease duration is out of range")?;
    let lease = database
        .acquire_supervisor_lease(&owner_id, now, lease_duration_ms)?
        .context("Another TALOS supervisor currently owns the project lease")?;

    let verification = async {
        ensure_execution_root_current(&execution_project, database, task_id)?;
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
                match run_project_check(&execution_project.root, check, remaining).await {
                    Ok(result) => result,
                    Err(_) => failed_check_execution(check, "check_runner_failed"),
                }
            };
            interrupted |= result.failure_code.as_deref() == Some("interrupted");
            database.record_verification_result(run.id, &result, &lease)?;
            results.push(result);
        }
        let snapshot_matches =
            RevisionSnapshot::capture(&execution_project.root, &facts.base_commit)
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
        task_status: database
            .get_task(task_id)?
            .map(|task| task.status.as_str().to_owned())
            .context("Task disappeared after verification")?,
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
        bail!(
            "Task #{task_id} verification checks failed (run #{}, status '{}')",
            run.id,
            run.status
        );
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

fn resolve_adapter_executable(
    adapter: AdapterKind,
    project_root: &Path,
) -> Result<(PathBuf, String)> {
    match adapter {
        AdapterKind::Codex => {
            let exe = crate::adapter::codex::resolve_codex_executable()
                .context("Codex executable was not found on PATH or default locations")?;
            Ok((exe, "0.169.0".to_owned()))
        }
        AdapterKind::Omp => {
            let exe = crate::adapter::omp::resolve_omp_executable()
                .context("OMP executable was not found on PATH or default locations")?;
            Ok((exe, "18.8.7".to_owned()))
        }
        AdapterKind::Pi => {
            let exe = crate::adapter::pi::resolve_pi_executable()
                .context("Pi executable was not found on PATH or default locations")?;
            Ok((exe, "0.82.1".to_owned()))
        }
        AdapterKind::Kilo => {
            let exe = crate::adapter::kilo::resolve_kilo_executable()
                .context("Kilo executable was not found on PATH or default locations")?;
            Ok((exe, "7.8.27".to_owned()))
        }
        AdapterKind::Claude => {
            let exe = crate::adapter::claude::resolve_claude_executable()
                .context("Claude executable was not found on PATH or default locations")?;
            Ok((exe, "2.1.296".to_owned()))
        }
        AdapterKind::OpenCode => {
            let exe = crate::adapter::opencode::resolve_opencode_executable()
                .context("OpenCode executable was not found on PATH or default locations")?;
            Ok((exe, "2.3.0".to_owned()))
        }
        AdapterKind::Agy => {
            let probe = AgyProbe::inspect(project_root);
            if probe.status.as_str() != "available" {
                bail!(
                    "Antigravity CLI is unavailable: {}",
                    probe.note.as_deref().unwrap_or("capability probe failed")
                );
            }
            let version = probe
                .version
                .context("Antigravity did not report a version")?;
            let executable = probe
                .executable
                .context("Antigravity executable path is unavailable")?;
            Ok((executable, version))
        }
    }
}

async fn execute_critic_review(
    project: &Project,
    database: &mut Database,
    task_id: i64,
    run_config: Option<&ProjectConfig>,
) -> Result<crate::supervisor::CriticReviewReport> {
    let mut rate_limit_retries = 0;
    const MAX_RATE_LIMIT_RETRIES: usize = 5;

    loop {
        let result = execute_critic_review_once(project, database, task_id, run_config).await;

        if let Err(ref e) = result {
            let err_msg = format!("{e:#}");
            if let Some(rate_limit) =
                crate::rate_limit::detect_rate_limit(&err_msg, rate_limit_retries)
            {
                rate_limit_retries += 1;
                if rate_limit_retries <= MAX_RATE_LIMIT_RETRIES {
                    let wait_secs = rate_limit.wait_duration.as_secs();
                    let wait_desc = if wait_secs < 60 {
                        format!("{wait_secs}s")
                    } else {
                        format!("{}m {}s", wait_secs / 60, wait_secs % 60)
                    };
                    println!(
                        "\n⏳ [TALOS RateLimit] Detected {} ('{}') during Critic review on task #{}.",
                        rate_limit.kind.as_str(),
                        rate_limit.matched_signature,
                        task_id
                    );
                    println!(
                        "⏳ [TALOS RateLimit] Auto-pausing execution for {wait_desc} before retry (attempt {rate_limit_retries}/{MAX_RATE_LIMIT_RETRIES})..."
                    );
                    tokio::time::sleep(rate_limit.wait_duration).await;
                    println!(
                        ">>> [TALOS RateLimit] Auto-pause elapsed. Retrying Critic review for task #{}...",
                        task_id
                    );
                    continue;
                }
            }
        }

        return result;
    }
}

async fn execute_critic_review_once(
    project: &Project,
    database: &mut Database,
    task_id: i64,
    run_config: Option<&ProjectConfig>,
) -> Result<crate::supervisor::CriticReviewReport> {
    let loaded;
    let config = if let Some(config) = run_config {
        config
    } else {
        loaded = ProjectConfig::load(project)?;
        &loaded
    };
    let configured_adapter = config.adapter_for(AgentRole::Critic);
    if !configured_adapter.is_implemented() {
        bail!(
            "Configured Critic profile {:?} is not implemented; no CLI was launched",
            configured_adapter.as_str()
        );
    }
    let (executable, version) = resolve_adapter_executable(configured_adapter, &project.root)?;
    let now = now_ms()?;
    let owner_id = format!("talos-task-review-{task_id}-{}-{now}", std::process::id());
    let lease_duration_ms = i64::try_from(SUPERVISOR_LEASE.as_millis())
        .context("Supervisor lease duration is out of range")?;
    let lease = database
        .acquire_supervisor_lease(&owner_id, now, lease_duration_ms)?
        .context("Another TALOS supervisor currently owns the project lease")?;
    let review = async {
        ensure_execution_root_current(project, database, task_id)?;
        run_critic_review(
            project,
            database,
            task_id,
            configured_adapter,
            executable,
            &version,
            config.critic_model.clone(),
            WRITER_TIMEOUT,
        )
        .await
    }
    .await;
    let lease_released = database.release_supervisor_lease(&lease)?;
    if !lease_released {
        bail!("TALOS lost its supervisor lease while running the Critic");
    }
    review
}

async fn execute_writer_task(
    json_output: bool,
    project: &Project,
    database: &mut Database,
    task_id: i64,
    continuation: Option<WriterContinuation>,
    run_config: Option<&ProjectConfig>,
) -> Result<crate::supervisor::WriterExecutionReport> {
    let mut rate_limit_retries = 0;
    const MAX_RATE_LIMIT_RETRIES: usize = 5;

    loop {
        let result = execute_writer_task_once(
            json_output,
            project,
            database,
            task_id,
            continuation.clone(),
            run_config,
        )
        .await;

        let error_text = match &result {
            Err(e) => Some(format!("{e:#}")),
            Ok(rep) if rep.outcome != crate::db::WriterAttemptOutcome::Succeeded => {
                rep.attempt.error_code.clone()
            }
            _ => None,
        };

        let rate_limit = error_text
            .as_deref()
            .and_then(|err_msg| crate::rate_limit::detect_rate_limit(err_msg, rate_limit_retries));

        if let Some(rate_limit) = rate_limit {
            rate_limit_retries += 1;
            if rate_limit_retries <= MAX_RATE_LIMIT_RETRIES {
                let wait_secs = rate_limit.wait_duration.as_secs();
                let wait_desc = if wait_secs < 60 {
                    format!("{wait_secs}s")
                } else {
                    format!("{}m {}s", wait_secs / 60, wait_secs % 60)
                };
                println!(
                    "\n⏳ [TALOS RateLimit] Detected {} ('{}') on task #{}.",
                    rate_limit.kind.as_str(),
                    rate_limit.matched_signature,
                    task_id
                );
                println!(
                    "⏳ [TALOS RateLimit] Auto-pausing execution for {wait_desc} before retry (attempt {rate_limit_retries}/{MAX_RATE_LIMIT_RETRIES})..."
                );
                let event_data = serde_json::json!({
                    "kind": rate_limit.kind.as_str(),
                    "signature": rate_limit.matched_signature,
                    "wait_duration_secs": wait_secs,
                    "retry_attempt": rate_limit_retries,
                })
                .to_string();
                let _ = database.record_task_event(
                    task_id,
                    "system",
                    "task.rate_limit_backoff",
                    &format!("Rate limit detected; auto-pausing for {wait_desc}"),
                    Some(&event_data),
                );
                tokio::time::sleep(rate_limit.wait_duration).await;
                println!(
                    ">>> [TALOS RateLimit] Auto-pause elapsed. Retrying task #{}...",
                    task_id
                );
                continue;
            }
        }

        return result;
    }
}

async fn execute_writer_task_once(
    _json_output: bool,
    project: &Project,
    database: &mut Database,
    task_id: i64,
    continuation: Option<WriterContinuation>,
    run_config: Option<&ProjectConfig>,
) -> Result<crate::supervisor::WriterExecutionReport> {
    let loaded;
    let config = if let Some(config) = run_config {
        config
    } else {
        loaded = ProjectConfig::load(project)?;
        &loaded
    };
    let configured_adapter = config.adapter_for(AgentRole::Writer);
    if !configured_adapter.is_implemented() {
        bail!(
            "Configured Writer profile {:?} is not implemented; no CLI was launched",
            configured_adapter.as_str()
        );
    }
    let execution_project = execution_project_for_task(project, database, task_id)?;
    let (executable, version) =
        resolve_adapter_executable(configured_adapter, &execution_project.root)?;
    ensure_execution_root_current(&execution_project, database, task_id)?;
    let options = WriterRunOptions {
        adapter: configured_adapter,
        executable,
        agy_version: version,
        model: config.writer_model.clone(),
        timeout: WRITER_TIMEOUT,
        max_concurrent_writers: std::cmp::min(
            3,
            std::cmp::max(
                config.max_concurrent_writers,
                config.orchestration.max_writer_workers,
            ),
        ),
    };
    if let Some(continuation) = continuation {
        continue_writer_attempt_with_options(
            &execution_project,
            database,
            task_id,
            continuation,
            options,
        )
        .await
    } else {
        run_writer_attempt_with_options(&execution_project, database, task_id, options).await
    }
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
    changed_files: Option<&GitStatusPaths>,
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
        "changed_file_count": changed_files.map(|paths| paths.total_count),
        "changed_files_preview": changed_files.map(|paths| &paths.preview),
        "changed_files_truncated": changed_files
            .map(|paths| paths.total_count > paths.preview.len()),
        "changed_path_values_truncated": changed_files
            .map(|paths| paths.preview_values_truncated),
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
                println!("Changed files ({} total):", changed_files.total_count);
                for path in &changed_files.preview {
                    println!("  {}", redact_sensitive(path));
                }
                if changed_files.total_count > changed_files.preview.len() {
                    println!(
                        "Showing the first {} paths; run `git status --short` for the full list.",
                        changed_files.preview.len()
                    );
                }
                if changed_files.preview_values_truncated {
                    println!("Some displayed path names are shortened.");
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

pub(crate) fn config_fingerprint(config: &ProjectConfig) -> Result<String> {
    Ok(Sha256::digest(serde_json::to_vec(config)?)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

async fn resolve_adaptive_orchestration_mode(
    project: &Project,
    database: &mut Database,
    task: &TaskRecord,
    config: &ProjectConfig,
) -> Result<crate::config::OrchestrationMode> {
    println!("\n>>> [TALOS Adaptive] Evaluating task scope for auto-orchestration...");

    let child_subtasks = database.team_subtasks_for_parent(task.id)?;
    if !child_subtasks.is_empty() {
        println!(">>> [TALOS Adaptive] Existing team subtasks detected -> choosing TEAM mode.");
        let _ = database.record_task_event(
            task.id,
            "talos_adaptive",
            "task.adaptive_route",
            "Auto-routed to team mode (subtasks present)",
            None,
        );
        return Ok(crate::config::OrchestrationMode::Team);
    }

    let title_lower = task.title.to_lowercase();
    let crit_lower = task.acceptance_criteria.to_lowercase();
    let is_complex_keyword = title_lower.contains("refactor")
        || title_lower.contains("architect")
        || title_lower.contains("team")
        || title_lower.contains("multi-module")
        || title_lower.contains("migration")
        || crit_lower.contains("refactor")
        || crit_lower.contains("multi-module")
        || crit_lower.contains("architectural");

    let repo_summary = match git_status_paths(project) {
        Ok(paths) => paths.display_preview(),
        Err(_) => "clean repository".to_string(),
    };
    let triage_prompt = crate::team::build_task_triage_prompt(task, &repo_summary);
    let triage_query =
        if let Ok((writer_exe, _)) = resolve_adapter_executable(config.writer, &project.root) {
            crate::supervisor::run_agent_query(
                &project.root,
                config.writer,
                writer_exe,
                config.writer_model.clone(),
                triage_prompt,
                false,
                Duration::from_secs(60),
            )
            .await
        } else {
            Err(anyhow::anyhow!(
                "Writer executable unavailable for triage query"
            ))
        };

    let chosen_mode = match triage_query {
        Ok(resp) => match crate::team::parse_task_triage(&resp) {
            Ok(rec) => {
                let mode = if rec.complexity == crate::team::TaskComplexity::Complex
                    || rec.recommended_depth == crate::team::RecommendedDepth::Deep
                {
                    crate::config::OrchestrationMode::Team
                } else {
                    crate::config::OrchestrationMode::Pair
                };
                println!(
                    ">>> [TALOS Adaptive] Pre-flight triage: {} complexity, {} depth -> choosing {} mode (rationale: {})",
                    rec.complexity.as_str(),
                    rec.recommended_depth.as_str(),
                    mode.as_str().to_uppercase(),
                    rec.rationale
                );
                let _ = database.record_task_event(
                    task.id,
                    "talos_adaptive",
                    "task.adaptive_route",
                    &format!(
                        "Auto-routed to {} mode (triage: {}, {})",
                        mode.as_str(),
                        rec.complexity.as_str(),
                        rec.recommended_depth.as_str()
                    ),
                    None,
                );
                mode
            }
            Err(e) => {
                let mode = if is_complex_keyword {
                    crate::config::OrchestrationMode::Team
                } else {
                    crate::config::OrchestrationMode::Pair
                };
                println!(
                    ">>> [TALOS Adaptive] Note: Triage parse failed ({e:#}); heuristic routed to {} mode.",
                    mode.as_str().to_uppercase()
                );
                let _ = database.record_task_event(
                    task.id,
                    "talos_adaptive",
                    "task.adaptive_route",
                    &format!("Auto-routed to {} mode (heuristic fallback)", mode.as_str()),
                    None,
                );
                mode
            }
        },
        Err(e) => {
            let mode = if is_complex_keyword {
                crate::config::OrchestrationMode::Team
            } else {
                crate::config::OrchestrationMode::Pair
            };
            println!(
                ">>> [TALOS Adaptive] Note: Triage query failed ({e:#}); heuristic routed to {} mode.",
                mode.as_str().to_uppercase()
            );
            let _ = database.record_task_event(
                task.id,
                "talos_adaptive",
                "task.adaptive_route",
                &format!("Auto-routed to {} mode (heuristic fallback)", mode.as_str()),
                None,
            );
            mode
        }
    };

    Ok(chosen_mode)
}

async fn run_one_shot_auto(
    json_output: bool,
    project: &Project,
    database: &mut Database,
    mode_override: Option<crate::config::OrchestrationMode>,
    selected_task: Option<i64>,
    expected_config: Option<&str>,
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
    let config = ProjectConfig::load(project)?;
    if let Some(expected) = expected_config
        && config_fingerprint(&config)? != expected
    {
        bail!(
            "Project settings changed since confirmation. Review the current roles before retrying."
        );
    }
    let mode = mode_override.unwrap_or(config.orchestration.mode);

    let mut tasks_processed = 0;
    loop {
        let task = database.runnable_task(selected_task)?;
        let Some(task) = task else {
            if let Some(id) = selected_task {
                bail!("Task #{id} is no longer runnable. Check History for its status.");
            }
            if tasks_processed == 0 {
                let has_queued_tasks = database.has_queued_tasks()?;
                let waiting_tasks = database.queued_tasks_waiting_on_dependencies()?;
                if json_output {
                    print_json(&json!({
                        "mode": "auto",
                        "orchestration_mode": mode.as_str(),
                        "result": if has_queued_tasks {
                            "waiting_on_dependencies"
                        } else {
                            "no_queued_tasks"
                        },
                        "waiting_task_count": waiting_tasks.waiting_task_count,
                        "waiting_tasks_preview": waiting_tasks.waiting_tasks,
                        "waiting_tasks_truncated": waiting_tasks.waiting_tasks_truncated,
                        "writer_launched": false,
                        "automatic_retry": false,
                    }))?;
                } else if has_queued_tasks {
                    print_waiting_task_summary(&waiting_tasks);
                } else {
                    println!("No queued tasks; no Writer was started.");
                }
            } else {
                println!(
                    "\n>>> [TALOS Auto] Queue completed: successfully finished {} task(s).",
                    tasks_processed
                );
            }
            return Ok(());
        };

        tasks_processed += 1;
        let effective_mode = match mode {
            crate::config::OrchestrationMode::Pair => crate::config::OrchestrationMode::Pair,
            crate::config::OrchestrationMode::Team => crate::config::OrchestrationMode::Team,
            crate::config::OrchestrationMode::Adaptive => {
                resolve_adaptive_orchestration_mode(project, database, &task, &config).await?
            }
        };

        println!(
            "\n=======================================================\n>>> [TALOS Auto ({})] Processing Task #{} ({}): {}\n=======================================================",
            effective_mode.as_str().to_uppercase(),
            task.id,
            task.status.as_str(),
            task.title
        );

        match effective_mode {
            crate::config::OrchestrationMode::Pair => {
                run_pair_task(json_output, project, database, &task, &config).await?;
            }
            crate::config::OrchestrationMode::Team => {
                run_team_task(json_output, project, database, &task, &config).await?;
            }
            crate::config::OrchestrationMode::Adaptive => unreachable!(),
        }
        if selected_task.is_some() {
            return Ok(());
        }
    }
}

fn commit_approved_changes(
    project: &Project,
    database: &Database,
    task: &TaskRecord,
) -> Result<()> {
    let review = database
        .latest_critic_review(task.id)?
        .context("Missing approved revision")?;
    let revision = RevisionSnapshot::capture(&project.root, &review.base_revision)?;
    if revision.revision_id != review.revision_id
        || review.decision != "approve"
        || revision.changed_paths.is_empty()
    {
        bail!(
            "Task verified, but project changes no longer match its approved revision. Inspect the diff before committing."
        );
    }
    let paths: Vec<String> = revision
        .changed_paths
        .iter()
        .map(|path| format!(":(literal){path}"))
        .collect();
    let commit_msg = format!("talos: task #{}: {}", task.id, task.title);
    let mut add = vec!["add", "--"];
    add.extend(paths.iter().map(String::as_str));
    let mut commit = vec!["commit", "--only", "-m", commit_msg.as_str(), "--"];
    commit.extend(paths.iter().map(String::as_str));
    for args in [&add, &commit] {
        let output = git_command(project, args)?;
        if !output.status.success() {
            bail!(
                "Task verified, but Git could not save the changes. Inspect the working tree and Git settings: {}",
                redact_sensitive(&String::from_utf8_lossy(&output.stderr))
            );
        }
    }
    Ok(())
}

fn record_task_result_commit(
    database: &mut Database,
    task_id: i64,
    commit: Option<&str>,
) -> Result<Vec<String>> {
    let review = database
        .latest_critic_review(task_id)?
        .context("Task has no approved review")?;
    if let Some(commit) = commit {
        let data = json!({"base": review.base_revision, "commit": commit}).to_string();
        database.record_task_event(
            task_id,
            "git",
            "task.commit_saved",
            "Task result saved in Git",
            Some(&data),
        )?;
    }
    Ok(review
        .changed_paths_json
        .and_then(|paths| serde_json::from_str(&paths).ok())
        .unwrap_or_default())
}

async fn run_pair_task(
    json_output: bool,
    project: &Project,
    database: &mut Database,
    task: &TaskRecord,
    config: &ProjectConfig,
) -> Result<()> {
    let start_time = SystemTime::now();
    let execution_project = execution_project_for_task(project, database, task.id)?;
    let is_in_progress = task.status == crate::db::TaskStatus::Implementing
        || task.status == crate::db::TaskStatus::Reviewing
        || task.status == crate::db::TaskStatus::Verifying;

    let before = git_status_paths(project)?;
    if !before.is_empty() && !is_in_progress {
        bail!(
            "`up --auto` requires a clean Git worktree; inspect or commit these existing paths first: {}",
            before.display_preview()
        );
    }
    let execution_before = git_status_paths(&execution_project)?;
    if !execution_before.is_empty() && !is_in_progress {
        bail!(
            "`up --auto` requires a clean task execution worktree; inspect or commit these existing paths first: {}",
            execution_before.display_preview()
        );
    }

    if task.status != crate::db::TaskStatus::Verifying
        && task.status != crate::db::TaskStatus::Reviewing
    {
        let execution = execute_writer_task(
            json_output,
            &execution_project,
            database,
            task.id,
            None,
            Some(config),
        )
        .await;
        let changed_files = git_status_paths(&execution_project)?;
        let report = execution?;
        print_writer_report(json_output, &report, Some(&changed_files))?;
        ensure_writer_succeeded(&report)?;
    }

    let has_worktree = database.task_worktree(task.id)?.is_some();
    if has_worktree && database.task_worktree_integration(task.id)?.is_none() {
        println!(
            "\n>>> [TALOS Auto] Integrating task #{} worktree into primary project before review...",
            task.id
        );
        match crate::worktree::integrate(project, database, task.id) {
            Ok(_) => {
                let commit_msg = format!("talos: task #{}: {}", task.id, task.title);
                let _ = git_command(project, &["add", "."]);
                let _ = git_command(project, &["commit", "-m", &commit_msg]);
                println!(
                    ">>> [TALOS Auto] Task #{} worktree patch integrated and committed in primary project.",
                    task.id
                );
            }
            Err(e) => {
                println!(">>> [TALOS Auto] Notice during worktree integration: {e}");
            }
        }
    }

    let review_project = if has_worktree {
        project
    } else {
        &execution_project
    };

    // Step 2: Critic Review & Automated Fix Loop
    let mut deferred_findings = Vec::new();
    if task.status != crate::db::TaskStatus::Verifying {
        const MAX_REVIEW_FIX_CYCLES: usize = 3;
        let mut cycles = 0;
        let mut previous_patch_digest: Option<String> = None;
        loop {
            cycles += 1;
            println!(
                "\n>>> [TALOS Auto] Starting Critic review for task #{} (round {cycles})...",
                task.id
            );
            let critic_report =
                execute_critic_review(review_project, database, task.id, Some(config)).await?;
            print_critic_report(json_output, &critic_report)?;

            let current_patch_digest = critic_report
                .review
                .patch_sha256
                .clone()
                .unwrap_or_else(|| critic_report.review.revision_id.clone());

            if critic_report.review.decision == "approve" {
                println!(
                    "\n>>> [TALOS Auto] Critic approved changes for task #{}.",
                    task.id
                );
                for finding in critic_report.findings {
                    if !finding.is_blocking() {
                        deferred_findings.push(finding);
                    }
                }
                break;
            }

            // Anti-stagnation check: If cycles >= 2 and diff patch is identical to previous round
            if cycles >= 2
                && crate::revision::is_diff_stagnant(
                    previous_patch_digest.as_deref(),
                    &current_patch_digest,
                )
            {
                println!(
                    "\n>>> [TALOS Auto] Stagnation detected: Writer produced identical diff without addressing Critic findings."
                );
                let _ = database.record_task_event(
                    task.id,
                    "talos",
                    "task.stagnation_detected",
                    "Stagnation detected: Writer produced identical diff without addressing Critic findings",
                    Some(&current_patch_digest),
                );
                if let Ok(Some(current_task)) = database.get_task(task.id)
                    && current_task.status != crate::db::TaskStatus::Implementing
                {
                    let _ = database.transition_task(
                        task.id,
                        crate::db::TaskStatus::Implementing,
                        "talos",
                        "Stagnation detected: Writer produced identical diff without addressing Critic findings",
                    );
                }
                bail!(
                    "Stagnation detected on task #{}: Writer produced identical diff across fix rounds without addressing Critic findings",
                    task.id
                );
            }
            previous_patch_digest = Some(current_patch_digest);

            if cycles >= MAX_REVIEW_FIX_CYCLES {
                println!(
                    "\n>>> [TALOS Auto] Critic requested changes, but maximum review/fix rounds ({MAX_REVIEW_FIX_CYCLES}) reached. Task remains in implementing for review."
                );
                return Ok(());
            }

            println!(
                "\n>>> [TALOS Auto] Critic requested changes. Launching Writer fix iteration..."
            );
            let fix_execution = execute_writer_task(
                json_output,
                &execution_project,
                database,
                task.id,
                None,
                Some(config),
            )
            .await?;
            let fix_changed = git_status_paths(&execution_project)?;
            print_writer_report(json_output, &fix_execution, Some(&fix_changed))?;
            ensure_writer_succeeded(&fix_execution)?;
        }
    }

    // Step 3: Project Verification / Tests
    let checks = database.task_verification_checks(task.id)?;
    if checks.is_empty() && !config.checks.is_empty() {
        println!(
            "\n>>> [TALOS Auto] Attaching {} configured project check(s) to task #{}...",
            config.checks.len(),
            task.id
        );
        database.add_task_verification_checks(task.id, &config.checks)?;
    }

    println!(
        "\n>>> [TALOS Auto] Running project verification for task #{}...",
        task.id
    );
    if let Err(err) = run_task_verification(json_output, project, database, task.id).await {
        println!(
            "\n>>> [TALOS Auto] Task #{} verification failed: {err}",
            task.id
        );
        let _ = database.transition_task(
            task.id,
            crate::db::TaskStatus::Implementing,
            "talos",
            &format!("Verification failed: {err}"),
        );
        let event_data = serde_json::json!({
            "error": err.to_string(),
        })
        .to_string();
        let _ = database.record_task_event(
            task.id,
            "talos",
            "task.verification_failed",
            &format!("Verification checks failed: {err}"),
            Some(&event_data),
        );
        return Err(err);
    }
    println!(
        "\n>>> [TALOS Auto] Task #{} verified and completed successfully!",
        task.id
    );

    // Step 4: Finalize Worktree Integration (if task executed in a worktree)
    if has_worktree {
        println!(
            "\n>>> [TALOS Auto] Finalizing task #{} worktree...",
            task.id
        );
        match crate::worktree::finalize(project, database, task.id) {
            Ok(_) => {
                println!(
                    ">>> [TALOS Auto] Task #{} worktree finalized successfully!",
                    task.id
                );
            }
            Err(e) => {
                println!(">>> [TALOS Auto] Worktree finalization note: {e}");
            }
        }
    }

    // Commit task changes locally if primary worktree is dirty
    let dirty = git_status_paths(project)?;
    if !dirty.is_empty() {
        commit_approved_changes(project, database, task)?;
        println!(
            ">>> [TALOS Auto] Committed task #{} changes locally in Git.",
            task.id
        );
    }

    // Step 5: Optional GitHub Draft PR creation (if enabled by policy)
    if let Some(policy) = &config.github
        && policy.allow_push
        && policy.allow_pull_request
    {
        println!(
            "\n>>> [TALOS Auto] GitHub publication policy is enabled. Preparing Draft PR for task #{}...",
            task.id
        );
        let branch = format!("{}task-{}", policy.feature_branch_prefix, task.id);
        let _ = git_command(project, &["checkout", "-B", &branch]);
        let req = GithubPublishRequest {
            branch: &branch,
            title: &format!("talos: task #{}: {}", task.id, task.title),
            body: &format!(
                "Autonomous task #{} completed by TALOS.\n\nCritic approved and verified.",
                task.id
            ),
            confirm_push: true,
            confirm_pr: true,
        };
        if let Err(e) = run_github_publish(json_output, project, database, req).await {
            println!(">>> [TALOS Auto] Note: GitHub publication was skipped or failed: {e}");
        }
    }

    let commit_hash = git_head(&project.root).ok().flatten();
    let changed = record_task_result_commit(database, task.id, commit_hash.as_deref())?;

    if deferred_findings.is_empty()
        && let Ok(Some(latest_review)) = database.latest_critic_review(task.id)
        && let Ok(critic_resp) =
            serde_json::from_str::<crate::review::CriticResponse>(&latest_review.result_json)
    {
        for finding in critic_resp.findings {
            if !finding.is_blocking() {
                deferred_findings.push(finding);
            }
        }
    }

    if !deferred_findings.is_empty() {
        let payload = serde_json::to_string(&deferred_findings).ok();
        let _ = database.record_task_event(
            task.id,
            "critic",
            "task.backlog_curated",
            &format!(
                "Curated {} deferred finding(s) for task backlog",
                deferred_findings.len()
            ),
            payload.as_deref(),
        );
    }

    let summary_data = crate::summary::TaskSummaryData {
        task,
        mode: crate::config::OrchestrationMode::Pair,
        writer_adapter: config.writer,
        writer_model: config.writer_model.as_deref(),
        critic_adapter: config.critic,
        critic_model: config.critic_model.as_deref(),
        start_time,
        total_pause_duration: Duration::from_secs(0),
        decisions: vec![format!(
            "Implemented task requirements: {}",
            task.acceptance_criteria
        )],
        checks_passed: config.checks.iter().map(|c| c.id.clone()).collect(),
        modified_files: changed,
        commit_hash,
        deferred_findings,
    };
    if let Ok(artifact) = crate::summary::write_task_summary(&project.root, &summary_data) {
        println!("\n>>> [TALOS Auto] 📄 Deliverable Summary Artifact written to: {artifact}");
    }

    Ok(())
}

async fn run_team_task(
    json_output: bool,
    project: &Project,
    database: &mut Database,
    task: &TaskRecord,
    config: &ProjectConfig,
) -> Result<()> {
    println!("\n>>> [TALOS Team] ===============================================");
    println!(
        ">>> [TALOS Team] Orchestrating Task #{}: {}",
        task.id, task.title
    );
    let delegation = crate::team::detect_delegation(config.writer, config.orchestration.delegation);
    println!(
        ">>> [TALOS Team] Mode: TEAM | Delegation: {} | Writer: {} | Critic: {}",
        delegation.as_str(),
        config.writer.as_str(),
        config.critic.as_str()
    );
    println!(
        ">>> [TALOS Team] Limits: max_writer_workers={}, max_critic_workers={}, max_review_rounds={}, max_help_attempts={}",
        config.orchestration.max_writer_workers,
        config.orchestration.max_critic_workers,
        config.orchestration.max_review_rounds,
        config.orchestration.max_help_attempts
    );
    println!(">>> [TALOS Team] ===============================================");

    let start_time = SystemTime::now();
    let mut architectural_decisions = Vec::new();

    let before = git_status_paths(project)?;
    let is_in_progress = task.status == crate::db::TaskStatus::Implementing
        || task.status == crate::db::TaskStatus::Reviewing
        || task.status == crate::db::TaskStatus::Verifying;
    if !before.is_empty() && !is_in_progress {
        bail!(
            "`up --auto --mode team` requires a clean Git worktree; inspect or commit these existing paths first: {}",
            before.display_preview()
        );
    }
    let initial_base_revision = clean_base_commit(&project.root)?;

    let (writer_exe, _) = resolve_adapter_executable(config.writer, &project.root)?;

    // Phase 0: Recommendation-based Pre-Flight Task Triage
    println!("\n>>> [TALOS Team] Phase 0: Pre-flight task triage & depth analysis...");
    let repo_summary = match git_status_paths(project) {
        Ok(paths) => paths.display_preview(),
        Err(_) => "clean repository".to_string(),
    };
    let triage_prompt = crate::team::build_task_triage_prompt(task, &repo_summary);
    let triage_response = crate::supervisor::run_agent_query(
        &project.root,
        config.writer,
        writer_exe.clone(),
        config.writer_model.clone(),
        triage_prompt,
        false,
        Duration::from_secs(60),
    )
    .await;

    let triage_rec = match triage_response {
        Ok(resp) => match crate::team::parse_task_triage(&resp) {
            Ok(rec) => {
                println!(
                    ">>> [TALOS Team] Triage Recommendation: complexity={}, depth={}, focus={:?}",
                    rec.complexity.as_str(),
                    rec.recommended_depth.as_str(),
                    rec.suggested_focus
                );
                println!(">>> [TALOS Team] Triage Rationale: {}", rec.rationale);
                let payload = serde_json::to_string(&rec).ok();
                let _ = database.record_task_event(
                    task.id,
                    "talos_triage",
                    "task.triage",
                    &format!(
                        "Triage recommendation: {} complexity, {} depth",
                        rec.complexity.as_str(),
                        rec.recommended_depth.as_str()
                    ),
                    payload.as_deref(),
                );
                architectural_decisions.push(format!(
                    "Pre-flight triage: {} complexity, {} depth (rationale: {})",
                    rec.complexity.as_str(),
                    rec.recommended_depth.as_str(),
                    rec.rationale
                ));
                Some(rec)
            }
            Err(e) => {
                println!(
                    ">>> [TALOS Team] Note: Pre-flight triage parse failed ({e:#}); proceeding with standard depth."
                );
                None
            }
        },
        Err(e) => {
            println!(
                ">>> [TALOS Team] Note: Pre-flight triage query failed ({e:#}); proceeding with standard depth."
            );
            None
        }
    };

    // Step 1: Writer Lead Planning & Decomposition
    println!("\n>>> [TALOS Team] Phase 1: Writer Lead analyzing requirements & planning...");
    let plan_prompt = crate::team::build_writer_lead_plan_prompt(
        task,
        config.orchestration.max_writer_workers,
        triage_rec.as_ref(),
    );
    let plan_response = crate::supervisor::run_agent_query(
        &project.root,
        config.writer,
        writer_exe.clone(),
        config.writer_model.clone(),
        plan_prompt,
        false,
        Duration::from_secs(120),
    )
    .await;

    let lead_plan = match plan_response {
        Ok(resp) => match crate::team::parse_writer_lead_plan(&resp) {
            Ok(plan) => Some(plan),
            Err(e) => {
                println!(
                    ">>> [TALOS Team] Note: Writer Lead plan parsing failed ({e:#}); defaulting to self-implementation."
                );
                None
            }
        },
        Err(e) => {
            println!(
                ">>> [TALOS Team] Note: Writer Lead plan query failed ({e:#}); defaulting to self-implementation."
            );
            None
        }
    };

    let (action, subtasks) = match lead_plan {
        Some(plan) => {
            if !plan.reasoning.is_empty() {
                architectural_decisions.push(format!("Writer Lead approach: {}", plan.reasoning));
            }
            for sub in &plan.subtasks {
                architectural_decisions.push(format!(
                    "Decomposed subtask '{}' with allowed paths: {:?}",
                    sub.title, sub.allowed_paths
                ));
            }
            (plan.action, plan.subtasks)
        }
        None => {
            architectural_decisions.push("Writer Lead selected self-implementation".to_string());
            ("self".to_owned(), Vec::new())
        }
    };

    let has_subtasks = action == "delegate" && !subtasks.is_empty();

    if has_subtasks {
        let workers_to_run = std::cmp::min(subtasks.len(), config.orchestration.max_writer_workers);
        println!(
            ">>> [TALOS Team] Writer Lead decided: DELEGATE to {} Writer Worker(s).",
            workers_to_run
        );
        let plan_json = json!({
            "action": "delegate",
            "worker_count": workers_to_run,
        })
        .to_string();
        let _ = database.record_task_event(
            task.id,
            "writer_lead",
            "task.team_planning",
            "Writer Lead decomposed task into worker subtasks",
            Some(&plan_json),
        );

        let mut subtask_items = Vec::new();
        for (idx, subtask) in subtasks.into_iter().take(workers_to_run).enumerate() {
            println!(
                "\n>>> [TALOS Team] Creating Worker Subtask #{} (Worker {}): '{}' [Scope: {:?}]",
                idx + 1,
                subtask.id,
                subtask.title,
                subtask.allowed_paths
            );

            let (child_task, contract) = database.add_child_task(
                task.id,
                &subtask.title,
                &subtask.acceptance_criteria,
                &subtask.allowed_paths,
                30 * 60 * 1000,
            )?;

            database.add_team_subtask(
                task.id,
                child_task.id,
                "writer_worker",
                config.writer.as_str(),
                delegation.as_str(),
                "queued",
                &[],
            )?;

            if let Ok((_managed_root, worktree_path, branch_name)) =
                crate::worktree::managed_identity(&project.root, child_task.id)
            {
                if worktree_path.exists() {
                    let _ = std::fs::remove_dir_all(&worktree_path);
                }
                let _ = git_command(project, &["worktree", "prune"]);
                let _ = git_command(project, &["branch", "-D", &branch_name]);
            }
            let worktree_record = crate::worktree::create(project, database, child_task.id)?;
            subtask_items.push((child_task, subtask, contract, worktree_record));
        }

        // Parallel Execution if multiple workers are configured
        let mut worker_exec_results: Vec<(
            crate::db::TaskRecord,
            crate::team::SubtaskPlan,
            crate::db::ChildTaskContract,
            crate::db::TaskWorktreeRecord,
            Result<crate::supervisor::WriterExecutionReport>,
        )> = Vec::new();
        if subtask_items.len() == 2 {
            let item1 = subtask_items.remove(0);
            let item2 = subtask_items.remove(0);
            let (child1, plan1, contract1, wt1) = item1;
            let (child2, plan2, contract2, wt2) = item2;

            database.update_team_subtask_status(child1.id, "in_progress", None, None)?;
            database.update_team_subtask_status(child2.id, "in_progress", None, None)?;

            println!(
                "\n>>> [TALOS Team] Dispatching 2 Writer Workers IN PARALLEL into managed worktrees..."
            );
            let project_clone1 = project.clone();
            let project_clone2 = project.clone();
            let child1_id = child1.id;
            let child2_id = child2.id;
            let title1 = child1.title.clone();
            let title2 = child2.title.clone();
            let wt_path1 = wt1.worktree_path.clone();
            let wt_path2 = wt2.worktree_path.clone();

            let fut1 = async move {
                let mut db = Database::open(&project_clone1)?;
                let start_instant = Instant::now();
                let start_time = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis();
                println!(
                    ">>> [TALOS Team] [Worker 1] STARTED at: {} ms | Subtask #{}: '{}' (Worktree: {})",
                    start_time, child1_id, title1, wt_path1
                );
                let exec = execute_writer_task(
                    json_output,
                    &project_clone1,
                    &mut db,
                    child1_id,
                    None,
                    Some(config),
                )
                .await;
                let end_time = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis();
                println!(
                    ">>> [TALOS Team] [Worker 1] FINISHED at: {} ms (Duration: {:?})",
                    end_time,
                    start_instant.elapsed()
                );
                Ok::<_, anyhow::Error>((child1_id, exec, start_time, end_time))
            };

            let fut2 = async move {
                let mut db = Database::open(&project_clone2)?;
                let start_instant = Instant::now();
                let start_time = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis();
                println!(
                    ">>> [TALOS Team] [Worker 2] STARTED at: {} ms | Subtask #{}: '{}' (Worktree: {})",
                    start_time, child2_id, title2, wt_path2
                );
                let exec = execute_writer_task(
                    json_output,
                    &project_clone2,
                    &mut db,
                    child2_id,
                    None,
                    Some(config),
                )
                .await;
                let end_time = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis();
                println!(
                    ">>> [TALOS Team] [Worker 2] FINISHED at: {} ms (Duration: {:?})",
                    end_time,
                    start_instant.elapsed()
                );
                Ok::<_, anyhow::Error>((child2_id, exec, start_time, end_time))
            };

            let (res1, res2) = tokio::join!(fut1, fut2);
            let r1 = match res1 {
                Ok((_, exec, _, _)) => exec,
                Err(e) => Err(e),
            };
            let r2 = match res2 {
                Ok((_, exec, _, _)) => exec,
                Err(e) => Err(e),
            };
            worker_exec_results.push((child1, plan1, contract1, wt1, r1));
            worker_exec_results.push((child2, plan2, contract2, wt2, r2));
        } else {
            for (child_task, subtask, contract, wt) in subtask_items {
                database.update_team_subtask_status(child_task.id, "in_progress", None, None)?;
                println!(
                    ">>> [TALOS Team] Dispatching Writer Worker for subtask #{} ('{}')...",
                    child_task.id, child_task.title
                );
                let exec = execute_writer_task(
                    json_output,
                    project,
                    database,
                    child_task.id,
                    None,
                    Some(config),
                )
                .await;
                worker_exec_results.push((child_task, subtask, contract, wt, exec));
            }
        }

        // Evaluate Worker outcomes and handle stuck/failing workers
        let mut completed_workers = Vec::new();
        let mut simulated_error_triggered = false;
        for (child_task, subtask, contract, wt, mut exec_result) in worker_exec_results {
            let inject_error = !simulated_error_triggered
                && std::env::var("TALOS_SIMULATE_WORKER_ERROR")
                    .map(|val| val == "1" || val == child_task.id.to_string())
                    .unwrap_or(false);

            let succeeded = if inject_error {
                simulated_error_triggered = true;
                exec_result = Err(anyhow::anyhow!(
                    "Worker process terminated unexpectedly (signal: SIGKILL, exit code: 137)"
                ));
                false
            } else {
                match &exec_result {
                    Ok(rep) => rep.attempt.state == "succeeded",
                    Err(_) => false,
                }
            };

            if succeeded {
                let rep = exec_result.unwrap();
                println!(
                    ">>> [TALOS Team] Writer Worker finished subtask #{} successfully.",
                    child_task.id
                );
                database.update_team_subtask_status(
                    child_task.id,
                    "completed",
                    rep.response.as_deref(),
                    None,
                )?;
                completed_workers.push((child_task, subtask, contract, wt));
            } else {
                let mut help_attempts = 0;
                let max_help = config.orchestration.max_help_attempts;
                loop {
                    help_attempts += 1;
                    let raw_err =
                        match &exec_result {
                            Err(e) => format!("{e:#}"),
                            Ok(rep) => rep.attempt.error_code.clone().unwrap_or_else(|| {
                                "Worker finished with non-success state".to_owned()
                            }),
                        };
                    let err_msg = crate::summary::truncate_for_token_economy(&raw_err, 4096);
                    println!(
                        "\n>>> [TALOS Team] Worker encountered issue on subtask #{} (help attempt {}/{}): {}",
                        child_task.id, help_attempts, max_help, err_msg
                    );

                    if help_attempts > max_help {
                        println!(
                            ">>> [TALOS Team] Maximum help attempts ({}) exceeded for subtask #{}; marking blocked.",
                            max_help, child_task.id
                        );
                        database.update_team_subtask_status(
                            child_task.id,
                            "blocked",
                            None,
                            Some(&err_msg),
                        )?;
                        bail!(
                            "Worker subtask #{} blocked after {} help attempts",
                            child_task.id,
                            max_help
                        );
                    }

                    println!(
                        ">>> [TALOS Team] Writer Lead analyzing failure and providing assistance..."
                    );
                    let help_prompt = crate::team::build_worker_help_prompt(
                        task,
                        &subtask.id,
                        &subtask.title,
                        &subtask.allowed_paths,
                        &err_msg,
                        help_attempts,
                        max_help,
                    );
                    let lead_help_response = crate::supervisor::run_agent_query(
                        &project.root,
                        config.writer,
                        writer_exe.clone(),
                        config.writer_model.clone(),
                        help_prompt,
                        false,
                        Duration::from_secs(120),
                    )
                    .await;

                    let decision = lead_help_response
                        .ok()
                        .and_then(|r| crate::team::parse_worker_help_decision(&r).ok());

                    match decision {
                        Some(d) if d.action == "takeover" => {
                            println!(
                                ">>> [TALOS Team] Writer Lead assistance decision: TAKEOVER. Lead takes over subtask #{}.",
                                child_task.id
                            );
                            let current = database.get_task(child_task.id)?;
                            if let Some(t) = current
                                && t.status != crate::db::TaskStatus::Queued
                            {
                                database.transition_task(
                                    child_task.id,
                                    crate::db::TaskStatus::Queued,
                                    "writer_lead",
                                    "Writer Lead takeover for blocked subtask",
                                )?;
                            }
                            let wt_path = Path::new(&wt.worktree_path);
                            if wt_path.exists() {
                                let _ = std::process::Command::new("git")
                                    .args([
                                        "-C",
                                        wt.worktree_path.as_str(),
                                        "reset",
                                        "--hard",
                                        "HEAD",
                                    ])
                                    .output();
                                let _ = std::process::Command::new("git")
                                    .args(["-C", wt.worktree_path.as_str(), "clean", "-fd"])
                                    .output();
                            }
                            let takeover_exec = execute_writer_task(
                                json_output,
                                project,
                                database,
                                child_task.id,
                                None,
                                Some(config),
                            )
                            .await?;
                            ensure_writer_succeeded(&takeover_exec)?;
                            database.update_team_subtask_status(
                                child_task.id,
                                "completed",
                                Some("Writer Lead takeover successful"),
                                None,
                            )?;
                            completed_workers.push((child_task, subtask, contract, wt));
                            break;
                        }
                        Some(d) if d.action == "block" => {
                            println!(
                                ">>> [TALOS Team] Writer Lead assistance decision: BLOCK. Reason: {}",
                                d.reason
                            );
                            database.update_team_subtask_status(
                                child_task.id,
                                "blocked",
                                None,
                                Some(&d.reason),
                            )?;
                            bail!(
                                "Writer Lead marked subtask #{} as blocked: {}",
                                child_task.id,
                                d.reason
                            );
                        }
                        Some(d) => {
                            println!(
                                ">>> [TALOS Team] Writer Lead assistance decision: RETRY WITH GUIDANCE: {:?}",
                                d.guidance
                            );
                            let current = database.get_task(child_task.id)?;
                            if let Some(t) = current
                                && t.status != crate::db::TaskStatus::Queued
                            {
                                database.transition_task(
                                    child_task.id,
                                    crate::db::TaskStatus::Queued,
                                    "writer_lead",
                                    "Writer Lead retry with guidance for subtask",
                                )?;
                            }
                            let wt_path = Path::new(&wt.worktree_path);
                            if wt_path.exists() {
                                let _ = std::process::Command::new("git")
                                    .args([
                                        "-C",
                                        wt.worktree_path.as_str(),
                                        "reset",
                                        "--hard",
                                        "HEAD",
                                    ])
                                    .output();
                                let _ = std::process::Command::new("git")
                                    .args(["-C", wt.worktree_path.as_str(), "clean", "-fd"])
                                    .output();
                            }
                            let _ = database.record_task_event(
                                child_task.id,
                                "writer_lead",
                                "task.team_worker_helped",
                                "Writer Lead provided guidance to worker",
                                d.guidance.as_deref(),
                            );
                            exec_result = execute_writer_task(
                                json_output,
                                project,
                                database,
                                child_task.id,
                                None,
                                Some(config),
                            )
                            .await;
                            if let Ok(rep) = &exec_result
                                && rep.attempt.state == "succeeded"
                            {
                                println!(
                                    ">>> [TALOS Team] Writer Worker finished subtask #{} successfully after guidance.",
                                    child_task.id
                                );
                                database.update_team_subtask_status(
                                    child_task.id,
                                    "completed",
                                    rep.response.as_deref(),
                                    None,
                                )?;
                                completed_workers.push((child_task, subtask, contract, wt));
                                break;
                            }
                        }
                        None => {
                            println!(
                                ">>> [TALOS Team] Writer Lead provided default corrective guidance; retrying worker..."
                            );
                            let current = database.get_task(child_task.id)?;
                            if let Some(t) = current
                                && t.status != crate::db::TaskStatus::Queued
                            {
                                database.transition_task(
                                    child_task.id,
                                    crate::db::TaskStatus::Queued,
                                    "writer_lead",
                                    "Writer Lead retry with guidance for subtask",
                                )?;
                            }
                            let wt_path = Path::new(&wt.worktree_path);
                            if wt_path.exists() {
                                let _ = std::process::Command::new("git")
                                    .args([
                                        "-C",
                                        wt.worktree_path.as_str(),
                                        "reset",
                                        "--hard",
                                        "HEAD",
                                    ])
                                    .output();
                                let _ = std::process::Command::new("git")
                                    .args(["-C", wt.worktree_path.as_str(), "clean", "-fd"])
                                    .output();
                            }
                            exec_result = execute_writer_task(
                                json_output,
                                project,
                                database,
                                child_task.id,
                                None,
                                Some(config),
                            )
                            .await;
                            if let Ok(rep) = &exec_result
                                && rep.attempt.state == "succeeded"
                            {
                                database.update_team_subtask_status(
                                    child_task.id,
                                    "completed",
                                    rep.response.as_deref(),
                                    None,
                                )?;
                                completed_workers.push((child_task, subtask, contract, wt));
                                break;
                            }
                        }
                    }
                }
            }
        }

        println!("\n>>> [TALOS Team] Phase 2: Writer Lead integrating worker contributions...");
        for (child_task, _subtask, contract, wt) in &completed_workers {
            let wt_path = Path::new(&wt.worktree_path);
            if wt_path.exists() {
                let snapshot =
                    crate::revision::RevisionSnapshot::capture(wt_path, &wt.base_commit)?;
                for path in &snapshot.changed_paths {
                    if !contract.permits_path(path) {
                        bail!(
                            "Subtask #{} modified unauthorized path {path:?}",
                            child_task.id
                        );
                    }
                    let src = wt_path.join(path);
                    let dst = project.root.join(path);
                    if let Some(parent) = dst.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    if src.is_file() {
                        std::fs::copy(&src, &dst)?;
                    }
                }
                database.complete_team_subtask_integration(
                    child_task.id,
                    "Integrated by Writer Lead into parent task workspace",
                )?;
                println!(
                    ">>> [TALOS Team] Writer Lead verified and integrated changes from Subtask #{}: {:?}",
                    child_task.id, snapshot.changed_paths
                );
            }
        }
        let _ = database.record_task_event(
            task.id,
            "writer_lead",
            "task.team_integration",
            "Writer Lead integrated all worker subtasks",
            None,
        );
        println!(">>> [TALOS Team] Writer Lead integration complete.");

        let prompt_sha256 = Sha256::digest(task.title.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        let lead_attempt = database.begin_writer_attempt_for_root(
            task.id,
            &prompt_sha256,
            std::process::id(),
            1,
            &project.root,
        )?;
        database.record_writer_base_revision(lead_attempt.id, &initial_base_revision)?;
        database.mark_writer_attempt_running_with_identity(
            lead_attempt.id,
            std::process::id(),
            None,
        )?;
        database.finish_writer_attempt(
            lead_attempt.id,
            crate::db::WriterAttemptOutcome::Succeeded,
            Some("writer-lead-integrated"),
            Some(0),
            Some("SUCCESS"),
            None,
        )?;
    } else {
        println!(
            ">>> [TALOS Team] Writer Lead decided: SELF implementation (direct execution without extra workers)."
        );
        let _ = database.record_task_event(
            task.id,
            "writer_lead",
            "task.team_planning",
            "Writer Lead chose direct execution",
            None,
        );
        let execution =
            execute_writer_task(json_output, project, database, task.id, None, Some(config))
                .await?;
        let changed_files = git_status_paths(project)?;
        print_writer_report(json_output, &execution, Some(&changed_files))?;
        ensure_writer_succeeded(&execution)?;
    }

    // Step 2: Critic Team Review & Deliberation Loop
    println!("\n>>> [TALOS Team] Phase 3: Critic Team independent review & evaluation...");
    let (critic_exe, _) = resolve_adapter_executable(config.critic, &project.root)?;

    // Optional Critic Worker specialized review
    let mut _worker_findings_summary = None;
    if config.orchestration.max_critic_workers > 0 {
        println!(
            ">>> [TALOS Team] Deploying Critic Worker for specialized security and correctness review..."
        );
        if let Ok(Some(writer)) = database.latest_writer_attempt(task.id) {
            let base_rev = writer.base_revision.as_deref().unwrap_or("HEAD");
            if let Ok(snapshot) =
                crate::revision::RevisionSnapshot::capture(&project.root, base_rev)
            {
                let focus_area = if let Some(rec) = &triage_rec {
                    if !rec.suggested_focus.is_empty() {
                        format!(
                            "{}, edge case handling, and requirement compliance",
                            rec.suggested_focus.join(", ")
                        )
                    } else {
                        "Security, edge case handling, and requirement compliance".to_string()
                    }
                } else {
                    "Security, edge case handling, and requirement compliance".to_string()
                };
                let worker_prompt =
                    crate::team::build_critic_worker_prompt(task, &snapshot.patch, &focus_area);
                let worker_resp = crate::supervisor::run_agent_query(
                    &project.root,
                    config.critic,
                    critic_exe.clone(),
                    config.critic_model.clone(),
                    worker_prompt,
                    true,
                    Duration::from_secs(120),
                )
                .await;

                if let Ok(resp) = worker_resp {
                    let _ = database.add_team_subtask(
                        task.id,
                        task.id,
                        "critic_worker",
                        config.critic.as_str(),
                        "external",
                        "completed",
                        &[],
                    );
                    let _ = database.update_team_subtask_status(
                        task.id,
                        "completed",
                        Some(&resp),
                        None,
                    );
                    println!(">>> [TALOS Team] Critic Worker completed specialized review.");
                    _worker_findings_summary = Some(resp);
                }
            }
        }
    }

    // Critic Lead consolidated review & deliberation loop
    let max_rounds = config.orchestration.max_review_rounds;
    let mut round = 0;
    let mut team_deferred_findings = Vec::new();
    let mut previous_patch_digest: Option<String> = None;
    loop {
        round += 1;
        println!(
            "\n>>> [TALOS Team] Critic Lead conducting consolidated review (Round {}/{})...",
            round, max_rounds
        );
        let critic_report = execute_critic_review(project, database, task.id, Some(config)).await?;
        print_critic_report(json_output, &critic_report)?;

        let current_patch_digest = critic_report
            .review
            .patch_sha256
            .clone()
            .unwrap_or_else(|| critic_report.review.revision_id.clone());

        if critic_report.review.decision == "approve" {
            println!(
                "\n>>> [TALOS Team] Critic Lead VERDICT: APPROVED (Round {}). Solution verified by engineering review.",
                round
            );
            let _ = database.record_task_event(
                task.id,
                "critic_lead",
                "task.team_verdict",
                "Critic Lead approved solution",
                Some(&critic_report.review.summary),
            );
            for finding in critic_report.findings {
                if !finding.is_blocking() {
                    team_deferred_findings.push(finding);
                }
            }
            break;
        }

        // Anti-stagnation check: If round >= 2 and diff patch is identical to previous round
        if round >= 2
            && crate::revision::is_diff_stagnant(
                previous_patch_digest.as_deref(),
                &current_patch_digest,
            )
        {
            println!(
                "\n>>> [TALOS Team] Stagnation detected: Writer produced identical diff without addressing Critic findings."
            );
            let _ = database.record_task_event(
                task.id,
                "talos",
                "task.stagnation_detected",
                "Stagnation detected: Writer produced identical diff without addressing Critic findings",
                Some(&current_patch_digest),
            );
            if let Ok(Some(current_task)) = database.get_task(task.id)
                && current_task.status != crate::db::TaskStatus::Implementing
            {
                let _ = database.transition_task(
                    task.id,
                    crate::db::TaskStatus::Implementing,
                    "talos",
                    "Stagnation detected: Writer produced identical diff without addressing Critic findings",
                );
            }
            bail!(
                "Stagnation detected on task #{}: Writer produced identical diff across fix rounds without addressing Critic findings",
                task.id
            );
        }
        previous_patch_digest = Some(current_patch_digest);

        if round >= max_rounds {
            println!(
                "\n>>> [TALOS Team] Maximum review rounds ({}) reached without resolution. Halting for user visibility.",
                max_rounds
            );
            bail!(
                "Critic Lead review rounds ({max_rounds}) exhausted for task #{}",
                task.id
            );
        }

        println!(
            "\n>>> [TALOS Team] Critic Lead requested changes. Writer Lead analyzing remarks and deliberating..."
        );
        let findings_str = serde_json::to_string(&critic_report.findings)?;
        let _ = database.record_task_event(
            task.id,
            "critic_lead",
            "task.team_review_feedback",
            "Critic Lead requested modifications",
            Some(&findings_str),
        );

        // Writer Lead deliberation & rebuttal query
        let rebuttal_prompt =
            crate::team::build_writer_lead_rebuttal_prompt(task, &findings_str, round, max_rounds);
        let rebuttal_resp = crate::supervisor::run_agent_query(
            &project.root,
            config.writer,
            writer_exe.clone(),
            config.writer_model.clone(),
            rebuttal_prompt,
            false,
            Duration::from_secs(120),
        )
        .await;

        if let Ok(resp) = rebuttal_resp {
            println!(">>> [TALOS Team] Writer Lead provided technical deliberation & response.");
            let _ = database.record_task_event(
                task.id,
                "writer_lead",
                "task.team_deliberation",
                "Writer Lead deliberation and technical justification",
                Some(&resp),
            );
            let summary_line = resp
                .trim()
                .lines()
                .next()
                .unwrap_or("Deliberated technical response")
                .to_string();
            architectural_decisions.push(format!(
                "Writer Lead deliberation (round {round}): {summary_line}"
            ));

            let _ = database.add_team_subtask(
                task.id,
                task.id,
                "writer_lead_rebuttal",
                config.writer.as_str(),
                "external",
                "completed",
                &[],
            );
            let _ = database.update_team_subtask_status(task.id, "completed", Some(&resp), None);
        }

        let fix_execution =
            execute_writer_task(json_output, project, database, task.id, None, Some(config))
                .await?;
        let fix_changed = git_status_paths(project)?;
        print_writer_report(json_output, &fix_execution, Some(&fix_changed))?;
        ensure_writer_succeeded(&fix_execution)?;
        println!(">>> [TALOS Team] Writer Lead applied corrections.");
    }

    // Step 3: Project Verification (Automated Checks / Tests)
    println!("\n>>> [TALOS Team] Phase 4: Automated project verification checks...");
    let checks = database.task_verification_checks(task.id)?;
    if checks.is_empty() && !config.checks.is_empty() {
        println!(
            ">>> [TALOS Team] Attaching {} configured check(s) to task #{}: {:?}",
            config.checks.len(),
            task.id,
            config.checks.iter().map(|c| &c.id).collect::<Vec<_>>()
        );
        database.add_task_verification_checks(task.id, &config.checks)?;
    }

    if let Err(err) = run_task_verification(json_output, project, database, task.id).await {
        println!(
            "\n>>> [TALOS Team] Task #{} verification failed: {err}",
            task.id
        );
        let _ = database.transition_task(
            task.id,
            crate::db::TaskStatus::Implementing,
            "talos",
            &format!("Verification failed: {err}"),
        );
        let event_data = serde_json::json!({
            "error": err.to_string(),
        })
        .to_string();
        let _ = database.record_task_event(
            task.id,
            "talos",
            "task.verification_failed",
            &format!("Verification checks failed: {err}"),
            Some(&event_data),
        );
        return Err(err);
    }
    println!(
        ">>> [TALOS Team] All automated project checks PASSED successfully for task #{}.",
        task.id
    );

    // Step 4: Local Git Commit & Completion
    let dirty = git_status_paths(project)?;
    if !dirty.is_empty() {
        commit_approved_changes(project, database, task)?;
        println!(
            ">>> [TALOS Team] Committed task #{} changes locally in Git.",
            task.id
        );
    }

    let commit_hash = git_head(&project.root).ok().flatten();
    let changed = record_task_result_commit(database, task.id, commit_hash.as_deref())?;

    if !team_deferred_findings.is_empty() {
        let payload = serde_json::to_string(&team_deferred_findings).ok();
        let _ = database.record_task_event(
            task.id,
            "critic_lead",
            "task.backlog_curated",
            &format!(
                "Curated {} deferred finding(s) for task backlog",
                team_deferred_findings.len()
            ),
            payload.as_deref(),
        );
    }

    let summary_data = crate::summary::TaskSummaryData {
        task,
        mode: crate::config::OrchestrationMode::Team,
        writer_adapter: config.writer,
        writer_model: config.writer_model.as_deref(),
        critic_adapter: config.critic,
        critic_model: config.critic_model.as_deref(),
        start_time,
        total_pause_duration: Duration::from_secs(0),
        decisions: architectural_decisions,
        checks_passed: config.checks.iter().map(|c| c.id.clone()).collect(),
        modified_files: changed,
        commit_hash,
        deferred_findings: team_deferred_findings,
    };
    if let Ok(artifact) = crate::summary::write_task_summary(&project.root, &summary_data) {
        println!("\n>>> [TALOS Team] 📄 Deliverable Summary Artifact written to: {artifact}");
    }

    println!("\n>>> [TALOS Team] ===============================================");
    println!(
        ">>> [TALOS Team] Task #{} COMPLETED SUCCESSFULLY BY TEAM ORCHESTRATION!",
        task.id
    );
    println!(">>> [TALOS Team] ===============================================\n");

    Ok(())
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
    let task = database.first_runnable_queued_task()?;
    let Some(task) = task else {
        let has_queued_tasks = database.has_queued_tasks()?;
        let waiting_tasks = database.queued_tasks_waiting_on_dependencies()?;
        if json_output {
            print_json(&json!({
                "mode": "semi",
                "result": if has_queued_tasks {
                    "waiting_on_dependencies"
                } else {
                    "no_queued_tasks"
                },
                "waiting_task_count": waiting_tasks.waiting_task_count,
                "waiting_tasks_preview": waiting_tasks.waiting_tasks,
                "waiting_tasks_truncated": waiting_tasks.waiting_tasks_truncated,
                "writer_launched": false,
            }))?;
        } else if has_queued_tasks {
            print_waiting_task_summary(&waiting_tasks);
        } else {
            println!("No queued tasks; no Writer was started.");
        }
        return Ok(());
    };
    let execution_project = execution_project_for_task(project, database, task.id)?;
    let before = git_status_paths(project)?;
    if !before.is_empty() {
        bail!(
            "`up --semi` requires a clean Git worktree; inspect or commit these existing paths first: {}",
            before.display_preview()
        );
    }
    let execution_before = git_status_paths(&execution_project)?;
    if !execution_before.is_empty() {
        bail!(
            "`up --semi` requires a clean task execution worktree; inspect or commit these existing paths first: {}",
            execution_before.display_preview()
        );
    }
    request_operator_approval(&format!(
        "Start the configured Writer for task #{} ({})?",
        task.id, task.title
    ))?;
    let execution = execute_writer_task(
        json_output,
        &execution_project,
        database,
        task.id,
        None,
        None,
    )
    .await;
    let changed_files = git_status_paths(&execution_project)?;
    let report = execution?;
    print_writer_report(json_output, &report, Some(&changed_files))?;
    ensure_writer_succeeded(&report)?;

    // Step 2: Critic Review & Automated Fix Loop
    const MAX_REVIEW_FIX_CYCLES: usize = 3;
    let mut cycles = 0;
    loop {
        cycles += 1;
        println!(
            "\n>>> [TALOS Auto] Starting Critic review for task #{} (round {cycles})...",
            task.id
        );
        let critic_report =
            execute_critic_review(&execution_project, database, task.id, None).await?;
        print_critic_report(json_output, &critic_report)?;

        if critic_report.review.decision == "approve" {
            println!(
                "\n>>> [TALOS Auto] Critic approved changes for task #{}.",
                task.id
            );
            break;
        }

        if cycles >= MAX_REVIEW_FIX_CYCLES {
            println!(
                "\n>>> [TALOS Auto] Critic requested changes, but maximum review/fix rounds ({MAX_REVIEW_FIX_CYCLES}) reached. Task remains in implementing for review."
            );
            return Ok(());
        }

        println!("\n>>> [TALOS Auto] Critic requested changes. Launching Writer fix iteration...");
        let fix_execution = execute_writer_task(
            json_output,
            &execution_project,
            database,
            task.id,
            None,
            None,
        )
        .await?;
        let fix_changed = git_status_paths(&execution_project)?;
        print_writer_report(json_output, &fix_execution, Some(&fix_changed))?;
        ensure_writer_succeeded(&fix_execution)?;
    }

    // Step 3: Project Verification / Tests
    let checks = database.task_verification_checks(task.id)?;
    if checks.is_empty() {
        let config = ProjectConfig::load(project)?;
        if !config.checks.is_empty() {
            println!(
                "\n>>> [TALOS Auto] Attaching {} configured project check(s) to task #{}...",
                config.checks.len(),
                task.id
            );
            database.add_task_verification_checks(task.id, &config.checks)?;
        }
    }

    println!(
        "\n>>> [TALOS Auto] Running project verification for task #{}...",
        task.id
    );
    if let Err(err) = run_task_verification(json_output, project, database, task.id).await {
        println!(
            "\n>>> [TALOS Auto] Task #{} verification failed: {err}",
            task.id
        );
        let _ = database.transition_task(
            task.id,
            crate::db::TaskStatus::Implementing,
            "talos",
            &format!("Verification failed: {err}"),
        );
        let event_data = serde_json::json!({
            "error": err.to_string(),
        })
        .to_string();
        let _ = database.record_task_event(
            task.id,
            "talos",
            "task.verification_failed",
            &format!("Verification checks failed: {err}"),
            Some(&event_data),
        );
        return Err(err);
    }
    println!(
        "\n>>> [TALOS Auto] Task #{} verified and completed successfully!",
        task.id
    );

    // Step 4: Worktree Integration (if task executed in a worktree)
    if database.task_worktree(task.id)?.is_some() {
        println!(
            "\n>>> [TALOS Auto] Integrating task #{} worktree into primary project...",
            task.id
        );
        match crate::worktree::integrate(project, database, task.id) {
            Ok(_) => {
                let commit_msg = format!("talos: task #{}: {}", task.id, task.title);
                let _ = git_command(project, &["add", "."]);
                let _ = git_command(project, &["commit", "-m", &commit_msg]);
                let _ = crate::worktree::finalize(project, database, task.id);
                println!(
                    ">>> [TALOS Auto] Task #{} worktree integrated successfully!",
                    task.id
                );
            }
            Err(e) => {
                println!(">>> [TALOS Auto] Worktree integration note: {e}");
            }
        }
    }

    // Step 5: Optional GitHub Draft PR creation (if enabled by policy)
    let config = ProjectConfig::load(project)?;
    if let Some(policy) = &config.github
        && policy.allow_push
        && policy.allow_pull_request
    {
        println!(
            "\n>>> [TALOS Auto] GitHub publication policy is enabled. Preparing Draft PR for task #{}...",
            task.id
        );
        let branch = format!("{}task-{}", policy.feature_branch_prefix, task.id);
        let _ = git_command(project, &["checkout", "-B", &branch]);
        let status = git_status_paths(project)?;
        if !status.is_empty() {
            let _ = git_command(project, &["add", "."]);
            let _ = git_command(
                project,
                &[
                    "commit",
                    "-m",
                    &format!("feat: task #{}: {}", task.id, task.title),
                ],
            );
        }
        let req = GithubPublishRequest {
            branch: &branch,
            title: &format!("talos: task #{}: {}", task.id, task.title),
            body: &format!(
                "Autonomous task #{} completed by TALOS.\n\nCritic approved and verified.",
                task.id
            ),
            confirm_push: true,
            confirm_pr: true,
        };
        if let Err(e) = run_github_publish(json_output, project, database, req).await {
            println!(">>> [TALOS Auto] Note: GitHub publication was skipped or failed: {e}");
        }
    }

    Ok(())
}

fn print_waiting_task_summary(summary: &QueueWaitSummary) {
    if summary.waiting_task_count == 0 {
        println!("Queued tasks exist, but no task is currently eligible to run.");
        println!("Inspect the queue with `talos task list`.");
        return;
    }

    println!(
        "No queued task is ready; {} task(s) wait on prerequisites:",
        summary.waiting_task_count
    );
    for waiting in &summary.waiting_tasks {
        let prerequisites = waiting
            .prerequisite_task_ids_preview
            .iter()
            .map(|id| format!("#{id}"))
            .collect::<Vec<_>>()
            .join(", ");
        let extra = if waiting.prerequisites_truncated {
            ", and more prerequisites"
        } else {
            ""
        };
        println!(
            "  Task #{} waits on [{}{}].",
            waiting.task_id, prerequisites, extra
        );
    }
    if summary.waiting_tasks_truncated {
        println!("Showing the first 25 waiting tasks.");
    }
    println!("Use `talos task list` and `talos task dependencies <ID>` to inspect the queue.");
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

pub(crate) fn ensure_talos_ignored(project: &Project) -> Result<()> {
    if !matches!(&project.discovery, DiscoveryMethod::Git) {
        return Ok(());
    }
    if git_output_has_any_bytes(
        &project.root,
        &["ls-files", "-z", "--", ".talos"],
        "checking whether .talos is tracked by Git",
    )? {
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
        Some(1) => {
            use std::os::{fd::AsRawFd, unix::fs::OpenOptionsExt};

            let output = git_command(
                project,
                &[
                    "rev-parse",
                    "--path-format=absolute",
                    "--git-path",
                    "info/exclude",
                ],
            )?;
            if !output.status.success() {
                bail!("Cannot locate the local Git exclude file");
            }
            let path = std::str::from_utf8(&output.stdout)
                .context("Git exclude path is not valid UTF-8")?
                .trim_end_matches('\n');
            let path = Path::new(path);
            let info = path
                .parent()
                .filter(|_| path.is_absolute())
                .context("Git returned an invalid exclude path")?;
            std::fs::create_dir_all(info).context("Cannot create the local Git info directory")?;
            if std::fs::symlink_metadata(info)?.is_symlink() {
                bail!("Local Git info directory must not be a symlink");
            }
            let mut file = std::fs::OpenOptions::new()
                .read(true)
                .append(true)
                .create(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(path)
                .context("Cannot open the local Git exclude file")?;
            if !file.metadata()?.is_file() {
                bail!("Local Git exclude path must be a regular file");
            }
            // Serialize initialization from multiple TALOS processes.
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
                return Err(io::Error::last_os_error())
                    .context("Cannot lock the local Git exclude file");
            }
            let mut content = Vec::new();
            file.read_to_end(&mut content)?;
            if !content
                .split(|byte| *byte == b'\n')
                .any(|line| line == b"/.talos/")
            {
                let rule = if content.is_empty() || content.ends_with(b"\n") {
                    b"/.talos/\n".as_slice()
                } else {
                    b"\n/.talos/\n".as_slice()
                };
                file.write_all(rule)
                    .context("Cannot add TALOS to the local Git exclude file")?;
            }
            let verified = git_command(
                project,
                &[
                    "check-ignore",
                    "--no-index",
                    "-q",
                    "--",
                    ".talos/state.sqlite3",
                ],
            )?;
            if !verified.status.success() {
                bail!(
                    "Git still exposes .talos/ after setting the local exclusion. Check conflicting ignore rules before starting TALOS."
                );
            }
            Ok(())
        }
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

pub(crate) fn git_status_paths(project: &Project) -> Result<GitStatusPaths> {
    let mut command = ProcessCommand::new("git");
    command
        .args(["status", "--porcelain=v1", "-z", "--untracked-files=all"])
        .current_dir(&project.root)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    for (key, _) in env::vars_os().filter(|(key, _)| key.to_string_lossy().starts_with("GIT_")) {
        command.env_remove(key);
    }
    let mut child = command
        .spawn()
        .context("Cannot inspect Git changes after the Writer run")?;
    let Some(mut stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        bail!("Cannot read Git status output");
    };

    let mut summary = GitStatusPaths {
        total_count: 0,
        preview: Vec::with_capacity(MAX_CHANGED_PATHS_PREVIEW),
        preview_values_truncated: false,
    };
    let mut entry = Vec::new();
    let mut expects_rename_path = false;
    let mut parse_error = None;
    let mut chunk = [0_u8; 8 * 1024];
    loop {
        let bytes_read = match stdout.read(&mut chunk) {
            Ok(bytes_read) => bytes_read,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error).context("Cannot read Git status output");
            }
        };
        if bytes_read == 0 {
            break;
        }
        for byte in &chunk[..bytes_read] {
            if *byte != 0 {
                if parse_error.is_none() {
                    entry.push(*byte);
                }
                continue;
            }
            if let Some(error) = parse_error {
                entry.clear();
                parse_error = Some(error);
                continue;
            }
            if entry.is_empty() {
                continue;
            }
            if expects_rename_path {
                record_git_status_path(&mut summary, &entry);
                expects_rename_path = false;
            } else if entry.len() >= 4 && entry[2] == b' ' {
                let status = &entry[..2];
                record_git_status_path(&mut summary, &entry[3..]);
                expects_rename_path = status.contains(&b'R') || status.contains(&b'C');
            } else {
                parse_error = Some("Git returned an invalid porcelain status entry");
            }
            entry.clear();
        }
    }
    drop(stdout);
    let status = child
        .wait()
        .context("Cannot wait for Git status to finish")?;
    if !status.success() {
        bail!(
            "Git status failed while inspecting project changes (exit code {})",
            status.code().unwrap_or(-1)
        );
    }
    if let Some(error) = parse_error {
        bail!("{error}");
    }
    if !entry.is_empty() || expects_rename_path {
        bail!("Git returned an incomplete rename or status entry");
    }
    Ok(summary)
}

fn record_git_status_path(summary: &mut GitStatusPaths, path: &[u8]) {
    summary.total_count = summary.total_count.saturating_add(1);
    if summary.preview.len() < MAX_CHANGED_PATHS_PREVIEW {
        let path = String::from_utf8_lossy(path);
        let (path_preview, truncated) = truncate_changed_path(&path);
        summary.preview_values_truncated |= truncated;
        summary.preview.push(path_preview);
    }
}

fn truncate_changed_path(path: &str) -> (String, bool) {
    if path.len() <= MAX_CHANGED_PATH_PREVIEW_BYTES {
        return (path.to_owned(), false);
    }
    let mut end = MAX_CHANGED_PATH_PREVIEW_BYTES;
    while !path.is_char_boundary(end) {
        end -= 1;
    }
    (format!("{}…", &path[..end]), true)
}

fn verification_evidence_json(
    page: &crate::db::VerificationEvidencePage,
) -> Result<serde_json::Value> {
    const MAX_PATH_PREVIEW: usize = 10;

    let runs = page
        .runs
        .iter()
        .map(|evidence| {
            let paths: Vec<String> = serde_json::from_str(&evidence.changed_paths_json)
                .context("Saved verification paths are not valid JSON")?;
            let mut changed_paths_preview = Vec::with_capacity(MAX_PATH_PREVIEW);
            let mut changed_path_values_truncated = false;
            for path in paths.iter().take(MAX_PATH_PREVIEW) {
                let (preview, truncated) = truncate_changed_path(path);
                changed_paths_preview.push(preview);
                changed_path_values_truncated |= truncated;
            }
            let changed_paths_truncated = paths.len() > changed_paths_preview.len();
            Ok(json!({
                "run": &evidence.run,
                "base_revision": &evidence.base_revision,
                "head_commit": &evidence.head_commit,
                "patch_sha256": &evidence.patch_sha256,
                "changed_path_count": paths.len(),
                "changed_paths_preview": changed_paths_preview,
                "changed_paths_truncated": changed_paths_truncated,
                "changed_path_values_truncated": changed_path_values_truncated,
                "checks": &evidence.checks,
            }))
        })
        .collect::<Result<Vec<_>>>()?;

    Ok(json!({
        "runs": runs,
        "before": page.before,
        "limit": page.limit,
        "has_more": page.has_more,
        "next_before": page.next_before,
    }))
}

fn recover_writer_attempts(json_output: bool, database: &mut Database) -> Result<()> {
    let now = now_ms()?;
    let owner_id = format!("talos-task-recover-{}-{now}", std::process::id());
    let lease_duration_ms = i64::try_from(SUPERVISOR_LEASE.as_millis())
        .context("Supervisor lease duration is out of range")?;
    let lease = acquire_recovery_lease(database, &owner_id, now, lease_duration_ms)?;

    let quarantine_result = (|| {
        let active_writer_leases = database.active_writer_execution_leases()?;
        #[cfg(unix)]
        for writer_lease in &active_writer_leases {
            let owner_process_id = writer_lease.owner_process_id.context(
                "An active Writer lease has no owner PID; recovery cannot prove that it stopped",
            )?;
            if process_id_is_alive(owner_process_id)? {
                bail!(
                    "Writer attempt #{} is owned by live TALOS process {}; recovery will not quarantine it",
                    writer_lease.attempt_id,
                    owner_process_id
                );
            }
        }
        #[cfg(not(unix))]
        if !active_writer_leases.is_empty() {
            bail!(
                "This platform cannot verify active Writer owner processes; recovery remains fail-closed"
            );
        }
        database.quarantine_unfinished_writer_attempts(&lease)
    })();
    let lease_released = database.release_supervisor_lease(&lease)?;
    if !lease_released {
        bail!("TALOS lost its supervisor lease during Writer recovery");
    }
    let quarantined_count = quarantine_result?;
    let unresolved_attempts = database.unresolved_writer_attempts()?;
    let process_identity_checks = unresolved_attempts
        .iter()
        .map(|attempt| {
            json!({
                "attempt_id": attempt.id,
                "status": crate::process_identity::compare(
                    attempt.process_id,
                    attempt.process_identity_fingerprint.as_deref(),
                )
                .as_str(),
                "lifecycle_evidence_only": true,
            })
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
            "process_identity_checks": process_identity_checks,
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
                "Attempt #{} for task #{} is {} (recorded PID: {}, process identity: {}).",
                attempt.id,
                attempt.task_id,
                attempt.state,
                recorded_pid,
                crate::process_identity::compare(
                    attempt.process_id,
                    attempt.process_identity_fingerprint.as_deref(),
                )
                .as_str()
            );
        }
        println!(
            "Owner PID liveness was checked before quarantine, but process identity remains lifecycle evidence only and does not establish a Writer outcome. An unresolved Writer may still have changed project files. TALOS did not signal processes, resume a conversation, or retry an attempt. Inspect process and project state before deciding how to proceed."
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
    if let Some(lease) = database.acquire_recovery_supervisor_lease(owner_id, now, duration_ms)? {
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
        database
            .acquire_recovery_supervisor_lease(owner_id, now, duration_ms)?
            .context("Another TALOS supervisor acquired the project lease during recovery")
    }

    #[cfg(not(unix))]
    {
        bail!("Another TALOS supervisor currently owns the project lease")
    }
}

fn talos_lease_owner_pid(owner_id: &str) -> Option<u32> {
    // Only recover leases written by known TALOS operation families. Their final fields are
    // always the process ID and creation timestamp, even when the operation includes task IDs.
    let mut suffix = owner_id.rsplitn(3, '-');
    let timestamp = suffix.next()?.parse::<i64>().ok()?;
    let process_id = suffix.next()?.parse::<u32>().ok()?;
    let owner_prefix = suffix.next()?;
    let operation = owner_prefix
        .strip_prefix("talos-task-")
        .or_else(|| owner_prefix.strip_prefix("talos-worktree-"))?;
    if timestamp <= 0
        || operation.is_empty()
        || operation.split('-').any(str::is_empty)
        || !operation
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return None;
    }
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
            .get_writer_attempt(attempt_id)?
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

pub(crate) fn adapter_available_for_cockpit(adapter: AdapterKind, root: &Path) -> bool {
    if adapter == AdapterKind::Agy {
        let probe = AgyProbe::inspect(root);
        return adapter_role_status(adapter, AgentRole::Writer, &probe, root) == "verified"
            && adapter_role_status(adapter, AgentRole::Critic, &probe, root) == "verified";
    }
    resolve_adapter_executable(adapter, root).is_ok()
}

fn adapter_capability_reports(
    probe: &AgyProbe,
    project_root: &Path,
) -> Vec<AdapterCapabilityReport> {
    AdapterKind::ALL
        .into_iter()
        .map(|adapter| {
            let (detection, writer_status, critic_status) = if adapter == AdapterKind::Agy {
                let det = probe.status.as_str();
                let wr = adapter_role_status(adapter, AgentRole::Writer, probe, project_root);
                let cr = adapter_role_status(adapter, AgentRole::Critic, probe, project_root);
                (det, wr, cr)
            } else {
                match resolve_adapter_executable(adapter, project_root) {
                    Ok(_) => ("available", "verified", "verified"),
                    Err(_) => ("not_installed", "unavailable", "unavailable"),
                }
            };
            AdapterCapabilityReport {
                id: adapter.as_str(),
                detection,
                implementation: if adapter.is_implemented() {
                    "implemented"
                } else {
                    "not_implemented"
                },
                writer: writer_status,
                critic: critic_status,
            }
        })
        .collect()
}

fn adapter_role_status(
    adapter: AdapterKind,
    _role: AgentRole,
    probe: &AgyProbe,
    project_root: &Path,
) -> &'static str {
    if !adapter.is_implemented() {
        return "not_tested";
    }
    if adapter == AdapterKind::Agy {
        if probe.status.as_str() != "available" {
            return "unavailable";
        }
        if let Some(version) = probe.version.as_deref() {
            if !crate::supervisor::is_verified_agy_version(version)
                || !has_required_writer_capabilities(probe)
            {
                return "not_verified";
            }
        } else if !has_required_writer_capabilities(probe) {
            return "not_verified";
        }
        return "verified";
    }

    match resolve_adapter_executable(adapter, project_root) {
        Ok(_) => "verified",
        Err(_) => "unavailable",
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

fn print_task_summary(task: &TaskSummaryRecord) {
    println!(
        "#{} [{}] {}",
        task.id,
        task.status.as_str(),
        redact_sensitive(&task.title)
    );
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
        recover_writer_attempts, resolve_writer_attempt, run_pair_task, run_task_verification,
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
            state_root: root.clone(),
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
        let _ = Command::new("git")
            .args(["config", "user.name", "TALOS Test"])
            .current_dir(root)
            .output();
        let _ = Command::new("git")
            .args(["config", "user.email", "talos-test@example.com"])
            .current_dir(root)
            .output();
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
    fn git_project_excludes_talos_without_changing_the_worktree() {
        let project = temp_project();
        init_git(&project.root);
        let git_project = Project {
            root: project.root.clone(),
            state_root: project.root.clone(),
            discovery: DiscoveryMethod::Git,
        };
        ensure_talos_ignored(&git_project).expect("adds the local exclusion");
        assert!(!project.root.join(".gitignore").exists());
        assert_eq!(
            git(&project.root, &["check-ignore", ".talos/state.sqlite3"]),
            ".talos/state.sqlite3"
        );
        assert!(git(&project.root, &["status", "--porcelain"]).is_empty());

        // Running it again is idempotent
        ensure_talos_ignored(&git_project).expect("already ignored TALOS state is allowed");
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
            state_root: project.root.clone(),
            discovery: DiscoveryMethod::Git,
        };
        let error = ensure_talos_ignored(&git_project).expect_err("tracked runtime must fail");
        assert!(format!("{error:#}").contains("contains tracked files"));
        fs::remove_dir_all(&project.root).expect("remove temporary Git project");
    }

    #[tokio::test]
    async fn verification_runs_saved_command_and_completes_only_matching_approved_revision() {
        let mut project = temp_project();
        fs::write(project.root.join(".gitignore"), ".talos/\n").expect("ignore TALOS runtime");
        fs::create_dir_all(project.root.join("src")).expect("create source directory");
        fs::write(
            project.root.join("src/lib.rs"),
            "pub fn value() -> u8 { 1 }\n",
        )
        .expect("write baseline source");
        init_git(&project.root);
        project.discovery = DiscoveryMethod::Git;
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

    #[tokio::test]
    async fn verification_failure_returns_error_and_does_not_complete_task() {
        let mut project = temp_project();
        fs::write(project.root.join(".gitignore"), ".talos/\n").expect("ignore TALOS runtime");
        fs::create_dir_all(project.root.join("src")).expect("create source directory");
        fs::write(
            project.root.join("src/lib.rs"),
            "pub fn value() -> u8 { 1 }\n",
        )
        .expect("write baseline source");
        init_git(&project.root);
        project.discovery = DiscoveryMethod::Git;
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
        let failing_check = ProjectCheck {
            id: "failing-check".to_owned(),
            argv: vec!["cargo".to_owned(), "--invalid-flag-that-fails".to_owned()],
            version_argv: vec!["cargo".to_owned(), "--version".to_owned()],
            working_directory: ".".to_owned(),
        };
        let task = database
            .add_task_with_checks(
                "Verify task",
                "The configured check exits zero",
                &[failing_check],
            )
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

        let err = run_task_verification(false, &project, &mut database, task.id)
            .await
            .expect_err("verification must fail when check exits non-zero");
        let err_msg = format!("{err:#}");
        assert!(
            err_msg.contains(&format!("Task #{} verification checks failed", task.id)),
            "unexpected error message: {err_msg}"
        );
        assert!(
            err_msg.contains("status 'failed'"),
            "unexpected error message: {err_msg}"
        );
        assert_eq!(
            database.list_tasks().expect("read task")[0].status,
            TaskStatus::Verifying
        );
        let history = database
            .verification_history(task.id)
            .expect("read verification history");
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].run.status, "failed");

        drop(database);
        fs::remove_dir_all(&project.root).expect("remove temporary verification project");
    }

    #[tokio::test]
    async fn pair_task_verification_failure_transitions_to_implementing_and_emits_event() {
        let mut project = temp_project();
        fs::write(project.root.join(".gitignore"), ".talos/\n").expect("ignore TALOS runtime");
        fs::create_dir_all(project.root.join("src")).expect("create source directory");
        fs::write(
            project.root.join("src/lib.rs"),
            "pub fn value() -> u8 { 1 }\n",
        )
        .expect("write baseline source");
        init_git(&project.root);
        project.discovery = DiscoveryMethod::Git;
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
        let failing_check = ProjectCheck {
            id: "failing-check".to_owned(),
            argv: vec!["cargo".to_owned(), "--invalid-flag-that-fails".to_owned()],
            version_argv: vec!["cargo".to_owned(), "--version".to_owned()],
            working_directory: ".".to_owned(),
        };
        let task = database
            .add_task_with_checks(
                "Verify task",
                "The configured check exits zero",
                &[failing_check],
            )
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

        let task_record = database.get_task(task.id).unwrap().unwrap();
        assert_eq!(task_record.status, TaskStatus::Verifying);

        let config = crate::config::ProjectConfig::default();
        let err = run_pair_task(false, &project, &mut database, &task_record, &config)
            .await
            .expect_err("run_pair_task must fail when verification checks fail");
        assert!(format!("{err:#}").contains("verification checks failed"));

        let updated_task = database.get_task(task.id).unwrap().unwrap();
        assert_eq!(
            updated_task.status,
            TaskStatus::Implementing,
            "task must transition back to implementing on verification failure"
        );

        let events = database
            .task_events(Some(task.id))
            .expect("read verification events");
        assert!(
            events
                .iter()
                .any(|event| event.event_type == "task.verification_failed"),
            "task.verification_failed event must be emitted"
        );

        let log = Command::new("git")
            .arg("-C")
            .arg(&project.root)
            .args(["log", "--oneline"])
            .output()
            .expect("read git log");
        let log_str = String::from_utf8_lossy(&log.stdout);
        assert!(
            !log_str.contains("talos: task"),
            "git log must not contain unverified completion commit"
        );

        drop(database);
        fs::remove_dir_all(&project.root).expect("remove temporary verification project");
    }

    #[test]
    fn recovery_refuses_a_live_writer_owner_and_blocks_supervisor_operations() {
        let project = temp_project();
        let mut database = Database::open(&project).expect("initialize database");
        let task = database
            .add_task("Do not recover under another owner", "")
            .expect("add task");
        let _attempt = database
            .begin_writer_attempt(task.id, &"56".repeat(32))
            .expect("begin Writer attempt");
        let now = now_ms().expect("read current time");
        let lease_duration_ms = i64::try_from(SUPERVISOR_LEASE.as_millis())
            .expect("lease duration fits in SQLite integer");
        let blocked_lease = database
            .acquire_supervisor_lease("another-supervisor", now, lease_duration_ms)
            .expect("inspect competing lease");
        assert!(blocked_lease.is_none());

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
            .acquire_recovery_supervisor_lease(owner, now, lease_duration_ms)
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

    #[test]
    fn team_orchestration_planning_and_delegation_flow() {
        let project = temp_project();
        init_git(&project.root);
        fs::write(project.root.join(".gitignore"), ".talos/\n").expect("write gitignore");
        fs::write(project.root.join("README.md"), "# Team Test\n").expect("write readme");
        git(&project.root, &["add", "."]);
        git(&project.root, &["commit", "-m", "initial commit"]);

        let mut database = Database::open(&project).expect("initialize database");
        let parent_task = database
            .add_task(
                "Build Authentication Module",
                "Implement token auth and endpoints",
            )
            .expect("add parent task");

        // Simulate Writer Lead plan response with 2 subtasks
        let plan_json = r#"{
            "action": "delegate",
            "reasoning": "Separate token logic and API endpoints into independent workers",
            "subtasks": [
                {
                    "id": "worker_api",
                    "title": "API auth endpoints",
                    "allowed_paths": ["src/api_auth.rs"],
                    "acceptance_criteria": "Endpoints return 200 on valid token",
                    "instructions": "Implement auth endpoints"
                },
                {
                    "id": "worker_token",
                    "title": "JWT token generator",
                    "allowed_paths": ["src/token.rs"],
                    "acceptance_criteria": "Token signs and verifies",
                    "instructions": "Implement token logic"
                }
            ]
        }"#;

        let lead_plan = crate::team::parse_writer_lead_plan(plan_json).expect("parse plan");
        assert_eq!(lead_plan.action, "delegate");
        assert_eq!(lead_plan.subtasks.len(), 2);

        // Register subtasks in database
        for subtask in &lead_plan.subtasks {
            let (child_task, contract) = database
                .add_child_task(
                    parent_task.id,
                    &subtask.title,
                    &subtask.acceptance_criteria,
                    &subtask.allowed_paths,
                    60_000,
                )
                .expect("add child task");
            assert_eq!(contract.parent_task_id, parent_task.id);

            let team_record = database
                .add_team_subtask(
                    parent_task.id,
                    child_task.id,
                    "writer_worker",
                    "codex",
                    "external",
                    "queued",
                    &[],
                )
                .expect("add team subtask");
            assert_eq!(team_record.role, "writer_worker");
            assert_eq!(team_record.delegation_kind, "external");
        }

        let subtasks = database
            .team_subtasks_for_parent(parent_task.id)
            .expect("fetch subtasks");
        assert_eq!(subtasks.len(), 2);
        assert_eq!(subtasks[0].status, "queued");
        assert_eq!(subtasks[1].status, "queued");

        // Simulate worker progress and completion
        database
            .update_team_subtask_status(
                subtasks[0].subtask_id,
                "completed",
                Some("Endpoints implemented"),
                None,
            )
            .expect("update subtask 0");
        database
            .update_team_subtask_status(
                subtasks[1].subtask_id,
                "completed",
                Some("Token logic implemented"),
                None,
            )
            .expect("update subtask 1");

        let updated = database
            .team_subtasks_for_parent(parent_task.id)
            .expect("fetch updated subtasks");
        assert_eq!(updated[0].status, "completed");
        assert_eq!(updated[1].status, "completed");
        assert_eq!(
            updated[0].result_summary.as_deref(),
            Some("Endpoints implemented")
        );

        fs::remove_dir_all(&project.root).expect("remove temporary project");
    }

    #[test]
    fn team_orchestration_stuck_worker_lead_takeover() {
        let project = temp_project();
        init_git(&project.root);
        fs::write(project.root.join(".gitignore"), ".talos/\n").expect("write gitignore");
        fs::write(project.root.join("README.md"), "# Team Test\n").expect("write readme");
        git(&project.root, &["add", "."]);
        git(&project.root, &["commit", "-m", "initial commit"]);

        let mut database = Database::open(&project).expect("initialize database");
        let parent = database
            .add_task("Complex Parser Feature", "Parse expressions")
            .expect("add task");

        let (worker_task, _) = database
            .add_child_task(
                parent.id,
                "Subtask 1: lexer",
                "Tokenize input",
                &["src/lexer.rs".to_string()],
                60_000,
            )
            .expect("add child task");

        database
            .add_team_subtask(
                parent.id,
                worker_task.id,
                "writer_worker",
                "codex",
                "external",
                "in_progress",
                &[],
            )
            .expect("add team subtask");

        // Simulate Worker encountering error and Writer Lead help decision
        let worker_error = "Syntax error in regex pattern for tokenizer";
        let help_prompt = crate::team::build_worker_help_prompt(
            &parent,
            "subtask_lexer",
            "Subtask 1: lexer",
            &["src/lexer.rs".to_string()],
            worker_error,
            1,
            2,
        );
        assert!(help_prompt.contains("Syntax error in regex pattern"));

        let help_response = r#"{
            "action": "takeover",
            "guidance": null,
            "reason": "Regex compilation error requires architectural simplification; Lead will handle directly"
        }"#;

        let help_decision =
            crate::team::parse_worker_help_decision(help_response).expect("parse decision");
        assert_eq!(help_decision.action, "takeover");

        // Simulate Lead takeover and successful completion
        database
            .update_team_subtask_status(
                worker_task.id,
                "completed",
                Some("Writer Lead takeover: tokenizer rewritten without complex regex"),
                None,
            )
            .expect("update subtask status");

        let record = database
            .team_subtask_by_id(worker_task.id)
            .expect("fetch subtask")
            .expect("record exists");
        assert_eq!(record.status, "completed");
        assert!(
            record
                .result_summary
                .as_deref()
                .unwrap()
                .contains("Writer Lead takeover")
        );

        fs::remove_dir_all(&project.root).expect("remove temporary project");
    }

    #[test]
    fn team_orchestration_critic_worker_findings_and_lead_feedback() {
        let project = temp_project();
        init_git(&project.root);
        fs::write(project.root.join(".gitignore"), ".talos/\n").expect("write gitignore");
        fs::write(project.root.join("README.md"), "# Team Test\n").expect("write readme");
        git(&project.root, &["add", "."]);
        git(&project.root, &["commit", "-m", "initial commit"]);

        let mut database = Database::open(&project).expect("initialize database");
        let task = database
            .add_task("Payment Processing", "Process credit cards securely")
            .expect("add task");

        // Simulate Critic Worker prompt & finding
        let worker_prompt = crate::team::build_critic_worker_prompt(
            &task,
            "+ fn process_card(number: &str) { println!(\"{}\", number); }",
            "Security & Compliance",
        );
        assert!(worker_prompt.contains("Security & Compliance"));

        // Register critic worker subtask record
        database
            .add_team_subtask(
                task.id,
                task.id,
                "critic_worker",
                "agy",
                "external",
                "completed",
                &[],
            )
            .expect("add critic worker subtask");

        let critic_worker_finding =
            "Found plain text logging of credit card numbers in process_card (CWE-312)";

        // Critic Lead receives worker finding in consolidated review prompt
        let lead_review_prompt = crate::team::build_critic_lead_review_prompt(
            &task,
            "Direct payment integration",
            "+ fn process_card(number: &str) { ... }",
            Some(critic_worker_finding),
            "sha256:testrev",
        );
        assert!(lead_review_prompt.contains("SPECIALIZED CRITIC WORKER FINDINGS:"));
        assert!(lead_review_prompt.contains("CWE-312"));
        assert!(lead_review_prompt.contains("Passing unit tests prove behavioral compliance with existing tests, but do NOT automatically excuse architectural flaws"));

        // Writer Lead receives rebuttal prompt with Critic's feedback
        let rebuttal_prompt =
            crate::team::build_writer_lead_rebuttal_prompt(&task, critic_worker_finding, 1, 3);
        assert!(rebuttal_prompt.contains("Round 1 of 3"));
        assert!(rebuttal_prompt.contains("CWE-312"));
        assert!(rebuttal_prompt.contains("technical rebuttal"));

        fs::remove_dir_all(&project.root).expect("remove temporary project");
    }

    #[test]
    fn stagnation_detection_identifies_identical_diff_and_transitions_task() {
        let project = temp_project();
        init_git(&project.root);
        fs::write(project.root.join(".gitignore"), ".talos/\n").expect("write gitignore");
        fs::write(project.root.join("README.md"), "# Stagnation Test\n").expect("write readme");
        git(&project.root, &["add", "."]);
        git(&project.root, &["commit", "-m", "initial commit"]);

        let mut database = Database::open(&project).expect("initialize database");
        let task = database
            .add_task("Fix bug", "Do not produce identical diff")
            .expect("add task");

        let digest1 = "sha256:1111111111111111111111111111111111111111111111111111111111111111";
        let digest2 = "sha256:1111111111111111111111111111111111111111111111111111111111111111";
        let digest3 = "sha256:2222222222222222222222222222222222222222222222222222222222222222";

        assert!(!crate::revision::is_diff_stagnant(None, digest1));
        assert!(!crate::revision::is_diff_stagnant(Some(digest1), digest3));
        assert!(crate::revision::is_diff_stagnant(Some(digest1), digest2));

        // Simulate anti-stagnation abort action
        database
            .record_task_event(
                task.id,
                "talos",
                "task.stagnation_detected",
                "Stagnation detected: Writer produced identical diff without addressing Critic findings",
                Some(digest2),
            )
            .expect("record stagnation event");

        let events = database
            .task_events(Some(task.id))
            .expect("read task events");
        assert!(
            events
                .iter()
                .any(|e| e.event_type == "task.stagnation_detected")
        );

        fs::remove_dir_all(&project.root).expect("remove temporary project");
    }

    #[test]
    fn team_mode_preserves_pair_mode_when_mode_not_specified() {
        let project = temp_project();
        let config = crate::config::ProjectConfig::default();
        assert_eq!(
            config.orchestration.mode,
            crate::config::OrchestrationMode::Adaptive
        );
        assert_eq!(
            config.orchestration.delegation,
            crate::config::DelegationMode::Hybrid
        );
        assert_eq!(config.orchestration.max_writer_workers, 2);
        assert_eq!(config.orchestration.max_critic_workers, 1);
        fs::remove_dir_all(&project.root).expect("remove temporary project");
    }
}
