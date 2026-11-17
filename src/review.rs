use std::path::{Component, Path};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::db::TaskRecord;

const MAX_REVIEW_PROMPT_BYTES: usize = 96 * 1024;

#[derive(Clone, Debug, Serialize)]
pub struct ReviewRequest<'a> {
    pub schema_version: u32,
    pub task_id: i64,
    pub writer_attempt_id: i64,
    pub task: &'a str,
    pub acceptance_criteria: &'a str,
    pub base_commit: &'a str,
    pub revision_id: &'a str,
    pub changed_paths: &'a [String],
    pub patch: &'a str,
}

impl<'a> ReviewRequest<'a> {
    pub fn new(
        task: &'a TaskRecord,
        writer_attempt_id: i64,
        base_commit: &'a str,
        revision_id: &'a str,
        changed_paths: &'a [String],
        patch: &'a str,
    ) -> Self {
        Self {
            schema_version: 1,
            task_id: task.id,
            writer_attempt_id,
            task: &task.title,
            acceptance_criteria: &task.acceptance_criteria,
            base_commit,
            revision_id,
            changed_paths,
            patch,
        }
    }

    pub fn prompt(&self) -> Result<String> {
        let request =
            serde_json::to_string_pretty(self).context("Cannot serialize Critic input")?;
        let prompt = format!(
            "You are the independent TALOS Critic. Review only the exact revision and task input below.\n\
             You are read-only: do not edit files, run commands, call external services, or use tools other than reading project files.\n\
             Check the change against the acceptance criteria. Report only actionable findings in changed paths.\n\
             Return exactly one JSON object with schema_version=1, the same revision_id, decision=\"approve\" or \"request_changes\", a concise summary, and a findings array.\n\
             Each finding must have severity (blocker|major|minor|note), path, line (positive integer or null), title, and reason.\n\
             Use request_changes if any blocker or major issue exists. Use approve only when none exists. Do not claim tests ran. Do not include markdown fences or prose outside the JSON object.\n\n\
             REVIEW INPUT:\n{request}"
        );
        if prompt.len() > MAX_REVIEW_PROMPT_BYTES {
            bail!(
                "Critic review input exceeds the {}-byte prompt budget; TALOS will not truncate project context",
                MAX_REVIEW_PROMPT_BYTES
            );
        }
        Ok(prompt)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReviewDecision {
    Approve,
    RequestChanges,
}

impl ReviewDecision {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Approve => "approve",
            Self::RequestChanges => "request_changes",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FindingSeverity {
    Blocker,
    Major,
    Minor,
    Note,
}

impl FindingSeverity {
    fn is_blocking(self) -> bool {
        matches!(self, Self::Blocker | Self::Major)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CriticFinding {
    pub severity: FindingSeverity,
    pub path: String,
    pub line: Option<u32>,
    pub title: String,
    pub reason: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CriticResponse {
    pub schema_version: u32,
    pub revision_id: String,
    pub decision: ReviewDecision,
    pub summary: String,
    pub findings: Vec<CriticFinding>,
}

impl CriticResponse {
    pub fn parse_and_validate(
        response: &str,
        expected_revision_id: &str,
        changed_paths: &[String],
    ) -> Result<Self> {
        let parsed: Self = serde_json::from_str(response)
            .context("Critic output is not a single valid JSON review object")?;
        parsed.validate(expected_revision_id, changed_paths)?;
        Ok(parsed)
    }

    pub fn validate(&self, expected_revision_id: &str, changed_paths: &[String]) -> Result<()> {
        if self.schema_version != 1 {
            bail!(
                "Critic output uses unsupported schema version {}",
                self.schema_version
            );
        }
        if self.revision_id != expected_revision_id {
            bail!("Critic output targets a different revision; it cannot approve this task");
        }
        if self.summary.trim().is_empty() || self.summary.len() > 4_000 {
            bail!("Critic summary must be non-empty and at most 4000 bytes");
        }
        if self.findings.len() > 100 {
            bail!("Critic output contains more than 100 findings");
        }
        let mut blocking = false;
        for finding in &self.findings {
            validate_finding(finding, changed_paths)?;
            blocking |= finding.severity.is_blocking();
        }
        match self.decision {
            ReviewDecision::Approve if blocking => {
                bail!("Critic cannot approve while blocker or major findings remain");
            }
            ReviewDecision::RequestChanges if !blocking => {
                bail!("Critic request_changes requires at least one blocker or major finding");
            }
            _ => {}
        }
        Ok(())
    }

    pub fn canonical_json(&self) -> Result<String> {
        serde_json::to_string(self).context("Cannot serialize validated Critic result")
    }
}

fn validate_finding(finding: &CriticFinding, changed_paths: &[String]) -> Result<()> {
    let path = Path::new(&finding.path);
    if finding.path.is_empty()
        || finding.path.len() > 1024
        || finding.path.chars().any(char::is_control)
        || path.is_absolute()
        || path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
        || !changed_paths.iter().any(|changed| changed == &finding.path)
    {
        bail!("Critic finding path must be a changed project-relative path");
    }
    if finding.line == Some(0) {
        bail!("Critic finding line numbers must be positive");
    }
    if finding.title.trim().is_empty() || finding.title.len() > 500 {
        bail!("Critic finding title must be non-empty and at most 500 bytes");
    }
    if finding.reason.trim().is_empty() || finding.reason.len() > 8_000 {
        bail!("Critic finding reason must be non-empty and at most 8000 bytes");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{CriticResponse, ReviewDecision};

    #[test]
    fn accepts_only_a_valid_approval_for_the_exact_revision() {
        let paths = vec!["src/lib.rs".to_owned()];
        let result = CriticResponse::parse_and_validate(
            r#"{"schema_version":1,"revision_id":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","decision":"approve","summary":"The change meets the criteria.","findings":[]}"#,
            &revision(),
            &paths,
        )
        .expect("valid approval");
        assert_eq!(result.decision, ReviewDecision::Approve);
    }

    #[test]
    fn rejects_mismatched_revision_and_blocking_approval() {
        let paths = vec!["src/lib.rs".to_owned()];
        let mismatch = response(
            "request_changes",
            "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            true,
        );
        assert!(CriticResponse::parse_and_validate(&mismatch, &revision(), &paths).is_err());

        let blocked_approval = response("approve", &revision(), true);
        assert!(
            CriticResponse::parse_and_validate(&blocked_approval, &revision(), &paths).is_err()
        );
    }

    #[test]
    fn accepts_a_blocking_request_for_changes() {
        let paths = vec!["src/lib.rs".to_owned()];
        let result = CriticResponse::parse_and_validate(
            &response("request_changes", &revision(), true),
            &revision(),
            &paths,
        )
        .expect("valid blocking request for changes");
        assert_eq!(result.decision, ReviewDecision::RequestChanges);
        assert_eq!(result.findings.len(), 1);
    }

    #[test]
    fn rejects_unchanged_or_escaping_finding_paths() {
        let paths = vec!["src/lib.rs".to_owned()];
        let mut value: serde_json::Value =
            serde_json::from_str(&response("request_changes", &revision(), true))
                .expect("parse fixture");
        value["findings"][0]["path"] = serde_json::Value::String("../secret".to_owned());
        assert!(
            CriticResponse::parse_and_validate(&value.to_string(), &revision(), &paths).is_err()
        );
    }

    fn revision() -> String {
        "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned()
    }

    fn response(decision: &str, revision: &str, blocking: bool) -> String {
        let severity = if blocking { "major" } else { "minor" };
        let findings = if decision == "request_changes" || blocking {
            format!(
                r#"[{{"severity":"{severity}","path":"src/lib.rs","line":7,"title":"Handle empty input","reason":"The new branch still returns an error for an empty input."}}]"#
            )
        } else {
            "[]".to_owned()
        };
        format!(
            r#"{{"schema_version":1,"revision_id":"{revision}","decision":"{decision}","summary":"Review summary.","findings":{findings}}}"#
        )
    }
}
