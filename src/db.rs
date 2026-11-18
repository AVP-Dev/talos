use std::{
    fs::{self, OpenOptions},
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    checks::{CheckExecution, ProjectCheck, validate_project_checks},
    project::Project,
    revision::RevisionFacts,
    secrets::contains_sensitive,
};

const SCHEMA_VERSION: i64 = 16;
const MAX_CHILD_TASK_ALLOWED_PATHS: usize = 64;
const MAX_CHILD_TASK_SCOPE_BYTES: usize = 16 * 1024;
const MIN_CHILD_TASK_TIMEOUT_MS: i64 = 1_000;
const MAX_CHILD_TASK_TIMEOUT_MS: i64 = 30 * 60 * 1_000;
type FinalizeReviewRow = (i64, String, String, String, Option<String>, Option<String>);

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Queued,
    Planning,
    Implementing,
    Reviewing,
    Verifying,
    Complete,
    Blocked,
    Failed,
}

impl TaskStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Planning => "planning",
            Self::Implementing => "implementing",
            Self::Reviewing => "reviewing",
            Self::Verifying => "verifying",
            Self::Complete => "complete",
            Self::Blocked => "blocked",
            Self::Failed => "failed",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct TaskRecord {
    pub id: i64,
    pub title: String,
    pub acceptance_criteria: String,
    pub status: TaskStatus,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct TaskDependencyRecord {
    pub task_id: i64,
    pub prerequisite_task_id: i64,
    pub prerequisite_status: TaskStatus,
    pub prerequisite_integration_state: Option<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct ChildTaskContract {
    pub child_task_id: i64,
    pub parent_task_id: i64,
    pub allowed_paths: Vec<String>,
    pub timeout_ms: i64,
    pub created_at_ms: i64,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskWorktreeState {
    Creating,
    Active,
    Failed,
    NeedsAttention,
    Retired,
}

impl TaskWorktreeState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Creating => "creating",
            Self::Active => "active",
            Self::Failed => "failed",
            Self::NeedsAttention => "needs_attention",
            Self::Retired => "retired",
        }
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct TaskWorktreeRecord {
    pub task_id: i64,
    pub worktree_path: String,
    pub branch_name: String,
    pub base_commit: String,
    pub state: TaskWorktreeState,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub retired_at_ms: Option<i64>,
    pub writer_routed_at_ms: Option<i64>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct TaskWorktreeIntegrationRecord {
    pub task_id: i64,
    pub state: String,
    pub base_commit: String,
    pub source_head_commit: String,
    pub revision_id: String,
    pub patch_sha256: String,
    pub changed_paths: Vec<String>,
    pub integration_commit: Option<String>,
    pub started_at_ms: i64,
    pub updated_at_ms: i64,
    pub integrated_at_ms: Option<i64>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct GithubPublicationRecord {
    pub id: i64,
    pub repository: String,
    pub remote_name: String,
    pub base_branch: String,
    pub head_branch: String,
    pub base_commit: String,
    pub head_commit: String,
    pub diff_sha256: String,
    pub state: String,
    pub pull_request_url: Option<String>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

pub struct GithubPublicationInput<'a> {
    pub repository: &'a str,
    pub remote_name: &'a str,
    pub base_branch: &'a str,
    pub head_branch: &'a str,
    pub base_commit: &'a str,
    pub head_commit: &'a str,
    pub diff_sha256: &'a str,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct GithubCiCheckRecord {
    pub bucket: String,
    pub name: String,
    pub state: String,
    pub workflow: Option<String>,
    #[serde(rename = "startedAt")]
    pub started_at: Option<String>,
    #[serde(rename = "completedAt")]
    pub completed_at: Option<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct GithubCiSnapshotRecord {
    pub id: i64,
    pub publication_id: i64,
    pub head_commit: String,
    pub result: String,
    pub snapshot_sha256: String,
    pub checks: Vec<GithubCiCheckRecord>,
    pub checked_at_ms: i64,
}

pub struct GithubCiSnapshotInput<'a> {
    pub publication_id: i64,
    pub head_commit: &'a str,
    pub result: &'a str,
    pub checks: &'a [GithubCiCheckRecord],
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct GithubPublicationEvent {
    pub id: i64,
    pub publication_id: i64,
    pub occurred_at_ms: i64,
    pub from_state: Option<String>,
    pub to_state: String,
    pub reason: String,
}

impl ChildTaskContract {
    pub fn permits_path(&self, changed_path: &str) -> bool {
        self.allowed_paths.iter().any(|allowed| {
            changed_path == allowed
                || changed_path
                    .strip_prefix(allowed)
                    .is_some_and(|suffix| suffix.starts_with('/'))
        })
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct TaskEvent {
    pub id: i64,
    pub task_id: i64,
    pub occurred_at_ms: i64,
    pub actor: String,
    pub event_type: String,
    pub reason: String,
    pub data_json: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct WriterAttemptRecord {
    pub id: i64,
    pub task_id: i64,
    pub state: String,
    pub process_id: Option<u32>,
    pub started_at_ms: i64,
    pub finished_at_ms: Option<i64>,
    pub prompt_sha256: String,
    pub conversation_id: Option<String>,
    pub exit_code: Option<i32>,
    pub agent_status: Option<String>,
    pub error_code: Option<String>,
    pub operator_resolution: Option<String>,
    pub resolved_at_ms: Option<i64>,
    pub resolution_reason: Option<String>,
    pub source_attempt_id: Option<i64>,
    pub base_revision: Option<String>,
    pub source_review_id: Option<i64>,
    pub process_identity_fingerprint: Option<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct CriticReviewRecord {
    pub id: i64,
    pub task_id: i64,
    pub writer_attempt_id: i64,
    pub base_revision: String,
    pub head_commit: Option<String>,
    pub revision_id: String,
    pub patch_sha256: Option<String>,
    pub changed_paths_json: Option<String>,
    pub decision: String,
    pub summary: String,
    pub result_json: String,
    pub adapter: String,
    pub adapter_version: String,
    pub created_at_ms: i64,
}

pub struct CriticReviewInput<'a> {
    pub task_id: i64,
    pub writer_attempt_id: i64,
    pub base_revision: &'a str,
    pub head_commit: &'a str,
    pub revision_id: &'a str,
    pub patch_sha256: &'a str,
    pub changed_paths_json: &'a str,
    pub decision: &'a str,
    pub summary: &'a str,
    pub result_json: &'a str,
    pub adapter: &'a str,
    pub adapter_version: &'a str,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct VerificationRunRecord {
    pub id: i64,
    pub task_id: i64,
    pub critic_review_id: i64,
    pub status: String,
    pub revision_id: String,
    pub started_at_ms: i64,
    pub finished_at_ms: Option<i64>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct VerificationEvidenceRecord {
    pub run: VerificationRunRecord,
    pub base_revision: String,
    pub head_commit: String,
    pub patch_sha256: String,
    pub changed_paths_json: String,
    pub checks: Vec<CheckExecution>,
}

#[derive(Clone, Debug, Serialize)]
pub struct LatestVerificationSummary {
    pub run: VerificationRunRecord,
    pub check_count: usize,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct ProjectControlState {
    pub queue_paused: bool,
    pub paused_at_ms: Option<i64>,
    pub pause_reason: Option<String>,
    pub pause_revision_id: Option<String>,
    pub stop_requested_attempt_id: Option<i64>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct ProjectControlEvent {
    pub id: i64,
    pub occurred_at_ms: i64,
    pub event_type: String,
    pub reason: String,
    pub data_json: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResumeProjectOutcome {
    AlreadyRunning,
    ReconciliationRequired {
        pause_revision_id: Option<String>,
        current_revision_id: Option<String>,
    },
    Resumed {
        reconciled_manual_changes: bool,
        reopened_task_ids: Vec<i64>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StopRequestOutcome {
    NoActiveWriter,
    Requested { attempt_id: i64 },
}

type WriterFinalizationState = (
    i64,
    String,
    String,
    Option<i64>,
    i64,
    Option<String>,
    Option<i64>,
    Option<String>,
    Option<i64>,
    Option<String>,
);

type CriticReviewSnapshot = (
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum WriterResolutionAction {
    AcceptForReview,
    AuthorizeRetry,
    Abandon,
}

impl WriterResolutionAction {
    fn as_str(self) -> &'static str {
        match self {
            Self::AcceptForReview => "accept_for_review",
            Self::AuthorizeRetry => "authorize_retry",
            Self::Abandon => "abandon",
        }
    }

    fn target_task_status(self) -> TaskStatus {
        match self {
            Self::AcceptForReview => TaskStatus::Reviewing,
            Self::AuthorizeRetry => TaskStatus::Queued,
            Self::Abandon => TaskStatus::Failed,
        }
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct WriterResolutionRecord {
    pub attempt_id: i64,
    pub task_id: i64,
    pub attempt_state: &'static str,
    pub action: WriterResolutionAction,
    pub task_status: TaskStatus,
    pub resolved_at_ms: i64,
    pub reason: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriterAttemptOutcome {
    Succeeded,
    Failed,
    TimedOut,
    Interrupted,
}

impl WriterAttemptOutcome {
    fn as_str(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::TimedOut => "timed_out",
            Self::Interrupted => "interrupted",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct QueueSummary {
    pub total: i64,
    pub queued: i64,
    pub active: i64,
    pub complete: i64,
    pub blocked_or_failed: i64,
}

#[derive(Clone, Debug, Serialize)]
pub struct SupervisorLease {
    pub owner_id: String,
    pub fencing_token: i64,
    pub expires_at_ms: i64,
}

pub struct Database {
    connection: Connection,
    path: std::path::PathBuf,
}

impl Database {
    pub fn open(project: &Project) -> Result<Self> {
        project.ensure_runtime_dir()?;
        let path = project.database_path();
        prepare_private_database_file(&path)?;
        secure_private_database_file(&path)?;

        let mut connection = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_FULL_MUTEX,
        )
        .with_context(|| format!("Cannot open database {}", path.display()))?;
        let current_version: i64 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .context("Cannot read database schema version")?;
        if current_version > SCHEMA_VERSION {
            bail!(
                "Database schema version {current_version} is newer than this TALOS version supports ({SCHEMA_VERSION}); database was left unchanged"
            );
        }
        connection
            .busy_timeout(Duration::from_secs(5))
            .context("Cannot configure SQLite busy timeout")?;
        connection
            .pragma_update(None, "foreign_keys", true)
            .context("Cannot enable SQLite foreign keys")?;
        let journal_mode: String = connection
            .pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get(0))
            .context("Cannot enable SQLite WAL mode")?;
        if !journal_mode.eq_ignore_ascii_case("wal") {
            bail!("SQLite did not enable WAL mode (reported {journal_mode})");
        }

        migrate(&mut connection)?;

        Ok(Self { connection, path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn schema_version(&self) -> Result<i64> {
        self.connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .context("Cannot read database schema version")
    }

    pub fn journal_mode(&self) -> Result<String> {
        self.connection
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .context("Cannot read SQLite journal mode")
    }

    pub fn add_task(&mut self, title: &str, acceptance_criteria: &str) -> Result<TaskRecord> {
        self.add_task_with_checks(title, acceptance_criteria, &[])
    }

    pub fn add_task_with_checks(
        &mut self,
        title: &str,
        acceptance_criteria: &str,
        checks: &[ProjectCheck],
    ) -> Result<TaskRecord> {
        validate_project_checks(checks)?;
        if title.trim().is_empty() || title.len() > 2_000 {
            bail!("Task title must be non-empty and at most 2000 bytes");
        }
        if acceptance_criteria.len() > 8_000 {
            bail!("Task acceptance criteria must be at most 8000 bytes");
        }
        if contains_sensitive(title) || contains_sensitive(acceptance_criteria) {
            bail!("Task title or acceptance criteria contains secret-like text");
        }
        let now = now_ms()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("Cannot start task creation transaction")?;
        transaction.execute(
            "INSERT INTO tasks (title, acceptance_criteria, status, created_at_ms, updated_at_ms)
             VALUES (?1, ?2, 'queued', ?3, ?3)",
            params![title, acceptance_criteria, now],
        )?;
        let task_id = transaction.last_insert_rowid();
        for (ordinal, check) in checks.iter().enumerate() {
            let argv_json = serde_json::to_string(&check.argv)?;
            let version_argv_json = serde_json::to_string(&check.version_argv)?;
            transaction.execute(
                "INSERT INTO task_verification_checks
                     (task_id, check_id, ordinal, argv_json, version_argv_json, working_directory)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    task_id,
                    check.id,
                    i64::try_from(ordinal).context("Too many project checks")?,
                    argv_json,
                    version_argv_json,
                    check.working_directory,
                ],
            )?;
        }
        transaction.execute(
            "INSERT INTO task_events (task_id, occurred_at_ms, actor, event_type, reason, data_json)
             VALUES (?1, ?2, 'talos', 'task.created', 'Task added to the durable queue', '{}')",
            params![task_id, now],
        )?;
        transaction
            .commit()
            .context("Cannot commit task creation")?;

        Ok(TaskRecord {
            id: task_id,
            title: title.to_owned(),
            acceptance_criteria: acceptance_criteria.to_owned(),
            status: TaskStatus::Queued,
            created_at_ms: now,
            updated_at_ms: now,
        })
    }

    pub fn add_child_task(
        &mut self,
        parent_task_id: i64,
        title: &str,
        acceptance_criteria: &str,
        allowed_paths: &[String],
        timeout_ms: i64,
    ) -> Result<(TaskRecord, ChildTaskContract)> {
        validate_child_task_text(title, acceptance_criteria)?;
        let allowed_paths = validate_child_task_allowed_paths(allowed_paths)?;
        if !(MIN_CHILD_TASK_TIMEOUT_MS..=MAX_CHILD_TASK_TIMEOUT_MS).contains(&timeout_ms) {
            bail!("Child task timeout must be between 1 second and 30 minutes");
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("Cannot start child-task creation transaction")?;
        let parent_status: Option<String> = transaction
            .query_row(
                "SELECT status FROM tasks WHERE id = ?1",
                [parent_task_id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(parent_status) = parent_status else {
            bail!("Parent task #{parent_task_id} does not exist");
        };
        if parent_status != TaskStatus::Queued.as_str() {
            bail!("A child task can only be added while its parent is queued");
        }
        let now = now_ms()?;
        transaction.execute(
            "INSERT INTO tasks (title, acceptance_criteria, status, created_at_ms, updated_at_ms)
             VALUES (?1, ?2, 'queued', ?3, ?3)",
            params![title, acceptance_criteria, now],
        )?;
        let child_task_id = transaction.last_insert_rowid();
        let allowed_paths_json = serde_json::to_string(&allowed_paths)?;
        transaction.execute(
            "INSERT INTO child_task_contracts
                 (child_task_id, parent_task_id, allowed_paths_json, timeout_ms, created_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                child_task_id,
                parent_task_id,
                allowed_paths_json,
                timeout_ms,
                now
            ],
        )?;
        transaction.execute(
            "INSERT INTO task_dependencies (task_id, depends_on_task_id, created_at_ms)
             VALUES (?1, ?2, ?3)",
            params![parent_task_id, child_task_id, now],
        )?;
        let child_data = serde_json::to_string(&serde_json::json!({
            "parent_task_id": parent_task_id,
            "allowed_paths": allowed_paths,
            "timeout_ms": timeout_ms,
            "report_schema": "talos-child-task-report-v1",
        }))?;
        transaction.execute(
            "INSERT INTO task_events (task_id, occurred_at_ms, actor, event_type, reason, data_json)
             VALUES (?1, ?2, 'talos', 'task.child_created', 'Bounded child task added; parent waits for its completion', ?3)",
            params![child_task_id, now, child_data],
        )?;
        let dependency_data = serde_json::to_string(&serde_json::json!({
            "prerequisite_task_id": child_task_id,
            "child_task": true,
        }))?;
        transaction.execute(
            "INSERT INTO task_events (task_id, occurred_at_ms, actor, event_type, reason, data_json)
             VALUES (?1, ?2, 'talos', 'task.dependency_added', 'Parent waits for bounded child task completion', ?3)",
            params![parent_task_id, now, dependency_data],
        )?;
        transaction
            .commit()
            .context("Cannot commit child-task creation")?;
        Ok((
            TaskRecord {
                id: child_task_id,
                title: title.to_owned(),
                acceptance_criteria: acceptance_criteria.to_owned(),
                status: TaskStatus::Queued,
                created_at_ms: now,
                updated_at_ms: now,
            },
            ChildTaskContract {
                child_task_id,
                parent_task_id,
                allowed_paths,
                timeout_ms,
                created_at_ms: now,
            },
        ))
    }

    pub fn child_task_contract(&self, child_task_id: i64) -> Result<Option<ChildTaskContract>> {
        self.connection
            .query_row(
                "SELECT child_task_id, parent_task_id, allowed_paths_json, timeout_ms, created_at_ms
                 FROM child_task_contracts WHERE child_task_id = ?1",
                [child_task_id],
                map_child_task_contract,
            )
            .optional()
            .context("Cannot read child-task contract")
    }

    pub fn children_for_parent(&self, parent_task_id: i64) -> Result<Vec<ChildTaskContract>> {
        let mut statement = self.connection.prepare(
            "SELECT child_task_id, parent_task_id, allowed_paths_json, timeout_ms, created_at_ms
             FROM child_task_contracts WHERE parent_task_id = ?1 ORDER BY child_task_id",
        )?;
        statement
            .query_map([parent_task_id], map_child_task_contract)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("Cannot list child-task contracts")
    }

    pub fn begin_task_worktree(
        &mut self,
        task_id: i64,
        worktree_path: &str,
        branch_name: &str,
        base_commit: &str,
    ) -> Result<TaskWorktreeRecord> {
        validate_record_text("Worktree path", worktree_path, 4_096)?;
        validate_record_text("Worktree branch", branch_name, 200)?;
        validate_git_revision(base_commit)?;
        if !Path::new(worktree_path).is_absolute()
            || worktree_path.chars().any(char::is_control)
            || !branch_name.starts_with("talos/task/")
            || !branch_name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'_'))
        {
            bail!("Worktree path or branch name is not in TALOS's safe format");
        }

        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("Cannot start worktree lifecycle transaction")?;
        let task_status: Option<String> = transaction
            .query_row("SELECT status FROM tasks WHERE id = ?1", [task_id], |row| {
                row.get(0)
            })
            .optional()?;
        let Some(task_status) = task_status else {
            bail!("Task #{task_id} does not exist");
        };
        if task_status != TaskStatus::Queued.as_str() {
            bail!("A task worktree can only be created for a queued task");
        }
        let prior_attempt_count: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM writer_attempts WHERE task_id = ?1",
            [task_id],
            |row| row.get(0),
        )?;
        if prior_attempt_count != 0 {
            bail!("A task worktree must be created before the first Writer attempt");
        }

        let existing: Option<TaskWorktreeRecord> = transaction
            .query_row(
                "SELECT task_id, worktree_path, branch_name, base_commit, state,
                        created_at_ms, updated_at_ms, retired_at_ms, writer_routed_at_ms
                 FROM task_worktrees WHERE task_id = ?1",
                [task_id],
                map_task_worktree,
            )
            .optional()?;
        let now = now_ms()?;
        let event_type;
        if let Some(existing) = existing {
            if existing.worktree_path != worktree_path
                || existing.branch_name != branch_name
                || existing.base_commit != base_commit
            {
                bail!("Task #{task_id} already has a worktree record with different identity");
            }
            if existing.state != TaskWorktreeState::Failed {
                bail!(
                    "Task #{task_id} already has a worktree in state {}",
                    existing.state.as_str()
                );
            }
            transaction.execute(
                "UPDATE task_worktrees SET state = 'creating', updated_at_ms = ?1
                 WHERE task_id = ?2 AND state = 'failed'",
                params![now, task_id],
            )?;
            event_type = "task.worktree_creation_retried";
        } else {
            transaction.execute(
                "INSERT INTO task_worktrees
                     (task_id, worktree_path, branch_name, base_commit, state, created_at_ms, updated_at_ms)
                 VALUES (?1, ?2, ?3, ?4, 'creating', ?5, ?5)",
                params![task_id, worktree_path, branch_name, base_commit, now],
            )?;
            event_type = "task.worktree_creation_started";
        }
        let event_data = serde_json::to_string(&serde_json::json!({
            "worktree_path": worktree_path,
            "branch_name": branch_name,
            "base_commit": base_commit,
        }))?;
        transaction.execute(
            "INSERT INTO task_events (task_id, occurred_at_ms, actor, event_type, reason, data_json)
             VALUES (?1, ?2, 'talos', ?3, 'TALOS began explicit task worktree creation', ?4)",
            params![task_id, now, event_type, event_data],
        )?;
        transaction
            .commit()
            .context("Cannot record worktree creation start")?;
        self.task_worktree(task_id)?
            .context("Worktree creation record disappeared")
    }

    pub fn task_worktree(&self, task_id: i64) -> Result<Option<TaskWorktreeRecord>> {
        self.connection
            .query_row(
                "SELECT task_id, worktree_path, branch_name, base_commit, state,
                        created_at_ms, updated_at_ms, retired_at_ms, writer_routed_at_ms
                 FROM task_worktrees WHERE task_id = ?1",
                [task_id],
                map_task_worktree,
            )
            .optional()
            .context("Cannot read task worktree")
    }

    pub fn task_worktrees(&self) -> Result<Vec<TaskWorktreeRecord>> {
        let mut statement = self.connection.prepare(
            "SELECT task_id, worktree_path, branch_name, base_commit, state,
                    created_at_ms, updated_at_ms, retired_at_ms, writer_routed_at_ms
             FROM task_worktrees ORDER BY task_id",
        )?;
        statement
            .query_map([], map_task_worktree)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("Cannot list task worktrees")
    }

    pub fn update_task_worktree_state(
        &mut self,
        task_id: i64,
        target: TaskWorktreeState,
        reason: &str,
    ) -> Result<TaskWorktreeRecord> {
        validate_record_text("Worktree state reason", reason, 1_000)?;
        if contains_sensitive(reason) {
            bail!("Worktree state reason must not contain credentials or private keys");
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("Cannot update task worktree state")?;
        let current = transaction
            .query_row(
                "SELECT task_id, worktree_path, branch_name, base_commit, state,
                        created_at_ms, updated_at_ms, retired_at_ms, writer_routed_at_ms
                 FROM task_worktrees WHERE task_id = ?1",
                [task_id],
                map_task_worktree,
            )
            .optional()?
            .with_context(|| format!("Task #{task_id} has no worktree record"))?;
        let allowed = matches!(
            (&current.state, &target),
            (TaskWorktreeState::Creating, TaskWorktreeState::Active)
                | (TaskWorktreeState::Creating, TaskWorktreeState::Failed)
                | (
                    TaskWorktreeState::Creating,
                    TaskWorktreeState::NeedsAttention
                )
                | (TaskWorktreeState::Active, TaskWorktreeState::NeedsAttention)
                | (TaskWorktreeState::Active, TaskWorktreeState::Retired)
        );
        if !allowed {
            bail!(
                "Invalid worktree state transition for task #{task_id}: {} to {}",
                current.state.as_str(),
                target.as_str()
            );
        }
        let now = now_ms()?;
        let retired_at = if target == TaskWorktreeState::Retired {
            Some(now)
        } else {
            None
        };
        transaction.execute(
            "UPDATE task_worktrees SET state = ?1, updated_at_ms = ?2, retired_at_ms = ?3
             WHERE task_id = ?4 AND state = ?5",
            params![
                target.as_str(),
                now,
                retired_at,
                task_id,
                current.state.as_str()
            ],
        )?;
        let event_type = match target {
            TaskWorktreeState::Active => "task.worktree_created",
            TaskWorktreeState::Failed => "task.worktree_creation_failed",
            TaskWorktreeState::NeedsAttention => "task.worktree_needs_attention",
            TaskWorktreeState::Retired => "task.worktree_retired",
            TaskWorktreeState::Creating => unreachable!("creating is not a transition target"),
        };
        let event_data = serde_json::to_string(&serde_json::json!({
            "worktree_path": current.worktree_path,
            "branch_name": current.branch_name,
            "state": target.as_str(),
        }))?;
        transaction.execute(
            "INSERT INTO task_events (task_id, occurred_at_ms, actor, event_type, reason, data_json)
             VALUES (?1, ?2, 'talos', ?3, ?4, ?5)",
            params![task_id, now, event_type, reason, event_data],
        )?;
        transaction
            .commit()
            .context("Cannot commit task worktree state")?;
        self.task_worktree(task_id)?
            .context("Task worktree disappeared after state update")
    }

    pub fn mark_task_worktree_writer_routed(&mut self, task_id: i64) -> Result<bool> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("Cannot record task worktree Writer routing")?;
        let record: Option<(String, Option<i64>)> = transaction
            .query_row(
                "SELECT state, writer_routed_at_ms FROM task_worktrees WHERE task_id = ?1",
                [task_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((state, routed_at_ms)) = record else {
            transaction.commit()?;
            return Ok(false);
        };
        if state == TaskWorktreeState::Retired.as_str() {
            transaction.commit()?;
            return Ok(false);
        }
        if state != TaskWorktreeState::Active.as_str() {
            bail!("Task #{task_id} worktree is {state}; it cannot become a Writer workspace");
        }
        if routed_at_ms.is_some() {
            transaction.commit()?;
            return Ok(false);
        }
        let prior_attempt_count: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM writer_attempts WHERE task_id = ?1",
            [task_id],
            |row| row.get(0),
        )?;
        if prior_attempt_count != 0 {
            transaction.commit()?;
            return Ok(false);
        }
        let task_status: Option<String> = transaction
            .query_row("SELECT status FROM tasks WHERE id = ?1", [task_id], |row| {
                row.get(0)
            })
            .optional()?;
        if task_status.as_deref() != Some(TaskStatus::Queued.as_str()) {
            bail!("Task #{task_id} must be queued before first Writer routing to its worktree");
        }
        let now = now_ms()?;
        transaction.execute(
            "UPDATE task_worktrees SET writer_routed_at_ms = ?1, updated_at_ms = ?1
             WHERE task_id = ?2 AND state = 'active' AND writer_routed_at_ms IS NULL",
            params![now, task_id],
        )?;
        transaction.execute(
            "INSERT INTO task_events (task_id, occurred_at_ms, actor, event_type, reason, data_json)
             VALUES (?1, ?2, 'talos', 'task.worktree_writer_routed', 'The first Writer attempt was routed to the active task worktree', '{}')",
            params![task_id, now],
        )?;
        transaction
            .commit()
            .context("Cannot record task worktree Writer route")?;
        Ok(true)
    }

    pub fn begin_task_worktree_integration(
        &mut self,
        task_id: i64,
        base_commit: &str,
        source_head_commit: &str,
        revision_id: &str,
        patch_sha256: &str,
        changed_paths_json: &str,
    ) -> Result<TaskWorktreeIntegrationRecord> {
        validate_git_revision(base_commit)?;
        validate_git_revision(source_head_commit)?;
        validate_review_revision_id(revision_id)?;
        validate_sha256_id(patch_sha256, "Worktree integration patch digest")?;
        validate_changed_paths_json(changed_paths_json)?;
        let changed_paths: Vec<String> = serde_json::from_str(changed_paths_json)?;
        if changed_paths.is_empty() {
            bail!("Cannot integrate an empty task worktree revision");
        }

        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("Cannot start task worktree integration")?;
        let task_status: Option<String> = transaction
            .query_row("SELECT status FROM tasks WHERE id = ?1", [task_id], |row| {
                row.get(0)
            })
            .optional()?;
        if task_status.as_deref() != Some(TaskStatus::Reviewing.as_str()) {
            bail!("Task #{task_id} must have a successful Writer result before integration");
        }
        let worktree: Option<(String, Option<i64>)> = transaction
            .query_row(
                "SELECT state, writer_routed_at_ms FROM task_worktrees WHERE task_id = ?1",
                [task_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if worktree.as_ref().map(|(state, _)| state.as_str())
            != Some(TaskWorktreeState::Active.as_str())
            || worktree
                .as_ref()
                .is_none_or(|(_, routed_at_ms)| routed_at_ms.is_none())
        {
            bail!("Task #{task_id} does not have an active managed worktree");
        }
        let latest_writer: Option<(String, Option<String>)> = transaction
            .query_row(
                "SELECT state, base_revision FROM writer_attempts
                 WHERE task_id = ?1 ORDER BY id DESC LIMIT 1",
                [task_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if latest_writer != Some(("succeeded".to_owned(), Some(base_commit.to_owned()))) {
            bail!("Worktree integration must match the latest successful Writer base revision");
        }
        let now = now_ms()?;
        transaction.execute(
            "INSERT INTO task_worktree_integrations
                 (task_id, state, base_commit, source_head_commit, revision_id, patch_sha256,
                  changed_paths_json, started_at_ms, updated_at_ms)
             VALUES (?1, 'applying', ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
            params![
                task_id,
                base_commit,
                source_head_commit,
                revision_id,
                patch_sha256,
                changed_paths_json,
                now
            ],
        )?;
        let event_data = serde_json::to_string(&serde_json::json!({
            "base_commit": base_commit,
            "source_head_commit": source_head_commit,
            "revision_id": revision_id,
            "patch_sha256": patch_sha256,
            "changed_paths": changed_paths,
        }))?;
        transaction.execute(
            "INSERT INTO task_events (task_id, occurred_at_ms, actor, event_type, reason, data_json)
             VALUES (?1, ?2, 'talos', 'task.worktree_integration_started', 'Operator explicitly requested integration of the reviewed task workspace into the primary project tree', ?3)",
            params![task_id, now, event_data],
        )?;
        transaction
            .commit()
            .context("Cannot record task worktree integration start")?;
        self.task_worktree_integration(task_id)?
            .context("Task worktree integration record disappeared")
    }

    pub fn set_task_worktree_integration_state(
        &mut self,
        task_id: i64,
        target_state: &str,
        reason: &str,
    ) -> Result<TaskWorktreeIntegrationRecord> {
        if !matches!(target_state, "pending" | "needs_attention") {
            bail!("Unsupported task worktree integration state");
        }
        validate_record_text("Worktree integration reason", reason, 1_000)?;
        if contains_sensitive(reason) {
            bail!("Worktree integration reason must not contain credentials or private keys");
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("Cannot update task worktree integration state")?;
        let current: Option<String> = transaction
            .query_row(
                "SELECT state FROM task_worktree_integrations WHERE task_id = ?1",
                [task_id],
                |row| row.get(0),
            )
            .optional()?;
        let valid_transition = matches!(
            (current.as_deref(), target_state),
            (Some("applying"), "pending" | "needs_attention")
                | (Some("pending"), "needs_attention")
                | (Some("needs_attention"), "pending")
        );
        if !valid_transition {
            bail!("Task #{task_id} has no worktree integration in progress");
        }
        let now = now_ms()?;
        transaction.execute(
            "UPDATE task_worktree_integrations SET state = ?1, updated_at_ms = ?2
             WHERE task_id = ?3 AND state = ?4",
            params![target_state, now, task_id, current],
        )?;
        let data_json = serde_json::to_string(&serde_json::json!({
            "state": target_state,
        }))?;
        let event_type = if target_state == "pending" {
            "task.worktree_integration_pending"
        } else {
            "task.worktree_integration_needs_attention"
        };
        transaction.execute(
            "INSERT INTO task_events (task_id, occurred_at_ms, actor, event_type, reason, data_json)
             VALUES (?1, ?2, 'talos', ?3, ?4, ?5)",
            params![task_id, now, event_type, reason, data_json],
        )?;
        transaction
            .commit()
            .context("Cannot commit worktree integration state")?;
        self.task_worktree_integration(task_id)?
            .context("Task worktree integration record disappeared")
    }

    pub fn task_worktree_integration(
        &self,
        task_id: i64,
    ) -> Result<Option<TaskWorktreeIntegrationRecord>> {
        self.connection
            .query_row(
                "SELECT task_id, state, base_commit, source_head_commit, revision_id, patch_sha256,
                        changed_paths_json, integration_commit, started_at_ms, updated_at_ms, integrated_at_ms
                 FROM task_worktree_integrations WHERE task_id = ?1",
                [task_id],
                map_task_worktree_integration,
            )
            .optional()
            .context("Cannot read task worktree integration")
    }

    pub fn complete_task_worktree_integration(
        &mut self,
        task_id: i64,
        integration_commit: &str,
    ) -> Result<TaskWorktreeIntegrationRecord> {
        validate_git_revision(integration_commit)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("Cannot finalize task worktree integration")?;
        let integration: Option<(String, String, String, String, String, i64)> = transaction
            .query_row(
                "SELECT state, base_commit, source_head_commit, revision_id, patch_sha256,
                        updated_at_ms
                 FROM task_worktree_integrations WHERE task_id = ?1",
                [task_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .optional()?;
        let Some((
            state,
            base_commit,
            source_head_commit,
            revision_id,
            patch_sha256,
            integration_updated_at_ms,
        )) = integration
        else {
            bail!("Task #{task_id} has no worktree integration record");
        };
        if state != "pending" {
            bail!("Task #{task_id} integration is {state}, not pending");
        }
        let task_status: Option<String> = transaction
            .query_row("SELECT status FROM tasks WHERE id = ?1", [task_id], |row| {
                row.get(0)
            })
            .optional()?;
        if task_status.as_deref() != Some(TaskStatus::Verifying.as_str()) {
            bail!("Task #{task_id} must remain verifying until integration is finalized");
        }
        let review: Option<FinalizeReviewRow> = transaction
            .query_row(
                "SELECT id, decision, base_revision, revision_id, patch_sha256, changed_paths_json
                     FROM critic_reviews WHERE task_id = ?1 ORDER BY id DESC LIMIT 1",
                [task_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .optional()?;
        let Some((review_id, decision, review_base, review_revision, review_patch, review_paths)) =
            review
        else {
            bail!("Task #{task_id} has no Critic review for integration");
        };
        if decision != "approve" || review_base != base_commit {
            bail!(
                "Task #{task_id} latest Critic approval is not based on the integrated project base"
            );
        }
        let verification: Option<(i64, String, String, String, String, String, i64)> = transaction
            .query_row(
                "SELECT critic_review_id, status, base_revision, revision_id, patch_sha256,
                        changed_paths_json, started_at_ms
                 FROM verification_runs WHERE task_id = ?1 ORDER BY id DESC LIMIT 1",
                [task_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                    ))
                },
            )
            .optional()?;
        let Some((
            verification_review,
            verification_status,
            verification_base,
            verification_revision,
            verification_patch,
            verification_paths,
            verification_started_at_ms,
        )) = verification
        else {
            bail!("Task #{task_id} has no verification record for integration");
        };
        if verification_review != review_id
            || verification_status != "passed"
            || verification_base != review_base
            || verification_revision != review_revision
            || Some(verification_patch.as_str()) != review_patch.as_deref()
            || Some(verification_paths.as_str()) != review_paths.as_deref()
            || verification_started_at_ms < integration_updated_at_ms
        {
            bail!(
                "Task #{task_id} latest verification does not match the approved post-integration revision"
            );
        }

        let now = now_ms()?;
        transaction.execute(
            "UPDATE task_worktree_integrations
             SET state = 'integrated', integration_commit = ?1, updated_at_ms = ?2, integrated_at_ms = ?2
             WHERE task_id = ?3 AND state = 'pending'",
            params![integration_commit, now, task_id],
        )?;
        let data_json = serde_json::to_string(&serde_json::json!({
            "integration_commit": integration_commit,
            "source_head_commit": source_head_commit,
            "revision_id": revision_id,
            "patch_sha256": patch_sha256,
        }))?;
        transaction.execute(
            "INSERT INTO task_events (task_id, occurred_at_ms, actor, event_type, reason, data_json)
             VALUES (?1, ?2, 'talos', 'task.worktree_integrated', 'The operator committed the exact reviewed and verified task patch into the primary project tree', ?3)",
            params![task_id, now, data_json],
        )?;
        let updated = transaction.execute(
            "UPDATE tasks SET status = 'complete', updated_at_ms = ?1
             WHERE id = ?2 AND status = 'verifying'",
            params![now, task_id],
        )?;
        if updated != 1 {
            bail!("Task #{task_id} changed before integration could complete it");
        }
        let transition_data = serde_json::to_string(&serde_json::json!({
            "from": "verifying",
            "to": "complete",
            "integration_commit": integration_commit,
        }))?;
        transaction.execute(
            "INSERT INTO task_events (task_id, occurred_at_ms, actor, event_type, reason, data_json)
             VALUES (?1, ?2, 'talos', 'task.status_changed', 'Exact verified changes were committed to the primary project tree', ?3)",
            params![task_id, now, transition_data],
        )?;
        transaction
            .commit()
            .context("Cannot commit completed task worktree integration")?;
        self.task_worktree_integration(task_id)?
            .context("Task worktree integration disappeared after finalization")
    }

    pub fn begin_github_publication(
        &mut self,
        input: &GithubPublicationInput<'_>,
    ) -> Result<GithubPublicationRecord> {
        validate_github_repository(input.repository)?;
        let repository = input.repository.to_ascii_lowercase();
        validate_git_remote_name(input.remote_name)?;
        validate_git_branch_name(input.base_branch, "base branch")?;
        validate_git_branch_name(input.head_branch, "head branch")?;
        if input.base_branch != "master" {
            bail!("GitHub publication base branch must be `master`");
        }
        if !input.head_branch.starts_with("talos/") || input.head_branch == input.base_branch {
            bail!("GitHub publication head must be a feature branch under `talos/`");
        }
        validate_git_revision(input.base_commit)?;
        validate_git_revision(input.head_commit)?;
        if input.base_commit == input.head_commit {
            bail!("GitHub publication requires a head commit that differs from its base");
        }
        validate_sha256_id(input.diff_sha256, "GitHub publication diff digest")?;

        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("Cannot start GitHub publication record")?;
        let existing: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM github_publications
                           WHERE repository = ?1 AND head_branch = ?2)",
            params![repository, input.head_branch],
            |row| row.get(0),
        )?;
        if existing {
            bail!(
                "A GitHub publication record already exists for {repository} branch {}",
                input.head_branch
            );
        }
        let now = now_ms()?;
        transaction.execute(
            "INSERT INTO github_publications
                 (repository, remote_name, base_branch, head_branch, base_commit, head_commit,
                  diff_sha256, state, created_at_ms, updated_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'push_pending', ?8, ?8)",
            params![
                repository,
                input.remote_name,
                input.base_branch,
                input.head_branch,
                input.base_commit,
                input.head_commit,
                input.diff_sha256,
                now,
            ],
        )?;
        let publication_id = transaction.last_insert_rowid();
        transaction.execute(
            "INSERT INTO github_publication_events
                 (publication_id, occurred_at_ms, from_state, to_state, reason)
             VALUES (?1, ?2, NULL, 'push_pending',
                     'TALOS recorded the exact publication identity before any remote push')",
            params![publication_id, now],
        )?;
        transaction
            .commit()
            .context("Cannot commit GitHub publication intent")?;
        self.github_publication(publication_id)?
            .context("GitHub publication record disappeared after creation")
    }

    pub fn github_publication(
        &self,
        publication_id: i64,
    ) -> Result<Option<GithubPublicationRecord>> {
        self.connection
            .query_row(
                "SELECT id, repository, remote_name, base_branch, head_branch, base_commit,
                        head_commit, diff_sha256, state, pull_request_url, created_at_ms, updated_at_ms
                 FROM github_publications WHERE id = ?1",
                [publication_id],
                map_github_publication,
            )
            .optional()
            .context("Cannot read GitHub publication record")
    }

    pub fn latest_github_publication_for_branch(
        &self,
        repository: &str,
        head_branch: &str,
    ) -> Result<Option<GithubPublicationRecord>> {
        validate_github_repository(repository)?;
        validate_git_branch_name(head_branch, "head branch")?;
        let repository = repository.to_ascii_lowercase();
        self.connection
            .query_row(
                "SELECT id, repository, remote_name, base_branch, head_branch, base_commit,
                        head_commit, diff_sha256, state, pull_request_url, created_at_ms, updated_at_ms
                 FROM github_publications WHERE repository = ?1 AND head_branch = ?2
                 ORDER BY id DESC LIMIT 1",
                params![repository, head_branch],
                map_github_publication,
            )
            .optional()
            .context("Cannot read GitHub publication for branch")
    }

    pub fn github_publication_events(
        &self,
        publication_id: i64,
    ) -> Result<Vec<GithubPublicationEvent>> {
        if self.github_publication(publication_id)?.is_none() {
            bail!("GitHub publication #{publication_id} does not exist");
        }
        let mut statement = self.connection.prepare(
            "SELECT id, publication_id, occurred_at_ms, from_state, to_state, reason
             FROM github_publication_events WHERE publication_id = ?1 ORDER BY id",
        )?;
        let events = statement
            .query_map([publication_id], map_github_publication_event)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(events)
    }

    pub fn record_github_ci_snapshot(
        &mut self,
        input: &GithubCiSnapshotInput<'_>,
    ) -> Result<GithubCiSnapshotRecord> {
        validate_git_revision(input.head_commit)?;
        if !matches!(
            input.result,
            "pass" | "fail" | "pending" | "timeout" | "unknown" | "no_checks"
        ) {
            bail!("Unsupported GitHub CI snapshot result");
        }
        validate_github_ci_checks(input.checks)?;
        let checks_json = serde_json::to_string(input.checks)?;
        let snapshot_sha256 =
            github_ci_snapshot_sha256(input.head_commit, input.result, &checks_json);

        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("Cannot start GitHub CI snapshot record")?;
        let publication: Option<(String, String)> = transaction
            .query_row(
                "SELECT state, head_commit FROM github_publications WHERE id = ?1",
                [input.publication_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((state, recorded_head_commit)) = publication else {
            bail!(
                "GitHub publication #{} does not exist",
                input.publication_id
            );
        };
        if state != "open" || recorded_head_commit != input.head_commit {
            bail!("CI evidence requires the exact commit of a recorded open pull request");
        }
        let checked_at_ms = now_ms()?;
        transaction.execute(
            "INSERT OR IGNORE INTO github_ci_snapshots
                 (publication_id, head_commit, result, snapshot_sha256, checks_json, checked_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                input.publication_id,
                input.head_commit,
                input.result,
                snapshot_sha256,
                checks_json,
                checked_at_ms,
            ],
        )?;
        transaction
            .commit()
            .context("Cannot commit GitHub CI snapshot")?;
        self.connection
            .query_row(
                "SELECT id, publication_id, head_commit, result, snapshot_sha256,
                        checks_json, checked_at_ms
                 FROM github_ci_snapshots
                 WHERE publication_id = ?1 AND head_commit = ?2 AND snapshot_sha256 = ?3",
                params![input.publication_id, input.head_commit, snapshot_sha256],
                map_github_ci_snapshot,
            )
            .optional()?
            .context("GitHub CI snapshot disappeared after insertion")
    }

    pub fn latest_github_ci_snapshot(
        &self,
        publication_id: i64,
    ) -> Result<Option<GithubCiSnapshotRecord>> {
        if self.github_publication(publication_id)?.is_none() {
            bail!("GitHub publication #{publication_id} does not exist");
        }
        self.connection
            .query_row(
                "SELECT id, publication_id, head_commit, result, snapshot_sha256,
                        checks_json, checked_at_ms
                 FROM github_ci_snapshots WHERE publication_id = ?1
                 ORDER BY id DESC LIMIT 1",
                [publication_id],
                map_github_ci_snapshot,
            )
            .optional()
            .context("Cannot read GitHub CI snapshot")
    }

    pub fn transition_github_publication(
        &mut self,
        publication_id: i64,
        target_state: &str,
        reason: &str,
        pull_request_url: Option<&str>,
    ) -> Result<GithubPublicationRecord> {
        if !matches!(
            target_state,
            "pushed" | "push_unknown" | "pr_pending" | "pr_unknown" | "open" | "needs_attention"
        ) {
            bail!("Unsupported GitHub publication state");
        }
        validate_record_text("GitHub publication reason", reason, 1_000)?;
        if contains_sensitive(reason) {
            bail!("GitHub publication reason must not contain credentials or private keys");
        }

        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("Cannot transition GitHub publication state")?;
        let current: Option<(String, String)> = transaction
            .query_row(
                "SELECT state, repository FROM github_publications WHERE id = ?1",
                [publication_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((current_state, repository)) = current else {
            bail!("GitHub publication #{publication_id} does not exist");
        };
        let valid_transition = matches!(
            (current_state.as_str(), target_state),
            (
                "push_pending",
                "pushed" | "push_unknown" | "needs_attention"
            ) | ("push_unknown", "pushed" | "needs_attention")
                | ("pushed", "pr_pending" | "needs_attention")
                | ("pr_pending", "open" | "pr_unknown" | "needs_attention")
                | ("pr_unknown", "open" | "needs_attention")
        );
        if !valid_transition {
            bail!(
                "GitHub publication #{publication_id} cannot transition from {current_state} to {target_state}"
            );
        }

        match (target_state, pull_request_url) {
            ("open", Some(url)) => validate_github_pull_request_url(url, &repository)?,
            ("open", None) => bail!("An open GitHub publication must record its pull request URL"),
            (_, Some(_)) => {
                bail!("A pull request URL can only be recorded when the publication becomes open")
            }
            (_, None) => {}
        }
        let now = now_ms()?;
        let changed = transaction.execute(
            "UPDATE github_publications SET state = ?1, pull_request_url = ?2, updated_at_ms = ?3
             WHERE id = ?4 AND state = ?5",
            params![
                target_state,
                pull_request_url,
                now,
                publication_id,
                current_state
            ],
        )?;
        if changed != 1 {
            bail!("GitHub publication #{publication_id} changed while updating its state");
        }
        transaction.execute(
            "INSERT INTO github_publication_events
                 (publication_id, occurred_at_ms, from_state, to_state, reason)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![publication_id, now, current_state, target_state, reason],
        )?;
        transaction
            .commit()
            .context("Cannot commit GitHub publication transition")?;
        self.github_publication(publication_id)?
            .context("GitHub publication record disappeared after transition")
    }

    pub fn add_task_dependency(&mut self, task_id: i64, prerequisite_task_id: i64) -> Result<bool> {
        if task_id == prerequisite_task_id {
            bail!("A task cannot depend on itself");
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("Cannot add task dependency")?;
        let task_status: Option<String> = transaction
            .query_row("SELECT status FROM tasks WHERE id = ?1", [task_id], |row| {
                row.get(0)
            })
            .optional()?;
        let Some(task_status) = task_status else {
            bail!("Task #{task_id} does not exist");
        };
        if task_status != TaskStatus::Queued.as_str() {
            bail!("Dependencies can only be added to a queued task");
        }
        let prerequisite_status: Option<String> = transaction
            .query_row(
                "SELECT status FROM tasks WHERE id = ?1",
                [prerequisite_task_id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(prerequisite_status) = prerequisite_status else {
            bail!("Prerequisite task #{prerequisite_task_id} does not exist");
        };
        let already_exists: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM task_dependencies WHERE task_id = ?1 AND depends_on_task_id = ?2)",
            params![task_id, prerequisite_task_id],
            |row| row.get(0),
        )?;
        if already_exists {
            transaction.commit()?;
            return Ok(false);
        }
        let closes_cycle: bool = transaction.query_row(
            "WITH RECURSIVE prerequisites(task_id) AS (
                 SELECT ?1
                 UNION
                 SELECT edges.depends_on_task_id
                 FROM task_dependencies AS edges
                 JOIN prerequisites ON edges.task_id = prerequisites.task_id
             )
             SELECT EXISTS(SELECT 1 FROM prerequisites WHERE task_id = ?2)",
            params![prerequisite_task_id, task_id],
            |row| row.get(0),
        )?;
        if closes_cycle {
            bail!("This dependency would create a cycle in the task graph");
        }
        let now = now_ms()?;
        transaction.execute(
            "INSERT INTO task_dependencies (task_id, depends_on_task_id, created_at_ms)
             VALUES (?1, ?2, ?3)",
            params![task_id, prerequisite_task_id, now],
        )?;
        let event_data = serde_json::to_string(&serde_json::json!({
            "prerequisite_task_id": prerequisite_task_id,
            "prerequisite_status": prerequisite_status,
        }))?;
        transaction.execute(
            "INSERT INTO task_events (task_id, occurred_at_ms, actor, event_type, reason, data_json)
             VALUES (?1, ?2, 'talos', 'task.dependency_added', 'Task now waits for a prerequisite task', ?3)",
            params![task_id, now, event_data],
        )?;
        transaction
            .commit()
            .context("Cannot commit task dependency")?;
        Ok(true)
    }

    pub fn task_dependencies(&self, task_id: i64) -> Result<Vec<TaskDependencyRecord>> {
        let mut statement = self.connection.prepare(
            "SELECT edges.task_id, edges.depends_on_task_id, tasks.status, integrations.state
             FROM task_dependencies AS edges
             JOIN tasks ON tasks.id = edges.depends_on_task_id
             LEFT JOIN task_worktree_integrations AS integrations
               ON integrations.task_id = tasks.id
             WHERE edges.task_id = ?1 ORDER BY edges.depends_on_task_id",
        )?;
        statement
            .query_map([task_id], map_task_dependency_record)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("Cannot list task dependencies")
    }

    pub fn task_dependency_summary(
        &self,
        task_id: i64,
        limit: usize,
    ) -> Result<(usize, Vec<TaskDependencyRecord>)> {
        let limit = i64::try_from(limit).context("Dependency summary limit is out of range")?;
        let dependency_count: i64 = self.connection.query_row(
            "SELECT COUNT(*) FROM task_dependencies WHERE task_id = ?1",
            [task_id],
            |row| row.get(0),
        )?;
        let dependency_count = usize::try_from(dependency_count)
            .context("Dependency count is outside the supported range")?;
        let mut statement = self.connection.prepare(
            "SELECT edges.task_id, edges.depends_on_task_id, tasks.status, integrations.state
             FROM task_dependencies AS edges
             JOIN tasks ON tasks.id = edges.depends_on_task_id
             LEFT JOIN task_worktree_integrations AS integrations
               ON integrations.task_id = tasks.id
             WHERE edges.task_id = ?1 ORDER BY edges.depends_on_task_id LIMIT ?2",
        )?;
        let dependencies = statement
            .query_map(params![task_id, limit], map_task_dependency_record)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("Cannot summarize task dependencies")?;
        Ok((dependency_count, dependencies))
    }

    pub fn runnable_queued_tasks(&self) -> Result<Vec<TaskRecord>> {
        let tasks = self.list_tasks()?;
        let mut statement = self.connection.prepare(
            "SELECT EXISTS(
                 SELECT 1 FROM task_dependencies AS edges
                 JOIN tasks AS prerequisite ON prerequisite.id = edges.depends_on_task_id
                 WHERE edges.task_id = ?1 AND (
                     prerequisite.status <> 'complete'
                     OR EXISTS(SELECT 1 FROM task_worktree_integrations AS integrations
                               WHERE integrations.task_id = prerequisite.id
                                 AND integrations.state <> 'integrated')
                 )
             )",
        )?;
        let mut runnable = Vec::new();
        for task in tasks {
            if task.status != TaskStatus::Queued {
                continue;
            }
            let blocked: bool = statement.query_row([task.id], |row| row.get(0))?;
            if !blocked {
                runnable.push(task);
            }
        }
        Ok(runnable)
    }

    pub fn queued_tasks_waiting_on_dependencies(&self) -> Result<Vec<(i64, Vec<i64>)>> {
        let mut statement = self.connection.prepare(
            "SELECT tasks.id, edges.depends_on_task_id
             FROM tasks
             JOIN task_dependencies AS edges ON edges.task_id = tasks.id
             JOIN tasks AS prerequisite ON prerequisite.id = edges.depends_on_task_id
             WHERE tasks.status = 'queued' AND (
                 prerequisite.status <> 'complete'
                 OR EXISTS(SELECT 1 FROM task_worktree_integrations AS integrations
                           WHERE integrations.task_id = prerequisite.id
                             AND integrations.state <> 'integrated')
             )
             ORDER BY tasks.id, edges.depends_on_task_id",
        )?;
        let rows = statement
            .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut waiting: Vec<(i64, Vec<i64>)> = Vec::new();
        for (task_id, prerequisite_task_id) in rows {
            if let Some((_, prerequisites)) = waiting.last_mut().filter(|(id, _)| *id == task_id) {
                prerequisites.push(prerequisite_task_id);
            } else {
                waiting.push((task_id, vec![prerequisite_task_id]));
            }
        }
        Ok(waiting)
    }

    pub fn task_verification_checks(&self, task_id: i64) -> Result<Vec<ProjectCheck>> {
        let mut statement = self.connection.prepare(
            "SELECT check_id, argv_json, version_argv_json, working_directory
             FROM task_verification_checks WHERE task_id = ?1 ORDER BY ordinal",
        )?;
        let rows = statement.query_map([task_id], |row| {
            let argv_json: String = row.get(1)?;
            let version_argv_json: String = row.get(2)?;
            Ok((
                row.get::<_, String>(0)?,
                argv_json,
                version_argv_json,
                row.get::<_, String>(3)?,
            ))
        })?;
        let mut checks = Vec::new();
        for row in rows {
            let (id, argv_json, version_argv_json, working_directory) = row?;
            checks.push(ProjectCheck {
                id,
                argv: serde_json::from_str(&argv_json)
                    .context("Saved project check command is malformed")?,
                version_argv: serde_json::from_str(&version_argv_json)
                    .context("Saved project check version command is malformed")?,
                working_directory,
            });
        }
        validate_project_checks(&checks)?;
        Ok(checks)
    }

    pub fn add_task_verification_checks(
        &mut self,
        task_id: i64,
        checks: &[ProjectCheck],
    ) -> Result<()> {
        if checks.is_empty() {
            bail!("Add at least one project check");
        }
        validate_project_checks(checks)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("Cannot start task verification requirement transaction")?;
        let status: Option<String> = transaction
            .query_row("SELECT status FROM tasks WHERE id = ?1", [task_id], |row| {
                row.get(0)
            })
            .optional()?;
        let Some(status) = status else {
            bail!("Task #{task_id} does not exist");
        };
        if status != TaskStatus::Queued.as_str() && status != TaskStatus::Reviewing.as_str() {
            bail!("Project checks can only be added while task #{task_id} is queued or reviewing");
        }
        for check in checks {
            let ordinal: i64 = transaction.query_row(
                "SELECT COALESCE(MAX(ordinal) + 1, 0) FROM task_verification_checks WHERE task_id = ?1",
                [task_id],
                |row| row.get(0),
            )?;
            transaction.execute(
                "INSERT INTO task_verification_checks
                     (task_id, check_id, ordinal, argv_json, version_argv_json, working_directory)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    task_id,
                    check.id,
                    ordinal,
                    serde_json::to_string(&check.argv)?,
                    serde_json::to_string(&check.version_argv)?,
                    check.working_directory,
                ],
            )?;
        }
        let now = now_ms()?;
        let event_data = serde_json::to_string(&serde_json::json!({
            "check_ids": checks.iter().map(|check| check.id.as_str()).collect::<Vec<_>>(),
        }))?;
        transaction.execute(
            "INSERT INTO task_events (task_id, occurred_at_ms, actor, event_type, reason, data_json)
             VALUES (?1, ?2, 'talos', 'task.verification_checks_added', 'Project checks were bound to the task before review', ?3)",
            params![task_id, now, event_data],
        )?;
        transaction
            .commit()
            .context("Cannot commit task verification requirements")?;
        Ok(())
    }

    pub fn transition_task(
        &mut self,
        task_id: i64,
        target: TaskStatus,
        actor: &str,
        reason: &str,
    ) -> Result<TaskRecord> {
        if actor.trim().is_empty() || reason.trim().is_empty() {
            bail!("Task transition requires a non-empty actor and reason");
        }
        if contains_sensitive(actor) || contains_sensitive(reason) {
            bail!("Task transition metadata contains secret-like text");
        }
        if target == TaskStatus::Complete {
            bail!(
                "Task completion is disabled until the verification evidence gate is implemented"
            );
        }

        let now = now_ms()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("Cannot start task transition transaction")?;
        let current: Option<(String, String, String, i64, i64)> = transaction
            .query_row(
                "SELECT title, acceptance_criteria, status, created_at_ms, updated_at_ms
                 FROM tasks WHERE id = ?1",
                [task_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .optional()?;
        let Some((title, acceptance_criteria, status_value, created_at_ms, _)) = current else {
            bail!("Task #{task_id} does not exist");
        };
        let current_status = parse_task_status(&status_value)
            .with_context(|| format!("Task #{task_id} has an unknown status"))?;
        if !is_allowed_transition(&current_status, &target) {
            bail!(
                "Task #{task_id} cannot transition from {} to {}",
                current_status.as_str(),
                target.as_str()
            );
        }

        let changed = transaction.execute(
            "UPDATE tasks SET status = ?1, updated_at_ms = ?2 WHERE id = ?3 AND status = ?4",
            params![target.as_str(), now, task_id, current_status.as_str()],
        )?;
        if changed != 1 {
            bail!("Task #{task_id} changed while its transition was being recorded");
        }
        let event_data = serde_json::to_string(&serde_json::json!({
            "from": current_status.as_str(),
            "to": target.as_str(),
        }))
        .context("Cannot serialize task transition event")?;
        transaction.execute(
            "INSERT INTO task_events (task_id, occurred_at_ms, actor, event_type, reason, data_json)
             VALUES (?1, ?2, ?3, 'task.status_changed', ?4, ?5)",
            params![task_id, now, actor, reason, event_data],
        )?;
        transaction
            .commit()
            .context("Cannot commit task transition")?;

        Ok(TaskRecord {
            id: task_id,
            title,
            acceptance_criteria,
            status: target,
            created_at_ms,
            updated_at_ms: now,
        })
    }

    pub fn list_tasks(&self) -> Result<Vec<TaskRecord>> {
        let mut statement = self.connection.prepare(
            "SELECT id, title, acceptance_criteria, status, created_at_ms, updated_at_ms
             FROM tasks ORDER BY id",
        )?;
        let records = statement
            .query_map([], map_task_record)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(records)
    }

    pub fn get_task(&self, task_id: i64) -> Result<Option<TaskRecord>> {
        self.connection
            .query_row(
                "SELECT id, title, acceptance_criteria, status, created_at_ms, updated_at_ms
                 FROM tasks WHERE id = ?1",
                [task_id],
                map_task_record,
            )
            .optional()
            .context("Cannot read task")
    }

    pub fn task_events(&self, task_id: Option<i64>) -> Result<Vec<TaskEvent>> {
        let mut statement = self.connection.prepare(
            "SELECT id, task_id, occurred_at_ms, actor, event_type, reason, data_json
             FROM task_events WHERE (?1 IS NULL OR task_id = ?1) ORDER BY id",
        )?;
        let events = statement
            .query_map([task_id], |row| {
                Ok(TaskEvent {
                    id: row.get(0)?,
                    task_id: row.get(1)?,
                    occurred_at_ms: row.get(2)?,
                    actor: row.get(3)?,
                    event_type: row.get(4)?,
                    reason: row.get(5)?,
                    data_json: row.get(6)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(events)
    }

    pub fn project_control_state(&self) -> Result<ProjectControlState> {
        self.connection
            .query_row(
                "SELECT queue_paused, paused_at_ms, pause_reason, pause_revision_id,
                        stop_requested_attempt_id
                 FROM project_control_state WHERE singleton = 1",
                [],
                |row| {
                    Ok(ProjectControlState {
                        queue_paused: row.get::<_, i64>(0)? != 0,
                        paused_at_ms: row.get(1)?,
                        pause_reason: row.get(2)?,
                        pause_revision_id: row.get(3)?,
                        stop_requested_attempt_id: row.get(4)?,
                    })
                },
            )
            .context("Cannot read project control state")
    }

    pub fn pause_project(
        &mut self,
        reason: &str,
        pause_revision_id: Option<&str>,
    ) -> Result<ProjectControlState> {
        validate_record_text("Pause reason", reason, 1_000)?;
        if contains_sensitive(reason) {
            bail!("Pause reason contains secret-like text and was not stored");
        }
        if let Some(revision_id) = pause_revision_id {
            validate_review_revision_id(revision_id)?;
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("Cannot pause the project queue")?;
        let existing = transaction.query_row(
            "SELECT queue_paused, paused_at_ms, pause_reason, pause_revision_id,
                    stop_requested_attempt_id
             FROM project_control_state WHERE singleton = 1",
            [],
            |row| {
                Ok(ProjectControlState {
                    queue_paused: row.get::<_, i64>(0)? != 0,
                    paused_at_ms: row.get(1)?,
                    pause_reason: row.get(2)?,
                    pause_revision_id: row.get(3)?,
                    stop_requested_attempt_id: row.get(4)?,
                })
            },
        )?;
        if existing.queue_paused {
            transaction.commit()?;
            return Ok(existing);
        }
        let now = now_ms()?;
        transaction.execute(
            "UPDATE project_control_state
             SET queue_paused = 1, paused_at_ms = ?1, pause_reason = ?2,
                 pause_revision_id = ?3, updated_at_ms = ?1
             WHERE singleton = 1 AND queue_paused = 0",
            params![now, reason, pause_revision_id],
        )?;
        let data_json = serde_json::to_string(&serde_json::json!({
            "pause_revision_id": pause_revision_id,
        }))?;
        insert_project_control_event(&transaction, now, "queue.paused", reason, &data_json)?;
        transaction.commit().context("Cannot commit queue pause")?;
        Ok(ProjectControlState {
            queue_paused: true,
            paused_at_ms: Some(now),
            pause_reason: Some(reason.to_owned()),
            pause_revision_id: pause_revision_id.map(str::to_owned),
            stop_requested_attempt_id: existing.stop_requested_attempt_id,
        })
    }

    pub fn request_writer_stop(&mut self) -> Result<StopRequestOutcome> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("Cannot request Writer shutdown")?;
        let active_attempt: Option<i64> = transaction
            .query_row(
                "SELECT id FROM writer_attempts
                 WHERE state IN ('launching', 'running') ORDER BY id DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()?;
        let Some(attempt_id) = active_attempt else {
            transaction.commit()?;
            return Ok(StopRequestOutcome::NoActiveWriter);
        };
        let existing: Option<i64> = transaction.query_row(
            "SELECT stop_requested_attempt_id FROM project_control_state WHERE singleton = 1",
            [],
            |row| row.get(0),
        )?;
        if existing == Some(attempt_id) {
            transaction.commit()?;
            return Ok(StopRequestOutcome::Requested { attempt_id });
        }
        let now = now_ms()?;
        transaction.execute(
            "UPDATE project_control_state SET stop_requested_attempt_id = ?1, updated_at_ms = ?2
             WHERE singleton = 1",
            params![attempt_id, now],
        )?;
        let data_json = serde_json::to_string(&serde_json::json!({ "attempt_id": attempt_id }))?;
        insert_project_control_event(
            &transaction,
            now,
            "writer.stop_requested",
            "Operator requested graceful Writer shutdown",
            &data_json,
        )?;
        transaction
            .commit()
            .context("Cannot commit Writer stop request")?;
        Ok(StopRequestOutcome::Requested { attempt_id })
    }

    pub fn writer_stop_requested(&self, attempt_id: i64) -> Result<bool> {
        self.connection
            .query_row(
                "SELECT stop_requested_attempt_id = ?1
                 FROM project_control_state WHERE singleton = 1",
                [attempt_id],
                |row| row.get::<_, Option<bool>>(0),
            )
            .optional()
            .map(|value| value.flatten().unwrap_or(false))
            .context("Cannot inspect Writer stop request")
    }

    pub fn resume_project(
        &mut self,
        current_revision_id: Option<&str>,
        reconcile: bool,
    ) -> Result<ResumeProjectOutcome> {
        if let Some(revision_id) = current_revision_id {
            validate_review_revision_id(revision_id)?;
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("Cannot resume the project queue")?;
        let existing = transaction.query_row(
            "SELECT queue_paused, pause_revision_id
             FROM project_control_state WHERE singleton = 1",
            [],
            |row| Ok((row.get::<_, i64>(0)? != 0, row.get::<_, Option<String>>(1)?)),
        )?;
        let (queue_paused, pause_revision_id) = existing;
        if !queue_paused {
            transaction.commit()?;
            return Ok(ResumeProjectOutcome::AlreadyRunning);
        }
        let unresolved: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM writer_attempts
             WHERE state IN ('launching', 'running')
                OR (state = 'unknown' AND operator_resolution IS NULL)",
            [],
            |row| row.get(0),
        )?;
        if unresolved != 0 {
            bail!("Cannot resume while a Writer attempt is active or unresolved");
        }

        let snapshots_match =
            pause_revision_id.is_some() && pause_revision_id.as_deref() == current_revision_id;
        if !snapshots_match && !reconcile {
            let now = now_ms()?;
            let data_json = serde_json::to_string(&serde_json::json!({
                "pause_revision_id": pause_revision_id,
                "current_revision_id": current_revision_id,
                "reconciliation_required": true,
            }))?;
            insert_project_control_event(
                &transaction,
                now,
                "project.manual_changes_detected",
                "Project Git facts changed during the pause or a safe baseline was unavailable",
                &data_json,
            )?;
            transaction
                .commit()
                .context("Cannot record manual project changes")?;
            return Ok(ResumeProjectOutcome::ReconciliationRequired {
                pause_revision_id,
                current_revision_id: current_revision_id.map(str::to_owned),
            });
        }

        let now = now_ms()?;
        let mut reopened_task_ids = Vec::new();
        if !snapshots_match {
            let mut statement = transaction
                .prepare("SELECT id FROM tasks WHERE status = 'verifying' ORDER BY id")?;
            reopened_task_ids = statement
                .query_map([], |row| row.get(0))?
                .collect::<rusqlite::Result<Vec<i64>>>()?;
            drop(statement);
            for task_id in &reopened_task_ids {
                let changed = transaction.execute(
                    "UPDATE tasks SET status = 'reviewing', updated_at_ms = ?1
                     WHERE id = ?2 AND status = 'verifying'",
                    params![now, task_id],
                )?;
                if changed != 1 {
                    bail!("Task #{task_id} changed during manual project reconciliation");
                }
                let data_json = serde_json::to_string(&serde_json::json!({
                    "from": "verifying",
                    "to": "reviewing",
                    "cause": "manual_project_reconciliation",
                }))?;
                transaction.execute(
                    "INSERT INTO task_events (task_id, occurred_at_ms, actor, event_type, reason, data_json)
                     VALUES (?1, ?2, 'talos', 'task.status_changed',
                             'Manual project changes invalidated the approved revision; a fresh Critic review is required', ?3)",
                    params![task_id, now, data_json],
                )?;
            }
            let data_json = serde_json::to_string(&serde_json::json!({
                "pause_revision_id": pause_revision_id,
                "current_revision_id": current_revision_id,
                "reopened_task_ids": reopened_task_ids,
            }))?;
            insert_project_control_event(
                &transaction,
                now,
                "project.manual_changes_reconciled",
                "Operator accepted the inspected project state; affected approvals were reopened",
                &data_json,
            )?;
        }
        transaction.execute(
            "UPDATE project_control_state
             SET queue_paused = 0, paused_at_ms = NULL, pause_reason = NULL,
                 pause_revision_id = NULL, updated_at_ms = ?1
             WHERE singleton = 1 AND queue_paused = 1",
            [now],
        )?;
        let data_json = serde_json::to_string(&serde_json::json!({
            "reconciled_manual_changes": !snapshots_match,
            "reopened_task_ids": reopened_task_ids,
        }))?;
        insert_project_control_event(
            &transaction,
            now,
            "queue.resumed",
            "Operator resumed the project queue",
            &data_json,
        )?;
        transaction.commit().context("Cannot commit queue resume")?;
        Ok(ResumeProjectOutcome::Resumed {
            reconciled_manual_changes: !snapshots_match,
            reopened_task_ids,
        })
    }

    pub fn project_control_events(&self) -> Result<Vec<ProjectControlEvent>> {
        let mut statement = self.connection.prepare(
            "SELECT id, occurred_at_ms, event_type, reason, data_json
             FROM project_control_events ORDER BY id",
        )?;
        let events = statement
            .query_map([], |row| {
                Ok(ProjectControlEvent {
                    id: row.get(0)?,
                    occurred_at_ms: row.get(1)?,
                    event_type: row.get(2)?,
                    reason: row.get(3)?,
                    data_json: row.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(events)
    }

    pub fn verification_history(&self, task_id: i64) -> Result<Vec<VerificationEvidenceRecord>> {
        let mut statement = self.connection.prepare(
            "SELECT id, task_id, critic_review_id, status, revision_id, started_at_ms,
                    finished_at_ms, base_revision, head_commit, patch_sha256, changed_paths_json
             FROM verification_runs WHERE task_id = ?1 ORDER BY id",
        )?;
        let runs = statement
            .query_map([task_id], |row| {
                Ok((
                    VerificationRunRecord {
                        id: row.get(0)?,
                        task_id: row.get(1)?,
                        critic_review_id: row.get(2)?,
                        status: row.get(3)?,
                        revision_id: row.get(4)?,
                        started_at_ms: row.get(5)?,
                        finished_at_ms: row.get(6)?,
                    },
                    row.get::<_, String>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, String>(9)?,
                    row.get::<_, String>(10)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut history = Vec::with_capacity(runs.len());
        for (run, base_revision, head_commit, patch_sha256, changed_paths_json) in runs {
            let mut results = self.connection.prepare(
                "SELECT check_id, argv_json, version_argv_json, working_directory, tool_version,
                        version_duration_ms, version_exit_code, duration_ms, exit_code, timed_out,
                        failure_code, stdout_sha256, stderr_sha256
                 FROM verification_results WHERE run_id = ?1 ORDER BY id",
            )?;
            let checks = results
                .query_map([run.id], |row| {
                    let argv_json: String = row.get(1)?;
                    let version_argv_json: String = row.get(2)?;
                    let argv = serde_json::from_str(&argv_json).map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            1,
                            rusqlite::types::Type::Text,
                            Box::new(error),
                        )
                    })?;
                    let version_argv =
                        serde_json::from_str(&version_argv_json).map_err(|error| {
                            rusqlite::Error::FromSqlConversionFailure(
                                2,
                                rusqlite::types::Type::Text,
                                Box::new(error),
                            )
                        })?;
                    Ok(CheckExecution {
                        check_id: row.get(0)?,
                        argv,
                        version_argv,
                        working_directory: row.get(3)?,
                        tool_version: row.get(4)?,
                        version_duration_ms: row.get(5)?,
                        version_exit_code: row.get(6)?,
                        duration_ms: row.get(7)?,
                        exit_code: row.get(8)?,
                        timed_out: row.get::<_, i64>(9)? != 0,
                        failure_code: row.get(10)?,
                        stdout_sha256: row.get(11)?,
                        stderr_sha256: row.get(12)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            history.push(VerificationEvidenceRecord {
                run,
                base_revision,
                head_commit,
                patch_sha256,
                changed_paths_json,
                checks,
            });
        }
        Ok(history)
    }

    pub fn latest_verification_summary(
        &self,
        task_id: i64,
    ) -> Result<Option<LatestVerificationSummary>> {
        let run = self
            .connection
            .query_row(
                "SELECT id, task_id, critic_review_id, status, revision_id, started_at_ms,
                        finished_at_ms
                 FROM verification_runs WHERE task_id = ?1 ORDER BY id DESC LIMIT 1",
                [task_id],
                map_verification_run_record,
            )
            .optional()?;
        let Some(run) = run else {
            return Ok(None);
        };
        let check_count: i64 = self.connection.query_row(
            "SELECT COUNT(*) FROM verification_results WHERE run_id = ?1",
            [run.id],
            |row| row.get(0),
        )?;
        let check_count = usize::try_from(check_count)
            .context("Verification check count is outside the supported range")?;
        Ok(Some(LatestVerificationSummary { run, check_count }))
    }

    pub fn begin_writer_attempt(
        &mut self,
        task_id: i64,
        prompt_sha256: &str,
    ) -> Result<WriterAttemptRecord> {
        validate_writer_prompt_digest(prompt_sha256)?;
        let now = now_ms()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("Cannot start Writer attempt transaction")?;
        ensure_queue_not_paused(&transaction)?;
        let current_status: Option<String> = transaction
            .query_row("SELECT status FROM tasks WHERE id = ?1", [task_id], |row| {
                row.get(0)
            })
            .optional()?;
        let Some(current_status) = current_status else {
            bail!("Task #{task_id} does not exist");
        };
        let worktree_record: Option<(String, Option<i64>)> = transaction
            .query_row(
                "SELECT state, writer_routed_at_ms FROM task_worktrees WHERE task_id = ?1",
                [task_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((state, _)) = worktree_record.as_ref()
            && state != TaskWorktreeState::Active.as_str()
            && state != TaskWorktreeState::Retired.as_str()
        {
            bail!(
                "Task #{task_id} has a managed worktree in state {state}; reconcile or inspect it before running a Writer"
            );
        }
        let integration_state: Option<String> = transaction
            .query_row(
                "SELECT state FROM task_worktree_integrations WHERE task_id = ?1",
                [task_id],
                |row| row.get(0),
            )
            .optional()?;
        if matches!(
            integration_state.as_deref(),
            Some("applying" | "needs_attention")
        ) {
            bail!(
                "Task #{task_id} worktree integration needs reconciliation before another Writer can run"
            );
        }
        let prior_attempt_count: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM writer_attempts WHERE task_id = ?1",
            [task_id],
            |row| row.get(0),
        )?;
        if worktree_record
            .as_ref()
            .is_some_and(|(state, _)| state == TaskWorktreeState::Retired.as_str())
            && integration_state.is_some()
        {
            bail!(
                "Task #{task_id} has integration history for its retired worktree and requires reconciliation"
            );
        }
        if worktree_record
            .as_ref()
            .is_some_and(|(state, routed_at_ms)| {
                state == TaskWorktreeState::Active.as_str() && routed_at_ms.is_none()
            })
            && prior_attempt_count == 0
        {
            bail!(
                "Task #{task_id} active worktree must be marked as the Writer workspace before the first attempt"
            );
        }
        let unmet_dependencies: i64 = transaction.query_row(
            "SELECT COUNT(*)
             FROM task_dependencies AS edges
             JOIN tasks AS prerequisite ON prerequisite.id = edges.depends_on_task_id
             WHERE edges.task_id = ?1 AND (
                 prerequisite.status <> 'complete'
                 OR EXISTS(SELECT 1 FROM task_worktree_integrations AS integrations
                           WHERE integrations.task_id = prerequisite.id
                             AND integrations.state <> 'integrated')
             )",
            [task_id],
            |row| row.get(0),
        )?;
        if unmet_dependencies > 0 {
            bail!("Task #{task_id} is waiting for prerequisite tasks to complete");
        }
        let revision_source = if current_status == TaskStatus::Implementing.as_str() {
            let latest_review: Option<(i64, String, String)> = transaction
                .query_row(
                    "SELECT id, base_revision, decision FROM critic_reviews
                     WHERE task_id = ?1 ORDER BY id DESC LIMIT 1",
                    [task_id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()?;
            let Some((review_id, base_revision, decision)) = latest_review else {
                bail!(
                    "Task #{task_id} is implementing without a prior Critic review; a Writer revision is not eligible"
                );
            };
            if decision != "request_changes" {
                bail!("Task #{task_id} has no latest Critic review requesting changes");
            }
            Some((review_id, base_revision))
        } else if current_status == TaskStatus::Queued.as_str() {
            None
        } else {
            bail!(
                "Task #{task_id} must be queued or awaiting Writer fixes after a Critic rejection"
            );
        };
        let unresolved: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM writer_attempts WHERE task_id = ?1
             AND (state IN ('launching', 'running') OR (state = 'unknown' AND operator_resolution IS NULL))",
            [task_id],
            |row| row.get(0),
        )?;
        if unresolved != 0 {
            bail!("Task #{task_id} already has an unresolved Writer attempt");
        }

        let project_unresolved: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM writer_attempts
             WHERE state IN ('launching', 'running')
                OR (state = 'unknown' AND operator_resolution IS NULL)",
            [],
            |row| row.get(0),
        )?;
        if project_unresolved != 0 {
            bail!("An unresolved Writer attempt exists for this project");
        }

        if current_status == TaskStatus::Queued.as_str() {
            let changed = transaction.execute(
                "UPDATE tasks SET status = 'implementing', updated_at_ms = ?1 WHERE id = ?2 AND status = 'queued'",
                params![now, task_id],
            )?;
            if changed != 1 {
                bail!("Task #{task_id} changed while its Writer attempt was being created");
            }
            transaction.execute(
                "INSERT INTO task_events (task_id, occurred_at_ms, actor, event_type, reason, data_json)
                 VALUES (?1, ?2, 'talos', 'task.status_changed', 'Writer attempt claimed the queued task', ?3)",
                params![task_id, now, r#"{"from":"queued","to":"implementing"}"#],
            )?;
        }
        transaction.execute(
            "INSERT INTO writer_attempts
                 (task_id, state, started_at_ms, prompt_sha256, base_revision, source_review_id)
             VALUES (?1, 'launching', ?2, ?3, ?4, ?5)",
            params![
                task_id,
                now,
                prompt_sha256.to_ascii_lowercase(),
                revision_source.as_ref().map(|(_, base)| base.as_str()),
                revision_source.as_ref().map(|(review_id, _)| *review_id),
            ],
        )?;
        let attempt_id = transaction.last_insert_rowid();
        let attempt_data = serde_json::to_string(&serde_json::json!({"attempt_id": attempt_id}))?;
        transaction.execute(
            "INSERT INTO task_events (task_id, occurred_at_ms, actor, event_type, reason, data_json)
             VALUES (?1, ?2, 'talos', 'writer_attempt.started', 'Writer attempt prepared for launch', ?3)",
            params![task_id, now, attempt_data],
        )?;
        if let Some((review_id, base_revision)) = &revision_source {
            let event_data = serde_json::to_string(&serde_json::json!({
                "attempt_id": attempt_id,
                "source_review_id": review_id,
                "base_revision": base_revision,
            }))?;
            transaction.execute(
                "INSERT INTO task_events (task_id, occurred_at_ms, actor, event_type, reason, data_json)
                 VALUES (?1, ?2, 'talos', 'writer_attempt.revision_started', 'Writer is addressing findings from the latest Critic review', ?3)",
                params![task_id, now, event_data],
            )?;
        }
        transaction
            .commit()
            .context("Cannot commit Writer attempt creation")?;

        Ok(WriterAttemptRecord {
            id: attempt_id,
            task_id,
            state: "launching".to_owned(),
            process_id: None,
            started_at_ms: now,
            finished_at_ms: None,
            prompt_sha256: prompt_sha256.to_ascii_lowercase(),
            conversation_id: None,
            exit_code: None,
            agent_status: None,
            error_code: None,
            operator_resolution: None,
            resolved_at_ms: None,
            resolution_reason: None,
            source_attempt_id: None,
            base_revision: revision_source.as_ref().map(|(_, base)| base.clone()),
            source_review_id: revision_source.map(|(review_id, _)| review_id),
            process_identity_fingerprint: None,
        })
    }

    pub fn record_writer_base_revision(
        &mut self,
        attempt_id: i64,
        base_revision: &str,
    ) -> Result<()> {
        validate_git_revision(base_revision)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("Cannot start Writer base revision transaction")?;
        let task_id: Option<i64> = transaction
            .query_row(
                "SELECT task_id FROM writer_attempts
                 WHERE id = ?1 AND state = 'launching' AND base_revision IS NULL",
                [attempt_id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(task_id) = task_id else {
            bail!(
                "Writer attempt #{attempt_id} changed before its base revision could be recorded"
            );
        };
        let changed = transaction.execute(
            "UPDATE writer_attempts SET base_revision = ?1
             WHERE id = ?2 AND state = 'launching' AND base_revision IS NULL",
            params![base_revision, attempt_id],
        )?;
        if changed != 1 {
            bail!(
                "Writer attempt #{attempt_id} changed before its base revision could be recorded"
            );
        }
        let now = now_ms()?;
        let data = serde_json::to_string(&serde_json::json!({
            "attempt_id": attempt_id,
            "base_revision": base_revision,
        }))?;
        transaction.execute(
            "INSERT INTO task_events (task_id, occurred_at_ms, actor, event_type, reason, data_json)
             VALUES (?1, ?2, 'talos', 'writer_attempt.base_captured', 'Writer review base revision was recorded', ?3)",
            params![task_id, now, data],
        )?;
        transaction
            .commit()
            .context("Cannot commit Writer base revision")?;
        Ok(())
    }

    pub fn mark_writer_attempt_running(&mut self, attempt_id: i64, process_id: u32) -> Result<()> {
        self.mark_writer_attempt_running_with_identity(attempt_id, process_id, None)
    }

    pub fn mark_writer_attempt_running_with_identity(
        &mut self,
        attempt_id: i64,
        process_id: u32,
        process_identity_fingerprint: Option<&str>,
    ) -> Result<()> {
        if process_id == 0 {
            bail!("Writer process ID must be greater than zero");
        }
        if let Some(fingerprint) = process_identity_fingerprint {
            validate_record_text("Process identity fingerprint", fingerprint, 256)?;
            if !fingerprint.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b':' || byte == b'-'
            }) {
                bail!("Process identity fingerprint contains unsupported characters");
            }
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("Cannot start Writer process update transaction")?;
        let task_id: Option<i64> = transaction
            .query_row(
                "SELECT task_id FROM writer_attempts WHERE id = ?1 AND state = 'launching'",
                [attempt_id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(task_id) = task_id else {
            bail!("Writer attempt #{attempt_id} is missing or is not awaiting launch");
        };
        let changed = transaction.execute(
            "UPDATE writer_attempts
             SET state = 'running', process_id = ?1, process_identity_fingerprint = ?2
             WHERE id = ?3 AND state = 'launching'",
            params![
                i64::from(process_id),
                process_identity_fingerprint,
                attempt_id
            ],
        )?;
        if changed != 1 {
            bail!("Writer attempt #{attempt_id} changed during process launch");
        }
        let now = now_ms()?;
        let event_data = serde_json::to_string(&serde_json::json!({
            "attempt_id": attempt_id,
            "process_id": process_id,
            "process_identity_recorded": process_identity_fingerprint.is_some(),
        }))?;
        transaction.execute(
            "INSERT INTO task_events (task_id, occurred_at_ms, actor, event_type, reason, data_json)
             VALUES (?1, ?2, 'talos', 'writer_attempt.process_started', 'Antigravity process started', ?3)",
            params![task_id, now, event_data],
        )?;
        transaction
            .commit()
            .context("Cannot commit Writer process update")?;
        Ok(())
    }

    pub fn record_writer_conversation_id(
        &mut self,
        attempt_id: i64,
        conversation_id: &str,
    ) -> Result<()> {
        validate_public_conversation_id(conversation_id)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("Cannot start Writer conversation ID transaction")?;
        let current: Option<(i64, String, Option<String>)> = transaction
            .query_row(
                "SELECT task_id, state, conversation_id FROM writer_attempts WHERE id = ?1",
                [attempt_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let Some((task_id, state, recorded_id)) = current else {
            bail!("Writer attempt #{attempt_id} does not exist");
        };
        if state != "launching" && state != "running" {
            bail!("Writer attempt #{attempt_id} is not active");
        }
        if let Some(recorded_id) = recorded_id {
            if recorded_id != conversation_id {
                bail!("Writer attempt #{attempt_id} already has a different conversation ID");
            }
            transaction.commit()?;
            return Ok(());
        }
        let changed = transaction.execute(
            "UPDATE writer_attempts SET conversation_id = ?1 WHERE id = ?2 AND state IN ('launching', 'running') AND conversation_id IS NULL",
            params![conversation_id, attempt_id],
        )?;
        if changed != 1 {
            bail!("Writer attempt #{attempt_id} changed while recording its conversation ID");
        }
        let occurred_at_ms = now_ms()?;
        let event_data = serde_json::to_string(&serde_json::json!({
            "attempt_id": attempt_id,
            "conversation_id": conversation_id,
        }))?;
        transaction.execute(
            "INSERT INTO task_events (task_id, occurred_at_ms, actor, event_type, reason, data_json)
             VALUES (?1, ?2, 'talos', 'writer_attempt.session_identified', 'Public Writer conversation ID was recorded', ?3)",
            params![task_id, occurred_at_ms, event_data],
        )?;
        transaction
            .commit()
            .context("Cannot commit Writer conversation ID")?;
        Ok(())
    }

    pub fn latest_continuable_attempt(&self, task_id: i64) -> Result<Option<WriterAttemptRecord>> {
        let task_status: Option<String> = self
            .connection
            .query_row("SELECT status FROM tasks WHERE id = ?1", [task_id], |row| {
                row.get(0)
            })
            .optional()?;
        let Some(task_status) = task_status else {
            bail!("Task #{task_id} does not exist");
        };
        if task_status != TaskStatus::Blocked.as_str() {
            return Ok(None);
        }
        let attempt = self
            .connection
            .query_row(
                "SELECT id, task_id, state, process_id, started_at_ms, finished_at_ms,
                        prompt_sha256, conversation_id, exit_code, agent_status, error_code,
                        operator_resolution, resolved_at_ms, resolution_reason, source_attempt_id,
                        base_revision, source_review_id, process_identity_fingerprint
                 FROM writer_attempts WHERE task_id = ?1 ORDER BY id DESC LIMIT 1",
                [task_id],
                map_writer_attempt_record,
            )
            .optional()?;
        Ok(attempt.filter(is_continuable_attempt))
    }

    pub fn begin_writer_continuation(
        &mut self,
        task_id: i64,
        prompt_sha256: &str,
        source_attempt_id: i64,
        conversation_id: &str,
        project_inspected_attested: bool,
    ) -> Result<WriterAttemptRecord> {
        if !project_inspected_attested {
            bail!("Writer continuation requires an explicit project-inspection attestation");
        }
        validate_writer_prompt_digest(prompt_sha256)?;
        validate_public_conversation_id(conversation_id)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("Cannot start Writer continuation transaction")?;
        ensure_queue_not_paused(&transaction)?;
        let now = now_ms()?;
        let unresolved: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM writer_attempts
             WHERE state IN ('launching', 'running')
                OR (state = 'unknown' AND operator_resolution IS NULL)",
            [],
            |row| row.get(0),
        )?;
        if unresolved != 0 {
            bail!("An unresolved Writer attempt exists for this project");
        }
        let task_status: Option<String> = transaction
            .query_row("SELECT status FROM tasks WHERE id = ?1", [task_id], |row| {
                row.get(0)
            })
            .optional()?;
        let Some(task_status) = task_status else {
            bail!("Task #{task_id} does not exist");
        };
        if task_status != TaskStatus::Blocked.as_str() {
            bail!("Task #{task_id} must be blocked after a failed Writer attempt to continue");
        }
        let latest: Option<WriterAttemptRecord> = transaction
            .query_row(
                "SELECT id, task_id, state, process_id, started_at_ms, finished_at_ms,
                        prompt_sha256, conversation_id, exit_code, agent_status, error_code,
                        operator_resolution, resolved_at_ms, resolution_reason, source_attempt_id,
                        base_revision, source_review_id, process_identity_fingerprint
                 FROM writer_attempts WHERE task_id = ?1 ORDER BY id DESC LIMIT 1",
                [task_id],
                map_writer_attempt_record,
            )
            .optional()?;
        let Some(source) = latest.filter(is_continuable_attempt) else {
            bail!("Task #{task_id} has no eligible latest Writer attempt to continue");
        };
        if source.id != source_attempt_id {
            bail!("Writer continuation source is not the latest eligible attempt");
        }
        if source.conversation_id.as_deref() != Some(conversation_id) {
            bail!("Writer continuation conversation ID does not match the source attempt");
        }

        let changed = transaction.execute(
            "UPDATE tasks SET status = 'implementing', updated_at_ms = ?1
             WHERE id = ?2 AND status = 'blocked'",
            params![now, task_id],
        )?;
        if changed != 1 {
            bail!("Task #{task_id} changed while starting Writer continuation");
        }
        let task_transition = serde_json::to_string(&serde_json::json!({
            "from": TaskStatus::Blocked.as_str(),
            "to": TaskStatus::Implementing.as_str(),
            "source_attempt_id": source_attempt_id,
        }))?;
        transaction.execute(
            "INSERT INTO task_events (task_id, occurred_at_ms, actor, event_type, reason, data_json)
             VALUES (?1, ?2, 'talos', 'task.status_changed', 'Starting an explicit continuation of the latest eligible Writer attempt', ?3)",
            params![task_id, now, task_transition],
        )?;
        transaction.execute(
            "INSERT INTO writer_attempts
                 (task_id, state, started_at_ms, prompt_sha256, conversation_id, source_attempt_id,
                  base_revision, source_review_id)
             VALUES (?1, 'launching', ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                task_id,
                now,
                prompt_sha256.to_ascii_lowercase(),
                conversation_id,
                source_attempt_id,
                source.base_revision,
                source.source_review_id,
            ],
        )?;
        let attempt_id = transaction.last_insert_rowid();
        let attempt_data = serde_json::to_string(&serde_json::json!({
            "attempt_id": attempt_id,
            "source_attempt_id": source_attempt_id,
            "conversation_id": conversation_id,
            "project_inspected_attested": project_inspected_attested,
        }))?;
        transaction.execute(
            "INSERT INTO task_events (task_id, occurred_at_ms, actor, event_type, reason, data_json)
             VALUES (?1, ?2, 'talos', 'writer_attempt.continuation_started', 'Starting one new Writer attempt using the saved public conversation ID', ?3)",
            params![task_id, now, attempt_data],
        )?;
        transaction
            .commit()
            .context("Cannot commit Writer continuation")?;

        Ok(WriterAttemptRecord {
            id: attempt_id,
            task_id,
            state: "launching".to_owned(),
            process_id: None,
            started_at_ms: now,
            finished_at_ms: None,
            prompt_sha256: prompt_sha256.to_ascii_lowercase(),
            conversation_id: Some(conversation_id.to_owned()),
            exit_code: None,
            agent_status: None,
            error_code: None,
            operator_resolution: None,
            resolved_at_ms: None,
            resolution_reason: None,
            source_attempt_id: Some(source_attempt_id),
            base_revision: source.base_revision,
            source_review_id: source.source_review_id,
            process_identity_fingerprint: None,
        })
    }

    pub fn finish_writer_attempt(
        &mut self,
        attempt_id: i64,
        outcome: WriterAttemptOutcome,
        conversation_id: Option<&str>,
        exit_code: Option<i32>,
        agent_status: Option<&str>,
        error_code: Option<&str>,
    ) -> Result<WriterAttemptRecord> {
        validate_optional_text("conversation ID", conversation_id, 200)?;
        validate_optional_text("agent status", agent_status, 32)?;
        validate_optional_text("attempt error code", error_code, 80)?;
        if let Some(code) = error_code
            && !code
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
        {
            bail!("Writer attempt error code contains unsupported characters");
        }

        let now = now_ms()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("Cannot start Writer attempt completion transaction")?;
        let current: Option<WriterFinalizationState> = transaction
            .query_row(
                "SELECT task_id, state, prompt_sha256, process_id, started_at_ms, conversation_id,
                        source_attempt_id, base_revision, source_review_id,
                        process_identity_fingerprint
                 FROM writer_attempts WHERE id = ?1",
                [attempt_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                        row.get(8)?,
                        row.get(9)?,
                    ))
                },
            )
            .optional()?;
        let Some((
            task_id,
            state,
            prompt_sha256,
            process_id,
            started_at_ms,
            recorded_conversation_id,
            source_attempt_id,
            base_revision,
            source_review_id,
            process_identity_fingerprint,
        )) = current
        else {
            bail!("Writer attempt #{attempt_id} does not exist");
        };
        let process_id = process_id
            .map(|pid| u32::try_from(pid).context("Recorded Writer process ID is out of range"))
            .transpose()?;
        if state != "launching" && state != "running" {
            bail!("Writer attempt #{attempt_id} is already resolved");
        }
        if let (Some(recorded), Some(returned)) =
            (recorded_conversation_id.as_deref(), conversation_id)
            && recorded != returned
        {
            bail!("Writer attempt #{attempt_id} returned a conflicting conversation ID");
        }
        let conversation_id = recorded_conversation_id.as_deref().or(conversation_id);

        let changed = transaction.execute(
            "UPDATE writer_attempts
             SET state = ?1, finished_at_ms = ?2, conversation_id = ?3, exit_code = ?4,
                 agent_status = ?5, error_code = ?6
             WHERE id = ?7 AND state IN ('launching', 'running')",
            params![
                outcome.as_str(),
                now,
                conversation_id,
                exit_code,
                agent_status,
                error_code,
                attempt_id
            ],
        )?;
        if changed != 1 {
            bail!("Writer attempt #{attempt_id} changed while being finalized");
        }

        let task_status: String =
            transaction.query_row("SELECT status FROM tasks WHERE id = ?1", [task_id], |row| {
                row.get(0)
            })?;
        if task_status != TaskStatus::Implementing.as_str() {
            bail!("Task #{task_id} is not implementing while its Writer attempt is finalized");
        }
        let target = if outcome == WriterAttemptOutcome::Succeeded {
            TaskStatus::Reviewing
        } else {
            TaskStatus::Blocked
        };
        let reason = if outcome == WriterAttemptOutcome::Succeeded {
            "Writer returned; independent review is pending"
        } else {
            "Writer attempt ended without a reviewable success; reconciliation is required"
        };
        let changed = transaction.execute(
            "UPDATE tasks SET status = ?1, updated_at_ms = ?2 WHERE id = ?3 AND status = 'implementing'",
            params![target.as_str(), now, task_id],
        )?;
        if changed != 1 {
            bail!("Task #{task_id} changed while its Writer attempt was finalized");
        }

        let event_data = serde_json::to_string(&serde_json::json!({
            "attempt_id": attempt_id,
            "outcome": outcome.as_str(),
            "conversation_id": conversation_id,
            "exit_code": exit_code,
            "agent_status": agent_status,
            "error_code": error_code,
        }))?;
        transaction.execute(
            "INSERT INTO task_events (task_id, occurred_at_ms, actor, event_type, reason, data_json)
             VALUES (?1, ?2, 'talos', 'writer_attempt.finished', ?3, ?4)",
            params![task_id, now, reason, event_data],
        )?;
        let transition_data = serde_json::to_string(&serde_json::json!({
            "from": TaskStatus::Implementing.as_str(),
            "to": target.as_str(),
            "attempt_id": attempt_id,
        }))?;
        transaction.execute(
            "INSERT INTO task_events (task_id, occurred_at_ms, actor, event_type, reason, data_json)
             VALUES (?1, ?2, 'talos', 'task.status_changed', ?3, ?4)",
            params![task_id, now, reason, transition_data],
        )?;
        let stop_requested: Option<i64> = transaction.query_row(
            "SELECT stop_requested_attempt_id FROM project_control_state WHERE singleton = 1",
            [],
            |row| row.get(0),
        )?;
        if stop_requested == Some(attempt_id) {
            transaction.execute(
                "UPDATE project_control_state SET stop_requested_attempt_id = NULL, updated_at_ms = ?1
                 WHERE singleton = 1 AND stop_requested_attempt_id = ?2",
                params![now, attempt_id],
            )?;
            let stopped_by_request = error_code == Some("writer_stopped_by_operator");
            let data_json = serde_json::to_string(&serde_json::json!({
                "attempt_id": attempt_id,
                "stop_request_honored": stopped_by_request,
                "attempt_outcome": outcome.as_str(),
            }))?;
            insert_project_control_event(
                &transaction,
                now,
                if stopped_by_request {
                    "writer.stop_completed"
                } else {
                    "writer.stop_request_closed"
                },
                if stopped_by_request {
                    "Writer stopped after the operator request"
                } else {
                    "Writer attempt ended before the stop request took effect"
                },
                &data_json,
            )?;
        }
        transaction
            .commit()
            .context("Cannot commit Writer attempt completion")?;

        Ok(WriterAttemptRecord {
            id: attempt_id,
            task_id,
            state: outcome.as_str().to_owned(),
            process_id,
            started_at_ms,
            finished_at_ms: Some(now),
            prompt_sha256,
            conversation_id: conversation_id.map(str::to_owned),
            exit_code,
            agent_status: agent_status.map(str::to_owned),
            error_code: error_code.map(str::to_owned),
            operator_resolution: None,
            resolved_at_ms: None,
            resolution_reason: None,
            source_attempt_id,
            base_revision,
            source_review_id,
            process_identity_fingerprint,
        })
    }

    pub fn has_unresolved_writer_attempts(&self) -> Result<bool> {
        let count: i64 = self.connection.query_row(
            "SELECT COUNT(*) FROM writer_attempts
             WHERE state IN ('launching', 'running')
                OR (state = 'unknown' AND operator_resolution IS NULL)",
            [],
            |row| row.get(0),
        )?;
        Ok(count != 0)
    }

    pub fn resolve_unknown_writer_attempt(
        &mut self,
        lease: &SupervisorLease,
        attempt_id: i64,
        action: WriterResolutionAction,
        writer_stopped_attested: bool,
        project_inspected_attested: bool,
        reason: &str,
    ) -> Result<WriterResolutionRecord> {
        if !writer_stopped_attested || !project_inspected_attested {
            bail!("Writer resolution requires both explicit operator attestations");
        }
        validate_optional_text("resolution reason", Some(reason), 1000)?;
        if contains_sensitive(reason) {
            bail!("Writer resolution reason contains secret-like text");
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("Cannot start Writer resolution transaction")?;
        let now = now_ms()?;
        validate_lease_in_transaction(&transaction, lease, now)?;
        let current: Option<(i64, String, Option<String>, Option<i64>)> = transaction
            .query_row(
                "SELECT task_id, state, operator_resolution, process_id
                 FROM writer_attempts WHERE id = ?1",
                [attempt_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?;
        let Some((task_id, state, existing_resolution, process_id)) = current else {
            bail!("Writer attempt #{attempt_id} does not exist");
        };
        if state != "unknown" || existing_resolution.is_some() {
            bail!("Writer attempt #{attempt_id} is not an unresolved unknown attempt");
        }
        let task_status: String =
            transaction.query_row("SELECT status FROM tasks WHERE id = ?1", [task_id], |row| {
                row.get(0)
            })?;
        if task_status != TaskStatus::Blocked.as_str() {
            bail!("Task #{task_id} is not blocked for operator resolution");
        }
        let target = action.target_task_status();
        let changed = transaction.execute(
            "UPDATE writer_attempts
             SET operator_resolution = ?1, resolved_at_ms = ?2, resolution_reason = ?3
             WHERE id = ?4 AND state = 'unknown' AND operator_resolution IS NULL",
            params![action.as_str(), now, reason.trim(), attempt_id],
        )?;
        if changed != 1 {
            bail!("Writer attempt #{attempt_id} changed during operator resolution");
        }
        let changed = transaction.execute(
            "UPDATE tasks SET status = ?1, updated_at_ms = ?2
             WHERE id = ?3 AND status = 'blocked'",
            params![target.as_str(), now, task_id],
        )?;
        if changed != 1 {
            bail!("Task #{task_id} changed during operator resolution");
        }
        let event_data = serde_json::to_string(&serde_json::json!({
            "attempt_id": attempt_id,
            "attempt_state": "unknown",
            "action": action.as_str(),
            "reason": reason.trim(),
            "writer_stopped_attested": writer_stopped_attested,
            "project_inspected_attested": project_inspected_attested,
            "recorded_process_id": process_id,
            "task_status": target.as_str(),
            "resolved_at_ms": now,
            "lease_fencing_token": lease.fencing_token,
        }))?;
        transaction.execute(
            "INSERT INTO task_events (task_id, occurred_at_ms, actor, event_type, reason, data_json)
             VALUES (?1, ?2, 'operator', 'writer_attempt.operator_resolved', 'Operator recorded an explicit resolution for an unknown Writer outcome', ?3)",
            params![task_id, now, event_data],
        )?;
        let transition_data = serde_json::to_string(&serde_json::json!({
            "from": TaskStatus::Blocked.as_str(),
            "to": target.as_str(),
            "attempt_id": attempt_id,
            "operator_resolution": action.as_str(),
        }))?;
        transaction.execute(
            "INSERT INTO task_events (task_id, occurred_at_ms, actor, event_type, reason, data_json)
             VALUES (?1, ?2, 'operator', 'task.status_changed', ?3, ?4)",
            params![task_id, now, reason.trim(), transition_data],
        )?;
        transaction
            .commit()
            .context("Cannot commit Writer operator resolution")?;

        Ok(WriterResolutionRecord {
            attempt_id,
            task_id,
            attempt_state: "unknown",
            action,
            task_status: target,
            resolved_at_ms: now,
            reason: reason.trim().to_owned(),
        })
    }

    /// Quarantine persisted in-flight attempts without inferring process or task outcome.
    /// The supplied lease is validated inside the recovery transaction.
    pub fn quarantine_unfinished_writer_attempts(
        &mut self,
        lease: &SupervisorLease,
    ) -> Result<usize> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("Cannot start Writer recovery transaction")?;
        let now = now_ms()?;
        let current_lease: Option<(String, i64, i64)> = transaction
            .query_row(
                "SELECT owner_id, fencing_token, expires_at_ms
                 FROM supervisor_lease WHERE singleton = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let lease_is_valid = matches!(
            current_lease,
            Some((ref owner_id, fencing_token, expires_at_ms))
                if owner_id == &lease.owner_id
                    && fencing_token == lease.fencing_token
                    && expires_at_ms > now
        );
        if !lease_is_valid {
            bail!("Writer recovery requires the current unexpired supervisor lease");
        }
        let unfinished = {
            let mut statement = transaction.prepare(
                "SELECT id, task_id, state, process_id, process_identity_fingerprint FROM writer_attempts
                 WHERE state IN ('launching', 'running') ORDER BY id",
            )?;
            statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<i64>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };

        for (attempt_id, task_id, previous_state, process_id, process_identity_fingerprint) in
            &unfinished
        {
            let task_status: String = transaction.query_row(
                "SELECT status FROM tasks WHERE id = ?1",
                [task_id],
                |row| row.get(0),
            )?;
            if task_status != TaskStatus::Implementing.as_str()
                && task_status != TaskStatus::Blocked.as_str()
            {
                bail!(
                    "Cannot quarantine Writer attempt #{attempt_id} because task #{task_id} is in unexpected state {task_status}"
                );
            }

            let changed = transaction.execute(
                "UPDATE writer_attempts
                 SET state = 'unknown', error_code = 'writer_outcome_unknown'
                 WHERE id = ?1 AND state IN ('launching', 'running')",
                [attempt_id],
            )?;
            if changed != 1 {
                bail!("Writer attempt #{attempt_id} changed during recovery");
            }

            if task_status == TaskStatus::Implementing.as_str() {
                let changed = transaction.execute(
                    "UPDATE tasks SET status = 'blocked', updated_at_ms = ?1
                     WHERE id = ?2 AND status = 'implementing'",
                    params![now, task_id],
                )?;
                if changed != 1 {
                    bail!("Task #{task_id} changed during Writer recovery");
                }
                let transition_data = serde_json::to_string(&serde_json::json!({
                    "from": TaskStatus::Implementing.as_str(),
                    "to": TaskStatus::Blocked.as_str(),
                    "attempt_id": attempt_id,
                }))?;
                transaction.execute(
                    "INSERT INTO task_events (task_id, occurred_at_ms, actor, event_type, reason, data_json)
                     VALUES (?1, ?2, 'talos', 'task.status_changed', 'Writer outcome is unknown after TALOS recovery; operator inspection is required', ?3)",
                    params![task_id, now, transition_data],
                )?;
            }

            let event_data = serde_json::to_string(&serde_json::json!({
                "attempt_id": attempt_id,
                "from": previous_state,
                "to": "unknown",
                "recorded_process_id": process_id,
                "recorded_process_identity_fingerprint": process_identity_fingerprint,
                "lease_fencing_token": lease.fencing_token,
            }))?;
            transaction.execute(
                "INSERT INTO task_events (task_id, occurred_at_ms, actor, event_type, reason, data_json)
                 VALUES (?1, ?2, 'talos', 'writer_attempt.quarantined', 'Writer outcome is unknown; process and project state require operator inspection', ?3)",
                params![task_id, now, event_data],
            )?;
            let stop_requested: Option<i64> = transaction.query_row(
                "SELECT stop_requested_attempt_id FROM project_control_state WHERE singleton = 1",
                [],
                |row| row.get(0),
            )?;
            if stop_requested == Some(*attempt_id) {
                transaction.execute(
                    "UPDATE project_control_state SET stop_requested_attempt_id = NULL, updated_at_ms = ?1
                     WHERE singleton = 1 AND stop_requested_attempt_id = ?2",
                    params![now, attempt_id],
                )?;
                let stop_data = serde_json::to_string(&serde_json::json!({
                    "attempt_id": attempt_id,
                    "attempt_state": "unknown",
                }))?;
                insert_project_control_event(
                    &transaction,
                    now,
                    "writer.stop_request_interrupted_by_recovery",
                    "TALOS exited before confirming the Writer stop request",
                    &stop_data,
                )?;
            }
        }

        transaction
            .commit()
            .context("Cannot commit Writer recovery transaction")?;
        Ok(unfinished.len())
    }

    pub fn writer_attempts(&self, task_id: Option<i64>) -> Result<Vec<WriterAttemptRecord>> {
        let mut statement = self.connection.prepare(
            "SELECT id, task_id, state, process_id, started_at_ms, finished_at_ms,
                    prompt_sha256, conversation_id, exit_code, agent_status, error_code,
                    operator_resolution, resolved_at_ms, resolution_reason, source_attempt_id,
                    base_revision, source_review_id, process_identity_fingerprint
             FROM writer_attempts WHERE (?1 IS NULL OR task_id = ?1) ORDER BY id",
        )?;
        let attempts = statement
            .query_map([task_id], map_writer_attempt_record)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(attempts)
    }

    pub fn latest_writer_attempt(&self, task_id: i64) -> Result<Option<WriterAttemptRecord>> {
        self.connection
            .query_row(
                "SELECT id, task_id, state, process_id, started_at_ms, finished_at_ms,
                        prompt_sha256, conversation_id, exit_code, agent_status, error_code,
                        operator_resolution, resolved_at_ms, resolution_reason, source_attempt_id,
                        base_revision, source_review_id, process_identity_fingerprint
                 FROM writer_attempts WHERE task_id = ?1 ORDER BY id DESC LIMIT 1",
                [task_id],
                map_writer_attempt_record,
            )
            .optional()
            .context("Cannot read latest Writer attempt")
    }

    pub fn latest_critic_review(&self, task_id: i64) -> Result<Option<CriticReviewRecord>> {
        let review = self
            .connection
            .query_row(
                "SELECT id, task_id, writer_attempt_id, base_revision, head_commit, revision_id,
                        patch_sha256, changed_paths_json, decision, summary, result_json, adapter,
                        adapter_version, created_at_ms
                 FROM critic_reviews WHERE task_id = ?1 ORDER BY id DESC LIMIT 1",
                [task_id],
                map_critic_review_record,
            )
            .optional()?;
        Ok(review)
    }

    pub fn record_critic_review(
        &mut self,
        input: CriticReviewInput<'_>,
    ) -> Result<CriticReviewRecord> {
        let CriticReviewInput {
            task_id,
            writer_attempt_id,
            base_revision,
            head_commit,
            revision_id,
            patch_sha256,
            changed_paths_json,
            decision,
            summary,
            result_json,
            adapter,
            adapter_version,
        } = input;
        validate_git_revision(base_revision)?;
        validate_git_revision(head_commit)?;
        validate_review_revision_id(revision_id)?;
        validate_sha256_id(patch_sha256, "Critic patch digest")?;
        validate_changed_paths_json(changed_paths_json)?;
        if decision != "approve" && decision != "request_changes" {
            bail!("Critic decision must be approve or request_changes");
        }
        validate_record_text("Critic summary", summary, 16_000)?;
        validate_record_text("Critic result", result_json, 256_000)?;
        validate_record_text("Critic adapter", adapter, 80)?;
        validate_record_text("Critic adapter version", adapter_version, 80)?;
        let result_value: serde_json::Value =
            serde_json::from_str(result_json).context("Critic result must be valid JSON")?;
        if result_value
            .get("decision")
            .and_then(serde_json::Value::as_str)
            != Some(decision)
            || result_value
                .get("revision_id")
                .and_then(serde_json::Value::as_str)
                != Some(revision_id)
        {
            bail!("Critic result decision and revision must match the validated review metadata");
        }
        if contains_sensitive(summary) || contains_sensitive(result_json) {
            bail!("Critic result contains secret-like text and was not stored");
        }

        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("Cannot start Critic review transaction")?;
        let now = now_ms()?;
        let task_status: Option<String> = transaction
            .query_row("SELECT status FROM tasks WHERE id = ?1", [task_id], |row| {
                row.get(0)
            })
            .optional()?;
        let Some(task_status) = task_status else {
            bail!("Task #{task_id} does not exist");
        };
        if task_status != TaskStatus::Reviewing.as_str() {
            bail!("Task #{task_id} is not eligible for Critic review");
        }

        let unresolved: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM writer_attempts
             WHERE state IN ('launching', 'running')
                OR (state = 'unknown' AND operator_resolution IS NULL)",
            [],
            |row| row.get(0),
        )?;
        if unresolved != 0 {
            bail!("A Writer attempt is active or unresolved; the Critic cannot approve this task");
        }

        let writer: Option<(String, Option<String>)> = transaction
            .query_row(
                "SELECT state, base_revision FROM writer_attempts
                 WHERE id = ?1 AND task_id = ?2",
                params![writer_attempt_id, task_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((writer_state, writer_base)) = writer else {
            bail!("Writer attempt #{writer_attempt_id} does not belong to task #{task_id}");
        };
        if writer_state != "succeeded" || writer_base.as_deref() != Some(base_revision) {
            bail!(
                "Critic review must target a successful Writer attempt with the same base revision"
            );
        }
        let latest_attempt_id: Option<i64> = transaction
            .query_row(
                "SELECT id FROM writer_attempts WHERE task_id = ?1 ORDER BY id DESC LIMIT 1",
                [task_id],
                |row| row.get(0),
            )
            .optional()?;
        if latest_attempt_id != Some(writer_attempt_id) {
            bail!("Critic review must target the latest Writer attempt");
        }

        transaction.execute(
            "INSERT INTO critic_reviews
                 (task_id, writer_attempt_id, base_revision, revision_id, decision, summary,
                  result_json, adapter, adapter_version, created_at_ms, head_commit, patch_sha256,
                  changed_paths_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                task_id,
                writer_attempt_id,
                base_revision,
                revision_id,
                decision,
                summary,
                result_json,
                adapter,
                adapter_version,
                now,
                head_commit,
                patch_sha256,
                changed_paths_json,
            ],
        )?;
        let review_id = transaction.last_insert_rowid();
        let target_status = if decision == "approve" {
            TaskStatus::Verifying
        } else {
            TaskStatus::Implementing
        };
        let reason = if decision == "approve" {
            "Independent Critic approved this revision; project verification is still required"
        } else {
            "Independent Critic requested changes to this revision"
        };
        if task_status != target_status.as_str() {
            let changed = transaction.execute(
                "UPDATE tasks SET status = ?1, updated_at_ms = ?2 WHERE id = ?3 AND status = ?4",
                params![target_status.as_str(), now, task_id, task_status],
            )?;
            if changed != 1 {
                bail!("Task #{task_id} changed while the Critic review was being recorded");
            }
            let transition_data = serde_json::to_string(&serde_json::json!({
                "from": task_status,
                "to": target_status.as_str(),
                "review_id": review_id,
                "writer_attempt_id": writer_attempt_id,
                "revision_id": revision_id,
            }))?;
            transaction.execute(
                "INSERT INTO task_events (task_id, occurred_at_ms, actor, event_type, reason, data_json)
                 VALUES (?1, ?2, 'talos', 'task.status_changed', ?3, ?4)",
                params![task_id, now, reason, transition_data],
            )?;
        }
        let review_data = serde_json::to_string(&serde_json::json!({
            "review_id": review_id,
            "writer_attempt_id": writer_attempt_id,
            "base_revision": base_revision,
            "revision_id": revision_id,
            "head_commit": head_commit,
            "patch_sha256": patch_sha256,
            "decision": decision,
            "adapter": adapter,
            "adapter_version": adapter_version,
        }))?;
        transaction.execute(
            "INSERT INTO task_events (task_id, occurred_at_ms, actor, event_type, reason, data_json)
             VALUES (?1, ?2, 'talos', 'critic_review.recorded', ?3, ?4)",
            params![task_id, now, reason, review_data],
        )?;
        transaction
            .commit()
            .context("Cannot commit Critic review")?;

        Ok(CriticReviewRecord {
            id: review_id,
            task_id,
            writer_attempt_id,
            base_revision: base_revision.to_owned(),
            head_commit: Some(head_commit.to_owned()),
            revision_id: revision_id.to_owned(),
            patch_sha256: Some(patch_sha256.to_owned()),
            changed_paths_json: Some(changed_paths_json.to_owned()),
            decision: decision.to_owned(),
            summary: summary.to_owned(),
            result_json: result_json.to_owned(),
            adapter: adapter.to_owned(),
            adapter_version: adapter_version.to_owned(),
            created_at_ms: now,
        })
    }

    pub fn begin_verification_run(
        &mut self,
        task_id: i64,
        critic_review_id: i64,
        facts: &RevisionFacts,
        lease: &SupervisorLease,
    ) -> Result<VerificationRunRecord> {
        validate_git_revision(&facts.base_commit)?;
        validate_git_revision(&facts.head_commit)?;
        validate_review_revision_id(&facts.revision_id)?;
        validate_sha256_id(&facts.patch_sha256, "Verification patch digest")?;
        let changed_paths_json = serde_json::to_string(&facts.changed_paths)?;
        validate_changed_paths_json(&changed_paths_json)?;

        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("Cannot start verification run transaction")?;
        validate_lease_in_transaction(&transaction, lease, now_ms()?)?;
        let task_status: Option<String> = transaction
            .query_row("SELECT status FROM tasks WHERE id = ?1", [task_id], |row| {
                row.get(0)
            })
            .optional()?;
        if task_status.as_deref() != Some(TaskStatus::Verifying.as_str()) {
            bail!("Task #{task_id} must be verifying after an approved Critic review");
        }
        let review: Option<CriticReviewSnapshot> = transaction
            .query_row(
                "SELECT decision, base_revision, head_commit, revision_id, patch_sha256,
                        changed_paths_json
                     FROM critic_reviews WHERE id = ?1 AND task_id = ?2",
                params![critic_review_id, task_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .optional()?;
        let Some((decision, base_revision, head_commit, revision_id, patch_sha256, reviewed_paths)) =
            review
        else {
            bail!("Critic review #{critic_review_id} does not belong to task #{task_id}");
        };
        if decision != "approve"
            || base_revision != facts.base_commit
            || head_commit.as_deref() != Some(facts.head_commit.as_str())
            || revision_id.as_deref() != Some(facts.revision_id.as_str())
            || patch_sha256.as_deref() != Some(facts.patch_sha256.as_str())
            || reviewed_paths.as_deref() != Some(changed_paths_json.as_str())
        {
            bail!("Git facts do not match the latest approved Critic revision");
        }
        let latest_review_id: Option<i64> = transaction
            .query_row(
                "SELECT id FROM critic_reviews WHERE task_id = ?1 ORDER BY id DESC LIMIT 1",
                [task_id],
                |row| row.get(0),
            )
            .optional()?;
        if latest_review_id != Some(critic_review_id) {
            bail!("Verification requires the latest Critic review for task #{task_id}");
        }
        let required_checks: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM task_verification_checks WHERE task_id = ?1",
            [task_id],
            |row| row.get(0),
        )?;
        if required_checks == 0 {
            bail!("Task #{task_id} has no saved project checks and cannot be completed");
        }
        let started_at_ms = now_ms()?;
        transaction.execute(
            "INSERT INTO verification_runs
                 (task_id, critic_review_id, base_revision, head_commit, revision_id,
                  patch_sha256, changed_paths_json, status, started_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'running', ?8)",
            params![
                task_id,
                critic_review_id,
                facts.base_commit,
                facts.head_commit,
                facts.revision_id,
                facts.patch_sha256,
                changed_paths_json,
                started_at_ms,
            ],
        )?;
        let run_id = transaction.last_insert_rowid();
        let event_data = serde_json::to_string(&serde_json::json!({
            "run_id": run_id,
            "critic_review_id": critic_review_id,
            "base_revision": facts.base_commit,
            "head_commit": facts.head_commit,
            "revision_id": facts.revision_id,
            "patch_sha256": facts.patch_sha256,
            "required_checks": required_checks,
        }))?;
        transaction.execute(
            "INSERT INTO task_events (task_id, occurred_at_ms, actor, event_type, reason, data_json)
             VALUES (?1, ?2, 'talos', 'verification.started', 'Project checks started for the approved Git revision', ?3)",
            params![task_id, started_at_ms, event_data],
        )?;
        transaction
            .commit()
            .context("Cannot commit verification run start")?;
        Ok(VerificationRunRecord {
            id: run_id,
            task_id,
            critic_review_id,
            status: "running".to_owned(),
            revision_id: facts.revision_id.clone(),
            started_at_ms,
            finished_at_ms: None,
        })
    }

    pub fn record_verification_result(
        &mut self,
        run_id: i64,
        execution: &CheckExecution,
        lease: &SupervisorLease,
    ) -> Result<()> {
        if execution.check_id.is_empty() || execution.check_id.len() > 64 {
            bail!("Verification result has an invalid check ID");
        }
        validate_sha256_digest(&execution.stdout_sha256, "Verification stdout digest")?;
        validate_sha256_digest(&execution.stderr_sha256, "Verification stderr digest")?;
        if execution
            .tool_version
            .as_deref()
            .is_some_and(|version| version.is_empty() || version.len() > 200)
        {
            bail!("Verification tool version must be at most 200 bytes");
        }
        if execution
            .tool_version
            .as_deref()
            .is_some_and(contains_sensitive)
        {
            bail!("Verification tool version contains secret-like text and was not stored");
        }
        if execution
            .version_duration_ms
            .into_iter()
            .chain(execution.duration_ms)
            .any(|duration| duration < 0)
        {
            bail!("Verification durations cannot be negative");
        }
        if execution.failure_code.as_deref().is_some_and(|code| {
            !code
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
        }) {
            bail!("Verification failure code is invalid");
        }
        let check = ProjectCheck {
            id: execution.check_id.clone(),
            argv: execution.argv.clone(),
            version_argv: execution.version_argv.clone(),
            working_directory: execution.working_directory.clone(),
        };
        validate_project_checks(std::slice::from_ref(&check))?;

        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("Cannot start verification result transaction")?;
        validate_lease_in_transaction(&transaction, lease, now_ms()?)?;
        let task_id: Option<i64> = transaction
            .query_row(
                "SELECT task_id FROM verification_runs WHERE id = ?1 AND status = 'running'",
                [run_id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(task_id) = task_id else {
            bail!("Verification run #{run_id} is missing or already finished");
        };
        let required: Option<(String, String, String)> = transaction
            .query_row(
                "SELECT argv_json, version_argv_json, working_directory
                 FROM task_verification_checks WHERE task_id = ?1 AND check_id = ?2",
                params![task_id, execution.check_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let Some((required_argv, required_version_argv, required_cwd)) = required else {
            bail!(
                "Check {:?} is not a saved requirement for this task",
                execution.check_id
            );
        };
        if serde_json::from_str::<Vec<String>>(&required_argv)? != execution.argv
            || serde_json::from_str::<Vec<String>>(&required_version_argv)?
                != execution.version_argv
            || required_cwd != execution.working_directory
        {
            bail!("Verification command does not match the saved task requirement");
        }
        let finished_at_ms = now_ms()?;
        transaction.execute(
            "INSERT INTO verification_results
                 (run_id, check_id, argv_json, version_argv_json, working_directory,
                  tool_version, version_duration_ms, version_exit_code, duration_ms, exit_code,
                  timed_out, failure_code,
                  stdout_sha256, stderr_sha256, finished_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            params![
                run_id,
                execution.check_id,
                required_argv,
                required_version_argv,
                execution.working_directory,
                execution.tool_version,
                execution.version_duration_ms,
                execution.version_exit_code,
                execution.duration_ms,
                execution.exit_code,
                execution.timed_out,
                execution.failure_code,
                execution.stdout_sha256,
                execution.stderr_sha256,
                finished_at_ms,
            ],
        )?;
        transaction
            .commit()
            .context("Cannot commit verification result")?;
        Ok(())
    }

    pub fn finish_verification_run(
        &mut self,
        run_id: i64,
        snapshot_matches_review: bool,
        lease: &SupervisorLease,
    ) -> Result<VerificationRunRecord> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("Cannot finish verification run transaction")?;
        validate_lease_in_transaction(&transaction, lease, now_ms()?)?;
        let run: Option<(i64, i64, String, String, i64)> = transaction
            .query_row(
                "SELECT task_id, critic_review_id, revision_id, status, started_at_ms
                 FROM verification_runs WHERE id = ?1",
                [run_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .optional()?;
        let Some((task_id, critic_review_id, revision_id, current_status, started_at_ms)) = run
        else {
            bail!("Verification run #{run_id} does not exist");
        };
        if current_status != "running" {
            bail!("Verification run #{run_id} has already finished");
        }
        let required_count: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM task_verification_checks WHERE task_id = ?1",
            [task_id],
            |row| row.get(0),
        )?;
        let result_summary: (i64, i64) = transaction.query_row(
            "SELECT COUNT(*), SUM(CASE WHEN version_exit_code = 0 AND exit_code = 0
                                      AND version_duration_ms IS NOT NULL AND duration_ms IS NOT NULL
                                      AND timed_out = 0 AND failure_code IS NULL THEN 1 ELSE 0 END)
             FROM verification_results WHERE run_id = ?1",
            [run_id],
            |row| Ok((row.get(0)?, row.get::<_, Option<i64>>(1)?.unwrap_or(0))),
        )?;
        let (result_count, passed_count) = result_summary;
        let latest_review: Option<i64> = transaction
            .query_row(
                "SELECT id FROM critic_reviews WHERE task_id = ?1 ORDER BY id DESC LIMIT 1",
                [task_id],
                |row| row.get(0),
            )
            .optional()?;
        let task_status: Option<String> = transaction
            .query_row("SELECT status FROM tasks WHERE id = ?1", [task_id], |row| {
                row.get(0)
            })
            .optional()?;
        let completion_authority_matches = latest_review == Some(critic_review_id)
            && task_status.as_deref() == Some(TaskStatus::Verifying.as_str());
        let status = if !snapshot_matches_review || !completion_authority_matches {
            "stale"
        } else if result_count != required_count || passed_count != required_count {
            "failed"
        } else {
            "passed"
        };
        let worktree_integration_required: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM task_worktrees
                           WHERE task_id = ?1 AND state <> 'retired'
                             AND writer_routed_at_ms IS NOT NULL)",
            [task_id],
            |row| row.get(0),
        )?;
        let finished_at_ms = now_ms()?;
        let changed = transaction.execute(
            "UPDATE verification_runs SET status = ?1, finished_at_ms = ?2
             WHERE id = ?3 AND status = 'running'",
            params![status, finished_at_ms, run_id],
        )?;
        if changed != 1 {
            bail!("Verification run #{run_id} changed before it could finish");
        }
        if status == "passed" && !worktree_integration_required {
            let updated = transaction.execute(
                "UPDATE tasks SET status = 'complete', updated_at_ms = ?1
                 WHERE id = ?2 AND status = 'verifying'",
                params![finished_at_ms, task_id],
            )?;
            if updated != 1 {
                bail!("Task #{task_id} changed before evidence could complete it");
            }
        }
        let reason = match (status, worktree_integration_required) {
            ("passed", true) => {
                "All saved project checks passed; task worktree still requires explicit integration and finalization"
            }
            ("passed", false) => {
                "All saved project checks passed for the exact approved Git revision"
            }
            ("stale", _) => {
                "Git facts or review authority changed while project checks were running; task remains open"
            }
            _ => "One or more saved project checks failed or did not produce complete evidence",
        };
        let event_data = serde_json::to_string(&serde_json::json!({
            "run_id": run_id,
            "critic_review_id": critic_review_id,
            "revision_id": revision_id,
            "status": status,
            "worktree_integration_required": worktree_integration_required,
            "required_checks": required_count,
            "recorded_checks": result_count,
            "passed_checks": passed_count,
            "snapshot_matches_review": snapshot_matches_review,
        }))?;
        transaction.execute(
            "INSERT INTO task_events (task_id, occurred_at_ms, actor, event_type, reason, data_json)
             VALUES (?1, ?2, 'talos', 'verification.finished', ?3, ?4)",
            params![task_id, finished_at_ms, reason, event_data],
        )?;
        if status == "passed" && !worktree_integration_required {
            let transition = serde_json::to_string(&serde_json::json!({
                "from": "verifying",
                "to": "complete",
                "run_id": run_id,
                "critic_review_id": critic_review_id,
                "revision_id": revision_id,
            }))?;
            transaction.execute(
                "INSERT INTO task_events (task_id, occurred_at_ms, actor, event_type, reason, data_json)
                 VALUES (?1, ?2, 'talos', 'task.status_changed', ?3, ?4)",
                params![task_id, finished_at_ms, reason, transition],
            )?;
        }
        transaction
            .commit()
            .context("Cannot commit verification run result")?;
        Ok(VerificationRunRecord {
            id: run_id,
            task_id,
            critic_review_id,
            status: status.to_owned(),
            revision_id,
            started_at_ms,
            finished_at_ms: Some(finished_at_ms),
        })
    }

    pub fn queue_summary(&self) -> Result<QueueSummary> {
        self.connection
            .query_row(
                "SELECT
                    COUNT(*),
                    SUM(CASE WHEN status = 'queued' THEN 1 ELSE 0 END),
                    SUM(CASE WHEN status IN ('planning', 'implementing', 'reviewing', 'verifying') THEN 1 ELSE 0 END),
                    SUM(CASE WHEN status = 'complete' THEN 1 ELSE 0 END),
                    SUM(CASE WHEN status IN ('blocked', 'failed') THEN 1 ELSE 0 END)
                 FROM tasks",
                [],
                |row| {
                    Ok(QueueSummary {
                        total: row.get(0)?,
                        queued: row.get::<_, Option<i64>>(1)?.unwrap_or(0),
                        active: row.get::<_, Option<i64>>(2)?.unwrap_or(0),
                        complete: row.get::<_, Option<i64>>(3)?.unwrap_or(0),
                        blocked_or_failed: row.get::<_, Option<i64>>(4)?.unwrap_or(0),
                    })
                },
            )
            .context("Cannot summarize task queue")
    }

    pub fn acquire_supervisor_lease(
        &mut self,
        owner_id: &str,
        now: i64,
        duration_ms: i64,
    ) -> Result<Option<SupervisorLease>> {
        if owner_id.trim().is_empty() || duration_ms <= 0 {
            bail!("Lease owner and a positive duration are required");
        }
        let expires_at_ms = now
            .checked_add(duration_ms)
            .context("Lease expiration timestamp overflow")?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("Cannot start supervisor lease transaction")?;
        let current: Option<(String, i64, i64)> = transaction
            .query_row(
                "SELECT owner_id, fencing_token, expires_at_ms FROM supervisor_lease WHERE singleton = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;

        if let Some((current_owner, token, current_expiration)) = current.as_ref() {
            if current_expiration > &now && current_owner != owner_id {
                transaction.commit()?;
                return Ok(None);
            }
            let next_token = if current_owner == owner_id && current_expiration > &now {
                *token
            } else {
                token
                    .checked_add(1)
                    .context("Supervisor fencing token exhausted")?
            };
            transaction.execute(
                "UPDATE supervisor_lease SET owner_id = ?1, fencing_token = ?2, expires_at_ms = ?3 WHERE singleton = 1",
                params![owner_id, next_token, expires_at_ms],
            )?;
            transaction.commit()?;
            return Ok(Some(SupervisorLease {
                owner_id: owner_id.to_owned(),
                fencing_token: next_token,
                expires_at_ms,
            }));
        }

        transaction.execute(
            "INSERT INTO supervisor_lease (singleton, owner_id, fencing_token, expires_at_ms)
             VALUES (1, ?1, 1, ?2)",
            params![owner_id, expires_at_ms],
        )?;
        transaction.commit()?;
        Ok(Some(SupervisorLease {
            owner_id: owner_id.to_owned(),
            fencing_token: 1,
            expires_at_ms,
        }))
    }

    pub fn release_supervisor_lease(&mut self, lease: &SupervisorLease) -> Result<bool> {
        let changed = self.connection.execute(
            "UPDATE supervisor_lease SET expires_at_ms = 0 WHERE singleton = 1 AND owner_id = ?1 AND fencing_token = ?2",
            params![lease.owner_id, lease.fencing_token],
        )?;
        Ok(changed == 1)
    }

    pub fn current_supervisor_lease(&self) -> Result<Option<SupervisorLease>> {
        self.connection
            .query_row(
                "SELECT owner_id, fencing_token, expires_at_ms
                 FROM supervisor_lease WHERE singleton = 1",
                [],
                |row| {
                    Ok(SupervisorLease {
                        owner_id: row.get(0)?,
                        fencing_token: row.get(1)?,
                        expires_at_ms: row.get(2)?,
                    })
                },
            )
            .optional()
            .context("Cannot inspect the current supervisor lease")
    }

    pub(crate) fn expire_supervisor_lease_if_matches(
        &mut self,
        lease: &SupervisorLease,
        now: i64,
    ) -> Result<bool> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("Cannot start stale supervisor lease recovery")?;
        let changed = transaction.execute(
            "UPDATE supervisor_lease SET expires_at_ms = 0
             WHERE singleton = 1 AND owner_id = ?1 AND fencing_token = ?2
               AND expires_at_ms > ?3",
            params![lease.owner_id, lease.fencing_token, now],
        )?;
        transaction
            .commit()
            .context("Cannot commit stale supervisor lease recovery")?;
        Ok(changed == 1)
    }
}

fn migrate(connection: &mut Connection) -> Result<()> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .context("Cannot start database schema transaction")?;
    let version: i64 = transaction
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .context("Cannot read database schema version")?;
    if version > SCHEMA_VERSION {
        bail!(
            "Database schema version {version} is newer than this TALOS version supports ({SCHEMA_VERSION}); database was left unchanged"
        );
    }
    if version == SCHEMA_VERSION {
        transaction.commit()?;
        return Ok(());
    }

    if version == 0 {
        transaction.execute_batch(
            "CREATE TABLE tasks (
             id INTEGER PRIMARY KEY AUTOINCREMENT,
             title TEXT NOT NULL CHECK(length(trim(title)) > 0),
             acceptance_criteria TEXT NOT NULL DEFAULT '',
             status TEXT NOT NULL CHECK(status IN ('queued', 'planning', 'implementing', 'reviewing', 'verifying', 'complete', 'blocked', 'failed')),
             created_at_ms INTEGER NOT NULL,
             updated_at_ms INTEGER NOT NULL
         );

         CREATE TABLE task_events (
             id INTEGER PRIMARY KEY AUTOINCREMENT,
             task_id INTEGER NOT NULL REFERENCES tasks(id) ON DELETE RESTRICT,
             occurred_at_ms INTEGER NOT NULL,
             actor TEXT NOT NULL,
             event_type TEXT NOT NULL,
             reason TEXT NOT NULL,
             data_json TEXT NOT NULL DEFAULT '{}'
         );
         CREATE INDEX task_events_task_id_id ON task_events(task_id, id);
         CREATE TRIGGER task_events_no_update
             BEFORE UPDATE ON task_events BEGIN
                 SELECT RAISE(ABORT, 'task events are append-only');
             END;
         CREATE TRIGGER task_events_no_delete
             BEFORE DELETE ON task_events BEGIN
                 SELECT RAISE(ABORT, 'task events are append-only');
             END;

         CREATE TABLE supervisor_lease (
             singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
             owner_id TEXT NOT NULL,
             fencing_token INTEGER NOT NULL CHECK(fencing_token > 0),
             expires_at_ms INTEGER NOT NULL
         );",
        )
        .context("Cannot create initial database schema")?;
        transaction.execute_batch("PRAGMA user_version = 1;")?;
    }

    let mut migrated_version: i64 = transaction
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .context("Cannot read database schema version during migration")?;
    if migrated_version == 1 {
        transaction
            .execute_batch(
                "CREATE TABLE writer_attempts (
                     id INTEGER PRIMARY KEY AUTOINCREMENT,
                     task_id INTEGER NOT NULL REFERENCES tasks(id) ON DELETE RESTRICT,
                     state TEXT NOT NULL CHECK(state IN ('launching', 'running', 'unknown', 'succeeded', 'failed', 'timed_out', 'interrupted')),
                     process_id INTEGER CHECK(process_id IS NULL OR process_id > 0),
                     started_at_ms INTEGER NOT NULL,
                     finished_at_ms INTEGER,
                     prompt_sha256 TEXT NOT NULL CHECK(length(prompt_sha256) = 64),
                     conversation_id TEXT,
                     exit_code INTEGER,
                     agent_status TEXT,
                     error_code TEXT,
                     CHECK(
                         (state IN ('launching', 'running', 'unknown') AND finished_at_ms IS NULL)
                         OR (state IN ('succeeded', 'failed', 'timed_out', 'interrupted') AND finished_at_ms IS NOT NULL)
                     )
                 );
                 CREATE INDEX writer_attempts_task_id_id ON writer_attempts(task_id, id);
                 CREATE UNIQUE INDEX writer_attempts_one_active_per_task
                     ON writer_attempts(task_id) WHERE state IN ('launching', 'running', 'unknown');
                 PRAGMA user_version = 2;",
            )
            .context("Cannot create Writer attempt schema")?;
        migrated_version = 2;
    }
    if migrated_version == 2 {
        transaction
            .execute_batch(
                "ALTER TABLE writer_attempts
                     ADD COLUMN operator_resolution TEXT
                         CHECK(operator_resolution IS NULL OR operator_resolution IN ('accept_for_review', 'authorize_retry', 'abandon'));
                 ALTER TABLE writer_attempts ADD COLUMN resolved_at_ms INTEGER;
                 ALTER TABLE writer_attempts ADD COLUMN resolution_reason TEXT
                     CHECK(resolution_reason IS NULL OR length(trim(resolution_reason)) > 0);
                 ALTER TABLE writer_attempts ADD COLUMN source_attempt_id INTEGER
                     REFERENCES writer_attempts(id) ON DELETE RESTRICT
                     CHECK(source_attempt_id IS NULL OR source_attempt_id <> id);
                 DROP INDEX writer_attempts_one_active_per_task;
                 CREATE UNIQUE INDEX writer_attempts_one_active_per_task
                     ON writer_attempts(task_id)
                     WHERE state IN ('launching', 'running')
                        OR (state = 'unknown' AND operator_resolution IS NULL);
                 CREATE TRIGGER writer_attempt_resolution_insert_consistency
                     BEFORE INSERT ON writer_attempts
                     WHEN NOT (
                         (NEW.operator_resolution IS NULL AND NEW.resolved_at_ms IS NULL AND NEW.resolution_reason IS NULL)
                         OR (NEW.state = 'unknown' AND NEW.operator_resolution IN ('accept_for_review', 'authorize_retry', 'abandon')
                             AND NEW.resolved_at_ms IS NOT NULL AND length(trim(NEW.resolution_reason)) > 0)
                     )
                     BEGIN SELECT RAISE(ABORT, 'writer attempt resolution fields are inconsistent'); END;
                 CREATE TRIGGER writer_attempt_resolution_update_consistency
                     BEFORE UPDATE OF state, operator_resolution, resolved_at_ms, resolution_reason ON writer_attempts
                     WHEN NOT (
                         (NEW.operator_resolution IS NULL AND NEW.resolved_at_ms IS NULL AND NEW.resolution_reason IS NULL)
                         OR (NEW.state = 'unknown' AND NEW.operator_resolution IN ('accept_for_review', 'authorize_retry', 'abandon')
                             AND NEW.resolved_at_ms IS NOT NULL AND length(trim(NEW.resolution_reason)) > 0)
                     )
                     BEGIN SELECT RAISE(ABORT, 'writer attempt resolution fields are inconsistent'); END;
                 CREATE TRIGGER writer_attempt_resolution_immutable
                     BEFORE UPDATE OF operator_resolution, resolved_at_ms, resolution_reason ON writer_attempts
                     WHEN OLD.operator_resolution IS NOT NULL
                         AND (NEW.operator_resolution IS NOT OLD.operator_resolution
                              OR NEW.resolved_at_ms IS NOT OLD.resolved_at_ms
                              OR NEW.resolution_reason IS NOT OLD.resolution_reason)
                     BEGIN SELECT RAISE(ABORT, 'writer attempt resolution is immutable'); END;
                 PRAGMA user_version = 3;",
            )
            .context("Cannot migrate Writer recovery and continuation schema")?;
        migrated_version = 3;
    }
    if migrated_version == 3 {
        transaction
            .execute_batch(
                "CREATE TABLE critic_reviews (
                     id INTEGER PRIMARY KEY AUTOINCREMENT,
                     task_id INTEGER NOT NULL REFERENCES tasks(id) ON DELETE RESTRICT,
                     writer_attempt_id INTEGER NOT NULL REFERENCES writer_attempts(id) ON DELETE RESTRICT,
                     base_revision TEXT NOT NULL,
                     revision_id TEXT NOT NULL CHECK(length(revision_id) = 71),
                     decision TEXT NOT NULL CHECK(decision IN ('approve', 'request_changes')),
                     summary TEXT NOT NULL,
                     result_json TEXT NOT NULL,
                     adapter TEXT NOT NULL,
                     adapter_version TEXT NOT NULL,
                     created_at_ms INTEGER NOT NULL
                 );
                 CREATE INDEX critic_reviews_task_id_id ON critic_reviews(task_id, id);
                 CREATE TRIGGER critic_reviews_no_update
                     BEFORE UPDATE ON critic_reviews BEGIN
                         SELECT RAISE(ABORT, 'critic reviews are immutable');
                     END;
                 CREATE TRIGGER critic_reviews_no_delete
                     BEFORE DELETE ON critic_reviews BEGIN
                         SELECT RAISE(ABORT, 'critic reviews are immutable');
                     END;
                 ALTER TABLE writer_attempts ADD COLUMN base_revision TEXT;
                 ALTER TABLE writer_attempts ADD COLUMN source_review_id INTEGER
                     REFERENCES critic_reviews(id) ON DELETE RESTRICT;
                 PRAGMA user_version = 4;",
            )
            .context("Cannot create Critic review schema")?;
        migrated_version = 4;
    }
    if migrated_version == 4 {
        transaction
            .execute_batch(
                "ALTER TABLE critic_reviews ADD COLUMN head_commit TEXT;
                 ALTER TABLE critic_reviews ADD COLUMN patch_sha256 TEXT;
                 ALTER TABLE critic_reviews ADD COLUMN changed_paths_json TEXT;

                 INSERT INTO task_events (task_id, occurred_at_ms, actor, event_type, reason, data_json)
                 SELECT tasks.id, CAST(strftime('%s', 'now') AS INTEGER) * 1000, 'talos',
                        'task.status_changed',
                        'Legacy Critic approval needs a fresh review with Git facts before verification',
                        '{\"from\":\"verifying\",\"to\":\"reviewing\",\"cause\":\"missing_git_facts\"}'
                 FROM tasks
                 WHERE tasks.status = 'verifying'
                   AND EXISTS (
                       SELECT 1 FROM critic_reviews
                       WHERE critic_reviews.task_id = tasks.id
                         AND critic_reviews.id = (
                             SELECT MAX(latest.id) FROM critic_reviews AS latest
                             WHERE latest.task_id = tasks.id
                         )
                         AND critic_reviews.decision = 'approve'
                         AND (critic_reviews.head_commit IS NULL
                              OR critic_reviews.patch_sha256 IS NULL
                              OR critic_reviews.changed_paths_json IS NULL)
                   );
                 UPDATE tasks SET status = 'reviewing', updated_at_ms = CAST(strftime('%s', 'now') AS INTEGER) * 1000
                 WHERE status = 'verifying'
                   AND EXISTS (
                       SELECT 1 FROM critic_reviews
                       WHERE critic_reviews.task_id = tasks.id
                         AND critic_reviews.id = (
                             SELECT MAX(latest.id) FROM critic_reviews AS latest
                             WHERE latest.task_id = tasks.id
                         )
                         AND critic_reviews.decision = 'approve'
                         AND (critic_reviews.head_commit IS NULL
                              OR critic_reviews.patch_sha256 IS NULL
                              OR critic_reviews.changed_paths_json IS NULL)
                   );

                 CREATE TABLE task_verification_checks (
                     task_id INTEGER NOT NULL REFERENCES tasks(id) ON DELETE RESTRICT,
                     check_id TEXT NOT NULL CHECK(length(check_id) BETWEEN 1 AND 64),
                     ordinal INTEGER NOT NULL CHECK(ordinal >= 0),
                     argv_json TEXT NOT NULL,
                     version_argv_json TEXT NOT NULL,
                     working_directory TEXT NOT NULL,
                     PRIMARY KEY(task_id, check_id),
                     UNIQUE(task_id, ordinal)
                 );
                 CREATE TRIGGER task_verification_checks_no_update
                     BEFORE UPDATE ON task_verification_checks BEGIN
                         SELECT RAISE(ABORT, 'task verification requirements are immutable');
                     END;
                 CREATE TRIGGER task_verification_checks_no_delete
                     BEFORE DELETE ON task_verification_checks BEGIN
                         SELECT RAISE(ABORT, 'task verification requirements are immutable');
                     END;

                 CREATE TABLE verification_runs (
                     id INTEGER PRIMARY KEY AUTOINCREMENT,
                     task_id INTEGER NOT NULL REFERENCES tasks(id) ON DELETE RESTRICT,
                     critic_review_id INTEGER NOT NULL REFERENCES critic_reviews(id) ON DELETE RESTRICT,
                     base_revision TEXT NOT NULL,
                     head_commit TEXT NOT NULL,
                     revision_id TEXT NOT NULL CHECK(length(revision_id) = 71),
                     patch_sha256 TEXT NOT NULL CHECK(length(patch_sha256) = 71),
                     changed_paths_json TEXT NOT NULL,
                     status TEXT NOT NULL CHECK(status IN ('running', 'passed', 'failed', 'stale')),
                     started_at_ms INTEGER NOT NULL,
                     finished_at_ms INTEGER
                 );
                 CREATE TABLE verification_results (
                     id INTEGER PRIMARY KEY AUTOINCREMENT,
                     run_id INTEGER NOT NULL REFERENCES verification_runs(id) ON DELETE RESTRICT,
                     check_id TEXT NOT NULL,
                     argv_json TEXT NOT NULL,
                     version_argv_json TEXT NOT NULL,
                     working_directory TEXT NOT NULL,
                     tool_version TEXT,
                     version_exit_code INTEGER,
                     exit_code INTEGER,
                     timed_out INTEGER NOT NULL CHECK(timed_out IN (0, 1)),
                     failure_code TEXT,
                     stdout_sha256 TEXT NOT NULL CHECK(length(stdout_sha256) = 64),
                     stderr_sha256 TEXT NOT NULL CHECK(length(stderr_sha256) = 64),
                     finished_at_ms INTEGER NOT NULL,
                     UNIQUE(run_id, check_id)
                 );
                 CREATE INDEX verification_runs_task_id_id ON verification_runs(task_id, id);
                 CREATE INDEX verification_results_run_id_id ON verification_results(run_id, id);
                 CREATE TRIGGER verification_results_no_update
                     BEFORE UPDATE ON verification_results BEGIN
                         SELECT RAISE(ABORT, 'verification results are immutable');
                     END;
                 CREATE TRIGGER verification_results_no_delete
                     BEFORE DELETE ON verification_results BEGIN
                         SELECT RAISE(ABORT, 'verification results are immutable');
                     END;
                 PRAGMA user_version = 5;",
            )
            .context("Cannot create verification evidence schema")?;
        migrated_version = 5;
    }
    if migrated_version == 5 {
        transaction
            .execute_batch(
                "ALTER TABLE verification_results ADD COLUMN version_duration_ms INTEGER
                     CHECK(version_duration_ms IS NULL OR version_duration_ms >= 0);
                 ALTER TABLE verification_results ADD COLUMN duration_ms INTEGER
                     CHECK(duration_ms IS NULL OR duration_ms >= 0);
                 PRAGMA user_version = 6;",
            )
            .context("Cannot add verification duration evidence")?;
        migrated_version = 6;
    }
    if migrated_version == 6 {
        transaction
            .execute_batch(
                "CREATE TABLE project_control_state (
                     singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
                     queue_paused INTEGER NOT NULL CHECK(queue_paused IN (0, 1)),
                     paused_at_ms INTEGER,
                     pause_reason TEXT,
                     pause_revision_id TEXT,
                     updated_at_ms INTEGER NOT NULL,
                     CHECK(
                         (queue_paused = 0 AND paused_at_ms IS NULL AND pause_reason IS NULL AND pause_revision_id IS NULL)
                         OR (queue_paused = 1 AND paused_at_ms IS NOT NULL AND length(trim(pause_reason)) > 0)
                     )
                 );
                 INSERT INTO project_control_state (singleton, queue_paused, updated_at_ms)
                     VALUES (1, 0, 0);
                 CREATE TABLE project_control_events (
                     id INTEGER PRIMARY KEY AUTOINCREMENT,
                     occurred_at_ms INTEGER NOT NULL,
                     event_type TEXT NOT NULL,
                     reason TEXT NOT NULL,
                     data_json TEXT NOT NULL DEFAULT '{}'
                 );
                 CREATE TRIGGER project_control_events_no_update
                     BEFORE UPDATE ON project_control_events BEGIN
                         SELECT RAISE(ABORT, 'project control events are append-only');
                     END;
                 CREATE TRIGGER project_control_events_no_delete
                     BEFORE DELETE ON project_control_events BEGIN
                         SELECT RAISE(ABORT, 'project control events are append-only');
                     END;
                 PRAGMA user_version = 7;",
            )
            .context("Cannot add durable project pause state")?;
        migrated_version = 7;
    }
    if migrated_version == 7 {
        transaction
            .execute_batch(
                "ALTER TABLE project_control_state
                     ADD COLUMN stop_requested_attempt_id INTEGER
                         REFERENCES writer_attempts(id) ON DELETE RESTRICT;
                 PRAGMA user_version = 8;",
            )
            .context("Cannot add durable Writer stop requests")?;
        migrated_version = 8;
    }
    if migrated_version == 8 {
        transaction
            .execute_batch(
                "ALTER TABLE writer_attempts
                     ADD COLUMN process_identity_fingerprint TEXT
                         CHECK(process_identity_fingerprint IS NULL
                               OR length(process_identity_fingerprint) BETWEEN 1 AND 256);
                 PRAGMA user_version = 9;",
            )
            .context("Cannot add durable Writer process identity")?;
        migrated_version = 9;
    }
    if migrated_version == 9 {
        transaction
            .execute_batch(
                "CREATE TABLE task_dependencies (
                     task_id INTEGER NOT NULL REFERENCES tasks(id) ON DELETE RESTRICT,
                     depends_on_task_id INTEGER NOT NULL REFERENCES tasks(id) ON DELETE RESTRICT,
                     created_at_ms INTEGER NOT NULL,
                     PRIMARY KEY(task_id, depends_on_task_id),
                     CHECK(task_id <> depends_on_task_id)
                 );
                 CREATE INDEX task_dependencies_prerequisite_id
                     ON task_dependencies(depends_on_task_id, task_id);
                 CREATE TRIGGER task_dependencies_no_update
                     BEFORE UPDATE ON task_dependencies BEGIN
                         SELECT RAISE(ABORT, 'task dependencies are append-only');
                     END;
                 CREATE TRIGGER task_dependencies_no_delete
                     BEFORE DELETE ON task_dependencies BEGIN
                         SELECT RAISE(ABORT, 'task dependencies are append-only');
                     END;
                 PRAGMA user_version = 10;",
            )
            .context("Cannot add task dependency graph")?;
        migrated_version = 10;
    }
    if migrated_version == 10 {
        transaction
            .execute_batch(
                "CREATE TABLE child_task_contracts (
                     child_task_id INTEGER PRIMARY KEY REFERENCES tasks(id) ON DELETE RESTRICT,
                     parent_task_id INTEGER NOT NULL REFERENCES tasks(id) ON DELETE RESTRICT,
                     allowed_paths_json TEXT NOT NULL,
                     timeout_ms INTEGER NOT NULL CHECK(timeout_ms BETWEEN 1000 AND 1800000),
                     created_at_ms INTEGER NOT NULL,
                     CHECK(child_task_id <> parent_task_id)
                 );
                 CREATE INDEX child_task_contracts_parent_id
                     ON child_task_contracts(parent_task_id, child_task_id);
                 CREATE TRIGGER child_task_contracts_no_update
                     BEFORE UPDATE ON child_task_contracts BEGIN
                         SELECT RAISE(ABORT, 'child task contracts are immutable');
                     END;
                 CREATE TRIGGER child_task_contracts_no_delete
                     BEFORE DELETE ON child_task_contracts BEGIN
                         SELECT RAISE(ABORT, 'child task contracts are immutable');
                     END;
                 PRAGMA user_version = 11;",
            )
            .context("Cannot add bounded child-task contracts")?;
        migrated_version = 11;
    }
    if migrated_version == 11 {
        transaction
            .execute_batch(
                "CREATE TABLE task_worktrees (
                     task_id INTEGER PRIMARY KEY REFERENCES tasks(id) ON DELETE RESTRICT,
                     worktree_path TEXT NOT NULL UNIQUE,
                     branch_name TEXT NOT NULL UNIQUE,
                     base_commit TEXT NOT NULL CHECK(length(base_commit) IN (40, 64)),
                     state TEXT NOT NULL CHECK(state IN ('creating', 'active', 'failed', 'needs_attention', 'retired')),
                     created_at_ms INTEGER NOT NULL,
                     updated_at_ms INTEGER NOT NULL,
                     retired_at_ms INTEGER,
                     CHECK(updated_at_ms >= created_at_ms),
                     CHECK((state = 'retired' AND retired_at_ms IS NOT NULL)
                           OR (state <> 'retired' AND retired_at_ms IS NULL))
                 );
                 CREATE TRIGGER task_worktrees_identity_immutable
                     BEFORE UPDATE OF task_id, worktree_path, branch_name, base_commit, created_at_ms ON task_worktrees BEGIN
                         SELECT RAISE(ABORT, 'task worktree identity is immutable');
                     END;
                 CREATE TRIGGER task_worktrees_state_transition
                     BEFORE UPDATE OF state ON task_worktrees
                     WHEN NOT (
                         (OLD.state = 'creating' AND NEW.state IN ('active', 'failed', 'needs_attention'))
                         OR (OLD.state = 'failed' AND NEW.state = 'creating')
                         OR (OLD.state = 'active' AND NEW.state IN ('needs_attention', 'retired'))
                     ) BEGIN
                         SELECT RAISE(ABORT, 'invalid task worktree state transition');
                     END;
                 CREATE TRIGGER task_worktrees_no_delete
                     BEFORE DELETE ON task_worktrees BEGIN
                         SELECT RAISE(ABORT, 'task worktree history is append-only');
                     END;
                 PRAGMA user_version = 12;",
            )
            .context("Cannot add task worktree lifecycle records")?;
        migrated_version = 12;
    }
    if migrated_version == 12 {
        transaction
            .execute_batch(
                "CREATE TABLE task_worktree_integrations (
                     task_id INTEGER PRIMARY KEY REFERENCES tasks(id) ON DELETE RESTRICT,
                     state TEXT NOT NULL CHECK(state IN ('applying', 'pending', 'needs_attention', 'integrated')),
                     base_commit TEXT NOT NULL CHECK(length(base_commit) IN (40, 64)),
                     source_head_commit TEXT NOT NULL CHECK(length(source_head_commit) IN (40, 64)),
                     revision_id TEXT NOT NULL CHECK(length(revision_id) = 71),
                     patch_sha256 TEXT NOT NULL CHECK(length(patch_sha256) = 71),
                     changed_paths_json TEXT NOT NULL,
                     integration_commit TEXT,
                     started_at_ms INTEGER NOT NULL,
                     updated_at_ms INTEGER NOT NULL,
                     integrated_at_ms INTEGER,
                     CHECK(updated_at_ms >= started_at_ms),
                     CHECK((state = 'integrated' AND integration_commit IS NOT NULL AND integrated_at_ms IS NOT NULL)
                           OR (state <> 'integrated' AND integration_commit IS NULL AND integrated_at_ms IS NULL))
                 );
                 CREATE TRIGGER task_worktree_integrations_identity_immutable
                     BEFORE UPDATE OF task_id, base_commit, source_head_commit, revision_id,
                                      patch_sha256, changed_paths_json, started_at_ms
                     ON task_worktree_integrations BEGIN
                         SELECT RAISE(ABORT, 'task worktree integration identity is immutable');
                     END;
                 CREATE TRIGGER task_worktree_integrations_state_transition
                     BEFORE UPDATE OF state ON task_worktree_integrations
                     WHEN NOT (
                         (OLD.state = 'applying' AND NEW.state IN ('pending', 'needs_attention'))
                         OR (OLD.state = 'pending' AND NEW.state IN ('needs_attention', 'integrated'))
                         OR (OLD.state = 'needs_attention' AND NEW.state = 'pending')
                     ) BEGIN
                         SELECT RAISE(ABORT, 'invalid task worktree integration transition');
                     END;
                 CREATE TRIGGER task_worktree_integrations_no_delete
                     BEFORE DELETE ON task_worktree_integrations BEGIN
                         SELECT RAISE(ABORT, 'task worktree integration history is append-only');
                     END;
                 PRAGMA user_version = 13;",
            )
            .context("Cannot add task worktree integration records")?;
        migrated_version = 13;
    }
    if migrated_version == 13 {
        transaction
            .execute_batch(
                "ALTER TABLE task_worktrees ADD COLUMN writer_routed_at_ms INTEGER;
                 CREATE TRIGGER task_worktrees_route_marker_immutable
                     BEFORE UPDATE OF writer_routed_at_ms ON task_worktrees
                     WHEN OLD.writer_routed_at_ms IS NOT NULL
                          AND NEW.writer_routed_at_ms IS NOT OLD.writer_routed_at_ms BEGIN
                         SELECT RAISE(ABORT, 'task worktree execution route is immutable once recorded');
                     END;
                 PRAGMA user_version = 14;",
            )
            .context("Cannot record task worktree Writer routing")?;
        migrated_version = 14;
    }
    if migrated_version == 14 {
        transaction
            .execute_batch(
                "CREATE TABLE github_publications (
                     id INTEGER PRIMARY KEY AUTOINCREMENT,
                     repository TEXT NOT NULL CHECK(length(repository) BETWEEN 3 AND 140),
                     remote_name TEXT NOT NULL CHECK(length(remote_name) BETWEEN 1 AND 64),
                     base_branch TEXT NOT NULL CHECK(length(base_branch) BETWEEN 1 AND 200),
                     head_branch TEXT NOT NULL CHECK(length(head_branch) BETWEEN 1 AND 200),
                     base_commit TEXT NOT NULL CHECK(length(base_commit) IN (40, 64)),
                     head_commit TEXT NOT NULL CHECK(length(head_commit) IN (40, 64)),
                     diff_sha256 TEXT NOT NULL CHECK(length(diff_sha256) = 71),
                     state TEXT NOT NULL CHECK(state IN (
                         'push_pending', 'push_unknown', 'pushed', 'pr_pending',
                         'pr_unknown', 'open', 'needs_attention'
                     )),
                     pull_request_url TEXT,
                     created_at_ms INTEGER NOT NULL,
                     updated_at_ms INTEGER NOT NULL,
                     UNIQUE(repository, head_branch),
                     CHECK(updated_at_ms >= created_at_ms),
                     CHECK((state = 'open' AND pull_request_url IS NOT NULL)
                           OR (state <> 'open' AND pull_request_url IS NULL))
                 );
                 CREATE TRIGGER github_publications_initial_state
                     BEFORE INSERT ON github_publications
                     WHEN NEW.state <> 'push_pending' OR NEW.pull_request_url IS NOT NULL BEGIN
                         SELECT RAISE(ABORT, 'GitHub publication must begin before push with no PR URL');
                     END;
                 CREATE TRIGGER github_publications_identity_immutable
                     BEFORE UPDATE OF repository, remote_name, base_branch, head_branch,
                                      base_commit, head_commit, diff_sha256, created_at_ms
                     ON github_publications BEGIN
                         SELECT RAISE(ABORT, 'GitHub publication identity is immutable');
                     END;
                 CREATE TRIGGER github_publications_state_transition
                     BEFORE UPDATE OF state ON github_publications
                     WHEN NOT (
                         (OLD.state = 'push_pending' AND NEW.state IN ('pushed', 'push_unknown', 'needs_attention'))
                         OR (OLD.state = 'push_unknown' AND NEW.state IN ('pushed', 'needs_attention'))
                         OR (OLD.state = 'pushed' AND NEW.state IN ('pr_pending', 'needs_attention'))
                         OR (OLD.state = 'pr_pending' AND NEW.state IN ('open', 'pr_unknown', 'needs_attention'))
                         OR (OLD.state = 'pr_unknown' AND NEW.state IN ('open', 'needs_attention'))
                     ) BEGIN
                         SELECT RAISE(ABORT, 'invalid GitHub publication state transition');
                     END;
                 CREATE TRIGGER github_publications_url_immutable
                     BEFORE UPDATE OF pull_request_url ON github_publications
                     WHEN OLD.pull_request_url IS NOT NULL
                          AND NEW.pull_request_url IS NOT OLD.pull_request_url BEGIN
                         SELECT RAISE(ABORT, 'GitHub pull request URL is immutable once recorded');
                     END;
                 CREATE TRIGGER github_publications_no_delete
                     BEFORE DELETE ON github_publications BEGIN
                         SELECT RAISE(ABORT, 'GitHub publication records are append-only');
                     END;

                 CREATE TABLE github_publication_events (
                     id INTEGER PRIMARY KEY AUTOINCREMENT,
                     publication_id INTEGER NOT NULL REFERENCES github_publications(id) ON DELETE RESTRICT,
                     occurred_at_ms INTEGER NOT NULL,
                     from_state TEXT CHECK(from_state IS NULL OR from_state IN (
                         'push_pending', 'push_unknown', 'pushed', 'pr_pending',
                         'pr_unknown', 'open', 'needs_attention'
                     )),
                     to_state TEXT NOT NULL CHECK(to_state IN (
                         'push_pending', 'push_unknown', 'pushed', 'pr_pending',
                         'pr_unknown', 'open', 'needs_attention'
                     )),
                     reason TEXT NOT NULL CHECK(length(reason) BETWEEN 1 AND 1000),
                     CHECK((from_state IS NULL AND to_state = 'push_pending')
                           OR (from_state IS NOT NULL AND from_state <> to_state))
                 );
                 CREATE INDEX github_publication_events_publication_id_id
                     ON github_publication_events(publication_id, id);
                 CREATE UNIQUE INDEX github_publication_events_one_initial
                     ON github_publication_events(publication_id) WHERE from_state IS NULL;
                 CREATE TRIGGER github_publication_events_initial_match
                     BEFORE INSERT ON github_publication_events
                     WHEN NEW.from_state IS NULL AND NOT EXISTS (
                         SELECT 1 FROM github_publications
                         WHERE id = NEW.publication_id AND state = 'push_pending'
                     ) BEGIN
                         SELECT RAISE(ABORT, 'initial GitHub publication event does not match its record');
                     END;
                 CREATE TRIGGER github_publication_events_transition_match
                     BEFORE INSERT ON github_publication_events
                     WHEN NEW.from_state IS NOT NULL AND (
                         NOT EXISTS (
                             SELECT 1 FROM github_publications
                             WHERE id = NEW.publication_id AND state = NEW.to_state
                         )
                         OR NOT EXISTS (
                             SELECT 1 FROM github_publication_events AS previous
                             WHERE previous.publication_id = NEW.publication_id
                               AND previous.id = (
                                   SELECT MAX(id) FROM github_publication_events
                                   WHERE publication_id = NEW.publication_id
                               )
                               AND previous.to_state = NEW.from_state
                         )
                     ) BEGIN
                         SELECT RAISE(ABORT, 'GitHub publication event does not match its current state');
                     END;
                 CREATE TRIGGER github_publication_events_no_update
                     BEFORE UPDATE ON github_publication_events BEGIN
                         SELECT RAISE(ABORT, 'GitHub publication events are append-only');
                     END;
                 CREATE TRIGGER github_publication_events_no_delete
                     BEFORE DELETE ON github_publication_events BEGIN
                         SELECT RAISE(ABORT, 'GitHub publication events are append-only');
                     END;
                 PRAGMA user_version = 15;",
            )
            .context("Cannot add durable GitHub publication records")?;
        migrated_version = 15;
    }
    if migrated_version == 15 {
        transaction
            .execute_batch(
                "CREATE TABLE github_ci_snapshots (
                     id INTEGER PRIMARY KEY AUTOINCREMENT,
                     publication_id INTEGER NOT NULL REFERENCES github_publications(id) ON DELETE RESTRICT,
                     head_commit TEXT NOT NULL CHECK(length(head_commit) IN (40, 64)),
                     result TEXT NOT NULL CHECK(result IN (
                         'pass', 'fail', 'pending', 'timeout', 'unknown', 'no_checks'
                     )),
                     snapshot_sha256 TEXT NOT NULL CHECK(length(snapshot_sha256) = 71),
                     checks_json TEXT NOT NULL CHECK(length(CAST(checks_json AS BLOB)) <= 32768),
                     checked_at_ms INTEGER NOT NULL,
                     UNIQUE(publication_id, head_commit, snapshot_sha256)
                 );
                 CREATE INDEX github_ci_snapshots_publication_id_id
                     ON github_ci_snapshots(publication_id, id);
                 CREATE TRIGGER github_ci_snapshots_head_matches_publication
                     BEFORE INSERT ON github_ci_snapshots
                     WHEN NOT EXISTS (
                         SELECT 1 FROM github_publications
                         WHERE id = NEW.publication_id
                           AND state = 'open'
                           AND head_commit = NEW.head_commit
                     ) BEGIN
                         SELECT RAISE(ABORT, 'GitHub CI evidence must match an open published commit');
                     END;
                 CREATE TRIGGER github_ci_snapshots_no_update
                     BEFORE UPDATE ON github_ci_snapshots BEGIN
                         SELECT RAISE(ABORT, 'GitHub CI snapshots are append-only');
                     END;
                 CREATE TRIGGER github_ci_snapshots_no_delete
                     BEFORE DELETE ON github_ci_snapshots BEGIN
                         SELECT RAISE(ABORT, 'GitHub CI snapshots are append-only');
                     END;
                 PRAGMA user_version = 16;",
            )
            .context("Cannot add immutable GitHub CI evidence")?;
        migrated_version = 16;
    }
    if migrated_version != SCHEMA_VERSION {
        bail!("Cannot migrate database from unsupported schema version {migrated_version}");
    }

    transaction
        .commit()
        .context("Cannot commit database schema")?;
    Ok(())
}

fn validate_lease_in_transaction(
    transaction: &rusqlite::Transaction<'_>,
    lease: &SupervisorLease,
    now_ms: i64,
) -> Result<()> {
    let current_lease: Option<(String, i64, i64)> = transaction
        .query_row(
            "SELECT owner_id, fencing_token, expires_at_ms
             FROM supervisor_lease WHERE singleton = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let lease_is_valid = matches!(
        current_lease,
        Some((ref owner_id, fencing_token, expires_at_ms))
            if owner_id == &lease.owner_id
                && fencing_token == lease.fencing_token
                && expires_at_ms > now_ms
    );
    if !lease_is_valid {
        bail!("Writer operation requires the current unexpired supervisor lease");
    }
    Ok(())
}

fn ensure_queue_not_paused(transaction: &rusqlite::Transaction<'_>) -> Result<()> {
    let queue_paused: i64 = transaction.query_row(
        "SELECT queue_paused FROM project_control_state WHERE singleton = 1",
        [],
        |row| row.get(0),
    )?;
    if queue_paused != 0 {
        bail!("TALOS queue is paused; run `talos resume` after inspecting project changes");
    }
    Ok(())
}

fn insert_project_control_event(
    transaction: &rusqlite::Transaction<'_>,
    occurred_at_ms: i64,
    event_type: &str,
    reason: &str,
    data_json: &str,
) -> Result<()> {
    validate_record_text("Project control event type", event_type, 100)?;
    validate_record_text("Project control event reason", reason, 1_000)?;
    if contains_sensitive(reason) || contains_sensitive(data_json) {
        bail!("Project control event contains secret-like text and was not stored");
    }
    transaction.execute(
        "INSERT INTO project_control_events (occurred_at_ms, event_type, reason, data_json)
         VALUES (?1, ?2, ?3, ?4)",
        params![occurred_at_ms, event_type, reason, data_json],
    )?;
    Ok(())
}

fn validate_writer_prompt_digest(prompt_sha256: &str) -> Result<()> {
    if prompt_sha256.len() != 64 || !prompt_sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("Writer attempt requires a 64-character SHA-256 digest");
    }
    Ok(())
}

fn validate_public_conversation_id(conversation_id: &str) -> Result<()> {
    if conversation_id.is_empty()
        || conversation_id.len() > 200
        || !conversation_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        bail!("Writer conversation ID is invalid or unsafe to persist");
    }
    Ok(())
}

fn is_continuable_attempt(attempt: &WriterAttemptRecord) -> bool {
    attempt.state == "failed"
        && attempt.exit_code == Some(0)
        && attempt.error_code.as_deref() == Some("agy_agent_error")
        && attempt.agent_status.as_deref() == Some("ERROR")
        && attempt
            .conversation_id
            .as_deref()
            .is_some_and(|conversation_id| validate_public_conversation_id(conversation_id).is_ok())
}

fn map_task_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<TaskRecord> {
    let status: String = row.get(3)?;
    Ok(TaskRecord {
        id: row.get(0)?,
        title: row.get(1)?,
        acceptance_criteria: row.get(2)?,
        status: parse_task_status(&status).ok_or_else(|| {
            rusqlite::Error::FromSqlConversionFailure(
                3,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("Unknown task status in database: {status}"),
                )),
            )
        })?,
        created_at_ms: row.get(4)?,
        updated_at_ms: row.get(5)?,
    })
}

fn map_task_dependency_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<TaskDependencyRecord> {
    let status: String = row.get(2)?;
    Ok(TaskDependencyRecord {
        task_id: row.get(0)?,
        prerequisite_task_id: row.get(1)?,
        prerequisite_status: parse_task_status(&status).ok_or_else(|| {
            rusqlite::Error::FromSqlConversionFailure(
                2,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("Unknown task status in database: {status}"),
                )),
            )
        })?,
        prerequisite_integration_state: row.get(3)?,
    })
}

fn map_verification_run_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<VerificationRunRecord> {
    Ok(VerificationRunRecord {
        id: row.get(0)?,
        task_id: row.get(1)?,
        critic_review_id: row.get(2)?,
        status: row.get(3)?,
        revision_id: row.get(4)?,
        started_at_ms: row.get(5)?,
        finished_at_ms: row.get(6)?,
    })
}

fn map_writer_attempt_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<WriterAttemptRecord> {
    let process_id: Option<i64> = row.get(3)?;
    let process_id = process_id
        .map(|pid| {
            u32::try_from(pid).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    3,
                    rusqlite::types::Type::Integer,
                    Box::new(error),
                )
            })
        })
        .transpose()?;
    Ok(WriterAttemptRecord {
        id: row.get(0)?,
        task_id: row.get(1)?,
        state: row.get(2)?,
        process_id,
        started_at_ms: row.get(4)?,
        finished_at_ms: row.get(5)?,
        prompt_sha256: row.get(6)?,
        conversation_id: row.get(7)?,
        exit_code: row.get(8)?,
        agent_status: row.get(9)?,
        error_code: row.get(10)?,
        operator_resolution: row.get(11)?,
        resolved_at_ms: row.get(12)?,
        resolution_reason: row.get(13)?,
        source_attempt_id: row.get(14)?,
        base_revision: row.get(15)?,
        source_review_id: row.get(16)?,
        process_identity_fingerprint: row.get(17)?,
    })
}

fn map_child_task_contract(row: &rusqlite::Row<'_>) -> rusqlite::Result<ChildTaskContract> {
    let allowed_paths_json: String = row.get(2)?;
    let allowed_paths = serde_json::from_str(&allowed_paths_json).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(2, rusqlite::types::Type::Text, Box::new(error))
    })?;
    Ok(ChildTaskContract {
        child_task_id: row.get(0)?,
        parent_task_id: row.get(1)?,
        allowed_paths,
        timeout_ms: row.get(3)?,
        created_at_ms: row.get(4)?,
    })
}

fn map_task_worktree(row: &rusqlite::Row<'_>) -> rusqlite::Result<TaskWorktreeRecord> {
    let state_value: String = row.get(4)?;
    let state = match state_value.as_str() {
        "creating" => TaskWorktreeState::Creating,
        "active" => TaskWorktreeState::Active,
        "failed" => TaskWorktreeState::Failed,
        "needs_attention" => TaskWorktreeState::NeedsAttention,
        "retired" => TaskWorktreeState::Retired,
        _ => {
            return Err(rusqlite::Error::FromSqlConversionFailure(
                4,
                rusqlite::types::Type::Text,
                format!("unknown task worktree state {state_value:?}").into(),
            ));
        }
    };
    Ok(TaskWorktreeRecord {
        task_id: row.get(0)?,
        worktree_path: row.get(1)?,
        branch_name: row.get(2)?,
        base_commit: row.get(3)?,
        state,
        created_at_ms: row.get(5)?,
        updated_at_ms: row.get(6)?,
        retired_at_ms: row.get(7)?,
        writer_routed_at_ms: row.get(8)?,
    })
}

fn map_task_worktree_integration(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<TaskWorktreeIntegrationRecord> {
    let changed_paths_json: String = row.get(6)?;
    let changed_paths = serde_json::from_str(&changed_paths_json).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            6,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("invalid task worktree integration paths: {error}"),
            )),
        )
    })?;
    Ok(TaskWorktreeIntegrationRecord {
        task_id: row.get(0)?,
        state: row.get(1)?,
        base_commit: row.get(2)?,
        source_head_commit: row.get(3)?,
        revision_id: row.get(4)?,
        patch_sha256: row.get(5)?,
        changed_paths,
        integration_commit: row.get(7)?,
        started_at_ms: row.get(8)?,
        updated_at_ms: row.get(9)?,
        integrated_at_ms: row.get(10)?,
    })
}

fn map_github_publication(row: &rusqlite::Row<'_>) -> rusqlite::Result<GithubPublicationRecord> {
    Ok(GithubPublicationRecord {
        id: row.get(0)?,
        repository: row.get(1)?,
        remote_name: row.get(2)?,
        base_branch: row.get(3)?,
        head_branch: row.get(4)?,
        base_commit: row.get(5)?,
        head_commit: row.get(6)?,
        diff_sha256: row.get(7)?,
        state: row.get(8)?,
        pull_request_url: row.get(9)?,
        created_at_ms: row.get(10)?,
        updated_at_ms: row.get(11)?,
    })
}

fn map_github_publication_event(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<GithubPublicationEvent> {
    Ok(GithubPublicationEvent {
        id: row.get(0)?,
        publication_id: row.get(1)?,
        occurred_at_ms: row.get(2)?,
        from_state: row.get(3)?,
        to_state: row.get(4)?,
        reason: row.get(5)?,
    })
}

fn map_github_ci_snapshot(row: &rusqlite::Row<'_>) -> rusqlite::Result<GithubCiSnapshotRecord> {
    let checks_json: String = row.get(5)?;
    let checks = serde_json::from_str(&checks_json).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(5, rusqlite::types::Type::Text, Box::new(error))
    })?;
    Ok(GithubCiSnapshotRecord {
        id: row.get(0)?,
        publication_id: row.get(1)?,
        head_commit: row.get(2)?,
        result: row.get(3)?,
        snapshot_sha256: row.get(4)?,
        checks,
        checked_at_ms: row.get(6)?,
    })
}

fn map_critic_review_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<CriticReviewRecord> {
    Ok(CriticReviewRecord {
        id: row.get(0)?,
        task_id: row.get(1)?,
        writer_attempt_id: row.get(2)?,
        base_revision: row.get(3)?,
        head_commit: row.get(4)?,
        revision_id: row.get(5)?,
        patch_sha256: row.get(6)?,
        changed_paths_json: row.get(7)?,
        decision: row.get(8)?,
        summary: row.get(9)?,
        result_json: row.get(10)?,
        adapter: row.get(11)?,
        adapter_version: row.get(12)?,
        created_at_ms: row.get(13)?,
    })
}

fn validate_git_revision(value: &str) -> Result<()> {
    if !matches!(value.len(), 40 | 64)
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!("Git revision must be a full hexadecimal object ID");
    }
    Ok(())
}

fn validate_github_repository(value: &str) -> Result<()> {
    validate_record_text("GitHub repository", value, 140)?;
    let Some((owner, repository)) = value.split_once('/') else {
        bail!("GitHub repository must use owner/repository form");
    };
    if owner.is_empty()
        || owner.len() > 39
        || !owner
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        || owner.starts_with('-')
        || owner.ends_with('-')
        || repository.is_empty()
        || repository.len() > 100
        || !repository
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        || repository.starts_with('.')
        || repository.ends_with('.')
        || repository.contains("..")
    {
        bail!("GitHub repository has an invalid owner or repository component");
    }
    Ok(())
}

fn validate_git_remote_name(value: &str) -> Result<()> {
    validate_record_text("Git remote name", value, 64)?;
    if value.starts_with('-')
        || value.starts_with('.')
        || value.ends_with('.')
        || value.contains("..")
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        bail!("Git remote name is invalid");
    }
    Ok(())
}

fn validate_git_branch_name(value: &str, label: &str) -> Result<()> {
    validate_record_text(label, value, 200)?;
    if value.starts_with('-')
        || value.starts_with('/')
        || value.ends_with('/')
        || value.ends_with('.')
        || value.contains("..")
        || value.contains("//")
        || value.contains("@{")
        || value
            .split('/')
            .any(|part| part.is_empty() || part.starts_with('.') || part.ends_with(".lock"))
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'/'))
    {
        bail!("Git {label} name is invalid");
    }
    Ok(())
}

fn validate_github_pull_request_url(value: &str, repository: &str) -> Result<()> {
    validate_record_text("GitHub pull request URL", value, 512)?;
    let Some(path) = value.strip_prefix("https://github.com/") else {
        bail!("GitHub pull request URL must use the github.com HTTPS host");
    };
    let Some((url_repository, number)) = path.split_once("/pull/") else {
        bail!("GitHub pull request URL must include a pull request number");
    };
    let parsed_number = number.parse::<u64>().ok();
    if !url_repository.eq_ignore_ascii_case(repository)
        || number.is_empty()
        || !number.bytes().all(|byte| byte.is_ascii_digit())
        || parsed_number.is_none_or(|number| number == 0)
    {
        bail!("GitHub pull request URL does not match the recorded repository and number");
    }
    Ok(())
}

fn validate_sha256_id(value: &str, label: &str) -> Result<()> {
    let Some(digest) = value.strip_prefix("sha256:") else {
        bail!("{label} must use sha256:<64 lowercase hex characters>");
    };
    validate_sha256_digest(digest, label)
}

fn validate_github_ci_checks(checks: &[GithubCiCheckRecord]) -> Result<()> {
    if checks.len() > 128 {
        bail!("GitHub CI snapshots may include at most 128 checks");
    }
    for check in checks {
        validate_record_text("GitHub CI check name", &check.name, 256)?;
        validate_record_text("GitHub CI check state", &check.state, 64)?;
        if !matches!(
            check.bucket.as_str(),
            "pass" | "fail" | "pending" | "skipping" | "cancel"
        ) {
            bail!("GitHub CI check has an unsupported status bucket");
        }
        if contains_sensitive(&check.name) || contains_sensitive(&check.state) {
            bail!("GitHub CI check metadata contains secret-like text");
        }
        if let Some(workflow) = &check.workflow {
            validate_record_text("GitHub CI workflow name", workflow, 256)?;
            if contains_sensitive(workflow) {
                bail!("GitHub CI workflow name contains secret-like text");
            }
        }
        for timestamp in [&check.started_at, &check.completed_at]
            .into_iter()
            .flatten()
        {
            validate_record_text("GitHub CI timestamp", timestamp, 64)?;
        }
    }
    Ok(())
}

fn github_ci_snapshot_sha256(head_commit: &str, result: &str, checks_json: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(head_commit.as_bytes());
    digest.update([0]);
    digest.update(result.as_bytes());
    digest.update([0]);
    digest.update(checks_json.as_bytes());
    let encoded = digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("sha256:{encoded}")
}

fn validate_sha256_digest(value: &str, label: &str) -> Result<()> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!("{label} must be a 64-character lowercase SHA-256 digest");
    }
    Ok(())
}

fn validate_changed_paths_json(value: &str) -> Result<()> {
    use std::path::{Component, Path};

    let paths: Vec<String> =
        serde_json::from_str(value).context("Changed project paths must be valid JSON")?;
    if paths.is_empty() || paths.len() > 4096 {
        bail!("Changed project paths must contain 1 to 4096 entries");
    }
    let mut unique = std::collections::HashSet::new();
    for path_value in &paths {
        let path = Path::new(path_value);
        if path_value.len() > 1024
            || path_value.chars().any(char::is_control)
            || path.is_absolute()
            || path
                .components()
                .any(|part| !matches!(part, Component::Normal(_)))
            || path_value == ".talos"
            || path_value.starts_with(".talos/")
            || path_value == ".git"
            || path_value.starts_with(".git/")
            || contains_sensitive(path_value)
            || !unique.insert(path_value)
        {
            bail!("Changed project path is invalid, duplicated, or outside the review scope");
        }
    }
    Ok(())
}

fn validate_child_task_text(title: &str, acceptance_criteria: &str) -> Result<()> {
    if title.trim().is_empty() || title.len() > 2_000 {
        bail!("Child task title must be non-empty and at most 2000 bytes");
    }
    if acceptance_criteria.len() > 8_000 {
        bail!("Child task acceptance criteria must be at most 8000 bytes");
    }
    if contains_sensitive(title) || contains_sensitive(acceptance_criteria) {
        bail!("Child task text contains secret-like text");
    }
    Ok(())
}

fn validate_child_task_allowed_paths(paths: &[String]) -> Result<Vec<String>> {
    use std::collections::BTreeSet;

    if paths.is_empty() || paths.len() > MAX_CHILD_TASK_ALLOWED_PATHS {
        bail!("A child task requires between 1 and 64 allowed paths");
    }
    let mut normalized = BTreeSet::new();
    let mut total_bytes = 0_usize;
    for path in paths {
        if path.is_empty()
            || path.len() > 512
            || path.trim() != path
            || path.starts_with('/')
            || path.contains('\\')
            || path.chars().any(char::is_control)
            || path.chars().any(|ch| matches!(ch, '*' | '?' | '[' | ']'))
        {
            bail!("Child task paths must be bounded, relative paths without glob syntax");
        }
        let parts = path.split('/').collect::<Vec<_>>();
        if parts
            .iter()
            .any(|part| part.is_empty() || *part == "." || *part == "..")
        {
            bail!("Child task paths cannot contain empty, current, or parent segments");
        }
        total_bytes = total_bytes
            .checked_add(path.len())
            .context("Child task path scope is too large")?;
        if total_bytes > MAX_CHILD_TASK_SCOPE_BYTES {
            bail!("Child task allowed paths exceed the 16 KiB scope budget");
        }
        normalized.insert(path.clone());
    }
    Ok(normalized.into_iter().collect())
}

fn validate_review_revision_id(value: &str) -> Result<()> {
    if let Some(digest) = value.strip_prefix("sha256:")
        && digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Ok(());
    }
    bail!("Critic revision ID must use sha256:<64 lowercase hex characters>");
}

fn validate_record_text(label: &str, value: &str, maximum_bytes: usize) -> Result<()> {
    if value.trim().is_empty() || value.len() > maximum_bytes {
        bail!("{label} must be non-empty and at most {maximum_bytes} bytes");
    }
    Ok(())
}

fn validate_optional_text(label: &str, value: Option<&str>, maximum_bytes: usize) -> Result<()> {
    if let Some(value) = value
        && (value.trim().is_empty() || value.len() > maximum_bytes)
    {
        bail!("Writer attempt {label} must be non-empty and at most {maximum_bytes} bytes");
    }
    Ok(())
}

fn parse_task_status(value: &str) -> Option<TaskStatus> {
    match value {
        "queued" => Some(TaskStatus::Queued),
        "planning" => Some(TaskStatus::Planning),
        "implementing" => Some(TaskStatus::Implementing),
        "reviewing" => Some(TaskStatus::Reviewing),
        "verifying" => Some(TaskStatus::Verifying),
        "complete" => Some(TaskStatus::Complete),
        "blocked" => Some(TaskStatus::Blocked),
        "failed" => Some(TaskStatus::Failed),
        _ => None,
    }
}

fn is_allowed_transition(current: &TaskStatus, target: &TaskStatus) -> bool {
    matches!(
        (current, target),
        (TaskStatus::Queued, TaskStatus::Planning)
            | (TaskStatus::Queued, TaskStatus::Implementing)
            | (TaskStatus::Queued, TaskStatus::Blocked)
            | (TaskStatus::Queued, TaskStatus::Failed)
            | (TaskStatus::Planning, TaskStatus::Implementing)
            | (TaskStatus::Planning, TaskStatus::Blocked)
            | (TaskStatus::Planning, TaskStatus::Failed)
            | (TaskStatus::Implementing, TaskStatus::Reviewing)
            | (TaskStatus::Implementing, TaskStatus::Blocked)
            | (TaskStatus::Implementing, TaskStatus::Failed)
            | (TaskStatus::Reviewing, TaskStatus::Implementing)
            | (TaskStatus::Reviewing, TaskStatus::Verifying)
            | (TaskStatus::Reviewing, TaskStatus::Blocked)
            | (TaskStatus::Reviewing, TaskStatus::Failed)
            | (TaskStatus::Verifying, TaskStatus::Implementing)
            | (TaskStatus::Verifying, TaskStatus::Reviewing)
            | (TaskStatus::Verifying, TaskStatus::Blocked)
            | (TaskStatus::Verifying, TaskStatus::Failed)
            | (TaskStatus::Blocked, TaskStatus::Queued)
            | (TaskStatus::Blocked, TaskStatus::Failed)
            | (TaskStatus::Failed, TaskStatus::Queued)
    )
}

fn now_ms() -> Result<i64> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("System clock is earlier than the Unix epoch")?;
    i64::try_from(duration.as_millis()).context("System clock timestamp is out of range")
}

fn prepare_private_database_file(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                bail!("Database path {} must be a regular file", path.display());
            }
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => create_private_file(path),
        Err(error) => {
            Err(error).with_context(|| format!("Cannot inspect database path {}", path.display()))
        }
    }
}

#[cfg(unix)]
fn secure_private_database_file(path: &Path) -> Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("Cannot inspect database file {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("Database path {} must be a regular file", path.display());
    }
    let effective_user = unsafe { libc::geteuid() };
    if metadata.uid() != effective_user {
        bail!(
            "Database file {} is not owned by the current user",
            path.display()
        );
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("Cannot restrict database file {}", path.display()))?;
    Ok(())
}

#[cfg(not(unix))]
fn secure_private_database_file(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn create_private_file(path: &Path) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;

    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
    {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            prepare_private_database_file(path)
        }
        Err(error) => {
            Err(error).with_context(|| format!("Cannot create database file {}", path.display()))
        }
    }
}

#[cfg(not(unix))]
fn create_private_file(path: &Path) -> Result<()> {
    match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            prepare_private_database_file(path)
        }
        Err(error) => {
            Err(error).with_context(|| format!("Cannot create database file {}", path.display()))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
        sync::{Arc, Barrier},
        thread,
    };

    use rusqlite::OptionalExtension;

    use super::{
        CheckExecution, CriticReviewInput, Database, ResumeProjectOutcome, StopRequestOutcome,
        TaskStatus, WriterAttemptOutcome, WriterResolutionAction,
    };
    use crate::project::{DiscoveryMethod, Project};
    use crate::{checks::ProjectCheck, revision::RevisionFacts};

    static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

    fn temp_project() -> Project {
        let sequence = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("talos-db-test-{}-{sequence}", std::process::id()));
        fs::create_dir_all(&root).expect("create temporary project");
        Project {
            state_root: root.clone(),
            root,
            discovery: DiscoveryMethod::CurrentDirectory,
        }
    }

    fn cleanup(project: &Project) {
        fs::remove_dir_all(&project.root).expect("remove temporary project");
    }

    fn recovery_lease(database: &mut Database, owner_id: &str) -> super::SupervisorLease {
        let now = super::now_ms().expect("read current time");
        database
            .acquire_supervisor_lease(owner_id, now, 60_000)
            .expect("acquire recovery lease")
            .expect("recovery lease is available")
    }

    fn prepare_approved_verification_task(
        database: &mut Database,
        lease_owner: &str,
    ) -> (i64, i64, RevisionFacts, super::SupervisorLease) {
        let check = ProjectCheck {
            id: "unit".to_owned(),
            argv: vec!["cargo".to_owned(), "test".to_owned()],
            version_argv: vec!["cargo".to_owned(), "--version".to_owned()],
            working_directory: ".".to_owned(),
        };
        let task = database
            .add_task_with_checks("Verify the change", "The unit check passes", &[check])
            .expect("create task with saved verification requirement");
        let base = "a".repeat(40);
        let writer = database
            .begin_writer_attempt(task.id, &"f".repeat(64))
            .expect("start Writer attempt");
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
        let revision_id = format!("sha256:{}", "b".repeat(64));
        let patch_sha256 = format!("sha256:{}", "c".repeat(64));
        let result_json = format!(r#"{{"decision":"approve","revision_id":"{revision_id}"}}"#);
        let review = database
            .record_critic_review(CriticReviewInput {
                task_id: task.id,
                writer_attempt_id: writer.id,
                base_revision: &base,
                head_commit: &base,
                revision_id: &revision_id,
                patch_sha256: &patch_sha256,
                changed_paths_json: r#"["src/lib.rs"]"#,
                decision: "approve",
                summary: "The change meets its criteria.",
                result_json: &result_json,
                adapter: "agy",
                adapter_version: "1.5.10",
            })
            .expect("record approved Critic review");
        let facts = RevisionFacts {
            base_commit: base.clone(),
            head_commit: base,
            revision_id,
            patch_sha256,
            changed_paths: vec!["src/lib.rs".to_owned()],
        };
        let lease = recovery_lease(database, lease_owner);
        (task.id, review.id, facts, lease)
    }

    fn passing_check_result() -> CheckExecution {
        CheckExecution {
            check_id: "unit".to_owned(),
            argv: vec!["cargo".to_owned(), "test".to_owned()],
            version_argv: vec!["cargo".to_owned(), "--version".to_owned()],
            working_directory: ".".to_owned(),
            tool_version: Some("cargo 1.90.0".to_owned()),
            version_duration_ms: Some(8),
            version_exit_code: Some(0),
            duration_ms: Some(23),
            exit_code: Some(0),
            timed_out: false,
            failure_code: None,
            stdout_sha256: "d".repeat(64),
            stderr_sha256: "e".repeat(64),
        }
    }

    #[test]
    fn initialization_is_idempotent_and_uses_wal() {
        let project = temp_project();
        let first = Database::open(&project).expect("initialize database");
        assert_eq!(first.schema_version().expect("read schema version"), 16);
        assert_eq!(
            first
                .journal_mode()
                .expect("read journal mode")
                .to_lowercase(),
            "wal"
        );
        drop(first);

        let second = Database::open(&project).expect("open database again");
        assert_eq!(second.schema_version().expect("read schema version"), 16);
        drop(second);
        cleanup(&project);
    }

    #[test]
    fn schema_v5_migrates_to_v14_with_optional_check_durations_and_control_state() {
        let project = temp_project();
        let database = Database::open(&project).expect("initialize current database");
        database
            .connection
            .execute_batch(
                "DROP TABLE task_worktree_integrations;
                 DROP TABLE task_worktrees;
                 DROP TABLE child_task_contracts;
                 DROP TABLE task_dependencies;
                 ALTER TABLE writer_attempts DROP COLUMN process_identity_fingerprint;
                 ALTER TABLE verification_results DROP COLUMN duration_ms;
                 ALTER TABLE verification_results DROP COLUMN version_duration_ms;
                 DROP TABLE project_control_events;
                 DROP TABLE project_control_state;
                 PRAGMA user_version = 5;",
            )
            .expect("simulate a schema-v5 database");
        drop(database);

        let migrated = Database::open(&project).expect("migrate schema-v5 duration fields");
        assert_eq!(migrated.schema_version().unwrap(), 16);
        let columns: (i64, i64) = migrated
            .connection
            .query_row(
                "SELECT COUNT(*) FILTER (WHERE name = 'version_duration_ms'),
                        COUNT(*) FILTER (WHERE name = 'duration_ms')
                 FROM pragma_table_info('verification_results')",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("read migrated duration columns");
        assert_eq!(columns, (1, 1));
        assert!(!migrated.project_control_state().unwrap().queue_paused);
        cleanup(&project);
    }

    #[test]
    fn schema_v6_migrates_to_v14_without_losing_tasks() {
        let project = temp_project();
        let mut database = Database::open(&project).expect("initialize current database");
        let task = database
            .add_task("Preserve v6 task", "Keep the existing row")
            .expect("add task before migration");
        database
            .connection
            .execute_batch(
                "DROP TABLE task_worktree_integrations;
                 DROP TABLE task_worktrees;
                 DROP TABLE child_task_contracts;
                 DROP TABLE task_dependencies;
                 ALTER TABLE writer_attempts DROP COLUMN process_identity_fingerprint;
                 DROP TABLE project_control_events;
                 DROP TABLE project_control_state;
                 PRAGMA user_version = 6;",
            )
            .expect("simulate schema v6");
        drop(database);

        let migrated = Database::open(&project).expect("migrate schema v6");
        assert_eq!(migrated.schema_version().unwrap(), 16);
        assert_eq!(migrated.list_tasks().unwrap()[0].id, task.id);
        assert!(!migrated.project_control_state().unwrap().queue_paused);
        cleanup(&project);
    }

    #[test]
    fn schema_v7_migrates_with_no_pending_stop_request() {
        let project = temp_project();
        let database = Database::open(&project).expect("initialize current database");
        database
            .connection
            .execute_batch(
                "DROP TABLE task_worktree_integrations;
                 DROP TABLE task_worktrees;
                 DROP TABLE child_task_contracts;
                 DROP TABLE task_dependencies;
                 ALTER TABLE writer_attempts DROP COLUMN process_identity_fingerprint;
                 ALTER TABLE project_control_state DROP COLUMN stop_requested_attempt_id;
                 PRAGMA user_version = 7;",
            )
            .expect("simulate schema v7");
        drop(database);

        let migrated = Database::open(&project).expect("migrate schema v7");
        assert_eq!(migrated.schema_version().unwrap(), 16);
        assert_eq!(
            migrated
                .project_control_state()
                .unwrap()
                .stop_requested_attempt_id,
            None
        );
        cleanup(&project);
    }

    #[test]
    fn schema_v8_migrates_with_no_recorded_process_identity() {
        let project = temp_project();
        let database = Database::open(&project).expect("initialize current database");
        database
            .connection
            .execute_batch(
                "DROP TABLE task_worktree_integrations;
                 DROP TABLE task_worktrees;
                 DROP TABLE child_task_contracts;
                 DROP TABLE task_dependencies;
                 ALTER TABLE writer_attempts DROP COLUMN process_identity_fingerprint;
                 PRAGMA user_version = 8;",
            )
            .expect("simulate schema v8");
        drop(database);

        let migrated = Database::open(&project).expect("migrate schema v8");
        assert_eq!(migrated.schema_version().unwrap(), 16);
        assert!(
            migrated
                .writer_attempts(None)
                .unwrap()
                .iter()
                .all(|attempt| attempt.process_identity_fingerprint.is_none())
        );
        cleanup(&project);
    }

    #[test]
    fn schema_v9_migrates_to_v14_with_dependency_tables() {
        let project = temp_project();
        let database = Database::open(&project).expect("initialize current database");
        database
            .connection
            .execute_batch(
                "DROP TABLE task_worktree_integrations;
                 DROP TABLE task_worktrees;
                 DROP TABLE child_task_contracts;
                 DROP TABLE task_dependencies;
                 PRAGMA user_version = 9;",
            )
            .expect("simulate schema v9");
        drop(database);

        let migrated = Database::open(&project).expect("migrate schema v9");
        assert_eq!(migrated.schema_version().unwrap(), 16);
        let dependencies_table_exists: bool = migrated
            .connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'task_dependencies')",
                [],
                |row| row.get(0),
            )
            .expect("inspect migrated dependency table");
        assert!(dependencies_table_exists);
        cleanup(&project);
    }

    #[test]
    fn schema_v10_migrates_to_v14_with_child_task_contracts() {
        let project = temp_project();
        let database = Database::open(&project).expect("initialize current database");
        database
            .connection
            .execute_batch(
                "DROP TABLE task_worktree_integrations;
                 DROP TABLE task_worktrees;
                 DROP TABLE child_task_contracts;
                 PRAGMA user_version = 10;",
            )
            .expect("simulate schema v10");
        drop(database);

        let migrated = Database::open(&project).expect("migrate schema v10");
        assert_eq!(migrated.schema_version().unwrap(), 16);
        let contracts_table_exists: bool = migrated
            .connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'child_task_contracts')",
                [],
                |row| row.get(0),
            )
            .expect("inspect migrated child-task table");
        assert!(contracts_table_exists);
        cleanup(&project);
    }

    #[test]
    fn pause_is_persistent_idempotent_and_blocks_new_writer_attempts() {
        let project = temp_project();
        let mut database = Database::open(&project).expect("initialize database");
        let task = database
            .add_task("Preserve while paused", "The queue keeps this task")
            .expect("add task");
        let pause_revision = format!("sha256:{}", "a".repeat(64));
        let first = database
            .pause_project("Operator review", Some(&pause_revision))
            .expect("pause queue");
        let again = database
            .pause_project("A replacement reason", None)
            .expect("repeat pause is idempotent");
        assert_eq!(again, first);
        assert!(
            database
                .begin_writer_attempt(task.id, &"b".repeat(64))
                .is_err()
        );
        let queued = database
            .add_task("Still accepted", "Adding a task does not resume work")
            .expect("add a task while paused");
        assert_eq!(queued.status, TaskStatus::Queued);
        drop(database);

        let mut reopened = Database::open(&project).expect("reopen paused project");
        assert!(reopened.project_control_state().unwrap().queue_paused);
        assert_eq!(
            reopened
                .resume_project(Some(&pause_revision), false)
                .unwrap(),
            ResumeProjectOutcome::Resumed {
                reconciled_manual_changes: false,
                reopened_task_ids: Vec::new(),
            }
        );
        assert_eq!(reopened.project_control_events().unwrap().len(), 2);
        cleanup(&project);
    }

    #[test]
    fn stop_request_is_idempotent_observable_and_closed_by_attempt_finalization() {
        let project = temp_project();
        let mut database = Database::open(&project).expect("initialize database");
        assert_eq!(
            database.request_writer_stop().unwrap(),
            StopRequestOutcome::NoActiveWriter
        );
        let task = database.add_task("Stop a Writer", "").expect("add task");
        let attempt = database
            .begin_writer_attempt(task.id, &"b2".repeat(32))
            .expect("begin Writer attempt");
        assert_eq!(
            database.request_writer_stop().unwrap(),
            StopRequestOutcome::Requested {
                attempt_id: attempt.id,
            }
        );
        assert!(database.writer_stop_requested(attempt.id).unwrap());
        assert_eq!(
            database.request_writer_stop().unwrap(),
            StopRequestOutcome::Requested {
                attempt_id: attempt.id,
            }
        );
        database
            .finish_writer_attempt(
                attempt.id,
                WriterAttemptOutcome::Interrupted,
                None,
                None,
                None,
                Some("writer_stopped_by_operator"),
            )
            .expect("finalize stopped Writer attempt");
        assert!(!database.writer_stop_requested(attempt.id).unwrap());
        let events = database.project_control_events().unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].event_type, "writer.stop_requested");
        assert_eq!(events[1].event_type, "writer.stop_completed");
        cleanup(&project);
    }

    #[test]
    fn changed_project_snapshot_requires_reconciliation_and_reopens_approvals() {
        let project = temp_project();
        let mut database = Database::open(&project).expect("initialize database");
        let (task_id, _, _, _) =
            prepare_approved_verification_task(&mut database, "pause-reconciliation");
        let pause_revision = format!("sha256:{}", "1".repeat(64));
        let changed_revision = format!("sha256:{}", "2".repeat(64));
        database
            .pause_project("Manual takeover", Some(&pause_revision))
            .expect("pause queue");

        assert_eq!(
            database
                .resume_project(Some(&changed_revision), false)
                .unwrap(),
            ResumeProjectOutcome::ReconciliationRequired {
                pause_revision_id: Some(pause_revision),
                current_revision_id: Some(changed_revision.clone()),
            }
        );
        assert!(database.project_control_state().unwrap().queue_paused);
        assert_eq!(
            database.list_tasks().unwrap()[0].status,
            TaskStatus::Verifying
        );

        assert_eq!(
            database
                .resume_project(Some(&changed_revision), true)
                .unwrap(),
            ResumeProjectOutcome::Resumed {
                reconciled_manual_changes: true,
                reopened_task_ids: vec![task_id],
            }
        );
        assert!(!database.project_control_state().unwrap().queue_paused);
        assert_eq!(
            database.list_tasks().unwrap()[0].status,
            TaskStatus::Reviewing
        );
        let events = database.project_control_events().unwrap();
        assert_eq!(events.len(), 4);
        assert!(
            events
                .iter()
                .any(|event| event.event_type == "project.manual_changes_detected")
        );
        assert!(
            events
                .iter()
                .any(|event| event.event_type == "project.manual_changes_reconciled")
        );
        cleanup(&project);
    }

    #[test]
    fn missing_pause_snapshot_requires_explicit_reconciliation() {
        let project = temp_project();
        let mut database = Database::open(&project).expect("initialize database");
        database
            .pause_project("No Git baseline", None)
            .expect("pause without Git facts");
        assert!(matches!(
            database.resume_project(None, false).unwrap(),
            ResumeProjectOutcome::ReconciliationRequired { .. }
        ));
        assert!(database.project_control_state().unwrap().queue_paused);
        assert!(matches!(
            database.resume_project(None, true).unwrap(),
            ResumeProjectOutcome::Resumed {
                reconciled_manual_changes: true,
                ..
            }
        ));
        cleanup(&project);
    }

    #[test]
    fn queue_cannot_resume_while_writer_is_unresolved() {
        let project = temp_project();
        let mut database = Database::open(&project).expect("initialize database");
        let task = database.add_task("Wait for writer", "").expect("add task");
        database
            .begin_writer_attempt(task.id, &"3a".repeat(32))
            .expect("begin Writer attempt");
        let revision = format!("sha256:{}", "3".repeat(64));
        database
            .pause_project("Wait for current Writer", Some(&revision))
            .expect("pause queue");
        assert!(database.resume_project(Some(&revision), false).is_err());
        assert!(database.project_control_state().unwrap().queue_paused);
        cleanup(&project);
    }

    #[test]
    fn project_control_events_are_append_only_and_reject_secrets() {
        let project = temp_project();
        let mut database = Database::open(&project).expect("initialize database");
        let assignment_name = ["api", "key"].join("_");
        let secret_reason = format!("{assignment_name}={}", "a".repeat(16));
        assert!(database.pause_project(&secret_reason, None).is_err());
        database
            .pause_project("Safe operator reason", None)
            .expect("record safe pause");
        assert!(
            database
                .connection
                .execute(
                    "UPDATE project_control_events SET reason = 'rewritten' WHERE id = 1",
                    [],
                )
                .is_err()
        );
        assert!(
            database
                .connection
                .execute("DELETE FROM project_control_events WHERE id = 1", [],)
                .is_err()
        );
        assert_eq!(database.project_control_events().unwrap().len(), 1);
        cleanup(&project);
    }

    #[test]
    fn task_and_event_survive_reopen() {
        let project = temp_project();
        let mut database = Database::open(&project).expect("initialize database");
        let created = database
            .add_task(
                "Persist my queue",
                "The task is still queued after reopening",
            )
            .expect("add task");
        drop(database);

        let database = Database::open(&project).expect("reopen database");
        let tasks = database.list_tasks().expect("list tasks");
        let events = database
            .task_events(Some(created.id))
            .expect("read task events");
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].status, TaskStatus::Queued);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "task.created");
        cleanup(&project);
    }

    #[test]
    fn task_checks_are_saved_as_an_immutable_snapshot_and_secrets_are_rejected() {
        let project = temp_project();
        let mut database = Database::open(&project).expect("initialize database");
        let check = ProjectCheck {
            id: "unit".to_owned(),
            argv: vec!["cargo".to_owned(), "test".to_owned()],
            version_argv: vec!["cargo".to_owned(), "--version".to_owned()],
            working_directory: ".".to_owned(),
        };
        let task = database
            .add_task_with_checks(
                "Run tests",
                "The unit check passes",
                std::slice::from_ref(&check),
            )
            .expect("save task check snapshot");
        assert_eq!(database.task_verification_checks(task.id).unwrap(), [check]);
        let assignment_name = ["api", "key"].join("_");
        let protected_text = format!("{assignment_name}={}", "a".repeat(16));
        let result = database.add_task("Do not persist", &protected_text);
        assert!(result.is_err());
        assert_eq!(database.list_tasks().expect("list tasks").len(), 1);
        let unconfigured = database
            .add_task("Attach checks later", "The saved check passes")
            .expect("create task without check requirements");
        let later_check = ProjectCheck {
            id: "format".to_owned(),
            argv: vec!["cargo".to_owned(), "fmt".to_owned(), "--all".to_owned()],
            version_argv: vec!["cargo".to_owned(), "--version".to_owned()],
            working_directory: ".".to_owned(),
        };
        database
            .add_task_verification_checks(unconfigured.id, std::slice::from_ref(&later_check))
            .expect("attach check before work is reviewed");
        assert_eq!(
            database.task_verification_checks(unconfigured.id).unwrap(),
            [later_check]
        );
        cleanup(&project);
    }

    #[test]
    fn v4_approved_tasks_reopen_for_fresh_git_facts_and_can_attach_checks() {
        let project = temp_project();
        let mut database = Database::open(&project).expect("initialize current schema");
        let task = database
            .add_task("Revalidate legacy approval", "The new check passes")
            .expect("create legacy task without checks");
        let base = "a".repeat(40);
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
        let revision = format!("sha256:{}", "b".repeat(64));
        let result = format!(r#"{{"decision":"approve","revision_id":"{revision}"}}"#);
        database
            .record_critic_review(CriticReviewInput {
                task_id: task.id,
                writer_attempt_id: writer.id,
                base_revision: &base,
                head_commit: &base,
                revision_id: &revision,
                patch_sha256: &format!("sha256:{}", "c".repeat(64)),
                changed_paths_json: r#"["src/lib.rs"]"#,
                decision: "approve",
                summary: "The legacy review was approved.",
                result_json: &result,
                adapter: "agy",
                adapter_version: "1.5.10",
            })
            .expect("record approved review");
        assert_eq!(
            database.list_tasks().unwrap()[0].status,
            TaskStatus::Verifying
        );

        database
            .connection
            .execute_batch(
                "DROP TABLE verification_results;
                 DROP TABLE verification_runs;
                 DROP TABLE task_verification_checks;
                 ALTER TABLE critic_reviews DROP COLUMN head_commit;
                 ALTER TABLE critic_reviews DROP COLUMN patch_sha256;
                 ALTER TABLE critic_reviews DROP COLUMN changed_paths_json;
                 DROP TABLE task_worktree_integrations;
                 DROP TABLE task_worktrees;
                 DROP TABLE child_task_contracts;
                 DROP TABLE task_dependencies;
                 ALTER TABLE writer_attempts DROP COLUMN process_identity_fingerprint;
                 DROP TABLE project_control_events;
                 DROP TABLE project_control_state;
                 PRAGMA user_version = 4;",
            )
            .expect("simulate a schema-v4 database");
        drop(database);

        let mut migrated = Database::open(&project).expect("migrate the v4 approval");
        assert_eq!(migrated.schema_version().unwrap(), 16);
        assert_eq!(
            migrated.list_tasks().unwrap()[0].status,
            TaskStatus::Reviewing
        );
        assert_eq!(
            migrated
                .latest_critic_review(task.id)
                .unwrap()
                .unwrap()
                .head_commit,
            None
        );
        let events = migrated.task_events(Some(task.id)).unwrap();
        assert!(
            events
                .iter()
                .any(|event| event.data_json.contains("missing_git_facts"))
        );
        let check = ProjectCheck {
            id: "unit".to_owned(),
            argv: vec!["cargo".to_owned(), "test".to_owned()],
            version_argv: vec!["cargo".to_owned(), "--version".to_owned()],
            working_directory: ".".to_owned(),
        };
        migrated
            .add_task_verification_checks(task.id, &[check])
            .expect("attach R5 check before new Critic review");
        cleanup(&project);
    }

    #[test]
    fn completion_requires_all_saved_check_evidence_for_the_approved_revision() {
        let project = temp_project();
        let mut database = Database::open(&project).expect("initialize database");
        let (task_id, review_id, facts, lease) =
            prepare_approved_verification_task(&mut database, "verification-test");

        let empty_run = database
            .begin_verification_run(task_id, review_id, &facts, &lease)
            .expect("begin first run");
        let failed = database
            .finish_verification_run(empty_run.id, true, &lease)
            .expect("finish without required evidence");
        assert_eq!(failed.status, "failed");
        assert_eq!(
            database.list_tasks().expect("read task")[0].status,
            TaskStatus::Verifying
        );

        let run = database
            .begin_verification_run(task_id, review_id, &facts, &lease)
            .expect("begin complete run");
        database
            .record_verification_result(run.id, &passing_check_result(), &lease)
            .expect("record matching check evidence");
        let completed = database
            .finish_verification_run(run.id, true, &lease)
            .expect("finish with matching evidence");
        assert_eq!(completed.status, "passed");
        assert_eq!(
            database.list_tasks().expect("read completed task")[0].status,
            TaskStatus::Complete
        );
        let history = database
            .verification_history(task_id)
            .expect("read persisted verification evidence");
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].run.status, "failed");
        assert!(history[0].checks.is_empty());
        assert_eq!(history[1].run.id, completed.id);
        assert_eq!(history[1].run.status, "passed");
        assert_eq!(history[1].checks, [passing_check_result()]);
        assert_eq!(history[1].checks[0].version_duration_ms, Some(8));
        assert_eq!(history[1].checks[0].duration_ms, Some(23));
        assert!(
            database
                .task_events(Some(task_id))
                .expect("read evidence events")
                .iter()
                .any(|event| event.event_type == "verification.finished")
        );
        cleanup(&project);
    }

    #[test]
    fn changed_git_facts_leave_task_open_and_mark_evidence_stale() {
        let project = temp_project();
        let mut database = Database::open(&project).expect("initialize database");
        let (task_id, review_id, facts, lease) =
            prepare_approved_verification_task(&mut database, "stale-verification-test");
        let run = database
            .begin_verification_run(task_id, review_id, &facts, &lease)
            .expect("begin run for approved revision");
        database
            .record_verification_result(run.id, &passing_check_result(), &lease)
            .expect("record passing check");
        let stale = database
            .finish_verification_run(run.id, false, &lease)
            .expect("finish after project changed");
        assert_eq!(stale.status, "stale");
        assert_eq!(
            database.list_tasks().expect("read open task")[0].status,
            TaskStatus::Verifying
        );
        cleanup(&project);
    }

    #[test]
    fn changed_task_authority_finishes_the_run_as_stale() {
        let project = temp_project();
        let mut database = Database::open(&project).expect("initialize database");
        let (task_id, review_id, facts, lease) =
            prepare_approved_verification_task(&mut database, "stale-authority-test");
        let run = database
            .begin_verification_run(task_id, review_id, &facts, &lease)
            .expect("begin verification run");
        database
            .record_verification_result(run.id, &passing_check_result(), &lease)
            .expect("record passing check");
        database
            .connection
            .execute(
                "UPDATE tasks SET status = 'reviewing' WHERE id = ?1",
                [task_id],
            )
            .expect("simulate task authority change");

        let stale = database
            .finish_verification_run(run.id, true, &lease)
            .expect("persist stale result instead of rolling back the run");
        assert_eq!(stale.status, "stale");
        assert_eq!(
            database.verification_history(task_id).unwrap()[0]
                .run
                .status,
            "stale"
        );
        cleanup(&project);
    }

    #[test]
    fn schema_v1_migrates_to_v14_without_losing_tasks() {
        let project = temp_project();
        project
            .ensure_runtime_dir()
            .expect("create runtime directory");
        let path = project.database_path();
        let connection = rusqlite::Connection::open(&path).expect("create v1 database");
        connection
            .execute_batch(
                "CREATE TABLE tasks (
                     id INTEGER PRIMARY KEY AUTOINCREMENT,
                     title TEXT NOT NULL CHECK(length(trim(title)) > 0),
                     acceptance_criteria TEXT NOT NULL DEFAULT '',
                     status TEXT NOT NULL CHECK(status IN ('queued', 'planning', 'implementing', 'reviewing', 'verifying', 'complete', 'blocked', 'failed')),
                     created_at_ms INTEGER NOT NULL,
                     updated_at_ms INTEGER NOT NULL
                 );
                 CREATE TABLE task_events (
                     id INTEGER PRIMARY KEY AUTOINCREMENT,
                     task_id INTEGER NOT NULL REFERENCES tasks(id) ON DELETE RESTRICT,
                     occurred_at_ms INTEGER NOT NULL,
                     actor TEXT NOT NULL,
                     event_type TEXT NOT NULL,
                     reason TEXT NOT NULL,
                     data_json TEXT NOT NULL DEFAULT '{}'
                 );
                 CREATE INDEX task_events_task_id_id ON task_events(task_id, id);
                 CREATE TRIGGER task_events_no_update BEFORE UPDATE ON task_events BEGIN SELECT RAISE(ABORT, 'task events are append-only'); END;
                 CREATE TRIGGER task_events_no_delete BEFORE DELETE ON task_events BEGIN SELECT RAISE(ABORT, 'task events are append-only'); END;
                 CREATE TABLE supervisor_lease (
                     singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
                     owner_id TEXT NOT NULL,
                     fencing_token INTEGER NOT NULL CHECK(fencing_token > 0),
                     expires_at_ms INTEGER NOT NULL
                 );
                 INSERT INTO tasks (title, status, created_at_ms, updated_at_ms) VALUES ('Existing v1 task', 'queued', 1, 1);
                 PRAGMA user_version = 1;",
            )
            .expect("write version 1 schema");
        drop(connection);

        let database = Database::open(&project).expect("migrate existing database");
        assert_eq!(database.schema_version().expect("read schema version"), 16);
        let tasks = database.list_tasks().expect("read preserved tasks");
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].title, "Existing v1 task");
        let has_attempts_table: bool = database
            .connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'writer_attempts')",
                [],
                |row| row.get(0),
            )
            .expect("check migrated table");
        assert!(has_attempts_table);
        cleanup(&project);
    }

    #[test]
    fn critic_rejection_allows_a_linked_fix_and_keeps_both_reviews_immutable() {
        let project = temp_project();
        let mut database = Database::open(&project).expect("initialize database");
        let task = database
            .add_task("Implement the requested behavior", "It handles empty input")
            .expect("add task");
        let base = "12".repeat(20);
        let first = database
            .begin_writer_attempt(task.id, &"ab".repeat(32))
            .expect("start first Writer attempt");
        database
            .record_writer_base_revision(first.id, &base)
            .expect("record immutable task base");
        database
            .finish_writer_attempt(
                first.id,
                WriterAttemptOutcome::Succeeded,
                None,
                Some(0),
                Some("SUCCESS"),
                None,
            )
            .expect("finish initial Writer attempt");

        let rejected = database
            .record_critic_review(CriticReviewInput {
                task_id: task.id,
                writer_attempt_id: first.id,
                base_revision: &base,
                head_commit: &base,
                revision_id: &format!("sha256:{}", "a".repeat(64)),
                patch_sha256: &format!("sha256:{}", "d".repeat(64)),
                changed_paths_json: r#"["src/lib.rs"]"#,
                decision: "request_changes",
                summary: "Empty input is still rejected.",
                result_json: &format!(
                    r#"{{"decision":"request_changes","revision_id":"sha256:{}"}}"#,
                    "a".repeat(64)
                ),
                adapter: "agy",
                adapter_version: "1.5.10",
            })
            .expect("record Critic rejection");
        assert_eq!(
            database.list_tasks().expect("task after rejection")[0].status,
            TaskStatus::Implementing
        );
        let duplicate_review = database.record_critic_review(CriticReviewInput {
            task_id: task.id,
            writer_attempt_id: first.id,
            base_revision: &base,
            head_commit: &base,
            revision_id: &format!("sha256:{}", "c".repeat(64)),
            patch_sha256: &format!("sha256:{}", "d".repeat(64)),
            changed_paths_json: r#"["src/lib.rs"]"#,
            decision: "approve",
            summary: "A duplicate review cannot skip the Writer fix.",
            result_json: &format!(
                r#"{{"decision":"approve","revision_id":"sha256:{}"}}"#,
                "c".repeat(64)
            ),
            adapter: "agy",
            adapter_version: "1.5.10",
        });
        assert!(duplicate_review.is_err());
        assert_eq!(
            database
                .latest_critic_review(task.id)
                .expect("read latest review")
                .expect("rejection remains latest")
                .id,
            rejected.id
        );

        let fix = database
            .begin_writer_attempt(task.id, &"cd".repeat(32))
            .expect("start the requested fix iteration");
        assert_eq!(fix.source_review_id, Some(rejected.id));
        assert_eq!(fix.base_revision.as_deref(), Some(base.as_str()));
        database
            .finish_writer_attempt(
                fix.id,
                WriterAttemptOutcome::Succeeded,
                None,
                Some(0),
                Some("SUCCESS"),
                None,
            )
            .expect("finish fix Writer attempt");
        let approved = database
            .record_critic_review(CriticReviewInput {
                task_id: task.id,
                writer_attempt_id: fix.id,
                base_revision: &base,
                head_commit: &base,
                revision_id: &format!("sha256:{}", "b".repeat(64)),
                patch_sha256: &format!("sha256:{}", "d".repeat(64)),
                changed_paths_json: r#"["src/lib.rs"]"#,
                decision: "approve",
                summary: "The revised change meets the stated criterion.",
                result_json: &format!(
                    r#"{{"decision":"approve","revision_id":"sha256:{}"}}"#,
                    "b".repeat(64)
                ),
                adapter: "agy",
                adapter_version: "1.5.10",
            })
            .expect("record review of fixed revision");
        assert_eq!(
            database.list_tasks().expect("task after approval")[0].status,
            TaskStatus::Verifying
        );
        assert_eq!(
            database
                .latest_critic_review(task.id)
                .expect("load latest review")
                .expect("latest review exists"),
            approved
        );
        let review_count: i64 = database
            .connection
            .query_row(
                "SELECT COUNT(*) FROM critic_reviews WHERE task_id = ?1",
                [task.id],
                |row| row.get(0),
            )
            .expect("count preserved reviews");
        assert_eq!(review_count, 2);
        assert!(
            database
                .connection
                .execute(
                    "UPDATE critic_reviews SET summary = 'rewritten' WHERE id = ?1",
                    [rejected.id],
                )
                .is_err()
        );
        assert!(
            database
                .connection
                .execute("DELETE FROM critic_reviews WHERE id = ?1", [rejected.id],)
                .is_err()
        );
        cleanup(&project);
    }

    #[test]
    fn active_writer_attempt_blocks_critic_approval_for_another_task() {
        let project = temp_project();
        let mut database = Database::open(&project).expect("initialize database");
        let review_task = database
            .add_task(
                "Prepare a reviewable change",
                "The change meets its criteria",
            )
            .expect("add review task");
        let base = "23".repeat(20);
        let writer = database
            .begin_writer_attempt(review_task.id, &"ab".repeat(32))
            .expect("start review task Writer");
        database
            .record_writer_base_revision(writer.id, &base)
            .expect("record review task base");
        database
            .finish_writer_attempt(
                writer.id,
                WriterAttemptOutcome::Succeeded,
                None,
                Some(0),
                Some("SUCCESS"),
                None,
            )
            .expect("finish review task Writer");
        let other_task = database
            .add_task("Keep one Writer active", "")
            .expect("add unrelated task");
        let active = database
            .begin_writer_attempt(other_task.id, &"cd".repeat(32))
            .expect("start unrelated Writer");

        let result = database.record_critic_review(CriticReviewInput {
            task_id: review_task.id,
            writer_attempt_id: writer.id,
            base_revision: &base,
            head_commit: &base,
            revision_id: &format!("sha256:{}", "c".repeat(64)),
            patch_sha256: &format!("sha256:{}", "d".repeat(64)),
            changed_paths_json: r#"["src/lib.rs"]"#,
            decision: "approve",
            summary: "The change meets the criteria.",
            result_json: &format!(
                r#"{{"decision":"approve","revision_id":"sha256:{}"}}"#,
                "c".repeat(64)
            ),
            adapter: "agy",
            adapter_version: "1.5.10",
        });
        assert!(result.is_err());
        assert!(
            database
                .latest_critic_review(review_task.id)
                .expect("read review history")
                .is_none()
        );
        assert_eq!(
            database
                .writer_attempts(Some(other_task.id))
                .expect("read active Writer")
                .last()
                .expect("active attempt")
                .id,
            active.id
        );
        cleanup(&project);
    }

    #[test]
    fn schema_v2_migrates_unknown_attempts_without_releasing_the_guard() {
        let project = temp_project();
        project
            .ensure_runtime_dir()
            .expect("create runtime directory");
        let path = project.database_path();
        let connection = rusqlite::Connection::open(&path).expect("create v2 database");
        connection
            .execute_batch(
                "CREATE TABLE tasks (
                     id INTEGER PRIMARY KEY AUTOINCREMENT,
                     title TEXT NOT NULL CHECK(length(trim(title)) > 0),
                     acceptance_criteria TEXT NOT NULL DEFAULT '',
                     status TEXT NOT NULL CHECK(status IN ('queued', 'planning', 'implementing', 'reviewing', 'verifying', 'complete', 'blocked', 'failed')),
                     created_at_ms INTEGER NOT NULL,
                     updated_at_ms INTEGER NOT NULL
                 );
                 CREATE TABLE task_events (
                     id INTEGER PRIMARY KEY AUTOINCREMENT,
                     task_id INTEGER NOT NULL REFERENCES tasks(id) ON DELETE RESTRICT,
                     occurred_at_ms INTEGER NOT NULL,
                     actor TEXT NOT NULL,
                     event_type TEXT NOT NULL,
                     reason TEXT NOT NULL,
                     data_json TEXT NOT NULL DEFAULT '{}'
                 );
                 CREATE TABLE supervisor_lease (
                     singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
                     owner_id TEXT NOT NULL,
                     fencing_token INTEGER NOT NULL CHECK(fencing_token > 0),
                     expires_at_ms INTEGER NOT NULL
                 );
                 CREATE TABLE writer_attempts (
                     id INTEGER PRIMARY KEY AUTOINCREMENT,
                     task_id INTEGER NOT NULL REFERENCES tasks(id) ON DELETE RESTRICT,
                     state TEXT NOT NULL CHECK(state IN ('launching', 'running', 'unknown', 'succeeded', 'failed', 'timed_out', 'interrupted')),
                     process_id INTEGER CHECK(process_id IS NULL OR process_id > 0),
                     started_at_ms INTEGER NOT NULL,
                     finished_at_ms INTEGER,
                     prompt_sha256 TEXT NOT NULL CHECK(length(prompt_sha256) = 64),
                     conversation_id TEXT,
                     exit_code INTEGER,
                     agent_status TEXT,
                     error_code TEXT,
                     CHECK(
                         (state IN ('launching', 'running', 'unknown') AND finished_at_ms IS NULL)
                         OR (state IN ('succeeded', 'failed', 'timed_out', 'interrupted') AND finished_at_ms IS NOT NULL)
                     )
                 );
                 CREATE INDEX writer_attempts_task_id_id ON writer_attempts(task_id, id);
                 CREATE UNIQUE INDEX writer_attempts_one_active_per_task
                     ON writer_attempts(task_id) WHERE state IN ('launching', 'running', 'unknown');
                 INSERT INTO tasks (title, status, created_at_ms, updated_at_ms)
                     VALUES ('Unresolved v2 task', 'blocked', 10, 10);
                 INSERT INTO writer_attempts (task_id, state, process_id, started_at_ms, prompt_sha256, error_code)
                     VALUES (1, 'unknown', 9876, 10, 'abababababababababababababababababababababababababababababababab', 'writer_outcome_unknown');
                 PRAGMA user_version = 2;",
            )
            .expect("write version 2 schema and unresolved attempt");
        drop(connection);

        let database = Database::open(&project).expect("migrate v2 database");
        assert_eq!(database.schema_version().expect("read schema version"), 16);
        assert!(
            database
                .has_unresolved_writer_attempts()
                .expect("unknown attempt still holds the guard")
        );
        let attempts = database
            .writer_attempts(Some(1))
            .expect("read migrated unknown attempt");
        assert_eq!(attempts.len(), 1);
        assert_eq!(attempts[0].state, "unknown");
        assert_eq!(attempts[0].process_id, Some(9876));
        assert_eq!(attempts[0].operator_resolution, None);
        assert_eq!(attempts[0].resolved_at_ms, None);
        assert_eq!(attempts[0].source_attempt_id, None);
        drop(database);

        let reopened = Database::open(&project).expect("reopen migrated database");
        assert!(
            reopened
                .has_unresolved_writer_attempts()
                .expect("unknown attempt remains unresolved after reopen")
        );
        cleanup(&project);
    }

    #[test]
    fn writer_attempt_persists_session_and_leaves_task_open_for_review() {
        let project = temp_project();
        let mut database = Database::open(&project).expect("initialize database");
        let task = database
            .add_task("Implement one change", "A reviewable file change exists")
            .expect("add task");
        let attempt = database
            .begin_writer_attempt(task.id, &"ab".repeat(32))
            .expect("begin writer attempt");
        assert_eq!(attempt.state, "launching");
        assert!(
            database
                .has_unresolved_writer_attempts()
                .expect("check attempt")
        );
        database
            .mark_writer_attempt_running(attempt.id, 1234)
            .expect("record child PID");
        let finished = database
            .finish_writer_attempt(
                attempt.id,
                WriterAttemptOutcome::Succeeded,
                Some("public-session-id"),
                Some(0),
                Some("SUCCESS"),
                None,
            )
            .expect("finish writer attempt");

        assert_eq!(finished.state, "succeeded");
        assert_eq!(finished.process_id, Some(1234));
        assert_eq!(
            finished.conversation_id.as_deref(),
            Some("public-session-id")
        );
        assert_eq!(finished.prompt_sha256, "ab".repeat(32));
        assert!(
            !database
                .has_unresolved_writer_attempts()
                .expect("check attempt")
        );
        let tasks = database.list_tasks().expect("list task");
        assert_eq!(tasks[0].status, TaskStatus::Reviewing);
        let attempts = database
            .writer_attempts(Some(task.id))
            .expect("list attempt");
        assert_eq!(attempts.len(), 1);
        assert_eq!(attempts[0].agent_status.as_deref(), Some("SUCCESS"));
        let events = database
            .task_events(Some(task.id))
            .expect("list task events");
        assert_eq!(
            events.last().expect("status event").event_type,
            "task.status_changed"
        );
        cleanup(&project);
    }

    #[test]
    fn early_conversation_id_is_immutable_and_survives_finalization() {
        let project = temp_project();
        let mut database = Database::open(&project).expect("initialize database");
        let task = database
            .add_task("Persist early session", "")
            .expect("add task");
        let attempt = database
            .begin_writer_attempt(task.id, &"bc".repeat(32))
            .expect("begin Writer attempt");
        database
            .record_writer_conversation_id(attempt.id, "public-session-id")
            .expect("record init conversation ID");
        let event_count = database
            .task_events(Some(task.id))
            .expect("read initial task events")
            .len();
        database
            .record_writer_conversation_id(attempt.id, "public-session-id")
            .expect("repeat same conversation ID idempotently");
        assert_eq!(
            database
                .task_events(Some(task.id))
                .expect("read repeated task events")
                .len(),
            event_count
        );
        assert!(
            database
                .record_writer_conversation_id(attempt.id, "different-session")
                .is_err()
        );

        let finished = database
            .finish_writer_attempt(
                attempt.id,
                WriterAttemptOutcome::Succeeded,
                None,
                Some(0),
                Some("SUCCESS"),
                None,
            )
            .expect("finish without repeating the session ID");
        assert_eq!(
            finished.conversation_id.as_deref(),
            Some("public-session-id")
        );
        cleanup(&project);
    }

    #[test]
    fn latest_agent_error_can_create_one_linked_continuation() {
        let project = temp_project();
        let mut database = Database::open(&project).expect("initialize database");
        let task = database
            .add_task("Continue the same session", "")
            .expect("add task");
        let source = database
            .begin_writer_attempt(task.id, &"cd".repeat(32))
            .expect("begin source attempt");
        database
            .record_writer_conversation_id(source.id, "session-continue-1")
            .expect("save public session ID before output");
        database
            .finish_writer_attempt(
                source.id,
                WriterAttemptOutcome::Failed,
                None,
                Some(0),
                Some("ERROR"),
                Some("agy_agent_error"),
            )
            .expect("record resumable agent error");

        let eligible = database
            .latest_continuable_attempt(task.id)
            .expect("query latest continuable attempt")
            .expect("failed Agent ERROR with public session is continuable");
        assert_eq!(eligible.id, source.id);
        assert!(
            database
                .begin_writer_continuation(
                    task.id,
                    &"ab".repeat(32),
                    source.id,
                    "session-continue-1",
                    false,
                )
                .is_err()
        );
        let pause_revision = format!("sha256:{}", "4".repeat(64));
        database
            .pause_project("Operator takeover", Some(&pause_revision))
            .expect("pause before continuation");
        assert!(
            database
                .begin_writer_continuation(
                    task.id,
                    &"ef".repeat(32),
                    source.id,
                    "session-continue-1",
                    true,
                )
                .is_err()
        );
        assert!(matches!(
            database
                .resume_project(Some(&pause_revision), false)
                .expect("resume with the same project snapshot"),
            ResumeProjectOutcome::Resumed {
                reconciled_manual_changes: false,
                ..
            }
        ));
        let continuation = database
            .begin_writer_continuation(
                task.id,
                &"ef".repeat(32),
                source.id,
                "session-continue-1",
                true,
            )
            .expect("begin one linked continuation");
        assert_ne!(continuation.id, source.id);
        assert_eq!(continuation.source_attempt_id, Some(source.id));
        assert_eq!(
            continuation.conversation_id.as_deref(),
            Some("session-continue-1")
        );
        assert_eq!(
            database.list_tasks().expect("read task status")[0].status,
            TaskStatus::Implementing
        );
        assert!(
            database
                .begin_writer_continuation(
                    task.id,
                    &"11".repeat(32),
                    source.id,
                    "session-continue-1",
                    true,
                )
                .is_err()
        );
        assert!(
            database
                .latest_continuable_attempt(task.id)
                .expect("query after continuation")
                .is_none()
        );
        drop(database);

        let reopened = Database::open(&project).expect("reopen continuation database");
        let attempts = reopened
            .writer_attempts(Some(task.id))
            .expect("read source and continuation rows");
        assert_eq!(attempts.len(), 2);
        assert_eq!(attempts[1].source_attempt_id, Some(source.id));
        assert_eq!(
            attempts[1].conversation_id.as_deref(),
            Some("session-continue-1")
        );
        assert!(
            reopened
                .has_unresolved_writer_attempts()
                .expect("continuation holds the global unresolved gate")
        );
        cleanup(&project);
    }

    #[test]
    fn unresolved_writer_attempt_survives_reopen_and_cannot_be_retried() {
        let project = temp_project();
        let mut database = Database::open(&project).expect("initialize database");
        let task = database.add_task("Do not duplicate", "").expect("add task");
        let attempt = database
            .begin_writer_attempt(task.id, &"cd".repeat(32))
            .expect("begin writer attempt");
        database
            .mark_writer_attempt_running(attempt.id, 5678)
            .expect("record child PID");
        drop(database);

        let mut reopened = Database::open(&project).expect("reopen database");
        assert!(
            reopened
                .has_unresolved_writer_attempts()
                .expect("check unresolved attempt")
        );
        assert!(
            reopened
                .begin_writer_attempt(task.id, &"ef".repeat(32))
                .is_err()
        );
        let attempts = reopened
            .writer_attempts(Some(task.id))
            .expect("read prior attempt");
        assert_eq!(attempts.len(), 1);
        assert_eq!(attempts[0].process_id, Some(5678));
        assert_eq!(attempts[0].state, "running");
        cleanup(&project);
    }

    #[test]
    fn recovery_quarantines_launching_attempt_after_reopen_without_pid() {
        let project = temp_project();
        let mut database = Database::open(&project).expect("initialize database");
        let task = database
            .add_task("Preserve the pre-spawn ambiguity", "")
            .expect("add task");
        let attempt = database
            .begin_writer_attempt(task.id, &"12".repeat(32))
            .expect("begin Writer attempt");
        drop(database);

        let mut reopened = Database::open(&project).expect("reopen database after crash");
        let lease = recovery_lease(&mut reopened, "recover-launching");
        assert_eq!(
            reopened
                .quarantine_unfinished_writer_attempts(&lease)
                .expect("quarantine unfinished attempt"),
            1
        );
        let recovered = reopened
            .writer_attempts(Some(task.id))
            .expect("read recovered attempt");
        assert_eq!(recovered[0].id, attempt.id);
        assert_eq!(recovered[0].state, "unknown");
        assert_eq!(recovered[0].process_id, None);
        assert_eq!(recovered[0].finished_at_ms, None);
        assert_eq!(
            recovered[0].error_code.as_deref(),
            Some("writer_outcome_unknown")
        );
        assert_eq!(
            reopened.list_tasks().expect("list recovered task")[0].status,
            TaskStatus::Blocked
        );
        assert!(
            reopened
                .has_unresolved_writer_attempts()
                .expect("unknown attempt remains unresolved")
        );

        let event_count = reopened
            .task_events(Some(task.id))
            .expect("read recovery events")
            .len();
        assert_eq!(
            reopened
                .quarantine_unfinished_writer_attempts(&lease)
                .expect("repeated recovery is idempotent"),
            0
        );
        assert_eq!(
            reopened
                .task_events(Some(task.id))
                .expect("re-read recovery events")
                .len(),
            event_count
        );
        cleanup(&project);
    }

    #[test]
    fn operator_resolution_keeps_attempt_unknown_and_audits_each_action() {
        let actions = [
            (
                WriterResolutionAction::AcceptForReview,
                TaskStatus::Reviewing,
            ),
            (WriterResolutionAction::AuthorizeRetry, TaskStatus::Queued),
            (WriterResolutionAction::Abandon, TaskStatus::Failed),
        ];
        for (index, (action, expected_task_status)) in actions.into_iter().enumerate() {
            let project = temp_project();
            let mut database = Database::open(&project).expect("initialize database");
            let task = database
                .add_task("Resolve the unknown Writer outcome", "")
                .expect("add task");
            let attempt = database
                .begin_writer_attempt(task.id, &"de".repeat(32))
                .expect("begin Writer attempt");
            database
                .mark_writer_attempt_running(attempt.id, 9000 + index as u32)
                .expect("record process ID");
            database
                .record_writer_conversation_id(attempt.id, "public-session-123")
                .expect("record public conversation ID");
            let lease = recovery_lease(&mut database, &format!("resolve-{index}"));
            assert_eq!(
                database
                    .quarantine_unfinished_writer_attempts(&lease)
                    .expect("quarantine attempt"),
                1
            );

            let resolution = database
                .resolve_unknown_writer_attempt(
                    &lease,
                    attempt.id,
                    action,
                    true,
                    true,
                    "Confirmed the Writer stopped and reviewed the project diff",
                )
                .expect("commit explicit operator resolution");
            assert_eq!(resolution.attempt_id, attempt.id);
            assert_eq!(resolution.action, action);
            assert_eq!(resolution.task_status, expected_task_status);
            assert_eq!(
                database.list_tasks().expect("read task")[0].status,
                expected_task_status
            );
            assert!(
                !database
                    .has_unresolved_writer_attempts()
                    .expect("resolved unknown no longer blocks the queue")
            );

            let resolved_attempt = database
                .writer_attempts(Some(task.id))
                .expect("read resolved attempt")
                .remove(0);
            assert_eq!(resolved_attempt.state, "unknown");
            assert_eq!(resolved_attempt.process_id, Some(9000 + index as u32));
            assert_eq!(
                resolved_attempt.conversation_id.as_deref(),
                Some("public-session-123")
            );
            assert_eq!(resolved_attempt.finished_at_ms, None);
            assert_eq!(
                resolved_attempt.operator_resolution,
                Some(action.as_str().to_owned())
            );
            assert!(resolved_attempt.resolved_at_ms.is_some());
            assert_eq!(
                resolved_attempt.resolution_reason.as_deref(),
                Some("Confirmed the Writer stopped and reviewed the project diff")
            );

            let events = database
                .task_events(Some(task.id))
                .expect("read resolution events");
            let event = events
                .iter()
                .find(|event| event.event_type == "writer_attempt.operator_resolved")
                .expect("operator resolution event");
            let data: serde_json::Value =
                serde_json::from_str(&event.data_json).expect("parse resolution event");
            assert_eq!(data["writer_stopped_attested"], true);
            assert_eq!(data["project_inspected_attested"], true);
            assert_eq!(data["action"], action.as_str());
            assert_eq!(
                data["reason"],
                "Confirmed the Writer stopped and reviewed the project diff"
            );
            assert_eq!(event.actor, "operator");

            if action == WriterResolutionAction::AuthorizeRetry {
                let next_attempt = database
                    .begin_writer_attempt(task.id, &"ef".repeat(32))
                    .expect("only explicit authorize-retry releases task for a new run");
                assert_ne!(next_attempt.id, attempt.id);
                assert_eq!(next_attempt.state, "launching");
            }
            drop(database);

            let reopened = Database::open(&project).expect("reopen resolved attempt");
            let attempts = reopened
                .writer_attempts(Some(task.id))
                .expect("read persisted attempt history");
            assert_eq!(attempts[0].state, "unknown");
            assert_eq!(
                attempts[0].operator_resolution,
                Some(action.as_str().to_owned())
            );
            assert_eq!(
                reopened
                    .has_unresolved_writer_attempts()
                    .expect("resolution and optional next attempt survive reopen"),
                action == WriterResolutionAction::AuthorizeRetry
            );
            cleanup(&project);
        }
    }

    #[test]
    fn operator_resolution_requires_attestations_and_current_lease() {
        let project = temp_project();
        let mut database = Database::open(&project).expect("initialize database");
        let task = database
            .add_task("Require explicit recovery attestations", "")
            .expect("add task");
        let attempt = database
            .begin_writer_attempt(task.id, &"fa".repeat(32))
            .expect("begin Writer attempt");
        let lease = recovery_lease(&mut database, "resolution-current-lease");
        database
            .quarantine_unfinished_writer_attempts(&lease)
            .expect("quarantine attempt");

        assert!(
            database
                .resolve_unknown_writer_attempt(
                    &lease,
                    attempt.id,
                    WriterResolutionAction::AuthorizeRetry,
                    false,
                    true,
                    "Missing process-stop attestation",
                )
                .is_err()
        );
        let mut forged_lease = lease.clone();
        forged_lease.fencing_token += 1;
        assert!(
            database
                .resolve_unknown_writer_attempt(
                    &forged_lease,
                    attempt.id,
                    WriterResolutionAction::AuthorizeRetry,
                    true,
                    true,
                    "Rejected stale fencing token",
                )
                .is_err()
        );
        assert!(
            database
                .resolve_unknown_writer_attempt(
                    &lease,
                    attempt.id,
                    WriterResolutionAction::AuthorizeRetry,
                    true,
                    true,
                    "   ",
                )
                .is_err()
        );
        assert!(
            database
                .has_unresolved_writer_attempts()
                .expect("failed resolution keeps global guard")
        );
        assert_eq!(
            database
                .writer_attempts(Some(task.id))
                .expect("read unchanged attempt")[0]
                .operator_resolution,
            None
        );

        database
            .resolve_unknown_writer_attempt(
                &lease,
                attempt.id,
                WriterResolutionAction::AuthorizeRetry,
                true,
                true,
                "Both operator checks are complete",
            )
            .expect("valid current lease and attestations resolve attempt");
        assert!(
            !database
                .has_unresolved_writer_attempts()
                .expect("committed resolution releases guard")
        );
        cleanup(&project);
    }

    #[test]
    fn recovery_does_not_signal_an_unrelated_live_process_with_the_recorded_pid() {
        let project = temp_project();
        let mut database = Database::open(&project).expect("initialize database");
        let task = database
            .add_task("Preserve PID reuse ambiguity", "")
            .expect("add task");
        let attempt = database
            .begin_writer_attempt(task.id, &"34".repeat(32))
            .expect("begin Writer attempt");
        // The persisted PID points at this live test process, not the Writer child.
        // Recovery must preserve the number as evidence without signaling it.
        let unrelated_process_id = std::process::id();
        database
            .mark_writer_attempt_running(attempt.id, unrelated_process_id)
            .expect("record child PID");
        drop(database);

        let mut reopened = Database::open(&project).expect("reopen database after crash");
        let lease = recovery_lease(&mut reopened, "recover-running");
        assert_eq!(
            reopened
                .quarantine_unfinished_writer_attempts(&lease)
                .expect("quarantine running attempt"),
            1
        );
        let recovered = reopened
            .writer_attempts(Some(task.id))
            .expect("read recovered attempt");
        assert_eq!(recovered[0].state, "unknown");
        assert_eq!(recovered[0].process_id, Some(unrelated_process_id));
        assert_eq!(recovered[0].finished_at_ms, None);
        assert_eq!(
            reopened.list_tasks().expect("list recovered task")[0].status,
            TaskStatus::Blocked
        );
        assert!(
            reopened
                .has_unresolved_writer_attempts()
                .expect("unknown attempt remains unresolved")
        );
        cleanup(&project);
    }

    #[test]
    fn quarantine_rejects_a_forged_or_stale_lease_without_mutating_state() {
        let project = temp_project();
        let mut database = Database::open(&project).expect("initialize database");
        let task = database
            .add_task("Require the current fencing token", "")
            .expect("add task");
        let attempt = database
            .begin_writer_attempt(task.id, &"9a".repeat(32))
            .expect("begin Writer attempt");
        let lease = recovery_lease(&mut database, "valid-recovery-lease");
        let mut stale_lease = lease.clone();
        stale_lease.fencing_token += 1;

        assert!(
            database
                .quarantine_unfinished_writer_attempts(&stale_lease)
                .is_err()
        );
        assert_eq!(
            database
                .writer_attempts(Some(task.id))
                .expect("read attempt")[0]
                .state,
            "launching"
        );
        assert_eq!(
            database.list_tasks().expect("read task")[0].status,
            TaskStatus::Implementing
        );

        assert_eq!(
            database
                .quarantine_unfinished_writer_attempts(&lease)
                .expect("valid lease can quarantine attempt"),
            1
        );
        assert_eq!(
            database
                .writer_attempts(Some(task.id))
                .expect("read attempt")[0]
                .id,
            attempt.id
        );
        cleanup(&project);
    }

    #[test]
    fn failed_writer_attempt_blocks_task_instead_of_marking_it_complete() {
        let project = temp_project();
        let mut database = Database::open(&project).expect("initialize database");
        let task = database.add_task("Preserve failure", "").expect("add task");
        let attempt = database
            .begin_writer_attempt(task.id, &"ef".repeat(32))
            .expect("begin writer attempt");
        database
            .finish_writer_attempt(
                attempt.id,
                WriterAttemptOutcome::TimedOut,
                None,
                None,
                None,
                Some("writer_timeout"),
            )
            .expect("record timeout");
        let tasks = database.list_tasks().expect("list task");
        assert_eq!(tasks[0].status, TaskStatus::Blocked);
        assert!(
            !database
                .has_unresolved_writer_attempts()
                .expect("check attempts")
        );
        assert_eq!(
            database
                .writer_attempts(Some(task.id))
                .expect("list attempts")[0]
                .state,
            "timed_out"
        );
        cleanup(&project);
    }

    #[test]
    fn task_events_are_append_only() {
        let project = temp_project();
        let mut database = Database::open(&project).expect("initialize database");
        let task = database
            .add_task("Immutable journal", "")
            .expect("add task");
        let result = database.connection.execute(
            "UPDATE task_events SET reason = 'rewritten' WHERE task_id = ?1",
            [task.id],
        );
        assert!(result.is_err());
        cleanup(&project);
    }

    #[test]
    fn task_transitions_update_state_and_append_a_reasoned_event() {
        let project = temp_project();
        let mut database = Database::open(&project).expect("initialize database");
        let task = database
            .add_task("Move through review", "")
            .expect("add task");

        let updated = database
            .transition_task(
                task.id,
                TaskStatus::Planning,
                "supervisor",
                "Starting task planning",
            )
            .expect("transition to planning");
        assert_eq!(updated.status, TaskStatus::Planning);

        let events = database
            .task_events(Some(task.id))
            .expect("read task events");
        assert_eq!(events.len(), 2);
        assert_eq!(events[1].actor, "supervisor");
        assert_eq!(events[1].reason, "Starting task planning");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&events[1].data_json)
                .expect("parse transition event"),
            serde_json::json!({ "from": "queued", "to": "planning" })
        );

        let result = database.transition_task(
            task.id,
            TaskStatus::Complete,
            "supervisor",
            "Attempt illegal closeout",
        );
        assert!(result.is_err());
        assert_eq!(
            database
                .task_events(Some(task.id))
                .expect("re-read events")
                .len(),
            2
        );
        cleanup(&project);
    }

    #[test]
    fn future_schema_is_rejected_without_erasing_database() {
        let project = temp_project();
        let database_path: PathBuf = project.database_path();
        project
            .ensure_runtime_dir()
            .expect("create runtime directory");
        let connection = rusqlite::Connection::open(&database_path).expect("create database");
        connection
            .pragma_update(None, "user_version", 999_i64)
            .expect("set future schema version");
        connection
            .execute_batch("CREATE TABLE sentinel (value TEXT NOT NULL);")
            .expect("create sentinel table");
        drop(connection);

        let error = Database::open(&project)
            .err()
            .expect("future schema should be refused");
        assert!(format!("{error:#}").contains("newer than this TALOS version supports"));
        let connection = rusqlite::Connection::open(database_path).expect("reopen database");
        let sentinel: Option<String> = connection
            .query_row(
                "SELECT name FROM sqlite_master WHERE name = 'sentinel'",
                [],
                |row| row.get(0),
            )
            .optional()
            .expect("query sentinel table");
        assert_eq!(sentinel.as_deref(), Some("sentinel"));
        let journal_mode: String = connection
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .expect("read journal mode");
        assert_eq!(journal_mode.to_lowercase(), "delete");
        cleanup(&project);
    }

    #[test]
    fn active_supervisor_lease_cannot_be_stolen_and_stale_lease_advances_token() {
        let project = temp_project();
        let mut database = Database::open(&project).expect("initialize database");
        let first = database
            .acquire_supervisor_lease("owner-a", 1_000, 500)
            .expect("acquire lease")
            .expect("first owner gets lease");
        assert!(
            database
                .acquire_supervisor_lease("owner-b", 1_200, 500)
                .expect("second acquire check")
                .is_none()
        );
        let successor = database
            .acquire_supervisor_lease("owner-b", 1_500, 500)
            .expect("acquire expired lease")
            .expect("expired lease can be acquired");
        assert_eq!(successor.fencing_token, first.fencing_token + 1);
        assert!(
            !database
                .release_supervisor_lease(&first)
                .expect("stale owner cannot release successor lease")
        );
        assert!(
            database
                .release_supervisor_lease(&successor)
                .expect("successor releases its own lease")
        );
        let next = database
            .acquire_supervisor_lease("owner-c", 1_600, 500)
            .expect("acquire released lease")
            .expect("released lease is available");
        assert_eq!(next.fencing_token, successor.fencing_token + 1);
        cleanup(&project);
    }

    #[test]
    fn concurrent_supervisors_cannot_both_acquire_the_lease() {
        let project = temp_project();
        drop(Database::open(&project).expect("initialize database"));
        let first = Database::open(&project).expect("open first connection");
        let second = Database::open(&project).expect("open second connection");
        let barrier = Arc::new(Barrier::new(2));
        let first_barrier = Arc::clone(&barrier);
        let second_barrier = Arc::clone(&barrier);
        let first_thread = thread::spawn(move || {
            let mut database = first;
            first_barrier.wait();
            database
                .acquire_supervisor_lease("owner-a", 1_000, 10_000)
                .expect("first owner acquire")
        });
        let second_thread = thread::spawn(move || {
            let mut database = second;
            second_barrier.wait();
            database
                .acquire_supervisor_lease("owner-b", 1_000, 10_000)
                .expect("second owner acquire")
        });

        let first_result = first_thread.join().expect("join first owner");
        let second_result = second_thread.join().expect("join second owner");
        assert_ne!(first_result.is_some(), second_result.is_some());
        cleanup(&project);
    }

    #[cfg(unix)]
    #[test]
    fn existing_runtime_and_database_permissions_are_restricted() {
        use std::os::unix::fs::PermissionsExt;

        let project = temp_project();
        let runtime_dir = project.runtime_dir();
        fs::create_dir(&runtime_dir).expect("create broad runtime directory");
        fs::set_permissions(&runtime_dir, fs::Permissions::from_mode(0o777))
            .expect("make runtime directory broad");
        let database_path = project.database_path();
        fs::write(&database_path, []).expect("create existing database file");
        fs::set_permissions(&database_path, fs::Permissions::from_mode(0o666))
            .expect("make database file broad");

        drop(Database::open(&project).expect("secure existing database"));
        let dir_mode = fs::metadata(runtime_dir)
            .expect("read runtime directory metadata")
            .permissions()
            .mode()
            & 0o777;
        let file_mode = fs::metadata(database_path)
            .expect("read database file metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(dir_mode, 0o700);
        assert_eq!(file_mode, 0o600);
        cleanup(&project);
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_database_file_is_rejected() {
        use std::os::unix::fs::symlink;

        let project = temp_project();
        project
            .ensure_runtime_dir()
            .expect("create runtime directory");
        let external = project.root.join("external.sqlite3");
        fs::write(&external, []).expect("create external file");
        symlink(&external, project.database_path()).expect("create database symlink");

        let error = Database::open(&project)
            .err()
            .expect("database symlink should be rejected");
        assert!(format!("{error:#}").contains("must be a regular file"));
        assert!(fs::read(&external).expect("read external file").is_empty());
        cleanup(&project);
    }
}
