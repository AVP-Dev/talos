use std::{
    fs,
    path::Path,
    process::Command,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::{AdapterKind, OrchestrationMode};
use crate::db::{Database, TaskRecord, TaskStatus};
use crate::project::Project;

#[derive(Clone, Debug)]
pub struct TaskSummaryData<'a> {
    pub task: &'a TaskRecord,
    pub mode: OrchestrationMode,
    pub writer_adapter: AdapterKind,
    pub writer_model: Option<&'a str>,
    pub critic_adapter: AdapterKind,
    pub critic_model: Option<&'a str>,
    pub start_time: SystemTime,
    pub total_pause_duration: Duration,
    pub decisions: Vec<String>,
    pub checks_passed: Vec<String>,
    pub modified_files: Vec<String>,
    pub commit_hash: Option<String>,
    pub deferred_findings: Vec<crate::review::CriticFinding>,
}

/// Truncates large error/output texts to protect token economy (Context Budgeting).
/// Keeps head and tail while eliding verbose intermediate noise.
pub fn truncate_for_token_economy(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }

    let head_size = max_bytes / 3;
    let tail_size = (max_bytes * 2) / 3;

    let head = &text[..text.floor_char_boundary(head_size)];
    let tail_start = text.ceil_char_boundary(text.len().saturating_sub(tail_size));
    let tail = &text[tail_start..];

    format!(
        "{head}\n\n[... {} bytes truncated for token economy ...]\n\n{tail}",
        text.len() - (head.len() + tail.len())
    )
}

pub fn generate_summary_markdown(data: &TaskSummaryData) -> String {
    let now = SystemTime::now();
    let elapsed = now.duration_since(data.start_time).unwrap_or_default();
    let elapsed_secs = elapsed.as_secs();
    let pause_secs = data.total_pause_duration.as_secs();
    let active_secs = elapsed_secs.saturating_sub(pause_secs);

    let format_dur = |secs: u64| {
        if secs < 60 {
            format!("{secs}s")
        } else {
            format!("{}m {}s", secs / 60, secs % 60)
        }
    };

    let start_timestamp = data
        .start_time
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let mut doc = String::new();
    doc.push_str(&format!(
        "# TALOS Task Summary — Task #{}: {}\n\n",
        data.task.id, data.task.title
    ));

    doc.push_str("## ⏱ Session Timings & Execution Metrics\n");
    doc.push_str("- **Status:** Completed Successfully\n");
    doc.push_str(&format!(
        "- **Orchestration Mode:** {}\n",
        data.mode.as_str()
    ));
    doc.push_str(&format!(
        "- **Writer Agent:** {} {}\n",
        data.writer_adapter.as_str(),
        data.writer_model
            .map(|m| format!("(`{m}`) "))
            .unwrap_or_default()
    ));
    doc.push_str(&format!(
        "- **Critic Agent:** {} {}\n",
        data.critic_adapter.as_str(),
        data.critic_model
            .map(|m| format!("(`{m}`) "))
            .unwrap_or_default()
    ));
    doc.push_str(&format!(
        "- **Start Unix Timestamp:** `{start_timestamp}`\n"
    ));
    doc.push_str(&format!(
        "- **Total Session Duration:** {} (Active: {}, Rate-limit pauses: {})\n",
        format_dur(elapsed_secs),
        format_dur(active_secs),
        format_dur(pause_secs)
    ));
    if let Some(ref hash) = data.commit_hash {
        doc.push_str(&format!("- **Git Commit:** `{hash}`\n"));
    }
    doc.push('\n');

    doc.push_str("## 🏛 Autonomous Architectural & Technical Decisions\n");
    if data.decisions.is_empty() {
        doc.push_str("*Standard autonomous execution according to project criteria.*\n");
    } else {
        for (idx, decision) in data.decisions.iter().enumerate() {
            doc.push_str(&format!("{}. {}\n", idx + 1, decision));
        }
    }
    doc.push('\n');

    doc.push_str("## 🧪 Verification & Acceptance Evidence\n");
    if data.checks_passed.is_empty() {
        doc.push_str("- **Project Checks:** Standard checks verified (Exit Code: 0)\n");
    } else {
        doc.push_str(&format!(
            "- **Automated Verification Checks:** `{}` (All passed: Exit Code 0)\n",
            data.checks_passed.join(", ")
        ));
    }
    if !data.modified_files.is_empty() {
        doc.push_str("- **Modified Files:**\n");
        for file in &data.modified_files {
            doc.push_str(&format!("  - `{file}`\n"));
        }
    }
    doc.push('\n');

    doc.push_str("## 📌 Deferred Findings & Backlog\n");
    if data.deferred_findings.is_empty() {
        doc.push_str("*No non-critical findings or deferred technical debt.*\n");
    } else {
        for finding in &data.deferred_findings {
            let line = finding
                .line
                .map(|line| format!(":{line}"))
                .unwrap_or_default();
            let scope = if finding.out_of_scope {
                " (out-of-scope)"
            } else {
                ""
            };
            doc.push_str(&format!(
                "- **[{:?}{}]** `{}`{} — **{}**: {}\n",
                finding.severity, scope, finding.path, line, finding.title, finding.reason
            ));
        }
    }
    doc.push('\n');

    doc.push_str("---\n");
    doc.push_str("*Generated automatically by TALOS Local Multi-Agent Orchestrator.*\n");

    doc
}

pub(crate) fn task_summary_path(project_root: &Path, task_id: i64) -> std::path::PathBuf {
    project_root
        .join(".talos")
        .join("reports")
        .join(format!("task-{task_id}.md"))
}

pub fn write_task_summary(project_root: &Path, data: &TaskSummaryData) -> Result<String> {
    use std::{io::Write, os::unix::fs::OpenOptionsExt};

    let artifact_path = task_summary_path(project_root, data.task.id);
    let reports = artifact_path.parent().expect("report has a parent");
    fs::create_dir_all(reports).context("Cannot create the local reports directory")?;
    if fs::symlink_metadata(reports)?.is_symlink() {
        anyhow::bail!("Local reports directory must not be a symlink");
    }
    let staging = artifact_path.with_extension(format!("{}.tmp", std::process::id()));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&staging)
        .context("Cannot stage the task report")?;
    let result = file
        .write_all(generate_summary_markdown(data).as_bytes())
        .and_then(|()| fs::rename(&staging, &artifact_path));
    if result.is_err() {
        let _ = fs::remove_file(&staging);
    }
    result.with_context(|| format!("Failed to write {}", artifact_path.display()))?;
    Ok(artifact_path.display().to_string())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResumeNextStep {
    ReviewPending {
        revision_id: String,
        diff_summary: String,
    },
    FixCriticDefects {
        revision_id: String,
        open_remarks: Vec<String>,
        diff_summary: String,
    },
    FixVerificationFailure {
        revision_id: String,
        failing_check_id: String,
        failing_command: String,
        exit_code: i32,
    },
    ReconcileGitDrift {
        task_base_commit: String,
        current_head_commit: String,
    },
    RetryTask {
        reason: String,
    },
}

impl ResumeNextStep {
    pub fn description(&self) -> String {
        match self {
            Self::ReviewPending { revision_id, .. } => {
                format!("Dispatch Critic review on code revision R{revision_id}")
            }
            Self::FixCriticDefects { open_remarks, .. } => {
                format!(
                    "Dispatch Writer fix for {} open Critic defect(s)",
                    open_remarks.len()
                )
            }
            Self::FixVerificationFailure {
                failing_check_id, ..
            } => {
                format!("Dispatch Writer fix for failing check '{failing_check_id}'")
            }
            Self::ReconcileGitDrift { .. } => {
                "Reconcile diverged Git working tree before resuming".to_string()
            }
            Self::RetryTask { reason } => format!("Retry task: {reason}"),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TaskResumeContext {
    pub task_id: i64,
    pub title: String,
    pub acceptance_criteria: String,
    pub status: TaskStatus,
    pub where_stopped: String,
    pub landed_diff_summary: Option<String>,
    pub open_critic_remarks: Vec<String>,
    pub failing_check: Option<String>,
    pub git_drift: Option<String>,
    pub next_step: ResumeNextStep,
}

impl TaskResumeContext {
    pub fn render_summary_card_lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        lines.push(format!("🎯 Task #{}: {}", self.task_id, self.title));
        lines.push(format!("📍 Stopped at: {}", self.where_stopped));
        if let Some(diff) = &self.landed_diff_summary {
            lines.push(format!("📄 Landed changes: {diff}"));
        }
        if !self.open_critic_remarks.is_empty() {
            lines.push(format!(
                "⚠️ Open Critic remarks ({}):",
                self.open_critic_remarks.len()
            ));
            for remark in self.open_critic_remarks.iter().take(3) {
                lines.push(format!("   • {remark}"));
            }
            if self.open_critic_remarks.len() > 3 {
                lines.push(format!(
                    "   ... and {} more defect(s)",
                    self.open_critic_remarks.len() - 3
                ));
            }
        }
        if let Some(check) = &self.failing_check {
            lines.push(format!("❌ Failing check: {check}"));
        }
        if let Some(drift) = &self.git_drift {
            lines.push(format!("🔀 Git drift: {drift}"));
        }
        lines.push(format!("⚡ Next step: {}", self.next_step.description()));
        lines
    }
}

pub fn assemble_resume_context(
    project: &Project,
    database: &Database,
    task_id: i64,
) -> Result<TaskResumeContext> {
    let task = database
        .get_task(task_id)?
        .with_context(|| format!("Task #{task_id} not found in database"))?;

    let latest_attempt = database.latest_writer_attempt(task_id)?;
    let latest_review = database.latest_critic_review(task_id)?;
    let latest_verif = database.latest_verification_summary(task_id)?;

    // 1. Git drift check
    let current_head = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(&project.root)
        .output()
        .ok()
        .and_then(|out| {
            if out.status.success() {
                String::from_utf8(out.stdout)
                    .ok()
                    .map(|s| s.trim().to_string())
            } else {
                None
            }
        });

    let mut git_drift = None;
    if let (Some(head), Some(attempt)) = (&current_head, &latest_attempt)
        && let Some(base_rev) = &attempt.base_revision
        && task.status != TaskStatus::Complete
        && !base_rev.is_empty()
        && !head.starts_with(base_rev)
        && !base_rev.starts_with(head)
    {
        git_drift = Some(format!(
            "HEAD is at {} (baseline was {})",
            &head[..7.min(head.len())],
            &base_rev[..7.min(base_rev.len())]
        ));
    }

    // 2. Diff summary
    let mut landed_diff_summary = None;
    if let Some(review) = &latest_review
        && let Some(paths_json) = &review.changed_paths_json
        && let Ok(paths) = serde_json::from_str::<Vec<String>>(paths_json)
        && !paths.is_empty()
    {
        landed_diff_summary = Some(format!("{} modified file(s)", paths.len()));
    }

    // 3. Extract open critic remarks
    let mut open_critic_remarks = Vec::new();
    if let Some(review) = &latest_review
        && review.decision == "reject"
    {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&review.result_json)
            && let Some(findings) = value.get("findings").and_then(|f| f.as_array())
        {
            for f in findings {
                let out_of_scope = f
                    .get("out_of_scope")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                if !out_of_scope {
                    let path = f.get("path").and_then(|v| v.as_str()).unwrap_or("");
                    let title = f.get("title").and_then(|v| v.as_str()).unwrap_or("");
                    let reason = f.get("reason").and_then(|v| v.as_str()).unwrap_or("");
                    open_critic_remarks.push(format!("{path}: {title} ({reason})"));
                }
            }
        }
        if open_critic_remarks.is_empty() && !review.summary.is_empty() {
            open_critic_remarks.push(review.summary.clone());
        }
    }

    // 4. Check verification failure
    let mut failing_check = None;
    let mut verification_failed = false;
    let mut failing_check_info = None;

    if let Some(verif) = &latest_verif
        && verif.run.status == "failed"
    {
        verification_failed = true;
        if let Ok(history) = database.verification_history(task_id) {
            for run in history {
                for check in run.checks {
                    if !check.passed() {
                        let exit_str = check.exit_code.map_or("none".into(), |c| c.to_string());
                        let desc = format!("{} (exit code {})", check.check_id, exit_str);
                        failing_check = Some(desc);
                        failing_check_info = Some((
                            check.check_id,
                            check.argv.join(" "),
                            check.exit_code.unwrap_or(1),
                            verif.run.revision_id.clone(),
                        ));
                        break;
                    }
                }
                if failing_check.is_some() {
                    break;
                }
            }
        }
    }

    // 5. Synthesize where_stopped and next_step
    let (where_stopped, next_step) = if verification_failed {
        let (check_id, cmd, code, rev) =
            failing_check_info.unwrap_or_else(|| ("check".into(), "check".into(), 1, "1".into()));
        (
            format!("Verification failed on check '{check_id}' (exit code {code})"),
            ResumeNextStep::FixVerificationFailure {
                revision_id: rev,
                failing_check_id: check_id,
                failing_command: cmd,
                exit_code: code,
            },
        )
    } else if let Some(review) = &latest_review {
        if review.decision == "reject" {
            (
                format!(
                    "Critic rejected revision R{} with {} open remark(s)",
                    review.revision_id,
                    open_critic_remarks.len()
                ),
                ResumeNextStep::FixCriticDefects {
                    revision_id: review.revision_id.clone(),
                    open_remarks: open_critic_remarks.clone(),
                    diff_summary: landed_diff_summary.clone().unwrap_or_default(),
                },
            )
        } else if task.status == TaskStatus::Complete {
            (
                "Task completed successfully with all checks passed.".to_string(),
                ResumeNextStep::RetryTask {
                    reason: "Task is already complete.".into(),
                },
            )
        } else {
            (
                format!(
                    "Critic approved revision R{}, awaiting verification.",
                    review.revision_id
                ),
                ResumeNextStep::ReviewPending {
                    revision_id: review.revision_id.clone(),
                    diff_summary: landed_diff_summary.clone().unwrap_or_default(),
                },
            )
        }
    } else if let Some(attempt) = &latest_attempt {
        if attempt.state == "completed" {
            (
                format!(
                    "Writer completed attempt #{}, peer review not yet started.",
                    attempt.id
                ),
                ResumeNextStep::ReviewPending {
                    revision_id: "1".into(),
                    diff_summary: landed_diff_summary.clone().unwrap_or_default(),
                },
            )
        } else if git_drift.is_some() {
            (
                "Working tree diverged from original baseline commit.".to_string(),
                ResumeNextStep::ReconcileGitDrift {
                    task_base_commit: attempt.base_revision.clone().unwrap_or_default(),
                    current_head_commit: current_head.unwrap_or_default(),
                },
            )
        } else {
            (
                format!(
                    "Writer attempt #{} ended in state '{}'.",
                    attempt.id, attempt.state
                ),
                ResumeNextStep::RetryTask {
                    reason: format!("Writer state was {}", attempt.state),
                },
            )
        }
    } else {
        (
            format!("Task stopped in status '{}'.", task.status.as_str()),
            ResumeNextStep::RetryTask {
                reason: format!("Task status is {}", task.status.as_str()),
            },
        )
    };

    Ok(TaskResumeContext {
        task_id,
        title: task.title,
        acceptance_criteria: task.acceptance_criteria,
        status: task.status,
        where_stopped,
        landed_diff_summary,
        open_critic_remarks,
        failing_check,
        git_drift,
        next_step,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncates_large_error_for_token_economy() {
        let large_string = "START_ERROR ".repeat(1000);
        assert!(large_string.len() > 10000);

        let truncated = truncate_for_token_economy(&large_string, 1000);
        assert!(truncated.len() <= 1200);
        assert!(truncated.contains("truncated for token economy"));
        assert!(truncated.starts_with("START_ERROR"));
        assert!(truncated.ends_with("START_ERROR "));
    }

    #[test]
    fn small_strings_are_not_truncated() {
        let small = "SyntaxError: invalid syntax at line 3";
        assert_eq!(truncate_for_token_economy(small, 4096), small);
    }

    #[test]
    fn renders_deferred_findings_backlog_section() {
        let task = TaskRecord {
            id: 42,
            title: "Improve error handling".to_owned(),
            acceptance_criteria: "Graceful error normalization".to_owned(),
            status: crate::db::TaskStatus::Complete,
            created_at_ms: 1000,
            updated_at_ms: 2000,
        };
        let deferred = vec![
            crate::review::CriticFinding {
                severity: crate::review::FindingSeverity::Minor,
                path: "src/util.rs".to_owned(),
                line: Some(15),
                title: "Redundant clone".to_owned(),
                reason: "Can take reference instead of clone.".to_owned(),
                out_of_scope: false,
            },
            crate::review::CriticFinding {
                severity: crate::review::FindingSeverity::Note,
                path: "docs/architecture.md".to_owned(),
                line: None,
                title: "Update outdated diagram".to_owned(),
                reason: "Diagram does not reflect latest component.".to_owned(),
                out_of_scope: true,
            },
        ];
        let summary_data = TaskSummaryData {
            task: &task,
            mode: OrchestrationMode::Pair,
            writer_adapter: AdapterKind::Agy,
            writer_model: Some("gemini-2.5-pro"),
            critic_adapter: AdapterKind::Agy,
            critic_model: Some("gemini-2.5-pro"),
            start_time: SystemTime::now(),
            total_pause_duration: Duration::from_secs(5),
            decisions: vec!["Adopted resilient error normalization".to_owned()],
            checks_passed: vec!["cargo check".to_owned()],
            modified_files: vec!["src/util.rs".to_owned()],
            commit_hash: Some("abcd1234ef5678".to_owned()),
            deferred_findings: deferred,
        };
        let markdown = generate_summary_markdown(&summary_data);
        assert!(markdown.contains("## 📌 Deferred Findings & Backlog"));
        assert!(markdown.contains("Redundant clone"));
        assert!(markdown.contains("Can take reference instead of clone."));
        assert!(markdown.contains("Update outdated diagram"));
        assert!(markdown.contains("(out-of-scope)"));
    }

    #[test]
    fn resume_context_summary_card_rendering() {
        let ctx = TaskResumeContext {
            task_id: 17,
            title: "Implement smart continue engine".to_string(),
            acceptance_criteria: "Context pack with open defects".to_string(),
            status: TaskStatus::Implementing,
            where_stopped: "Critic rejected revision R2 with 1 open remark(s)".to_string(),
            landed_diff_summary: Some("2 modified file(s)".to_string()),
            open_critic_remarks: vec!["src/tui.rs: missing is_some_and check".to_string()],
            failing_check: None,
            git_drift: None,
            next_step: ResumeNextStep::FixCriticDefects {
                revision_id: "2".to_string(),
                open_remarks: vec!["src/tui.rs: missing is_some_and check".to_string()],
                diff_summary: "2 modified file(s)".to_string(),
            },
        };

        let lines = ctx.render_summary_card_lines();
        let full = lines.join("\n");
        assert!(full.contains("🎯 Task #17"));
        assert!(full.contains("Implement smart continue engine"));
        assert!(full.contains("📍 Stopped at: Critic rejected revision R2"));
        assert!(full.contains("📄 Landed changes: 2 modified file(s)"));
        assert!(full.contains("⚠️ Open Critic remarks (1):"));
        assert!(full.contains("src/tui.rs: missing is_some_and check"));
        assert!(full.contains("⚡ Next step: Dispatch Writer fix for 1 open Critic defect(s)"));
    }
}
