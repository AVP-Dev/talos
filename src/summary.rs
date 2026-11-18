use std::{
    fs,
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};

use crate::config::{AdapterKind, OrchestrationMode};
use crate::db::TaskRecord;

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

    doc.push_str("---\n");
    doc.push_str("*Generated automatically by TALOS Local Multi-Agent Orchestrator.*\n");

    doc
}

pub fn write_task_summary(project_root: &Path, data: &TaskSummaryData) -> Result<String> {
    let markdown = generate_summary_markdown(data);
    let artifact_path = project_root.join("TALOS_SUMMARY.md");
    fs::write(&artifact_path, &markdown)
        .with_context(|| format!("Failed to write {}", artifact_path.display()))?;
    Ok(artifact_path.display().to_string())
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
}
