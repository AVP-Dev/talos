use std::{fs, path::Path};

use anyhow::{Context, Result, bail};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};

pub use crate::adapter::AdapterId as AdapterKind;

use crate::{
    checks::{ProjectCheck, validate_project_checks},
    project::Project,
};

const CONFIG_FILE_NAME: &str = "config.json";
const MAX_CONFIG_BYTES: u64 = 16 * 1024;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrchestrationMode {
    #[default]
    Pair,
    Team,
}

impl OrchestrationMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pair => "pair",
            Self::Team => "team",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DelegationMode {
    #[default]
    Hybrid,
    Native,
    External,
}

impl DelegationMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Hybrid => "hybrid",
            Self::Native => "native",
            Self::External => "external",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrchestrationConfig {
    #[serde(default)]
    pub mode: OrchestrationMode,
    #[serde(default)]
    pub delegation: DelegationMode,
    #[serde(default = "default_max_writer_workers")]
    pub max_writer_workers: usize,
    #[serde(default = "default_max_critic_workers")]
    pub max_critic_workers: usize,
    #[serde(default = "default_max_review_rounds")]
    pub max_review_rounds: usize,
    #[serde(default = "default_max_help_attempts")]
    pub max_help_attempts: usize,
}

fn default_max_writer_workers() -> usize {
    2
}

fn default_max_critic_workers() -> usize {
    1
}

fn default_max_review_rounds() -> usize {
    3
}

fn default_max_help_attempts() -> usize {
    2
}

impl Default for OrchestrationConfig {
    fn default() -> Self {
        Self {
            mode: OrchestrationMode::Pair,
            delegation: DelegationMode::Hybrid,
            max_writer_workers: 2,
            max_critic_workers: 1,
            max_review_rounds: 3,
            max_help_attempts: 2,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentRole {
    Writer,
    Critic,
}

impl AgentRole {
    fn as_str(self) -> &'static str {
        match self {
            Self::Writer => "writer",
            Self::Critic => "critic",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectConfig {
    pub writer: AdapterKind,
    pub critic: AdapterKind,
    pub writer_model: Option<String>,
    pub critic_model: Option<String>,
    pub checks: Vec<ProjectCheck>,
    pub github: Option<GithubPolicy>,
    pub max_concurrent_writers: usize,
    pub orchestration: OrchestrationConfig,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GithubPolicy {
    pub repository: String,
    #[serde(default = "default_github_remote")]
    pub remote: String,
    #[serde(default = "default_github_base_branch")]
    pub base_branch: String,
    #[serde(default = "default_github_branch_prefix")]
    pub feature_branch_prefix: String,
    #[serde(default)]
    pub allow_push: bool,
    #[serde(default)]
    pub allow_pull_request: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    schema_version: u32,
    writer: String,
    critic: String,
    #[serde(default)]
    writer_model: Option<String>,
    #[serde(default)]
    critic_model: Option<String>,
    #[serde(default)]
    checks: Vec<ProjectCheck>,
    #[serde(default)]
    github: Option<GithubPolicy>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_max_concurrent_writers"
    )]
    max_concurrent_writers: Option<usize>,
    #[serde(default)]
    orchestration: Option<OrchestrationConfig>,
}

impl Default for ProjectConfig {
    fn default() -> Self {
        Self {
            writer: AdapterKind::Agy,
            critic: AdapterKind::Agy,
            writer_model: None,
            critic_model: None,
            checks: Vec::new(),
            github: None,
            max_concurrent_writers: 1,
            orchestration: OrchestrationConfig::default(),
        }
    }
}

impl ProjectConfig {
    pub fn load(project: &Project) -> Result<Self> {
        project.ensure_runtime_dir()?;
        let path = project.runtime_dir().join(CONFIG_FILE_NAME);
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("Cannot inspect project config {}", path.display()));
            }
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            bail!(
                "Project config {} must be a regular file, not a symlink",
                path.display()
            );
        }
        if metadata.len() > MAX_CONFIG_BYTES {
            bail!(
                "Project config {} exceeds the {MAX_CONFIG_BYTES}-byte limit",
                path.display()
            );
        }
        let bytes = fs::read(&path)
            .with_context(|| format!("Cannot read project config {}", path.display()))?;
        let parsed: FileConfig = serde_json::from_slice(&bytes)
            .with_context(|| format!("Project config {} is not valid JSON", path.display()))?;
        if !matches!(parsed.schema_version, 1..=5) {
            bail!(
                "Project config schema version {} is unsupported",
                parsed.schema_version
            );
        }
        if parsed.schema_version < 5 && parsed.orchestration.is_some() {
            bail!("orchestration requires project config schema version 5");
        }
        if parsed.schema_version < 4 && parsed.max_concurrent_writers.is_some() {
            bail!("max_concurrent_writers requires project config schema version 4");
        }
        if parsed.github.is_some() && parsed.schema_version < 3 {
            bail!("GitHub publication policy requires project config schema version 3");
        }
        let max_concurrent_writers = parsed.max_concurrent_writers.unwrap_or(1);
        if !(1..=3).contains(&max_concurrent_writers) {
            bail!("max_concurrent_writers must be between 1 and 3");
        }
        validate_project_checks(&parsed.checks)?;
        if let Some(github) = &parsed.github {
            validate_github_policy(github)?;
        }
        let orchestration = parsed.orchestration.unwrap_or_default();
        validate_orchestration_config(&orchestration)?;
        Ok(Self {
            writer: parse_adapter(&parsed.writer, AgentRole::Writer)?,
            critic: parse_adapter(&parsed.critic, AgentRole::Critic)?,
            writer_model: parsed.writer_model,
            critic_model: parsed.critic_model,
            checks: parsed.checks,
            github: parsed.github,
            max_concurrent_writers,
            orchestration,
        })
    }

    pub fn adapter_for(&self, role: AgentRole) -> AdapterKind {
        match role {
            AgentRole::Writer => self.writer,
            AgentRole::Critic => self.critic,
        }
    }
}

fn deserialize_optional_max_concurrent_writers<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<usize>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    usize::deserialize(deserializer).map(Some)
}

fn default_github_remote() -> String {
    "origin".to_owned()
}

fn default_github_base_branch() -> String {
    "master".to_owned()
}

fn default_github_branch_prefix() -> String {
    "talos/".to_owned()
}

fn validate_github_policy(policy: &GithubPolicy) -> Result<()> {
    let Some((owner, repository)) = policy.repository.split_once('/') else {
        bail!("GitHub policy repository must use owner/repository form");
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
        bail!("GitHub policy repository name has an invalid owner or repository component");
    }
    if policy.remote.is_empty()
        || policy.remote.len() > 64
        || !policy
            .remote
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        || policy.remote.starts_with('-')
        || policy.remote.starts_with('.')
        || policy.remote.ends_with('.')
        || policy.remote.contains("..")
    {
        bail!("GitHub policy remote name is invalid");
    }
    if policy.base_branch != "master" {
        bail!("GitHub policy base branch must be `master`");
    }
    if !policy.feature_branch_prefix.starts_with("talos/")
        || !policy.feature_branch_prefix.ends_with('/')
        || policy.feature_branch_prefix.len() > 64
        || !policy
            .feature_branch_prefix
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'.' | b'_' | b'-'))
        || policy.feature_branch_prefix.contains("..")
        || policy.feature_branch_prefix.contains("//")
    {
        bail!("GitHub feature branch prefix must be a valid path beginning with `talos/`");
    }
    Ok(())
}

fn validate_orchestration_config(config: &OrchestrationConfig) -> Result<()> {
    if !(1..=4).contains(&config.max_writer_workers) {
        bail!("max_writer_workers must be between 1 and 4");
    }
    if config.max_critic_workers > 3 {
        bail!("max_critic_workers must be between 0 and 3");
    }
    if !(1..=5).contains(&config.max_review_rounds) {
        bail!("max_review_rounds must be between 1 and 5");
    }
    if !(1..=5).contains(&config.max_help_attempts) {
        bail!("max_help_attempts must be between 1 and 5");
    }
    Ok(())
}

fn parse_adapter(value: &str, role: AgentRole) -> Result<AdapterKind> {
    AdapterKind::parse(value).ok_or_else(|| {
        anyhow::anyhow!(
            "Configured {} adapter profile {value:?} is not a known TALOS profile",
            role.as_str()
        )
    })
}

pub fn config_path(project_root: &Path) -> std::path::PathBuf {
    project_root.join(".talos").join(CONFIG_FILE_NAME)
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };

    use super::{AdapterKind, AgentRole, ProjectConfig};
    use crate::project::Project;

    static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn defaults_both_roles_to_the_only_verified_adapter() {
        let root = temp_dir();
        let project = Project {
            root: root.clone(),
            state_root: root.clone(),
            discovery: crate::project::DiscoveryMethod::ProjectMarker("Cargo.toml".to_owned()),
        };
        let config = ProjectConfig::load(&project).expect("load defaults");
        assert_eq!(config.adapter_for(AgentRole::Writer), AdapterKind::Agy);
        assert_eq!(config.adapter_for(AgentRole::Critic), AdapterKind::Agy);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn accepts_claude_profile_in_configuration() {
        let root = temp_dir();
        let project = Project {
            root: root.clone(),
            state_root: root.clone(),
            discovery: crate::project::DiscoveryMethod::ProjectMarker("Cargo.toml".to_owned()),
        };
        project
            .ensure_runtime_dir()
            .expect("create runtime directory");
        fs::write(
            super::config_path(&root),
            r#"{"schema_version":1,"writer":"agy","critic":"claude"}"#,
        )
        .expect("write config");
        let config =
            ProjectConfig::load(&project).expect("known profile ID is valid configuration");
        assert_eq!(config.adapter_for(AgentRole::Writer), AdapterKind::Agy);
        assert_eq!(config.adapter_for(AgentRole::Critic), AdapterKind::Claude);
        assert!(config.critic.is_implemented());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn accepts_codex_profile_in_configuration() {
        let root = temp_dir();
        let project = Project {
            root: root.clone(),
            state_root: root.clone(),
            discovery: crate::project::DiscoveryMethod::ProjectMarker("Cargo.toml".to_owned()),
        };
        project
            .ensure_runtime_dir()
            .expect("create runtime directory");
        fs::write(
            super::config_path(&root),
            r#"{"schema_version":1,"writer":"codex","critic":"agy"}"#,
        )
        .expect("write config");
        let config = ProjectConfig::load(&project).expect("load config");
        assert_eq!(config.adapter_for(AgentRole::Writer), AdapterKind::Codex);
        assert!(config.writer.is_implemented());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn refuses_unknown_adapter_profile() {
        let root = temp_dir();
        let project = Project {
            root: root.clone(),
            state_root: root.clone(),
            discovery: crate::project::DiscoveryMethod::ProjectMarker("Cargo.toml".to_owned()),
        };
        project
            .ensure_runtime_dir()
            .expect("create runtime directory");
        fs::write(
            super::config_path(&root),
            r#"{"schema_version":1,"writer":"agy","critic":"unknown"}"#,
        )
        .expect("write config");
        let error = ProjectConfig::load(&project).expect_err("unknown profile must be refused");
        assert!(format!("{error:#}").contains("not a known TALOS profile"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn loads_allowlisted_project_checks_from_config_version_two() {
        let root = temp_dir();
        let project = Project {
            root: root.clone(),
            state_root: root.clone(),
            discovery: crate::project::DiscoveryMethod::ProjectMarker("Cargo.toml".to_owned()),
        };
        project
            .ensure_runtime_dir()
            .expect("create runtime directory");
        fs::write(
            super::config_path(&root),
            r#"{"schema_version":2,"writer":"agy","critic":"agy","checks":[{"id":"unit","argv":["cargo","test","--locked"],"version_argv":["cargo","--version"],"working_directory":"."}]}"#,
        )
        .expect("write verification config");
        let config = ProjectConfig::load(&project).expect("load verification check");
        assert_eq!(config.checks.len(), 1);
        assert_eq!(config.checks[0].id, "unit");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn refuses_project_checks_outside_the_tool_allowlist() {
        let root = temp_dir();
        let project = Project {
            root: root.clone(),
            state_root: root.clone(),
            discovery: crate::project::DiscoveryMethod::ProjectMarker("Cargo.toml".to_owned()),
        };
        project
            .ensure_runtime_dir()
            .expect("create runtime directory");
        fs::write(
            super::config_path(&root),
            r#"{"schema_version":2,"writer":"agy","critic":"agy","checks":[{"id":"unsafe","argv":["sh","-c","touch marker"],"version_argv":["sh","--version"],"working_directory":"."}]}"#,
        )
        .expect("write invalid verification config");
        let error = ProjectConfig::load(&project).expect_err("unapproved tool must be refused");
        assert!(format!("{error:#}").contains("approved check-tool allowlist"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn loads_orchestration_config_from_schema_version_five() {
        let root = temp_dir();
        let project = Project {
            root: root.clone(),
            state_root: root.clone(),
            discovery: crate::project::DiscoveryMethod::ProjectMarker("Cargo.toml".to_owned()),
        };
        project
            .ensure_runtime_dir()
            .expect("create runtime directory");
        fs::write(
            super::config_path(&root),
            r#"{
                "schema_version": 5,
                "writer": "codex",
                "critic": "agy",
                "orchestration": {
                    "mode": "team",
                    "delegation": "hybrid",
                    "max_writer_workers": 2,
                    "max_critic_workers": 1,
                    "max_review_rounds": 3,
                    "max_help_attempts": 2
                }
            }"#,
        )
        .expect("write team config");
        let config = ProjectConfig::load(&project).expect("load team config");
        assert_eq!(config.orchestration.mode, super::OrchestrationMode::Team);
        assert_eq!(
            config.orchestration.delegation,
            super::DelegationMode::Hybrid
        );
        assert_eq!(config.orchestration.max_writer_workers, 2);
        assert_eq!(config.orchestration.max_critic_workers, 1);
        assert_eq!(config.orchestration.max_review_rounds, 3);
        assert_eq!(config.orchestration.max_help_attempts, 2);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn refuses_invalid_orchestration_config() {
        let root = temp_dir();
        let project = Project {
            root: root.clone(),
            state_root: root.clone(),
            discovery: crate::project::DiscoveryMethod::ProjectMarker("Cargo.toml".to_owned()),
        };
        project
            .ensure_runtime_dir()
            .expect("create runtime directory");
        fs::write(
            super::config_path(&root),
            r#"{
                "schema_version": 5,
                "writer": "codex",
                "critic": "agy",
                "orchestration": {
                    "mode": "team",
                    "max_writer_workers": 10
                }
            }"#,
        )
        .expect("write invalid team config");
        let error =
            ProjectConfig::load(&project).expect_err("excessive worker count must be refused");
        assert!(format!("{error:#}").contains("max_writer_workers must be between 1 and 4"));
        let _ = fs::remove_dir_all(root);
    }

    fn temp_dir() -> PathBuf {
        let id = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("talos-config-test-{}-{id}", std::process::id()));
        fs::create_dir_all(&path).expect("create temp project");
        path
    }
}
