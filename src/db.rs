use std::{
    fs::{self, OpenOptions},
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};
use serde::Serialize;

use crate::project::Project;

const SCHEMA_VERSION: i64 = 3;

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
}

type WriterFinalizationState = (
    i64,
    String,
    String,
    Option<i64>,
    i64,
    Option<String>,
    Option<i64>,
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
            .query_map([], |row| {
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
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(records)
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
        let current_status: Option<String> = transaction
            .query_row("SELECT status FROM tasks WHERE id = ?1", [task_id], |row| {
                row.get(0)
            })
            .optional()?;
        let Some(current_status) = current_status else {
            bail!("Task #{task_id} does not exist");
        };
        if current_status != TaskStatus::Queued.as_str() {
            bail!("Task #{task_id} must be queued before a Writer attempt can start");
        }
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
        transaction.execute(
            "INSERT INTO writer_attempts (task_id, state, started_at_ms, prompt_sha256)
             VALUES (?1, 'launching', ?2, ?3)",
            params![task_id, now, prompt_sha256.to_ascii_lowercase()],
        )?;
        let attempt_id = transaction.last_insert_rowid();
        let attempt_data = serde_json::to_string(&serde_json::json!({"attempt_id": attempt_id}))?;
        transaction.execute(
            "INSERT INTO task_events (task_id, occurred_at_ms, actor, event_type, reason, data_json)
             VALUES (?1, ?2, 'talos', 'writer_attempt.started', 'Writer attempt prepared for launch', ?3)",
            params![task_id, now, attempt_data],
        )?;
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
        })
    }

    pub fn mark_writer_attempt_running(&mut self, attempt_id: i64, process_id: u32) -> Result<()> {
        if process_id == 0 {
            bail!("Writer process ID must be greater than zero");
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
            "UPDATE writer_attempts SET state = 'running', process_id = ?1 WHERE id = ?2 AND state = 'launching'",
            params![i64::from(process_id), attempt_id],
        )?;
        if changed != 1 {
            bail!("Writer attempt #{attempt_id} changed during process launch");
        }
        let now = now_ms()?;
        let event_data = serde_json::to_string(&serde_json::json!({
            "attempt_id": attempt_id,
            "process_id": process_id,
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
                        operator_resolution, resolved_at_ms, resolution_reason, source_attempt_id
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
                        operator_resolution, resolved_at_ms, resolution_reason, source_attempt_id
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
                 (task_id, state, started_at_ms, prompt_sha256, conversation_id, source_attempt_id)
             VALUES (?1, 'launching', ?2, ?3, ?4, ?5)",
            params![
                task_id,
                now,
                prompt_sha256.to_ascii_lowercase(),
                conversation_id,
                source_attempt_id
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
                        source_attempt_id
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
                "SELECT id, task_id, state, process_id FROM writer_attempts
                 WHERE state IN ('launching', 'running') ORDER BY id",
            )?;
            statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<i64>>(3)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };

        for (attempt_id, task_id, previous_state, process_id) in &unfinished {
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
                "lease_fencing_token": lease.fencing_token,
            }))?;
            transaction.execute(
                "INSERT INTO task_events (task_id, occurred_at_ms, actor, event_type, reason, data_json)
                 VALUES (?1, ?2, 'talos', 'writer_attempt.quarantined', 'Writer outcome is unknown; process and project state require operator inspection', ?3)",
                params![task_id, now, event_data],
            )?;
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
                    operator_resolution, resolved_at_ms, resolution_reason, source_attempt_id
             FROM writer_attempts WHERE (?1 IS NULL OR task_id = ?1) ORDER BY id",
        )?;
        let attempts = statement
            .query_map([task_id], map_writer_attempt_record)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(attempts)
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
    })
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

    use super::{Database, TaskStatus, WriterAttemptOutcome, WriterResolutionAction};
    use crate::project::{DiscoveryMethod, Project};

    static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

    fn temp_project() -> Project {
        let sequence = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("talos-db-test-{}-{sequence}", std::process::id()));
        fs::create_dir_all(&root).expect("create temporary project");
        Project {
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

    #[test]
    fn initialization_is_idempotent_and_uses_wal() {
        let project = temp_project();
        let first = Database::open(&project).expect("initialize database");
        assert_eq!(first.schema_version().expect("read schema version"), 3);
        assert_eq!(
            first
                .journal_mode()
                .expect("read journal mode")
                .to_lowercase(),
            "wal"
        );
        drop(first);

        let second = Database::open(&project).expect("open database again");
        assert_eq!(second.schema_version().expect("read schema version"), 3);
        drop(second);
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
    fn schema_v1_migrates_to_v3_without_losing_tasks() {
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
        assert_eq!(database.schema_version().expect("read schema version"), 3);
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
        assert_eq!(database.schema_version().expect("read schema version"), 3);
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
