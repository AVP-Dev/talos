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
    /// Create or list durable development tasks.
    Task(TaskArgs),
    /// Show project and queue state.
    Status,
    /// Show task event history.
    Logs(LogsArgs),
    /// Check local project and database readiness.
    Doctor,
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
    },
    /// List tasks in the local queue.
    List,
    /// Run one queued task with the version-verified Antigravity Writer.
    Run {
        /// ID of the queued task to execute.
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

    use super::{Cli, Command, ResolveAction, TaskArgs, TaskCommand};

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
    /// Limit the event history to one task ID.
    pub task_id: Option<i64>,
}
