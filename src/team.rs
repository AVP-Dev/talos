use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::{adapter::AdapterId, config::DelegationMode, db::TaskRecord};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DelegationKind {
    Native,
    External,
    Unsupported,
}

impl DelegationKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::External => "external",
            Self::Unsupported => "unsupported",
        }
    }
}

pub fn detect_delegation(_adapter: AdapterId, mode: DelegationMode) -> DelegationKind {
    match mode {
        DelegationMode::External => DelegationKind::External,
        DelegationMode::Native => {
            // Check if native programmatic subagent delegation is verified for this adapter.
            // Under strict TALOS boundaries, advertised help flags alone do not prove native delegation.
            // When unconfirmed or unsafe, native mode reports unsupported so TALOS or caller can fall back.
            DelegationKind::Unsupported
        }
        DelegationMode::Hybrid => {
            // In hybrid mode: check if native is confirmed; if not, safely fallback to external.
            // All 7 TALOS CLI adapters support managed external execution.
            match detect_delegation(_adapter, DelegationMode::Native) {
                DelegationKind::Native => DelegationKind::Native,
                _ => DelegationKind::External,
            }
        }
    }
}

fn deserialize_string_or_array<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StringOrVec {
        Str(String),
        Vec(Vec<String>),
    }

    match StringOrVec::deserialize(deserializer)? {
        StringOrVec::Str(s) => Ok(s),
        StringOrVec::Vec(v) => Ok(v.join("\n")),
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubtaskPlan {
    pub id: String,
    pub title: String,
    pub allowed_paths: Vec<String>,
    #[serde(deserialize_with = "deserialize_string_or_array")]
    pub acceptance_criteria: String,
    #[serde(default, deserialize_with = "deserialize_string_or_array")]
    pub instructions: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WriterLeadPlan {
    pub action: String, // "self" or "delegate"
    pub reasoning: String,
    #[serde(default)]
    pub subtasks: Vec<SubtaskPlan>,
}

pub fn build_writer_lead_plan_prompt(task: &TaskRecord, max_workers: usize) -> String {
    let criteria = if task.acceptance_criteria.trim().is_empty() {
        "Satisfy all implied requirements and tests."
    } else {
        task.acceptance_criteria.trim()
    };
    format!(
        "You are the TALOS Writer Lead for this engineering task.\n\
         You lead the engineering implementation team.\n\
         Analyze the task requirements and choose an implementation strategy:\n\
         Option A: Execute the task directly yourself (action: \"self\") if the task is simple, tightly coupled, single-file, or does not benefit from subagents.\n\
         Option B: Decompose the task (action: \"delegate\") into up to {max_workers} independent, non-overlapping subtasks for Writer Workers if there are clearly separated files/modules. If the task involves multiple distinct files or independent modules, prefer Option B (\"delegate\") to maximize parallel engineering across up to {max_workers} workers.\n\
         For each subtask, you must provide:\n\
         - \"id\": short unique string (e.g. \"worker_1\")\n\
         - \"title\": concise description\n\
         - \"allowed_paths\": array of project-relative paths the worker is exclusively permitted to touch\n\
         - \"acceptance_criteria\": clear acceptance criteria\n\
         - \"instructions\": technical implementation guidance\n\n\
         Task ID: #{}\n\
         Task Title: {}\n\
         Acceptance Criteria: {}\n\n\
         Return EXACTLY one JSON object matching this schema (no markdown fences, no text before or after):\n\
         {{\"action\": \"self\" | \"delegate\", \"reasoning\": \"...\", \"subtasks\": [...]}}",
        task.id, task.title, criteria
    )
}

pub fn parse_writer_lead_plan(response: &str) -> Result<WriterLeadPlan> {
    let clean = extract_json_block(response);
    let plan: WriterLeadPlan = serde_json::from_str(&clean).with_context(|| {
        format!("Failed to parse Writer Lead plan JSON from response: {response}")
    })?;
    Ok(plan)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerHelpDecision {
    pub action: String, // "retry_with_guidance", "takeover", "block"
    #[serde(default)]
    pub guidance: Option<String>,
    pub reason: String,
}

pub fn build_worker_help_prompt(
    parent_task: &TaskRecord,
    subtask_id: &str,
    subtask_title: &str,
    allowed_paths: &[String],
    error_message: &str,
    attempt: usize,
    max_attempts: usize,
) -> String {
    format!(
        "You are the TALOS Writer Lead. One of your Writer Workers encountered a failure while executing a subtask.\n\
         Parent Task: #{}: {}\n\
         Subtask: {}: {}\n\
         Allowed Paths: {:?}\n\
         Failure Details: {}\n\
         Help Attempt: {} of {}\n\n\
         Analyze the issue and decide on the remedy:\n\
         Option 1: \"retry_with_guidance\" - Provide refined instructions/context to retry the worker.\n\
         Option 2: \"takeover\" - You (Writer Lead) will take over and implement the fix directly.\n\
         Option 3: \"block\" - The subtask cannot safely proceed.\n\n\
         Return EXACTLY one JSON object:\n\
         {{\"action\": \"retry_with_guidance\" | \"takeover\" | \"block\", \"guidance\": \"...\", \"reason\": \"...\"}}",
        parent_task.id,
        parent_task.title,
        subtask_id,
        subtask_title,
        allowed_paths,
        error_message,
        attempt,
        max_attempts
    )
}

pub fn parse_worker_help_decision(response: &str) -> Result<WorkerHelpDecision> {
    let clean = extract_json_block(response);
    let decision: WorkerHelpDecision = serde_json::from_str(&clean).with_context(|| {
        format!("Failed to parse WorkerHelpDecision JSON from response: {response}")
    })?;
    Ok(decision)
}

pub fn build_critic_worker_prompt(task: &TaskRecord, diff: &str, focus: &str) -> String {
    let criteria = if task.acceptance_criteria.trim().is_empty() {
        "Satisfy all implied requirements and tests."
    } else {
        task.acceptance_criteria.trim()
    };
    format!(
        "You are a TALOS Critic Worker performing a specialized independent review.\n\
         Focus Area: {focus}\n\
         Task #{}: {}\n\
         Acceptance Criteria: {}\n\
         Git Diff to inspect:\n{diff}\n\n\
         Report your findings focusing strictly on {focus}.\n\
         Return EXACTLY one JSON object with schema_version=1, decision (\"approve\" | \"request_changes\"), summary, and findings array:\n\
         {{\"schema_version\": 1, \"decision\": \"approve\"|\"request_changes\", \"summary\": \"...\", \"findings\": [{{ \"severity\": \"blocker\"|\"major\"|\"minor\"|\"note\", \"path\": \"...\", \"line\": null, \"title\": \"...\", \"reason\": \"...\" }}]}}",
        task.id, task.title, criteria
    )
}

pub fn build_critic_lead_review_prompt(
    task: &TaskRecord,
    plan_summary: &str,
    diff: &str,
    worker_findings: Option<&str>,
    revision_id: &str,
) -> String {
    let worker_section = match worker_findings {
        Some(findings) => format!("\nSPECIALIZED CRITIC WORKER FINDINGS:\n{findings}\n"),
        None => String::new(),
    };
    let criteria = if task.acceptance_criteria.trim().is_empty() {
        "Satisfy all implied requirements and tests."
    } else {
        task.acceptance_criteria.trim()
    };
    format!(
        "You are the independent TALOS Critic Lead. You provide the consolidated review and verdict.\n\
         You are read-only: do not edit files, run commands, or call external services.\n\
         Check the change against the task acceptance criteria and evaluate the technical justification.\n\
         Task #{}: {}\n\
         Acceptance Criteria: {}\n\
         Writer Team Technical Rationale:\n{plan_summary}\n\
         {worker_section}\
         Git Diff:\n{diff}\n\n\
         Return EXACTLY one JSON object with schema_version=1, revision_id=\"{revision_id}\", decision=\"approve\" or \"request_changes\", summary, and findings array:\n\
         {{\"schema_version\": 1, \"revision_id\": \"{revision_id}\", \"decision\": \"approve\" | \"request_changes\", \"summary\": \"...\", \"findings\": [...]}}",
        task.id, task.title, criteria
    )
}

pub fn build_writer_lead_rebuttal_prompt(
    task: &TaskRecord,
    critic_findings: &str,
    round: usize,
    max_rounds: usize,
) -> String {
    format!(
        "You are the TALOS Writer Lead. The Critic Lead reviewed your team's changes and requested adjustments.\n\
         Task #{}: {}\n\
         Critic Review Findings (Round {round} of {max_rounds}):\n{critic_findings}\n\n\
         Analyze the findings. You may either:\n\
         1) Implement the corrections directly to address the concerns.\n\
         2) Provide a clear technical justification/explanation for why the implementation is sound, accompanied by any necessary adjustments.\n\
         Inspect the files, make any required edits, and summarize your response concisely in English.",
        task.id, task.title
    )
}

fn extract_json_block(text: &str) -> String {
    let trimmed = text.trim();
    if let Some(start) = trimmed.find('{')
        && let Some(end) = trimmed.rfind('}')
        && end >= start
    {
        return trimmed[start..=end].to_string();
    }
    trimmed.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_writer_lead_plan_with_array_criteria() {
        let json = r#"{
            "action": "delegate",
            "reasoning": "Separate modules",
            "subtasks": [
                {
                    "id": "worker_calc",
                    "title": "Implement calc",
                    "allowed_paths": ["calc.py"],
                    "acceptance_criteria": ["add and sub work", "all tests pass"],
                    "instructions": "write calc.py"
                }
            ]
        }"#;
        let plan = parse_writer_lead_plan(json).expect("parse plan");
        assert_eq!(plan.action, "delegate");
        assert_eq!(
            plan.subtasks[0].acceptance_criteria,
            "add and sub work\nall tests pass"
        );
    }

    #[test]
    fn parses_valid_writer_lead_plan() {
        let json = r#"{
            "action": "delegate",
            "reasoning": "Task involves two distinct modules: parser and evaluator",
            "subtasks": [
                {
                    "id": "subtask_1",
                    "title": "Implement parser",
                    "allowed_paths": ["src/parser.rs"],
                    "acceptance_criteria": "Parser parses tokens",
                    "instructions": "Write parser function"
                },
                {
                    "id": "subtask_2",
                    "title": "Implement evaluator",
                    "allowed_paths": ["src/eval.rs"],
                    "acceptance_criteria": "Evaluator computes expressions",
                    "instructions": "Write eval function"
                }
            ]
        }"#;
        let plan = parse_writer_lead_plan(json).expect("parse plan");
        assert_eq!(plan.action, "delegate");
        assert_eq!(plan.subtasks.len(), 2);
        assert_eq!(plan.subtasks[0].id, "subtask_1");
        assert_eq!(plan.subtasks[1].allowed_paths, vec!["src/eval.rs"]);
    }

    #[test]
    fn parses_self_execution_plan() {
        let json = r#"{
            "action": "self",
            "reasoning": "Single file change, no delegation needed",
            "subtasks": []
        }"#;
        let plan = parse_writer_lead_plan(json).expect("parse self plan");
        assert_eq!(plan.action, "self");
        assert!(plan.subtasks.is_empty());
    }

    #[test]
    fn parses_worker_help_decision() {
        let json = r#"{
            "action": "retry_with_guidance",
            "guidance": "Focus only on edge cases in token validation",
            "reason": "Worker missed empty input case"
        }"#;
        let decision = parse_worker_help_decision(json).expect("parse decision");
        assert_eq!(decision.action, "retry_with_guidance");
        assert_eq!(
            decision.guidance.as_deref(),
            Some("Focus only on edge cases in token validation")
        );
    }

    #[test]
    fn detect_delegation_hybrid_falls_back_to_external() {
        let kind = detect_delegation(AdapterId::Codex, DelegationMode::Hybrid);
        assert_eq!(kind, DelegationKind::External);

        let native = detect_delegation(AdapterId::Codex, DelegationMode::Native);
        assert_eq!(native, DelegationKind::Unsupported);

        let external = detect_delegation(AdapterId::Codex, DelegationMode::External);
        assert_eq!(external, DelegationKind::External);
    }
}
