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
                "Critic review input exceeds the {}-byte prompt budget; TALOS will not truncate project context. Consider splitting changes across smaller tasks.",
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
pub struct CriticFinding {
    pub severity: FindingSeverity,
    pub path: String,
    pub line: Option<u32>,
    pub title: String,
    pub reason: String,
    #[serde(default)]
    pub out_of_scope: bool,
}

impl CriticFinding {
    pub fn is_blocking(&self) -> bool {
        !self.out_of_scope && self.severity.is_blocking()
    }

    pub fn title_preview(&self, max_bytes: usize) -> &str {
        if self.title.len() <= max_bytes {
            &self.title
        } else {
            &self.title[..self.title.floor_char_boundary(max_bytes)]
        }
    }

    pub fn reason_preview(&self, max_bytes: usize) -> &str {
        if self.reason.len() <= max_bytes {
            &self.reason
        } else {
            &self.reason[..self.reason.floor_char_boundary(max_bytes)]
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
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
        let json_str = extract_json(response);
        let raw_val: serde_json::Value = serde_json::from_str(json_str)
            .context("Critic output is not a single valid JSON review object")?;
        if raw_val.get("patch").is_some() {
            bail!("the findings-only Critic contract must reject patch payloads");
        }
        let mut parsed: Self = serde_json::from_value(raw_val)
            .context("Critic output is not a valid review structure")?;
        parsed.validate(expected_revision_id, changed_paths)?;
        Ok(parsed)
    }

    pub fn validate(&mut self, expected_revision_id: &str, changed_paths: &[String]) -> Result<()> {
        if self.schema_version != 1 {
            bail!(
                "Critic output uses unsupported schema version {}",
                self.schema_version
            );
        }
        if self.revision_id != expected_revision_id {
            bail!("Critic output targets a different revision; it cannot approve this task");
        }
        if self.summary.trim().is_empty() {
            bail!("Critic summary must be non-empty");
        }
        if self.findings.len() > 100 {
            bail!("Critic output contains more than 100 findings");
        }
        let mut blocking = false;
        for finding in &mut self.findings {
            validate_and_normalize_finding(finding, changed_paths)?;
            blocking |= finding.is_blocking();
        }
        match self.decision {
            ReviewDecision::Approve if blocking => {
                bail!("Critic cannot approve while blocker or major findings remain");
            }
            ReviewDecision::RequestChanges if !blocking => {
                // Gracefully treat as approval with remarks or downgrade instead of bailing
                self.decision = ReviewDecision::Approve;
            }
            _ => {}
        }
        Ok(())
    }

    pub fn canonical_json(&self) -> Result<String> {
        serde_json::to_string(self).context("Cannot serialize validated Critic result")
    }

    pub fn summary_preview(&self, max_bytes: usize) -> &str {
        if self.summary.len() <= max_bytes {
            &self.summary
        } else {
            &self.summary[..self.summary.floor_char_boundary(max_bytes)]
        }
    }
}

fn validate_and_normalize_finding(
    finding: &mut CriticFinding,
    changed_paths: &[String],
) -> Result<()> {
    let path = Path::new(&finding.path);
    if finding.path.is_empty()
        || finding.path.len() > 1024
        || finding.path.chars().any(char::is_control)
        || path.is_absolute()
        || path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
        || finding.path.starts_with(".talos/")
        || finding.path == ".talos"
        || finding.path.starts_with(".git/")
        || finding.path == ".git"
    {
        bail!("Critic finding path is not a safe project-relative path");
    }
    if !changed_paths.iter().any(|changed| changed == &finding.path) {
        finding.out_of_scope = true;
    }
    if finding.line == Some(0) {
        finding.line = None;
    }
    if finding.title.trim().is_empty() {
        bail!("Critic finding title must be non-empty");
    }
    if finding.reason.trim().is_empty() {
        bail!("Critic finding reason must be non-empty");
    }
    Ok(())
}

fn extract_json(raw: &str) -> &str {
    let trimmed = raw.trim();
    if let (Some(start), Some(end)) = (trimmed.find('{'), trimmed.rfind('}'))
        && end >= start
    {
        return &trimmed[start..=end];
    }
    trimmed
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

    #[test]
    fn critic_patch_fields_are_rejected_instead_of_being_applied() {
        let response = format!(
            r#"{{"schema_version":1,"revision_id":"{}","decision":"approve","summary":"Approved.","findings":[],"patch":"diff --git a/a b/a"}}"#,
            revision()
        );
        assert!(
            CriticResponse::parse_and_validate(&response, &revision(), &[]).is_err(),
            "the findings-only Critic contract must reject patch payloads"
        );
    }

    #[test]
    fn normalizes_line_zero_to_none() {
        let paths = vec!["src/lib.rs".to_owned()];
        let json = format!(
            r#"{{"schema_version":1,"revision_id":"{}","decision":"approve","summary":"Approved.","findings":[{{"severity":"minor","path":"src/lib.rs","line":0,"title":"Minor style remark","reason":"Trailing space."}}]}}"#,
            revision()
        );
        let parsed = CriticResponse::parse_and_validate(&json, &revision(), &paths)
            .expect("line 0 should be normalized to None rather than bailing");
        assert_eq!(parsed.findings[0].line, None);
    }

    #[test]
    fn preserves_thorough_explanation_without_bailing() {
        let paths = vec!["src/lib.rs".to_owned()];
        let long_reason = "Explanation section with technical details. ".repeat(400); // ~18KB > 8000 bytes
        let long_summary = "Detailed summary of findings. ".repeat(200); // ~6KB > 4000 bytes
        let long_title = "A".repeat(600); // 600 bytes > 500 bytes
        let json = format!(
            r#"{{"schema_version":1,"revision_id":"{}","decision":"approve","summary":{},"findings":[{{"severity":"minor","path":"src/lib.rs","line":1,"title":{},"reason":{}}}]}}"#,
            revision(),
            serde_json::to_string(&long_summary).unwrap(),
            serde_json::to_string(&long_title).unwrap(),
            serde_json::to_string(&long_reason).unwrap(),
        );
        let parsed = CriticResponse::parse_and_validate(&json, &revision(), &paths)
            .expect("thorough explanation and summary must be preserved without bailing");
        assert_eq!(parsed.summary, long_summary);
        assert_eq!(parsed.findings[0].title, long_title);
        assert_eq!(parsed.findings[0].reason, long_reason);
        assert_eq!(parsed.findings[0].reason_preview(50).len(), 50);
        assert_eq!(parsed.summary_preview(50).len(), 50);
    }

    #[test]
    fn accepts_out_of_scope_path_without_bailing() {
        let paths = vec!["src/lib.rs".to_owned()];
        let json = format!(
            r#"{{"schema_version":1,"revision_id":"{}","decision":"request_changes","summary":"Needs review.","findings":[{{"severity":"minor","path":"src/other.rs","line":42,"title":"Deferred debt","reason":"Found in untouched file."}}]}}"#,
            revision()
        );
        let parsed = CriticResponse::parse_and_validate(&json, &revision(), &paths)
            .expect("valid project-relative path outside changed paths should not bail");
        assert!(parsed.findings[0].out_of_scope);
        assert!(!parsed.findings[0].is_blocking());
        // With only non-blocking out-of-scope finding, request_changes is downgraded to Approve
        assert_eq!(parsed.decision, ReviewDecision::Approve);
    }

    #[test]
    fn accepts_unknown_fields_in_critic_response_and_findings() {
        let paths = vec!["src/lib.rs".to_owned()];
        let json = format!(
            r#"{{"schema_version":1,"revision_id":"{}","decision":"approve","summary":"Approved.","extra_meta":"llm_noise","findings":[{{"severity":"minor","path":"src/lib.rs","line":10,"title":"Clean code","reason":"Ok","confidence":0.95}}]}}"#,
            revision()
        );
        let parsed = CriticResponse::parse_and_validate(&json, &revision(), &paths)
            .expect("minor schema variations with unknown fields must not fail parsing");
        assert_eq!(parsed.decision, ReviewDecision::Approve);
        assert_eq!(parsed.findings.len(), 1);
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
