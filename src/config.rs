use std::{fs, path::Path};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::{
    checks::{ProjectCheck, validate_project_checks},
    project::Project,
};

const CONFIG_FILE_NAME: &str = "config.json";
const MAX_CONFIG_BYTES: u64 = 16 * 1024;

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdapterKind {
    Agy,
}

impl AdapterKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Agy => "agy",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectConfig {
    pub writer: AdapterKind,
    pub critic: AdapterKind,
    pub checks: Vec<ProjectCheck>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    schema_version: u32,
    writer: String,
    critic: String,
    #[serde(default)]
    checks: Vec<ProjectCheck>,
}

impl Default for ProjectConfig {
    fn default() -> Self {
        Self {
            writer: AdapterKind::Agy,
            critic: AdapterKind::Agy,
            checks: Vec::new(),
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
        if !matches!(parsed.schema_version, 1 | 2) {
            bail!(
                "Project config schema version {} is unsupported",
                parsed.schema_version
            );
        }
        validate_project_checks(&parsed.checks)?;
        Ok(Self {
            writer: parse_adapter(&parsed.writer, AgentRole::Writer)?,
            critic: parse_adapter(&parsed.critic, AgentRole::Critic)?,
            checks: parsed.checks,
        })
    }

    pub fn adapter_for(&self, role: AgentRole) -> AdapterKind {
        match role {
            AgentRole::Writer => self.writer,
            AgentRole::Critic => self.critic,
        }
    }
}

fn parse_adapter(value: &str, role: AgentRole) -> Result<AdapterKind> {
    if value == "agy" {
        Ok(AdapterKind::Agy)
    } else {
        bail!(
            "Configured {} adapter {value:?} is unavailable or has not passed TALOS's role-specific safety verification",
            role.as_str()
        )
    }
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
            discovery: crate::project::DiscoveryMethod::ProjectMarker("Cargo.toml".to_owned()),
        };
        let config = ProjectConfig::load(&project).expect("load defaults");
        assert_eq!(config.adapter_for(AgentRole::Writer), AdapterKind::Agy);
        assert_eq!(config.adapter_for(AgentRole::Critic), AdapterKind::Agy);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn refuses_an_unverified_role_adapter() {
        let root = temp_dir();
        let project = Project {
            root: root.clone(),
            discovery: crate::project::DiscoveryMethod::ProjectMarker("Cargo.toml".to_owned()),
        };
        project
            .ensure_runtime_dir()
            .expect("create runtime directory");
        fs::write(
            super::config_path(&root),
            r#"{"schema_version":1,"writer":"agy","critic":"codex"}"#,
        )
        .expect("write config");
        let error = ProjectConfig::load(&project).expect_err("unverified Critic must be refused");
        assert!(format!("{error:#}").contains("critic adapter"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn loads_allowlisted_project_checks_from_config_version_two() {
        let root = temp_dir();
        let project = Project {
            root: root.clone(),
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

    fn temp_dir() -> PathBuf {
        let id = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("talos-config-test-{}-{id}", std::process::id()));
        fs::create_dir_all(&path).expect("create temp project");
        path
    }
}
