use clap::{Args, Parser, Subcommand, ValueEnum};

#[derive(Debug, Parser)]
#[command(
    name = "talos",
    version,
    about = "A local multi-agent development orchestrator",
    long_about = "TALOS manages durable development tasks and, in later rounds, supervises local AI coding CLIs."
)]
pub struct Cli {
    /// Print machine-readable JSON output where supported.
    #[arg(long, global = true)]
    pub json: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Inspect readiness without starting an agent or worker.
    Up(UpArgs),
    /// Manage and inspect durable development tasks.
    Task(TaskArgs),
    /// Show project and queue state.
    Status,
    /// Show task event history.
    Logs(LogsArgs),
    /// Pause new Writer assignments for this project.
    Pause(PauseArgs),
    /// Resume a paused project queue after checking for manual edits.
    Resume(ResumeArgs),
    /// Request graceful shutdown of the active Writer process group.
    Stop,
    /// Check local project and database readiness.
    Doctor,
    /// Create or reconnect to this project's persistent tmux control shell.
    Attach,
    /// Inspect the local GitHub CLI and optionally check its active authentication.
    Github(GithubArgs),
}

#[derive(Debug, Args)]
pub struct GithubArgs {
    #[command(subcommand)]
    pub command: GithubCommand,
}

#[derive(Debug, Subcommand)]
pub enum GithubCommand {
    /// Report local `gh` availability; contact GitHub only with `--check-auth`.
    Status {
        /// Explicitly check the active GitHub CLI authentication state.
        #[arg(long)]
        check_auth: bool,
    },
    /// Show local GitHub policy and the manual merge/deletion boundary.
    Policy,
    /// Create or inspect a local feature branch; this does not push it.
    Branch {
        #[command(subcommand)]
        command: GithubBranchCommand,
    },
    /// Push one named feature branch and open a draft PR under the configured policy.
    Publish {
        /// Exact local and remote feature branch to publish.
        #[arg(long)]
        branch: String,
        /// Pull request title.
        #[arg(long)]
        title: String,
        /// Pull request body.
        #[arg(long)]
        body: String,
        /// Explicitly authorize pushing this feature branch to the configured remote.
        #[arg(long, required = true, action = clap::ArgAction::SetTrue)]
        confirm_push: bool,
        /// Explicitly authorize opening a draft pull request against `master`.
        #[arg(long, required = true, action = clap::ArgAction::SetTrue)]
        confirm_pr: bool,
    },
    /// Inspect and reconcile an interrupted push or pull request operation.
    Reconcile {
        /// ID of the recorded GitHub publication.
        publication_id: i64,
        /// Create a draft PR after reconciliation confirms the branch is pushed and no PR exists.
        #[arg(long, requires_all = ["title", "body"])]
        create_pr: bool,
        /// Title to use only with --create-pr.
        #[arg(long)]
        title: Option<String>,
        /// Body to use only with --create-pr.
        #[arg(long)]
        body: Option<String>,
    },
    /// Record the status of checks for a previously published pull request.
    Ci {
        /// ID of the recorded GitHub publication.
        publication_id: i64,
        /// Poll while checks remain pending.
        #[arg(long)]
        wait: bool,
        /// Maximum CI polling window in seconds with `--wait` (1–3600); bounded pre/post identity checks are separate.
        #[arg(long, default_value_t = 900)]
        timeout_seconds: u64,
        /// Delay between status requests in seconds when `--wait` is set (5–120).
        #[arg(long, default_value_t = 15)]
        interval_seconds: u64,
    },
}

#[derive(Debug, Subcommand)]
pub enum GithubBranchCommand {
    /// Create and switch to a clean feature branch from the configured `master` base.
    Create {
        /// Lowercase slug appended to the configured `talos/` branch prefix.
        slug: String,
    },
}

#[derive(Debug, Args)]
pub struct PauseArgs {
    /// Explain why the queue is being paused. Secret-like text is rejected.
    #[arg(long)]
    pub reason: Option<String>,
}

#[derive(Debug, Args)]
pub struct ResumeArgs {
    /// Confirm that current project changes were inspected and reopen approvals affected by them.
    #[arg(long)]
    pub reconcile: bool,
}

#[derive(Debug, Args)]
pub struct UpArgs {
    /// Select the safe readiness-only mode. This is the default.
    #[arg(long, conflicts_with_all = ["auto", "semi"])]
    pub observe: bool,
    /// Run at most one queued task in a clean Git project.
    #[arg(long, conflicts_with = "semi")]
    pub auto: bool,
    /// Select execution with operator approval at policy boundaries.
    #[arg(long)]
    pub semi: bool,
}

#[derive(Debug, Args)]
pub struct TaskArgs {
    #[command(subcommand)]
    pub command: TaskCommand,
}

#[derive(Debug, Subcommand)]
pub enum TaskCommand {
    /// Add a task to the local durable queue.
    Add {
        /// Short description of the requested work.
        title: String,
        /// Verifiable acceptance criteria for a later execution round.
        #[arg(long)]
        criteria: Option<String>,
        /// Named project check from .talos/config.json; may be repeated.
        #[arg(long = "check", action = clap::ArgAction::Append)]
        checks: Vec<String>,
    },
    /// Add immutable project-check requirements to a queued or review-ready task.
    Checks {
        /// ID of the task that will require the checks.
        task_id: i64,
        /// Named project check from .talos/config.json; may be repeated.
        #[arg(long = "check", action = clap::ArgAction::Append, required = true)]
        checks: Vec<String>,
    },
    /// Make a queued task wait for another task to complete.
    Depends {
        /// ID of the task that will wait.
        task_id: i64,
        /// ID of the prerequisite task.
        #[arg(long = "on")]
        prerequisite_task_id: i64,
    },
    /// Show the prerequisite tasks and their current states.
    Dependencies {
        /// ID of the dependent task.
        task_id: i64,
    },
    /// Create a bounded child task and make its parent wait for completion.
    Child {
        /// ID of the queued parent task.
        parent_task_id: i64,
        /// Short description of the child task.
        title: String,
        /// Verifiable child acceptance criteria.
        #[arg(long)]
        criteria: String,
        /// Project-relative path or directory the child may change; repeat for multiple paths.
        #[arg(long = "allow", action = clap::ArgAction::Append, required = true)]
        allowed_paths: Vec<String>,
        /// Maximum Writer runtime in seconds, from 1 through 1800.
        #[arg(long, default_value_t = 1800)]
        timeout_seconds: u64,
    },
    /// List a parent's bounded child-task contracts.
    Children {
        /// ID of the parent task.
        parent_task_id: i64,
    },
    /// Print a compact, read-only context packet for continuing a task in a new session.
    Handoff {
        /// ID of the task to summarize.
        task_id: i64,
    },
    /// Print the compact, versioned result report for a child task.
    ChildReport {
        /// ID of the child task.
        task_id: i64,
    },
    /// Explicitly create, inspect, reconcile, or remove a task worktree.
    Worktree {
        #[command(subcommand)]
        command: TaskWorktreeCommand,
    },
    /// List tasks in the local queue.
    List,
    /// Run one queued task or a Critic-requested fix with the configured Writer.
    Run {
        /// ID of the queued task to execute.
        task_id: i64,
    },
    /// Review the latest successful Writer revision with a separate read-only Critic process.
    Review {
        /// ID of the task in review or awaiting fixes.
        task_id: i64,
    },
    /// Run the task's recorded project checks and complete it only with matching evidence.
    Verify {
        /// ID of the task with an approved Critic review.
        task_id: i64,
    },
    /// Show immutable project-check evidence for a task.
    Evidence {
        /// ID of the task whose verification history will be shown.
        task_id: i64,
    },
    /// Continue the latest eligible terminal Antigravity error conversation.
    Continue {
        /// ID of the blocked task to continue.
        task_id: i64,
        /// Confirm that existing project changes were inspected before continuation.
        #[arg(long, required = true, action = clap::ArgAction::SetTrue)]
        confirm_project_inspected: bool,
    },
    /// Quarantine unfinished Writer attempts for operator inspection without restarting them.
    Recover,
    /// Record an explicit, audited operator decision for an unknown Writer outcome.
    Resolve {
        /// ID of the quarantined Writer attempt.
        attempt_id: i64,
        /// Explicit disposition for the unknown Writer outcome.
        #[arg(long, value_enum)]
        action: ResolveAction,
        /// Confirm that the Writer process group is no longer running.
        #[arg(long, requires = "confirm_project_inspected")]
        confirm_writer_stopped: bool,
        /// Confirm that the project changes were inspected.
        #[arg(long, requires = "confirm_writer_stopped")]
        confirm_project_inspected: bool,
        /// Reason for the operator's decision, saved to the event journal.
        #[arg(long)]
        reason: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum TaskWorktreeCommand {
    /// Create an isolated linked worktree for a queued task.
    Create {
        /// ID of the queued task.
        task_id: i64,
    },
    /// List TALOS-managed task worktrees.
    List,
    /// Reconcile a recorded worktree after an interrupted lifecycle operation.
    Reconcile {
        /// ID of the task whose worktree should be reconciled.
        task_id: i64,
    },
    /// Remove an explicitly selected worktree only if it has no changes or ignored files.
    Remove {
        /// ID of the task whose clean worktree should be removed.
        task_id: i64,
    },
    /// Apply a task worktree patch to the clean primary project for independent review.
    Integrate {
        /// ID of the task whose patch should be integrated.
        task_id: i64,
    },
    /// Mark a task complete after its integrated changes are committed and verified.
    Finalize {
        /// ID of the task whose committed integration should be finalized.
        task_id: i64,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum ResolveAction {
    #[value(name = "accept-for-review")]
    AcceptForReview,
    #[value(name = "authorize-retry")]
    AuthorizeRetry,
    Abandon,
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::{Cli, Command, ResolveAction, ResumeArgs, TaskArgs, TaskCommand};

    #[test]
    fn task_continue_requires_project_inspection_confirmation() {
        let missing = Cli::try_parse_from(["talos", "task", "continue", "17"]);
        assert!(missing.is_err());

        let parsed = Cli::try_parse_from([
            "talos",
            "task",
            "continue",
            "17",
            "--confirm-project-inspected",
        ])
        .expect("parse explicit continuation command");
        let Command::Task(TaskArgs {
            command:
                TaskCommand::Continue {
                    task_id,
                    confirm_project_inspected,
                },
        }) = parsed.command
        else {
            panic!("expected task continue command");
        };
        assert_eq!(task_id, 17);
        assert!(confirm_project_inspected);
    }

    #[test]
    fn task_review_parses_a_task_id() {
        let cli = Cli::try_parse_from(["talos", "task", "review", "17"])
            .expect("parse independent review command");
        let Command::Task(TaskArgs {
            command: TaskCommand::Review { task_id },
        }) = cli.command
        else {
            panic!("expected task review command");
        };
        assert_eq!(task_id, 17);
    }

    #[test]
    fn task_add_accepts_explicit_verification_check_ids() {
        let cli = Cli::try_parse_from([
            "talos",
            "task",
            "add",
            "Implement a verified change",
            "--criteria",
            "The unit check passes",
            "--check",
            "unit",
            "--check",
            "format",
        ])
        .expect("parse task with saved project checks");
        let Command::Task(TaskArgs {
            command:
                TaskCommand::Add {
                    title,
                    criteria,
                    checks,
                },
        }) = cli.command
        else {
            panic!("expected task add command");
        };
        assert_eq!(title, "Implement a verified change");
        assert_eq!(criteria.as_deref(), Some("The unit check passes"));
        assert_eq!(checks, ["unit", "format"]);
    }

    #[test]
    fn task_verify_parses_a_task_id() {
        let cli = Cli::try_parse_from(["talos", "task", "verify", "17"])
            .expect("parse task verification command");
        let Command::Task(TaskArgs {
            command: TaskCommand::Verify { task_id },
        }) = cli.command
        else {
            panic!("expected task verify command");
        };
        assert_eq!(task_id, 17);
    }

    #[test]
    fn task_evidence_parses_a_task_id() {
        let cli = Cli::try_parse_from(["talos", "task", "evidence", "17"])
            .expect("parse task evidence command");
        let Command::Task(TaskArgs {
            command: TaskCommand::Evidence { task_id },
        }) = cli.command
        else {
            panic!("expected task evidence command");
        };
        assert_eq!(task_id, 17);
    }

    #[test]
    fn attach_parses_without_starting_a_worker_option() {
        let cli = Cli::try_parse_from(["talos", "attach"])
            .expect("parse persistent control-session command");
        assert!(matches!(cli.command, Command::Attach));
    }

    #[test]
    fn pause_resume_and_project_logs_parse_explicit_controls() {
        let paused = Cli::try_parse_from(["talos", "pause", "--reason", "Operator review"])
            .expect("parse queue pause");
        let Command::Pause(arguments) = paused.command else {
            panic!("expected pause command");
        };
        assert_eq!(arguments.reason.as_deref(), Some("Operator review"));

        let resumed = Cli::try_parse_from(["talos", "resume", "--reconcile"])
            .expect("parse explicit reconciliation");
        let Command::Resume(ResumeArgs { reconcile }) = resumed.command else {
            panic!("expected resume command");
        };
        assert!(reconcile);

        assert!(Cli::try_parse_from(["talos", "logs", "17", "--project"]).is_err());
        let project_logs = Cli::try_parse_from(["talos", "logs", "--project"])
            .expect("parse project control log view");
        let Command::Logs(arguments) = project_logs.command else {
            panic!("expected logs command");
        };
        assert!(arguments.project);
        assert_eq!(arguments.task_id, None);

        assert!(matches!(
            Cli::try_parse_from(["talos", "stop"])
                .expect("parse graceful Writer stop")
                .command,
            Command::Stop
        ));
    }

    #[test]
    fn task_checks_requires_explicit_check_names() {
        assert!(Cli::try_parse_from(["talos", "task", "checks", "17"]).is_err());
        let cli = Cli::try_parse_from(["talos", "task", "checks", "17", "--check", "unit"])
            .expect("parse task checks command");
        let Command::Task(TaskArgs {
            command: TaskCommand::Checks { task_id, checks },
        }) = cli.command
        else {
            panic!("expected task checks command");
        };
        assert_eq!(task_id, 17);
        assert_eq!(checks, ["unit"]);
    }

    #[test]
    fn task_checks_add_parses_id_and_check_names() {
        let cli = Cli::try_parse_from([
            "talos", "task", "checks", "17", "--check", "unit", "--check", "format",
        ])
        .expect("parse verification requirement command");
        let Command::Task(TaskArgs {
            command: TaskCommand::Checks { task_id, checks },
        }) = cli.command
        else {
            panic!("expected task checks command");
        };
        assert_eq!(task_id, 17);
        assert_eq!(checks, ["unit", "format"]);
    }

    #[test]
    fn task_resolve_parses_action_and_both_attestations() {
        let cli = Cli::try_parse_from([
            "talos",
            "task",
            "resolve",
            "17",
            "--action",
            "accept-for-review",
            "--confirm-writer-stopped",
            "--confirm-project-inspected",
            "--reason",
            "I inspected the process and project diff",
        ])
        .expect("parse explicit resolution command");

        let Command::Task(TaskArgs {
            command:
                TaskCommand::Resolve {
                    attempt_id,
                    action,
                    confirm_writer_stopped,
                    confirm_project_inspected,
                    reason,
                },
        }) = cli.command
        else {
            panic!("expected task resolve command");
        };
        assert_eq!(attempt_id, 17);
        assert_eq!(action, ResolveAction::AcceptForReview);
        assert!(confirm_writer_stopped);
        assert!(confirm_project_inspected);
        assert_eq!(reason, "I inspected the process and project diff");
    }

    #[test]
    fn task_resolve_requires_both_attestation_flags() {
        let parsed = Cli::try_parse_from([
            "talos",
            "task",
            "resolve",
            "17",
            "--action",
            "authorize-retry",
            "--confirm-writer-stopped",
            "--reason",
            "I inspected the process and project diff",
        ]);
        assert!(parsed.is_err());
    }
}

#[derive(Debug, Args)]
pub struct LogsArgs {
    /// Limit the task event history to one task ID.
    #[arg(conflicts_with = "project")]
    pub task_id: Option<i64>,
    /// Show the append-only project control history.
    #[arg(long)]
    pub project: bool,
}
