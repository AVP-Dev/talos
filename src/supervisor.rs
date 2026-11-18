use std::{
    env,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::{
    adapter::agy_process::{AgyProcessEvent, AgyRunConfig, AgyRunResult, TerminationReason},
    db::{
        ChildTaskContract, CriticReviewInput, CriticReviewRecord, Database, TaskRecord, TaskStatus,
        WriterAttemptOutcome, WriterAttemptRecord, WriterContinuationClaim,
    },
    project::{DiscoveryMethod, Project},
    review::{CriticResponse, ReviewRequest},
    revision::{RevisionSnapshot, clean_base_commit},
};

pub const VERIFIED_AGY_WRITE_VERSION: &str = "1.5.10";
pub const VERIFIED_AGY_CRITIC_VERSION: &str = "1.5.10";

pub fn is_verified_agy_version(v: &str) -> bool {
    v == "1.5.10" || v == "1.5.11"
}
const MAX_CRITIC_RESPONSE_BYTES: usize = 64 * 1024;
const STOP_POLL_INTERVAL: Duration = Duration::from_millis(100);

struct StopRequestWatcher {
    receiver: tokio::sync::watch::Receiver<bool>,
    shutdown: Arc<AtomicBool>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl StopRequestWatcher {
    fn start(project: Project, attempt_id: i64) -> Self {
        let (sender, receiver) = tokio::sync::watch::channel(false);
        let shutdown = Arc::new(AtomicBool::new(false));
        let poll_shutdown = Arc::clone(&shutdown);
        let task = tokio::task::spawn_blocking(move || {
            let database = match Database::open(&project) {
                Ok(database) => database,
                Err(error) => {
                    tracing::warn!(%error, attempt_id, "Could not open stop-request observer");
                    return;
                }
            };
            while !poll_shutdown.load(Ordering::Acquire) {
                match database.writer_stop_requested(attempt_id) {
                    Ok(true) => {
                        let _ = sender.send(true);
                        return;
                    }
                    Ok(false) => {}
                    Err(error) => {
                        tracing::warn!(%error, attempt_id, "Could not inspect Writer stop request");
                    }
                }
                std::thread::sleep(STOP_POLL_INTERVAL);
            }
        });
        Self {
            receiver,
            shutdown,
            task: Some(task),
        }
    }

    async fn finish(mut self) -> Result<()> {
        self.shutdown.store(true, Ordering::Release);
        self.task
            .take()
            .context("Writer stop-request observer is missing")?
            .await
            .context("Writer stop-request observer task failed")
    }
}

impl Drop for StopRequestWatcher {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
    }
}

/// Conservatively check whether the process group TALOS created for this Writer still exists.
/// This reports liveness only; it does not identify a process or establish a Writer outcome.
#[cfg(unix)]
pub fn writer_process_group_running(process_id: u32) -> Result<bool> {
    use std::io;

    let group_id = i32::try_from(process_id)
        .context("Recorded Writer process ID is out of range for a process group")?;
    let result = unsafe { libc::kill(-group_id, 0) };
    if result == 0 {
        return Ok(true);
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(false)
    } else {
        Err(error).context("Cannot verify whether the recorded Writer process group exists")
    }
}

#[cfg(not(unix))]
pub fn writer_process_group_running(_process_id: u32) -> Result<bool> {
    bail!("Writer process-group inspection is unavailable on this platform")
}

#[derive(Debug)]
pub struct WriterExecutionReport {
    pub attempt: WriterAttemptRecord,
    pub response: Option<String>,
    pub outcome: WriterAttemptOutcome,
}

#[derive(Clone, Debug, Serialize)]
pub struct CriticReviewReport {
    pub review: CriticReviewRecord,
    pub findings: Vec<crate::review::CriticFinding>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WriterContinuation {
    pub source_attempt_id: i64,
    pub conversation_id: String,
    pub project_inspected_attested: bool,
}

#[derive(Clone, Debug)]
pub struct WriterRunOptions {
    pub adapter: crate::config::AdapterKind,
    pub executable: PathBuf,
    pub agy_version: String,
    pub model: Option<String>,
    pub timeout: Duration,
    pub max_concurrent_writers: usize,
}

pub async fn run_writer_attempt(
    project: &Project,
    database: &mut Database,
    task_id: i64,
    executable: PathBuf,
    agy_version: &str,
    timeout: Duration,
) -> Result<WriterExecutionReport> {
    run_writer_attempt_with_options(
        project,
        database,
        task_id,
        WriterRunOptions {
            adapter: crate::config::AdapterKind::Agy,
            executable,
            agy_version: agy_version.to_owned(),
            model: None,
            timeout,
            max_concurrent_writers: 1,
        },
    )
    .await
}

pub async fn run_writer_attempt_with_options(
    project: &Project,
    database: &mut Database,
    task_id: i64,
    options: WriterRunOptions,
) -> Result<WriterExecutionReport> {
    run_writer_attempt_inner(project, database, task_id, options, None).await
}

pub async fn continue_writer_attempt(
    project: &Project,
    database: &mut Database,
    task_id: i64,
    executable: PathBuf,
    agy_version: &str,
    timeout: Duration,
    continuation: WriterContinuation,
) -> Result<WriterExecutionReport> {
    continue_writer_attempt_with_options(
        project,
        database,
        task_id,
        continuation,
        WriterRunOptions {
            adapter: crate::config::AdapterKind::Agy,
            executable,
            agy_version: agy_version.to_owned(),
            model: None,
            timeout,
            max_concurrent_writers: 1,
        },
    )
    .await
}

pub async fn continue_writer_attempt_with_options(
    project: &Project,
    database: &mut Database,
    task_id: i64,
    continuation: WriterContinuation,
    options: WriterRunOptions,
) -> Result<WriterExecutionReport> {
    run_writer_attempt_inner(project, database, task_id, options, Some(continuation)).await
}

#[allow(clippy::too_many_arguments)]
pub async fn run_agent_query(
    project_root: &Path,
    adapter: crate::config::AdapterKind,
    executable: PathBuf,
    model: Option<String>,
    prompt: String,
    read_only: bool,
    timeout: Duration,
) -> Result<String> {
    match adapter {
        crate::config::AdapterKind::Codex => {
            let res = crate::adapter::codex::run_codex(
                crate::adapter::codex::CodexRunConfig {
                    executable,
                    project_root: project_root.to_path_buf(),
                    prompt,
                    sandbox: if read_only {
                        crate::adapter::codex::CodexSandboxMode::ReadOnly
                    } else {
                        crate::adapter::codex::CodexSandboxMode::WorkspaceWrite
                    },
                    model,
                    session_id: None,
                    timeout,
                },
                |_| Ok(()),
            )
            .await?;
            if res.exit_code != Some(0) {
                bail!(
                    "Codex exited with code {:?}: {:?}",
                    res.exit_code,
                    res.error
                );
            }
            Ok(res.response.unwrap_or_default())
        }
        crate::config::AdapterKind::Omp => {
            let res = crate::adapter::omp::run_omp(crate::adapter::omp::OmpRunConfig {
                executable,
                project_root: project_root.to_path_buf(),
                prompt,
                role: if read_only {
                    crate::adapter::omp::OmpRole::Critic
                } else {
                    crate::adapter::omp::OmpRole::Writer
                },
                model,
                timeout,
            })
            .await?;
            if res.exit_code != Some(0) {
                bail!("OMP exited with code {:?}: {:?}", res.exit_code, res.error);
            }
            Ok(res.response.unwrap_or_default())
        }
        crate::config::AdapterKind::Pi => {
            let res = crate::adapter::pi::run_pi(crate::adapter::pi::PiRunConfig {
                executable,
                project_root: project_root.to_path_buf(),
                prompt,
                role: if read_only {
                    crate::adapter::pi::PiRole::Critic
                } else {
                    crate::adapter::pi::PiRole::Writer
                },
                model,
                timeout,
            })
            .await?;
            if res.exit_code != Some(0) {
                bail!("Pi exited with code {:?}: {:?}", res.exit_code, res.error);
            }
            Ok(res.response.unwrap_or_default())
        }
        crate::config::AdapterKind::Kilo => {
            let res = crate::adapter::kilo::run_kilo(crate::adapter::kilo::KiloRunConfig {
                executable,
                project_root: project_root.to_path_buf(),
                prompt,
                role: if read_only {
                    crate::adapter::kilo::KiloRole::Critic
                } else {
                    crate::adapter::kilo::KiloRole::Writer
                },
                model,
                timeout,
            })
            .await?;
            if res.exit_code != Some(0) {
                bail!("Kilo exited with code {:?}: {:?}", res.exit_code, res.error);
            }
            Ok(res.response.unwrap_or_default())
        }
        crate::config::AdapterKind::Claude => {
            let res = crate::adapter::claude::run_claude(
                crate::adapter::claude::ClaudeRunConfig {
                    executable,
                    project_root: project_root.to_path_buf(),
                    prompt,
                    role: if read_only {
                        crate::adapter::claude::ClaudeRoleMode::Critic
                    } else {
                        crate::adapter::claude::ClaudeRoleMode::Writer
                    },
                    model,
                    session_id: None,
                    timeout,
                },
                |_| Ok(()),
            )
            .await?;
            if res.exit_code != Some(0) {
                bail!(
                    "Claude exited with code {:?}: {:?}",
                    res.exit_code,
                    res.error
                );
            }
            Ok(res.response.unwrap_or_default())
        }
        crate::config::AdapterKind::OpenCode => {
            let res = crate::adapter::opencode::run_opencode(
                crate::adapter::opencode::OpenCodeRunConfig {
                    executable,
                    project_root: project_root.to_path_buf(),
                    prompt,
                    role: if read_only {
                        crate::adapter::opencode::OpenCodeRoleMode::Critic
                    } else {
                        crate::adapter::opencode::OpenCodeRoleMode::Writer
                    },
                    model,
                    session_id: None,
                    timeout,
                },
                |_| Ok(()),
            )
            .await?;
            if res.exit_code != Some(0) {
                bail!(
                    "OpenCode exited with code {:?}: {:?}",
                    res.exit_code,
                    res.error
                );
            }
            Ok(res.response.unwrap_or_default())
        }
        crate::config::AdapterKind::Agy => {
            let project = Project::discover(project_root)?;
            let mut settings = create_critic_session_settings(&project, 0, "agy_query")?;
            let execution = crate::adapter::agy_process::run_read_only(
                AgyRunConfig {
                    executable,
                    project_root: project.root.clone(),
                    settings_file: settings.path.clone(),
                    prompt,
                    conversation_id: None,
                    timeout,
                },
                |_| Ok(()),
            )
            .await;
            settings.remove_after_resolution();
            let res = execution.context("Antigravity process failed")?;
            if res.exit_code != Some(0) {
                bail!("Antigravity exited with code {:?}", res.exit_code);
            }
            Ok(res.response.unwrap_or_default())
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn run_critic_review(
    project: &Project,
    database: &mut Database,
    task_id: i64,
    adapter: crate::config::AdapterKind,
    executable: PathBuf,
    adapter_version: &str,
    model: Option<String>,
    timeout: Duration,
) -> Result<CriticReviewReport> {
    if database.has_unresolved_writer_attempts()? {
        bail!("An active or unresolved Writer attempt prevents Critic review");
    }
    let task = database
        .get_task(task_id)?
        .with_context(|| format!("Task #{task_id} does not exist"))?;
    if task.status != TaskStatus::Reviewing {
        bail!(
            "Task #{task_id} must be reviewing after a successful Writer attempt before Critic review"
        );
    }
    validate_project_scope(project)?;
    if !matches!(&project.discovery, DiscoveryMethod::Git) {
        bail!(
            "Critic review requires a Git project so TALOS can identify the exact base revision and diff"
        );
    }

    let writer = database
        .latest_writer_attempt(task_id)?
        .context("Task has no saved Writer attempt to review")?;
    if writer.state != "succeeded" {
        bail!("Critic review requires the latest Writer attempt to have succeeded");
    }
    let base_revision = writer.base_revision.as_deref().context(
        "Writer attempt has no saved Git base revision; this result cannot be reviewed safely",
    )?;
    let snapshot = RevisionSnapshot::capture(&project.root, base_revision)?;
    if snapshot.changed_paths.is_empty() {
        bail!("Critic review requires a non-empty project diff");
    }
    let child_contract = database.child_task_contract(task_id)?;
    let mut review_task = task.clone();
    if let Some(contract) = &child_contract {
        let allowed_paths = serde_json::to_string(&contract.allowed_paths)?;
        review_task.acceptance_criteria = format!(
            "{}\n\nChild-task scope: only these project-relative paths may change: {allowed_paths}. Treat every changed path outside this list as a blocker.",
            review_task.acceptance_criteria
        );
    }
    if let Ok(team_subtasks) = database.team_subtasks_for_parent(task_id) {
        for st in team_subtasks {
            if st.role == "critic_worker"
                && let Some(summary) = &st.result_summary
            {
                review_task.acceptance_criteria = format!(
                    "{}\n\nCRITIC WORKER AUDIT FINDINGS (independently detected issues that Critic Lead must inspect and evaluate for request_changes):\n{summary}",
                    review_task.acceptance_criteria
                );
            }
        }
    }
    let request = ReviewRequest::new(
        &review_task,
        writer.id,
        &snapshot.base_commit,
        &snapshot.revision_id,
        &snapshot.changed_paths,
        &snapshot.patch,
    );
    let prompt = request.prompt()?;
    let (response_str, exit_code, termination) = match adapter {
        crate::config::AdapterKind::Codex => {
            let res = crate::adapter::codex::run_codex(
                crate::adapter::codex::CodexRunConfig {
                    executable,
                    project_root: project.root.clone(),
                    prompt,
                    sandbox: crate::adapter::codex::CodexSandboxMode::ReadOnly,
                    model,
                    session_id: None,
                    timeout,
                },
                |_| Ok(()),
            )
            .await?;
            (res.response, res.exit_code, res.termination)
        }
        crate::config::AdapterKind::Omp => {
            let res = crate::adapter::omp::run_omp(crate::adapter::omp::OmpRunConfig {
                executable,
                project_root: project.root.clone(),
                prompt,
                role: crate::adapter::omp::OmpRole::Critic,
                model,
                timeout,
            })
            .await?;
            (res.response, res.exit_code, res.termination)
        }
        crate::config::AdapterKind::Pi => {
            let res = crate::adapter::pi::run_pi(crate::adapter::pi::PiRunConfig {
                executable,
                project_root: project.root.clone(),
                prompt,
                role: crate::adapter::pi::PiRole::Critic,
                model,
                timeout,
            })
            .await?;
            (res.response, res.exit_code, res.termination)
        }
        crate::config::AdapterKind::Kilo => {
            let res = crate::adapter::kilo::run_kilo(crate::adapter::kilo::KiloRunConfig {
                executable,
                project_root: project.root.clone(),
                prompt,
                role: crate::adapter::kilo::KiloRole::Critic,
                model,
                timeout,
            })
            .await?;
            (res.response, res.exit_code, res.termination)
        }
        crate::config::AdapterKind::Claude => {
            let res = crate::adapter::claude::run_claude(
                crate::adapter::claude::ClaudeRunConfig {
                    executable,
                    project_root: project.root.clone(),
                    prompt,
                    role: crate::adapter::claude::ClaudeRoleMode::Critic,
                    model,
                    session_id: None,
                    timeout,
                },
                |_| Ok(()),
            )
            .await?;
            (res.response, res.exit_code, res.termination)
        }
        crate::config::AdapterKind::OpenCode => {
            let res = crate::adapter::opencode::run_opencode(
                crate::adapter::opencode::OpenCodeRunConfig {
                    executable,
                    project_root: project.root.clone(),
                    prompt,
                    role: crate::adapter::opencode::OpenCodeRoleMode::Critic,
                    model,
                    session_id: None,
                    timeout,
                },
                |_| Ok(()),
            )
            .await?;
            (res.response, res.exit_code, res.termination)
        }
        _ => {
            if !is_verified_agy_version(adapter_version) {
                bail!(
                    "Read-only Critic use is verified for Antigravity 1.5.10 / 1.5.11; installed version {adapter_version} requires a new disposable-project read-only probe"
                );
            }
            let mut settings =
                create_critic_session_settings(project, task_id, &snapshot.revision_id)?;
            let execution = crate::adapter::agy_process::run_read_only(
                AgyRunConfig {
                    executable,
                    project_root: project.root.clone(),
                    settings_file: settings.path.clone(),
                    prompt,
                    conversation_id: None,
                    timeout,
                },
                |_| Ok(()),
            )
            .await;
            settings.remove_after_resolution();
            let execution = execution.context("Critic process failed; no approval was recorded")?;
            (
                execution.response,
                execution.exit_code,
                execution.termination,
            )
        }
    };
    if termination != TerminationReason::Exited
        || exit_code != Some(0)
        || response_str.as_deref().is_none_or(str::is_empty)
    {
        bail!("Critic did not return a complete successful response; no approval was recorded");
    }
    let response_text = response_str.as_deref().unwrap();
    if response_text.len() > MAX_CRITIC_RESPONSE_BYTES {
        bail!(
            "Critic response exceeds the {MAX_CRITIC_RESPONSE_BYTES}-byte result budget; no approval was recorded"
        );
    }
    let mut response = CriticResponse::parse_and_validate(
        response_text,
        &snapshot.revision_id,
        &snapshot.changed_paths,
    )?;
    if let Some(contract) = &child_contract
        && let Some(out_of_scope) = snapshot
            .changed_paths
            .iter()
            .find(|path| !contract.permits_path(path))
    {
        response.decision = crate::review::ReviewDecision::RequestChanges;
        if response.findings.len() >= 100 {
            response.findings.truncate(99);
        }
        response.findings.push(crate::review::CriticFinding {
            severity: crate::review::FindingSeverity::Blocker,
            path: out_of_scope.clone(),
            line: None,
            title: "Child task changed a path outside its allowed scope".to_owned(),
            reason: "The child-task contract does not permit this changed path. Revert the out-of-scope change or move it to a separately scoped task.".to_owned(),
        });
        if response.summary.len() <= 3_800 {
            response
                .summary
                .push_str(" Deterministic child-scope validation found an out-of-scope path.");
        }
        response.validate(&snapshot.revision_id, &snapshot.changed_paths)?;
    }
    let current_snapshot = RevisionSnapshot::capture(&project.root, base_revision)?;
    if current_snapshot.revision_id != snapshot.revision_id {
        bail!(
            "Project changed while the Critic was reviewing; its result is stale and was not recorded"
        );
    }
    let canonical_json = response.canonical_json()?;
    let changed_paths_json = serde_json::to_string(&snapshot.changed_paths)
        .context("Cannot serialize Critic changed paths")?;
    let review = database.record_critic_review(CriticReviewInput {
        task_id,
        writer_attempt_id: writer.id,
        base_revision,
        head_commit: &snapshot.head_commit,
        revision_id: &snapshot.revision_id,
        patch_sha256: &snapshot.patch_sha256,
        changed_paths_json: &changed_paths_json,
        decision: response.decision.as_str(),
        summary: &response.summary,
        result_json: &canonical_json,
        adapter: adapter.as_str(),
        adapter_version,
    })?;
    Ok(CriticReviewReport {
        review,
        findings: response.findings,
    })
}

async fn run_writer_attempt_inner(
    project: &Project,
    database: &mut Database,
    task_id: i64,
    options: WriterRunOptions,
    continuation: Option<WriterContinuation>,
) -> Result<WriterExecutionReport> {
    let WriterRunOptions {
        adapter,
        executable,
        agy_version,
        model,
        timeout,
        max_concurrent_writers,
    } = options;
    if adapter == crate::config::AdapterKind::Agy && !is_verified_agy_version(&agy_version) {
        bail!(
            "Safe unattended writes were verified for Antigravity 1.5.10 / 1.5.11; installed version {agy_version} requires a new disposable-project boundary probe"
        );
    }
    if !(1..=3).contains(&max_concurrent_writers) {
        bail!("max_concurrent_writers must be between 1 and 3");
    }
    let task = database
        .get_task(task_id)?
        .with_context(|| format!("Task #{task_id} does not exist"))?;
    let child_contract = database.child_task_contract(task_id)?;
    let effective_timeout = child_contract
        .as_ref()
        .map(|contract| {
            u64::try_from(contract.timeout_ms)
                .map(Duration::from_millis)
                .context("Child task timeout is out of range")
        })
        .transpose()?
        .unwrap_or(timeout);
    let revision_review = match &continuation {
        Some(continuation) => {
            let latest = database
                .latest_continuable_attempt(task_id)?
                .context("Task has no eligible terminal Writer error to continue")?;
            if latest.id != continuation.source_attempt_id
                || latest.conversation_id.as_deref() != Some(continuation.conversation_id.as_str())
            {
                bail!("The selected Writer conversation is no longer the latest eligible attempt");
            }
            if task.status != TaskStatus::Blocked {
                bail!(
                    "Task #{task_id} must be blocked after a terminal Writer error before continuation"
                );
            }
            None
        }
        None if task.status == TaskStatus::Implementing => {
            let latest = database
                .latest_critic_review(task_id)?
                .context("Task is implementing without a saved Critic review")?;
            if latest.decision != "request_changes" {
                bail!("Task #{task_id} has no latest Critic review requesting Writer changes");
            }
            Some(latest)
        }
        None if task.status == TaskStatus::Queued => None,
        None => bail!(
            "Task #{task_id} must be queued or awaiting Writer fixes after a Critic rejection"
        ),
    };

    validate_project_scope(project)?;
    let initial_base_revision = if continuation.is_none()
        && revision_review.is_none()
        && task.status == TaskStatus::Queued
        && matches!(&project.discovery, DiscoveryMethod::Git)
    {
        Some(clean_base_commit(&project.root)?)
    } else {
        None
    };
    project.ensure_runtime_dir()?;
    let prompt = if let Some(continuation) = &continuation {
        build_writer_continuation_prompt(&task, continuation.source_attempt_id)
    } else if let Some(review) = &revision_review {
        build_writer_revision_prompt(&task, review)
    } else {
        build_writer_prompt(&task)
    };
    let prompt = match child_contract.as_ref() {
        Some(contract) => append_child_task_contract(prompt, contract)?,
        None => prompt,
    };
    let prompt_sha256 = encode_lower_hex(&Sha256::digest(prompt.as_bytes()));
    let attempt = if let Some(continuation) = &continuation {
        database.begin_writer_continuation_for_root(WriterContinuationClaim {
            task_id: task.id,
            prompt_sha256: &prompt_sha256,
            source_attempt_id: continuation.source_attempt_id,
            conversation_id: &continuation.conversation_id,
            project_inspected_attested: continuation.project_inspected_attested,
            owner_pid: std::process::id(),
            max_concurrent_writers,
            expected_execution_root: project.root.as_path(),
        })?
    } else {
        database.begin_writer_attempt_for_root(
            task.id,
            &prompt_sha256,
            std::process::id(),
            max_concurrent_writers,
            project.root.as_path(),
        )?
    };
    if let Some(base_revision) = initial_base_revision {
        database.record_writer_base_revision(attempt.id, &base_revision)?;
    }
    let execution: AgyRunResult = match adapter {
        crate::config::AdapterKind::Codex => {
            let attempt_id = attempt.id;
            let project_clone = project.clone();
            database.mark_writer_attempt_running_with_identity(
                attempt.id,
                std::process::id(),
                None,
            )?;
            let codex_res = crate::adapter::codex::run_codex(
                crate::adapter::codex::CodexRunConfig {
                    executable: executable.clone(),
                    project_root: project.root.clone(),
                    prompt,
                    sandbox: crate::adapter::codex::CodexSandboxMode::WorkspaceWrite,
                    model: model.clone(),
                    session_id: continuation.as_ref().map(|v| v.conversation_id.clone()),
                    timeout: effective_timeout,
                },
                move |session_id| {
                    if let Ok(mut db) = Database::open(&project_clone) {
                        let _ = db.record_writer_conversation_id(attempt_id, session_id);
                    }
                    Ok(())
                },
            )
            .await;
            match codex_res {
                Ok(res) => {
                    let agent_status = if res.exit_code == Some(0) {
                        Some("SUCCESS".to_owned())
                    } else {
                        Some("ERROR".to_owned())
                    };
                    AgyRunResult {
                        process_id: res.process_id,
                        exit_code: res.exit_code,
                        termination: res.termination,
                        conversation_id: res.session_id,
                        agent_status,
                        response: res.response,
                        agent_error: res.error,
                        stderr_excerpt: res.stderr_excerpt,
                        stdout_truncated: false,
                        stderr_truncated: false,
                        input_error: None,
                    }
                }
                Err(error) => {
                    return Err(error.context(format!(
                        "Writer attempt #{} has an unknown outcome and remains unresolved; run `talos task recover` to quarantine it for inspection, and do not retry it",
                        attempt.id
                    )));
                }
            }
        }
        crate::config::AdapterKind::Omp => {
            database.mark_writer_attempt_running_with_identity(
                attempt.id,
                std::process::id(),
                None,
            )?;
            let omp_res = crate::adapter::omp::run_omp(crate::adapter::omp::OmpRunConfig {
                executable: executable.clone(),
                project_root: project.root.clone(),
                prompt,
                role: crate::adapter::omp::OmpRole::Writer,
                model: model.clone(),
                timeout: effective_timeout,
            })
            .await;
            match omp_res {
                Ok(res) => {
                    let agent_status = if res.exit_code == Some(0) {
                        Some("SUCCESS".to_owned())
                    } else {
                        Some("ERROR".to_owned())
                    };
                    AgyRunResult {
                        process_id: res.process_id,
                        exit_code: res.exit_code,
                        termination: res.termination,
                        conversation_id: res.session_id,
                        agent_status,
                        response: res.response,
                        agent_error: res.error,
                        stderr_excerpt: res.stderr_excerpt,
                        stdout_truncated: false,
                        stderr_truncated: false,
                        input_error: None,
                    }
                }
                Err(error) => {
                    return Err(error.context(format!(
                        "Writer attempt #{} has an unknown outcome and remains unresolved; run `talos task recover` to quarantine it for inspection, and do not retry it",
                        attempt.id
                    )));
                }
            }
        }
        crate::config::AdapterKind::Pi => {
            database.mark_writer_attempt_running_with_identity(
                attempt.id,
                std::process::id(),
                None,
            )?;
            let pi_res = crate::adapter::pi::run_pi(crate::adapter::pi::PiRunConfig {
                executable: executable.clone(),
                project_root: project.root.clone(),
                prompt,
                role: crate::adapter::pi::PiRole::Writer,
                model: model.clone(),
                timeout: effective_timeout,
            })
            .await;
            match pi_res {
                Ok(res) => {
                    let agent_status = if res.exit_code == Some(0) {
                        Some("SUCCESS".to_owned())
                    } else {
                        Some("ERROR".to_owned())
                    };
                    AgyRunResult {
                        process_id: res.process_id,
                        exit_code: res.exit_code,
                        termination: res.termination,
                        conversation_id: res.session_id,
                        agent_status,
                        response: res.response,
                        agent_error: res.error,
                        stderr_excerpt: res.stderr_excerpt,
                        stdout_truncated: false,
                        stderr_truncated: false,
                        input_error: None,
                    }
                }
                Err(error) => {
                    return Err(error.context(format!(
                        "Writer attempt #{} has an unknown outcome and remains unresolved; run `talos task recover` to quarantine it for inspection, and do not retry it",
                        attempt.id
                    )));
                }
            }
        }
        crate::config::AdapterKind::Kilo => {
            database.mark_writer_attempt_running_with_identity(
                attempt.id,
                std::process::id(),
                None,
            )?;
            let kilo_res = crate::adapter::kilo::run_kilo(crate::adapter::kilo::KiloRunConfig {
                executable: executable.clone(),
                project_root: project.root.clone(),
                prompt,
                role: crate::adapter::kilo::KiloRole::Writer,
                model: model.clone(),
                timeout: effective_timeout,
            })
            .await;
            match kilo_res {
                Ok(res) => {
                    let agent_status = if res.exit_code == Some(0) {
                        Some("SUCCESS".to_owned())
                    } else {
                        Some("ERROR".to_owned())
                    };
                    AgyRunResult {
                        process_id: res.process_id,
                        exit_code: res.exit_code,
                        termination: res.termination,
                        conversation_id: res.session_id,
                        agent_status,
                        response: res.response,
                        agent_error: res.error,
                        stderr_excerpt: res.stderr_excerpt,
                        stdout_truncated: false,
                        stderr_truncated: false,
                        input_error: None,
                    }
                }
                Err(error) => {
                    return Err(error.context(format!(
                        "Writer attempt #{} has an unknown outcome and remains unresolved; run `talos task recover` to quarantine it for inspection, and do not retry it",
                        attempt.id
                    )));
                }
            }
        }
        crate::config::AdapterKind::Claude => {
            database.mark_writer_attempt_running_with_identity(
                attempt.id,
                std::process::id(),
                None,
            )?;
            let claude_res = crate::adapter::claude::run_claude(
                crate::adapter::claude::ClaudeRunConfig {
                    executable: executable.clone(),
                    project_root: project.root.clone(),
                    prompt,
                    role: crate::adapter::claude::ClaudeRoleMode::Writer,
                    model: model.clone(),
                    session_id: None,
                    timeout: effective_timeout,
                },
                |_| Ok(()),
            )
            .await;
            match claude_res {
                Ok(res) => {
                    let agent_status = if res.exit_code == Some(0) {
                        Some("SUCCESS".to_owned())
                    } else {
                        Some("ERROR".to_owned())
                    };
                    AgyRunResult {
                        process_id: res.process_id,
                        exit_code: res.exit_code,
                        termination: res.termination,
                        conversation_id: res.session_id,
                        agent_status,
                        response: res.response,
                        agent_error: res.error,
                        stderr_excerpt: res.stderr_excerpt,
                        stdout_truncated: false,
                        stderr_truncated: false,
                        input_error: None,
                    }
                }
                Err(error) => {
                    return Err(error.context(format!(
                        "Writer attempt #{} has an unknown outcome and remains unresolved; run `talos task recover` to quarantine it for inspection, and do not retry it",
                        attempt.id
                    )));
                }
            }
        }
        crate::config::AdapterKind::OpenCode => {
            database.mark_writer_attempt_running_with_identity(
                attempt.id,
                std::process::id(),
                None,
            )?;
            let opencode_res = crate::adapter::opencode::run_opencode(
                crate::adapter::opencode::OpenCodeRunConfig {
                    executable: executable.clone(),
                    project_root: project.root.clone(),
                    prompt,
                    role: crate::adapter::opencode::OpenCodeRoleMode::Writer,
                    model: model.clone(),
                    session_id: None,
                    timeout: effective_timeout,
                },
                |_| Ok(()),
            )
            .await;
            match opencode_res {
                Ok(res) => {
                    let agent_status = if res.exit_code == Some(0) {
                        Some("SUCCESS".to_owned())
                    } else {
                        Some("ERROR".to_owned())
                    };
                    AgyRunResult {
                        process_id: res.process_id,
                        exit_code: res.exit_code,
                        termination: res.termination,
                        conversation_id: res.session_id,
                        agent_status,
                        response: res.response,
                        agent_error: res.error,
                        stderr_excerpt: res.stderr_excerpt,
                        stdout_truncated: false,
                        stderr_truncated: false,
                        input_error: None,
                    }
                }
                Err(error) => {
                    return Err(error.context(format!(
                        "Writer attempt #{} has an unknown outcome and remains unresolved; run `talos task recover` to quarantine it for inspection, and do not retry it",
                        attempt.id
                    )));
                }
            }
        }
        _ => {
            let mut settings = match create_session_settings(project, attempt.id) {
                Ok(settings) => settings,
                Err(error) => {
                    database.finish_writer_attempt(
                        attempt.id,
                        WriterAttemptOutcome::Failed,
                        None,
                        None,
                        None,
                        Some("agy_policy_creation_failed"),
                    )?;
                    return Err(error);
                }
            };

            let stop_watcher = StopRequestWatcher::start(project.clone(), attempt.id);
            let execution = crate::adapter::agy_process::run_with_stop_request(
                AgyRunConfig {
                    executable,
                    project_root: project.root.clone(),
                    settings_file: settings.path.clone(),
                    prompt,
                    conversation_id: continuation
                        .as_ref()
                        .map(|value| value.conversation_id.clone()),
                    timeout: effective_timeout,
                },
                |event| match event {
                    AgyProcessEvent::Started(process_id) => {
                        let process_identity = match crate::process_identity::capture(process_id) {
                            Ok(identity) => Some(identity.fingerprint),
                            Err(error) => {
                                tracing::debug!(%error, process_id, "Writer process identity is unavailable");
                                None
                            }
                        };
                        database.mark_writer_attempt_running_with_identity(
                            attempt.id,
                            process_id,
                            process_identity.as_deref(),
                        )
                    }
                    AgyProcessEvent::ConversationId(conversation_id) => {
                        database.record_writer_conversation_id(attempt.id, &conversation_id)
                    }
                },
                stop_watcher.receiver.clone(),
            )
            .await;
            stop_watcher.finish().await?;
            let execution = execution.map_err(|error| {
                error.context(format!(
                    "Writer attempt #{} has an unknown outcome and remains unresolved; run `talos task recover` to quarantine it for inspection, and do not retry it",
                    attempt.id
                ))
            })?;
            settings.remove_after_resolution();
            execution
        }
    };

    let (outcome, error_code) = classify_execution(&execution);
    let conversation_id = safe_conversation_id(execution.conversation_id.as_deref());
    let agent_status = safe_agent_status(execution.agent_status.as_deref());
    let outcome = if outcome == WriterAttemptOutcome::Succeeded && conversation_id.is_none() {
        WriterAttemptOutcome::Failed
    } else {
        outcome
    };
    let error_code = if error_code.is_none()
        && conversation_id.is_none()
        && outcome == WriterAttemptOutcome::Failed
    {
        Some("agy_missing_session_id")
    } else {
        error_code
    };
    let attempt = database.finish_writer_attempt(
        attempt.id,
        outcome,
        conversation_id,
        execution.exit_code,
        agent_status,
        error_code,
    )?;

    Ok(WriterExecutionReport {
        attempt,
        response: execution.response,
        outcome,
    })
}

fn classify_execution(result: &AgyRunResult) -> (WriterAttemptOutcome, Option<&'static str>) {
    match result.termination {
        TerminationReason::TimedOut => (WriterAttemptOutcome::TimedOut, Some("writer_timeout")),
        TerminationReason::InterruptedByCtrlC | TerminationReason::InterruptedByTerminateSignal => {
            (
                WriterAttemptOutcome::Interrupted,
                Some("writer_interrupted"),
            )
        }
        TerminationReason::StoppedByOperator => (
            WriterAttemptOutcome::Interrupted,
            Some("writer_stopped_by_operator"),
        ),
        TerminationReason::Exited
            if result.exit_code == Some(0)
                && result.agent_status.as_deref() == Some("SUCCESS")
                && result
                    .response
                    .as_deref()
                    .is_some_and(|response| !response.trim().is_empty())
                && !result.stdout_truncated
                && result.input_error.is_none() =>
        {
            (WriterAttemptOutcome::Succeeded, None)
        }
        TerminationReason::Exited if result.stdout_truncated => {
            (WriterAttemptOutcome::Failed, Some("agy_output_limit"))
        }
        TerminationReason::Exited if result.exit_code != Some(0) => {
            (WriterAttemptOutcome::Failed, Some("agy_exit_nonzero"))
        }
        TerminationReason::Exited if result.agent_status.as_deref() == Some("ERROR") => {
            (WriterAttemptOutcome::Failed, Some("agy_agent_error"))
        }
        TerminationReason::Exited if result.input_error.is_some() => {
            (WriterAttemptOutcome::Failed, Some("agy_input_error"))
        }
        TerminationReason::Exited => (WriterAttemptOutcome::Failed, Some("agy_result_invalid")),
    }
}

fn safe_conversation_id(value: Option<&str>) -> Option<&str> {
    value.filter(|value| {
        !value.is_empty()
            && value.len() <= 200
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    })
}

fn safe_agent_status(value: Option<&str>) -> Option<&str> {
    value.filter(|value| {
        !value.is_empty()
            && value.len() <= 32
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
    })
}

fn encode_lower_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
}

fn build_writer_prompt(task: &TaskRecord) -> String {
    let criteria = if task.acceptance_criteria.trim().is_empty() {
        "No separate acceptance criteria were supplied."
    } else {
        task.acceptance_criteria.trim()
    };
    format!(
        "You are the TALOS Writer for one queued software task.\n\
         Make the smallest source changes needed to satisfy the task.\n\
         Do not run terminal commands, use the network, interact with external services,\n\
         or modify `.talos/` or `.git/`. TALOS will run project checks separately.\n\
         Do not claim that checks passed unless TALOS reports their results.\n\
         At the end, return a concise English summary of changed files and any remaining work.\n\
         Task ID: {}\n\
         Task: {}\n\
         Acceptance criteria: {}",
        task.id, task.title, criteria
    )
}

fn append_child_task_contract(mut prompt: String, contract: &ChildTaskContract) -> Result<String> {
    let allowed_paths = serde_json::to_string(&contract.allowed_paths)?;
    prompt.push_str(&format!(
        "\n\nBOUNDED CHILD-TASK CONTRACT:\n\
         Parent task ID: {}\n\
         Allowed project-relative paths (JSON): {}\n\
         Change files only inside these paths. Do not edit or create files outside this scope.\n\
         Writer timeout budget: {} milliseconds.\n\
         Return a concise English final summary with changed files, completed work, and remaining work.\
         TALOS will validate the changed paths and must finish Critic review and project verification before the parent can run.",
        contract.parent_task_id, allowed_paths, contract.timeout_ms
    ));
    Ok(prompt)
}

fn build_writer_revision_prompt(task: &TaskRecord, review: &CriticReviewRecord) -> String {
    let criteria = if task.acceptance_criteria.trim().is_empty() {
        "No separate acceptance criteria were supplied."
    } else {
        task.acceptance_criteria.trim()
    };
    format!(
        "You are the TALOS Writer starting a new fix iteration after an independent Critic review.\n\
         Inspect the current project state and make the smallest changes needed to address the blocking findings.\n\
         Do not run terminal commands, use the network, interact with external services,\n\
         or modify `.talos/` or `.git/`. TALOS will run project checks separately.\n\
         Do not claim that checks passed unless TALOS reports their results.\n\
         Keep the task's original base revision and preserve unrelated files.\n\
         Task ID: {}\n\
         Task: {}\n\
         Acceptance criteria: {}\n\
         Source Critic review ID: {}\n\
         Source revision: {}\n\
         Critic findings JSON: {}\n\
         At the end, return a concise English summary of changed files and any remaining work.",
        task.id, task.title, criteria, review.id, review.revision_id, review.result_json
    )
}

fn build_writer_continuation_prompt(task: &TaskRecord, source_attempt_id: i64) -> String {
    let criteria = if task.acceptance_criteria.trim().is_empty() {
        "No separate acceptance criteria were supplied."
    } else {
        task.acceptance_criteria.trim()
    };
    format!(
        "You are continuing the existing TALOS Writer conversation for one task.\n\
         First inspect the current project state and existing edits. Do not replay or repeat\n\
         changes that are already present. Continue only the remaining work needed for the\n\
         acceptance criteria; if they are already satisfied, make no further edits and report that.\n\
         Do not run terminal commands, use the network, interact with external services,\n\
         or modify `.talos/` or `.git/`. TALOS will run project checks separately.\n\
         Do not claim that checks passed unless TALOS reports their results.\n\
         At the end, return a concise English summary of changed files and any remaining work.\n\
         Task ID: {}\n\
         Source Writer attempt: #{}\n\
         Task: {}\n\
         Acceptance criteria: {}",
        task.id, source_attempt_id, task.title, criteria
    )
}

fn create_session_settings(project: &Project, attempt_id: i64) -> Result<SessionSettings> {
    let root = rule_path(&project.root)?;
    let talos_dir = project.runtime_dir();
    let talos = rule_path(&talos_dir)?;
    let settings = build_session_policy(&root, &talos);
    write_session_settings(project, attempt_id, &settings)
}

fn create_critic_session_settings(
    project: &Project,
    task_id: i64,
    revision_id: &str,
) -> Result<SessionSettings> {
    let root = rule_path(&project.root)?;
    let talos = rule_path(&project.runtime_dir())?;
    let policy = build_critic_session_policy(&root, &talos);
    let digest = revision_id
        .strip_prefix("sha256:")
        .context("Critic revision ID must be a SHA-256 digest")?;
    if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("Critic revision ID has an invalid SHA-256 digest");
    }
    write_session_settings_with_name(
        project,
        &format!(
            "agy-critic-{task_id}-{}-{}",
            &digest[..12],
            std::process::id()
        ),
        &policy,
    )
}

fn build_session_policy(root: &str, talos: &str) -> Value {
    json!({
        "enableTerminalSandbox": true,
        "toolPermission": "proceed-in-sandbox",
        "permissions": {
            "allow": [format!("write_file({root}/)")],
            "deny": [
                format!("read_file({talos})"),
                format!("read_file({talos}/)"),
                format!("write_file({talos})"),
                format!("write_file({talos}/)"),
                format!("read_file({root}/.git)"),
                format!("read_file({root}/.git/)"),
                format!("write_file({root}/.git)"),
                format!("write_file({root}/.git/)"),
                "command(regex:.*)".to_owned(),
                "mcp(*)".to_owned(),
                "read_url(*)".to_owned(),
                "execute_url(*)".to_owned(),
            ],
        },
    })
}

fn build_critic_session_policy(root: &str, talos: &str) -> Value {
    json!({
        "enableTerminalSandbox": true,
        "toolPermission": "proceed-in-sandbox",
        "permissions": {
            "allow": [format!("read_file({root}/)")],
            "deny": [
                "write_file(*)",
                "command(*)",
                "mcp(*)",
                "read_url(*)",
                "execute_url(*)",
                format!("read_file({talos})"),
                format!("read_file({talos}/)"),
                format!("read_file({root}/.git)"),
                format!("read_file({root}/.git/)"),
            ],
        },
    })
}

fn rule_path(path: &Path) -> Result<String> {
    let value = path
        .to_str()
        .context("Project paths must be valid UTF-8 for Antigravity's permission rules")?;
    if value
        .chars()
        .any(|character| character.is_control() || matches!(character, '(' | ')' | '\\' | '*'))
    {
        bail!(
            "Project path contains characters that cannot be safely represented in Agy permission rules"
        );
    }
    Ok(value.trim_end_matches('/').to_owned())
}

pub fn validate_project_scope(project: &Project) -> Result<()> {
    if project.root == Path::new("/") {
        bail!("The filesystem root cannot be used as a Writer workspace");
    }
    if matches!(&project.discovery, DiscoveryMethod::CurrentDirectory) {
        bail!(
            "Writer execution requires a detected project root or an initialized .talos workspace"
        );
    }
    if let Some(home) = env::var_os("HOME").and_then(|path| PathBuf::from(path).canonicalize().ok())
        && home.starts_with(&project.root)
    {
        bail!("A user's home directory or its parent cannot be used as a Writer workspace");
    }
    let _ = rule_path(&project.root)?;
    let _ = rule_path(&project.runtime_dir())?;
    Ok(())
}

fn write_session_settings(
    project: &Project,
    attempt_id: i64,
    value: &Value,
) -> Result<SessionSettings> {
    write_session_settings_with_name(project, &format!("agy-attempt-{attempt_id}"), value)
}

fn write_session_settings_with_name(
    project: &Project,
    name: &str,
    value: &Value,
) -> Result<SessionSettings> {
    let path = project.runtime_dir().join(format!("{name}-settings.json"));
    let bytes = serde_json::to_vec_pretty(value).context("Cannot serialize Agy session policy")?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&path)
        .with_context(|| format!("Cannot create session-only Agy settings {}", path.display()))?;
    if let Err(error) = file
        .write_all(&bytes)
        .and_then(|()| file.write_all(b"\n"))
        .and_then(|()| file.sync_all())
    {
        drop(file);
        let _ = fs::remove_file(&path);
        return Err(error).with_context(|| {
            format!(
                "Cannot persist session-only Agy settings {}",
                path.display()
            )
        });
    }
    Ok(SessionSettings {
        path,
        remove_on_drop: false,
    })
}

struct SessionSettings {
    path: PathBuf,
    remove_on_drop: bool,
}

impl SessionSettings {
    fn remove_after_resolution(&mut self) {
        self.remove_on_drop = true;
    }
}

impl Drop for SessionSettings {
    fn drop(&mut self) {
        if self.remove_on_drop {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::Value;
    use sha2::{Digest, Sha256};
    use std::path::{Path, PathBuf};
    use std::{
        fs,
        os::unix::fs::PermissionsExt,
        os::unix::process::CommandExt,
        process::Command,
        sync::atomic::{AtomicU64, Ordering},
        time::{Duration, SystemTime, UNIX_EPOCH},
    };

    use super::{
        VERIFIED_AGY_WRITE_VERSION, WriterContinuation, build_critic_session_policy,
        build_session_policy, build_writer_continuation_prompt, build_writer_prompt,
        classify_execution, continue_writer_attempt, encode_lower_hex, rule_path,
        run_writer_attempt, safe_agent_status, safe_conversation_id, validate_project_scope,
        writer_process_group_running,
    };
    use crate::{
        adapter::agy_process::{AgyRunResult, TerminationReason},
        db::{Database, TaskRecord, TaskStatus, WriterAttemptOutcome},
        project::{DiscoveryMethod, Project},
    };

    static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn process_group_probe_reports_liveness_without_signaling() {
        let mut child = Command::new("sh")
            .args(["-c", "exec sleep 30"])
            .process_group(0)
            .spawn()
            .expect("start isolated process group");
        let process_id = child.id();

        assert!(writer_process_group_running(process_id).expect("probe live process group"));

        let group_id = i32::try_from(process_id).expect("PID fits a process-group ID");
        let signal_result = unsafe { libc::kill(-group_id, libc::SIGTERM) };
        assert_eq!(signal_result, 0, "terminate isolated test process group");
        child.wait().expect("reap test process");

        assert!(!writer_process_group_running(process_id).expect("probe stopped process group"));
    }

    fn test_project() -> Project {
        let sequence = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "talos-supervisor-test-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir_all(&root).expect("create temporary project");
        Project {
            state_root: root.clone(),
            root,
            discovery: DiscoveryMethod::ExistingTalosDirectory,
        }
    }

    fn test_recovery_lease(database: &mut Database) -> crate::db::SupervisorLease {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time is after the Unix epoch");
        let now_ms = i64::try_from(now.as_millis()).expect("timestamp fits in SQLite integer");
        database
            .acquire_recovery_supervisor_lease("supervisor-recovery-test", now_ms, 60_000)
            .expect("acquire recovery lease")
            .expect("recovery lease is available")
    }

    #[cfg(unix)]
    async fn assert_fake_cli_failure_is_durable(
        script: &str,
        timeout: Duration,
        expected_state: &str,
        expected_error: &str,
        expected_exit_code: Option<i32>,
        expected_agent_status: Option<&str>,
        expected_conversation_id: Option<&str>,
    ) {
        let project = test_project();
        let mut database = Database::open(&project).expect("initialize project database");
        let task = database
            .add_task(
                "Preserve this task after Writer failure",
                "Keep its criteria",
            )
            .expect("queue task");
        let executable = project.root.join("fake-agy");
        fs::write(&executable, script).expect("write fake CLI");
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700))
            .expect("make fake CLI executable");

        let report = run_writer_attempt(
            &project,
            &mut database,
            task.id,
            executable,
            VERIFIED_AGY_WRITE_VERSION,
            timeout,
        )
        .await
        .expect("a completed fake CLI failure should have a durable report");

        assert_eq!(report.attempt.state, expected_state);
        assert_eq!(report.attempt.error_code.as_deref(), Some(expected_error));
        assert_eq!(report.attempt.exit_code, expected_exit_code);
        assert_eq!(
            report.attempt.agent_status.as_deref(),
            expected_agent_status
        );
        assert_eq!(
            report.attempt.conversation_id.as_deref(),
            expected_conversation_id
        );
        assert_ne!(report.outcome, WriterAttemptOutcome::Succeeded);

        let persisted_attempts = database
            .writer_attempts(Some(task.id))
            .expect("read persisted writer attempt");
        assert_eq!(persisted_attempts.len(), 1);
        assert_eq!(persisted_attempts[0].state, expected_state);
        assert_eq!(
            persisted_attempts[0].error_code.as_deref(),
            Some(expected_error)
        );
        assert_eq!(persisted_attempts[0].exit_code, expected_exit_code);
        assert_eq!(
            persisted_attempts[0].conversation_id.as_deref(),
            expected_conversation_id
        );
        let persisted_task = database
            .list_tasks()
            .expect("read persisted task")
            .into_iter()
            .find(|candidate| candidate.id == task.id)
            .expect("task remains present");
        assert_eq!(
            persisted_task.title,
            "Preserve this task after Writer failure"
        );
        assert_eq!(persisted_task.acceptance_criteria, "Keep its criteria");
        assert_eq!(persisted_task.status, TaskStatus::Blocked);
        assert_ne!(persisted_task.status, TaskStatus::Complete);
        assert!(
            !database
                .has_unresolved_writer_attempts()
                .expect("check terminal attempt state")
        );
        assert!(
            !project
                .runtime_dir()
                .join(format!(
                    "agy-attempt-{}-settings.json",
                    persisted_attempts[0].id
                ))
                .exists()
        );

        fs::remove_dir_all(project.root).expect("remove temporary project");
    }

    #[test]
    fn only_the_live_tested_antigravity_version_is_enabled_for_writes() {
        assert_eq!(VERIFIED_AGY_WRITE_VERSION, "1.5.10");
    }

    #[test]
    fn writer_prompt_is_english_and_includes_the_saved_criteria() {
        let task = TaskRecord {
            id: 12,
            title: "Add one field".to_owned(),
            acceptance_criteria: "The field is serialized".to_owned(),
            status: TaskStatus::Queued,
            created_at_ms: 1,
            updated_at_ms: 1,
        };
        let prompt = build_writer_prompt(&task);
        assert!(prompt.contains("Task: Add one field"));
        assert!(prompt.contains("Acceptance criteria: The field is serialized"));
        assert!(prompt.contains("Do not run terminal commands"));
    }

    #[test]
    fn permission_rule_paths_reject_expression_delimiters() {
        assert!(rule_path(Path::new("/tmp/project (unsafe)")).is_err());
        assert!(rule_path(Path::new("/tmp/project*/unsafe")).is_err());
        assert_eq!(
            rule_path(Path::new("/tmp/project with spaces")).unwrap(),
            "/tmp/project with spaces"
        );
    }

    #[cfg(unix)]
    #[test]
    fn filesystem_root_is_not_a_writer_workspace() {
        let project = Project {
            root: PathBuf::from("/"),
            state_root: PathBuf::from("/"),
            discovery: DiscoveryMethod::CurrentDirectory,
        };
        assert!(validate_project_scope(&project).is_err());
    }

    #[test]
    fn current_directory_fallback_is_not_a_writer_workspace() {
        let project = Project {
            root: PathBuf::from("/tmp/unclassified-writer-root"),
            state_root: PathBuf::from("/tmp/unclassified-writer-root"),
            discovery: DiscoveryMethod::CurrentDirectory,
        };
        assert!(validate_project_scope(&project).is_err());
    }

    #[test]
    fn home_directory_and_its_parent_are_not_writer_workspaces() {
        let Ok(home) = std::env::var("HOME") else {
            return;
        };
        let home = PathBuf::from(home)
            .canonicalize()
            .expect("canonicalize home");
        let project = Project {
            root: home.clone(),
            state_root: home.clone(),
            discovery: DiscoveryMethod::ExistingTalosDirectory,
        };
        assert!(validate_project_scope(&project).is_err());
        if let Some(parent) = home.parent() {
            let project = Project {
                root: parent.to_path_buf(),
                state_root: parent.to_path_buf(),
                discovery: DiscoveryMethod::Git,
            };
            assert!(validate_project_scope(&project).is_err());
        }
    }

    #[test]
    fn session_policy_has_project_allow_and_strong_denies() {
        let policy: Value = build_session_policy("/tmp/project", "/tmp/project/.talos");
        assert_eq!(
            policy["permissions"]["allow"][0],
            "write_file(/tmp/project/)"
        );
        assert!(policy["permissions"]["deny"].as_array().unwrap().len() >= 12);
    }

    #[test]
    fn critic_policy_allows_reading_project_files_and_denies_mutation_and_external_tools() {
        let policy: Value = build_critic_session_policy("/tmp/project", "/tmp/project/.talos");
        assert_eq!(
            policy["permissions"]["allow"][0],
            "read_file(/tmp/project/)"
        );
        let denied = policy["permissions"]["deny"].as_array().expect("deny list");
        for rule in [
            "write_file(*)",
            "command(*)",
            "mcp(*)",
            "read_url(*)",
            "execute_url(*)",
            "read_file(/tmp/project/.talos)",
            "read_file(/tmp/project/.talos/)",
            "read_file(/tmp/project/.git)",
            "read_file(/tmp/project/.git/)",
        ] {
            assert!(denied.iter().any(|value| value == rule), "missing {rule}");
        }
        assert_eq!(policy["enableTerminalSandbox"], true);
        assert_eq!(policy["toolPermission"], "proceed-in-sandbox");
    }

    #[test]
    fn only_complete_success_results_are_reviewable() {
        let result = AgyRunResult {
            process_id: 1,
            exit_code: Some(0),
            termination: TerminationReason::Exited,
            conversation_id: Some("conversation-1".to_owned()),
            agent_status: Some("SUCCESS".to_owned()),
            response: Some("Updated one file".to_owned()),
            agent_error: None,
            stderr_excerpt: String::new(),
            stdout_truncated: false,
            stderr_truncated: false,
            input_error: None,
        };
        assert_eq!(
            classify_execution(&result).0,
            WriterAttemptOutcome::Succeeded
        );
        let mut partial = result;
        partial.stdout_truncated = true;
        assert_eq!(classify_execution(&partial).0, WriterAttemptOutcome::Failed);
    }

    #[test]
    fn persisted_cli_identifiers_are_bounded_and_safe() {
        assert_eq!(
            safe_conversation_id(Some("abc-def_123")),
            Some("abc-def_123")
        );
        assert_eq!(safe_conversation_id(Some("bad session id")), None);
        assert_eq!(safe_agent_status(Some("SUCCESS")), Some("SUCCESS"));
        assert_eq!(safe_agent_status(Some("ERROR\nsecret")), None);
    }

    #[test]
    fn sha256_digest_is_lowercase_hexadecimal() {
        assert_eq!(
            encode_lower_hex(&Sha256::digest(b"hello world")),
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn queued_task_is_persisted_sent_once_and_left_open_for_review() {
        let project = test_project();
        let mut database = Database::open(&project).expect("initialize project database");
        let task = database
            .add_task(
                "Create the requested fixture",
                "The Writer received this acceptance criterion.",
            )
            .expect("queue task");
        let executable = project.root.join("fake-agy");
        fs::write(
            &executable,
            "#!/bin/sh\ninput=\"$(cat)\"\ncase \"$input\" in *'Acceptance criteria: The Writer received this acceptance criterion.'*) ;; *) exit 87;; esac\nprintf '%s\\n' '{\"event\":\"result\",\"result\":{\"conversation_id\":\"fake-session-1\",\"status\":\"SUCCESS\",\"response\":\"Prepared changes for review.\"}}'\n",
        )
        .expect("write fake CLI");
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700))
            .expect("make fake CLI executable");

        let report = run_writer_attempt(
            &project,
            &mut database,
            task.id,
            executable,
            VERIFIED_AGY_WRITE_VERSION,
            Duration::from_secs(5),
        )
        .await
        .expect("execute queued task with fake CLI");

        assert_eq!(report.outcome, WriterAttemptOutcome::Succeeded);
        assert_eq!(report.attempt.state, "succeeded");
        assert_eq!(
            report.attempt.conversation_id.as_deref(),
            Some("fake-session-1")
        );
        assert_eq!(
            report.response.as_deref(),
            Some("Prepared changes for review.")
        );
        assert_eq!(
            database.list_tasks().expect("list tasks")[0].status,
            TaskStatus::Reviewing
        );
        assert!(
            !database
                .has_unresolved_writer_attempts()
                .expect("check unresolved attempt")
        );
        assert!(
            !project
                .runtime_dir()
                .join(format!("agy-attempt-{}-settings.json", report.attempt.id))
                .exists()
        );
        fs::remove_dir_all(project.root).expect("remove temporary project");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn terminal_agent_error_continues_the_same_session_once_after_inspection() {
        let project = test_project();
        let mut database = Database::open(&project).expect("initialize project database");
        let task = database
            .add_task(
                "Continue one terminal Writer error",
                "Create one result file",
            )
            .expect("queue task");
        let executable = project.root.join("fake-agy");
        fs::write(
            &executable,
            "#!/bin/sh\nprintf '%s\\n' '{\"event\":\"init\",\"conversation_id\":\"continuation-session\"}'\ninput=\"$(cat)\"\ncase \"$input\" in *'Acceptance criteria: Create one result file'*) ;; *) exit 87;; esac\nprintf '%s\\n' '{\"event\":\"result\",\"result\":{\"conversation_id\":\"continuation-session\",\"status\":\"ERROR\",\"error\":\"Recoverable terminal error\"}}'\n",
        )
        .expect("write first fake CLI");
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700))
            .expect("make first fake CLI executable");

        let first = run_writer_attempt(
            &project,
            &mut database,
            task.id,
            executable.clone(),
            VERIFIED_AGY_WRITE_VERSION,
            Duration::from_secs(5),
        )
        .await
        .expect("record terminal Agy error");
        assert_eq!(first.attempt.state, "failed");
        assert_eq!(first.attempt.exit_code, Some(0));
        assert_eq!(first.attempt.agent_status.as_deref(), Some("ERROR"));
        assert_eq!(
            first.attempt.conversation_id.as_deref(),
            Some("continuation-session")
        );
        assert_eq!(
            database.list_tasks().expect("read blocked task")[0].status,
            TaskStatus::Blocked
        );

        fs::write(
            &executable,
            "#!/bin/sh\nfound=0\nwhile [ \"$#\" -gt 0 ]; do\n  if [ \"$1\" = \"--conversation\" ]; then shift; [ \"$1\" = \"continuation-session\" ] || exit 91; found=1; fi\n  shift\ndone\n[ \"$found\" -eq 1 ] || exit 92\nprintf '%s\\n' '{\"event\":\"init\",\"conversation_id\":\"continuation-session\"}'\ninput=\"$(cat)\"\ncase \"$input\" in *'Do not replay or repeat'*) ;; *) exit 93;; esac\nprintf '%s\\n' '{\"event\":\"result\",\"result\":{\"conversation_id\":\"continuation-session\",\"status\":\"SUCCESS\",\"response\":\"Continued without replaying existing edits.\"}}'\nprintf '%s\\n' 'continued once' > \"$PWD/TALOS_CONTINUED.md\"\n",
        )
        .expect("write continuation fake CLI");
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700))
            .expect("make continuation fake CLI executable");

        let continuation = continue_writer_attempt(
            &project,
            &mut database,
            task.id,
            executable,
            VERIFIED_AGY_WRITE_VERSION,
            Duration::from_secs(5),
            WriterContinuation {
                source_attempt_id: first.attempt.id,
                conversation_id: "continuation-session".to_owned(),
                project_inspected_attested: true,
            },
        )
        .await
        .expect("continue the exact public conversation");

        assert_eq!(continuation.outcome, WriterAttemptOutcome::Succeeded);
        assert_eq!(
            continuation.attempt.source_attempt_id,
            Some(first.attempt.id)
        );
        assert_eq!(
            continuation.attempt.conversation_id.as_deref(),
            Some("continuation-session")
        );
        assert_eq!(
            fs::read_to_string(project.root.join("TALOS_CONTINUED.md"))
                .expect("read continuation side effect"),
            "continued once\n"
        );
        assert_eq!(
            database.list_tasks().expect("read task after continuation")[0].status,
            TaskStatus::Reviewing
        );
        let attempts = database
            .writer_attempts(Some(task.id))
            .expect("read linked attempts");
        assert_eq!(attempts.len(), 2);
        assert_eq!(attempts[1].source_attempt_id, Some(attempts[0].id));
        let events = database
            .task_events(Some(task.id))
            .expect("read continuation audit");
        assert!(events.iter().any(|event| {
            event.event_type == "writer_attempt.continuation_started"
                && event
                    .data_json
                    .contains("\"project_inspected_attested\":true")
        }));
        assert!(
            build_writer_continuation_prompt(&task, first.attempt.id)
                .contains("Do not replay or repeat")
        );
        fs::remove_dir_all(project.root).expect("remove temporary project");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn timed_out_writer_is_persisted_as_blocked_without_false_completion() {
        assert_fake_cli_failure_is_durable(
            "#!/bin/sh\ncat >/dev/null\nexec sleep 30\n",
            Duration::from_millis(100),
            "timed_out",
            "writer_timeout",
            None,
            None,
            None,
        )
        .await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn nonzero_writer_exit_is_persisted_even_with_a_success_payload() {
        assert_fake_cli_failure_is_durable(
            "#!/bin/sh\ncat >/dev/null\nprintf '%s\\n' '{\"event\":\"result\",\"result\":{\"conversation_id\":\"fake-session-nonzero\",\"status\":\"SUCCESS\",\"response\":\"Do not trust this payload\"}}'\nexit 17\n",
            Duration::from_secs(5),
            "failed",
            "agy_exit_nonzero",
            Some(17),
            Some("SUCCESS"),
            Some("fake-session-nonzero"),
        )
        .await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn agent_error_result_is_persisted_as_failed() {
        assert_fake_cli_failure_is_durable(
            "#!/bin/sh\ncat >/dev/null\nprintf '%s\\n' '{\"event\":\"result\",\"result\":{\"conversation_id\":\"fake-session-error\",\"status\":\"ERROR\",\"error\":\"Writer reported an error\"}}'\n",
            Duration::from_secs(5),
            "failed",
            "agy_agent_error",
            Some(0),
            Some("ERROR"),
            Some("fake-session-error"),
        )
        .await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn success_payload_without_a_valid_session_id_is_not_reviewable() {
        assert_fake_cli_failure_is_durable(
            "#!/bin/sh\ncat >/dev/null\nprintf '%s\\n' '{\"event\":\"result\",\"result\":{\"conversation_id\":\"bad session id\",\"status\":\"SUCCESS\",\"response\":\"Missing durable session identity\"}}'\n",
            Duration::from_secs(5),
            "failed",
            "agy_missing_session_id",
            Some(0),
            Some("SUCCESS"),
            None,
        )
        .await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn invalid_writer_result_is_persisted_as_failed() {
        assert_fake_cli_failure_is_durable(
            "#!/bin/sh\ncat >/dev/null\nprintf '%s\\n' '{invalid json'\n",
            Duration::from_secs(5),
            "failed",
            "agy_result_invalid",
            Some(0),
            None,
            None,
        )
        .await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn process_launch_error_keeps_attempt_unresolved_for_recovery() {
        let project = test_project();
        let mut database = Database::open(&project).expect("initialize project database");
        let task = database
            .add_task("Keep an uncertain launch open", "")
            .expect("queue task");
        let error = run_writer_attempt(
            &project,
            &mut database,
            task.id,
            project.root.join("missing-agy"),
            VERIFIED_AGY_WRITE_VERSION,
            Duration::from_secs(5),
        )
        .await
        .expect_err("missing executable must not produce a terminal attempt");

        assert!(format!("{error:#}").contains("remains unresolved"));
        assert!(
            database
                .has_unresolved_writer_attempts()
                .expect("attempt remains unresolved")
        );
        assert_eq!(
            database
                .writer_attempts(Some(task.id))
                .expect("list attempts")[0]
                .state,
            "launching"
        );
        assert_eq!(
            database.list_tasks().expect("list tasks")[0].status,
            TaskStatus::Implementing
        );
        assert!(
            project
                .runtime_dir()
                .join("agy-attempt-1-settings.json")
                .exists()
        );

        let lease = test_recovery_lease(&mut database);
        assert_eq!(
            database
                .quarantine_unfinished_writer_attempts(&lease)
                .expect("quarantine uncertain attempt"),
            1
        );
        assert_eq!(
            database
                .writer_attempts(Some(task.id))
                .expect("list recovered attempts")[0]
                .state,
            "unknown"
        );
        assert!(
            database
                .has_unresolved_writer_attempts()
                .expect("recovered attempt remains unresolved")
        );
        assert_eq!(
            database.list_tasks().expect("read quarantined task")[0].status,
            TaskStatus::Blocked
        );
        fs::remove_dir_all(project.root).expect("remove temporary project");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn unknown_attempt_blocks_a_different_queued_writer_before_launch() {
        let project = test_project();
        let mut database = Database::open(&project).expect("initialize project database");
        let uncertain_task = database
            .add_task("Keep the prior effect unresolved", "")
            .expect("queue uncertain task");
        let queued_task = database
            .add_task("Do not launch another Writer", "")
            .expect("queue second task");
        database
            .begin_writer_attempt(uncertain_task.id, &"78".repeat(32))
            .expect("begin uncertain attempt");
        let lease = test_recovery_lease(&mut database);
        database
            .quarantine_unfinished_writer_attempts(&lease)
            .expect("quarantine uncertain attempt");
        assert!(
            database
                .release_supervisor_lease(&lease)
                .expect("release recovery lease")
        );

        let executable = project.root.join("must-not-launch");
        fs::write(
            &executable,
            "#!/bin/sh\ntouch \"$(dirname \"$0\")/writer-started\"\ncat >/dev/null\n",
        )
        .expect("write fake CLI");
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700))
            .expect("make fake CLI executable");
        let error = run_writer_attempt(
            &project,
            &mut database,
            queued_task.id,
            executable,
            VERIFIED_AGY_WRITE_VERSION,
            Duration::from_secs(5),
        )
        .await
        .expect_err("unresolved attempt must block every new Writer");

        assert!(format!("{error:#}").contains("unresolved Writer outcome"));
        assert!(!project.root.join("writer-started").exists());
        assert_eq!(
            database.list_tasks().expect("list tasks")[1].status,
            TaskStatus::Queued
        );
        fs::remove_dir_all(project.root).expect("remove temporary project");
    }
}
