use std::{
    env,
    path::{Path, PathBuf},
    process::Command,
};

use serde::Serialize;

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityState {
    Advertised,
    NotAdvertised,
    NotInstalled,
    NotTested,
}

impl CapabilityState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Advertised => "advertised",
            Self::NotAdvertised => "not_advertised",
            Self::NotInstalled => "not_installed",
            Self::NotTested => "not_tested",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeStatus {
    Available,
    NotInstalled,
    ProbeFailed,
}

impl ProbeStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::NotInstalled => "not_installed",
            Self::ProbeFailed => "probe_failed",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct AgyCapabilities {
    pub headless_print: CapabilityState,
    pub structured_json_output: CapabilityState,
    pub public_session_resume: CapabilityState,
    pub terminal_sandbox: CapabilityState,
    pub auto_mode: CapabilityState,
    pub filesystem_write_scope: CapabilityState,
    pub safe_unattended_write: CapabilityState,
}

#[derive(Clone, Debug, Serialize)]
pub struct AgyProbe {
    pub status: ProbeStatus,
    pub executable: Option<PathBuf>,
    pub version: Option<String>,
    pub capabilities: AgyCapabilities,
    pub note: Option<String>,
}

impl AgyProbe {
    pub fn inspect(project_root: &Path) -> Self {
        let Some(executable) = find_executable("agy") else {
            return Self {
                status: ProbeStatus::NotInstalled,
                executable: None,
                version: None,
                capabilities: unavailable_capabilities(),
                note: Some("The agy executable was not found on PATH".to_owned()),
            };
        };

        let version_output = match run_probe(&executable, "--version", project_root) {
            Ok(output) if output.status.success() => output,
            Ok(_) => {
                return failed_probe(
                    executable,
                    "The agy --version probe exited with a non-zero status",
                );
            }
            Err(_) => return failed_probe(executable, "The agy --version probe could not run"),
        };
        let version = first_line(&version_output.stdout, &version_output.stderr);

        let help_output = match run_probe(&executable, "--help", project_root) {
            Ok(output) if output.status.success() => output,
            Ok(_) => {
                return Self {
                    status: ProbeStatus::ProbeFailed,
                    executable: Some(executable),
                    version,
                    capabilities: unavailable_capabilities(),
                    note: Some("The agy --help probe exited with a non-zero status".to_owned()),
                };
            }
            Err(_) => {
                return Self {
                    status: ProbeStatus::ProbeFailed,
                    executable: Some(executable),
                    version,
                    capabilities: unavailable_capabilities(),
                    note: Some("The agy --help probe could not run".to_owned()),
                };
            }
        };

        let help = format!(
            "{}\n{}",
            String::from_utf8_lossy(&help_output.stdout),
            String::from_utf8_lossy(&help_output.stderr)
        );
        Self {
            status: ProbeStatus::Available,
            executable: Some(executable),
            version,
            capabilities: parse_capabilities(&help),
            note: Some(
                "Capability flags are based on local help output. This probe does not evaluate session-specific file permissions; explicit writes are enabled only for the Agy version covered by TALOS's disposable-project boundary probe.".to_owned(),
            ),
        }
    }
}

fn parse_capabilities(help: &str) -> AgyCapabilities {
    let help = help.to_ascii_lowercase();
    AgyCapabilities {
        headless_print: state_for(help.contains("--print") || help.contains("-p ")),
        structured_json_output: state_for(
            help.contains("--output-format") && help.contains("json"),
        ),
        public_session_resume: state_for(
            help.contains("--conversation") || help.contains("--continue"),
        ),
        terminal_sandbox: state_for(help.contains("--sandbox")),
        auto_mode: state_for(help.contains("--mode") && help.contains("auto")),
        filesystem_write_scope: CapabilityState::NotTested,
        safe_unattended_write: CapabilityState::NotTested,
    }
}

fn state_for(advertised: bool) -> CapabilityState {
    if advertised {
        CapabilityState::Advertised
    } else {
        CapabilityState::NotAdvertised
    }
}

fn unavailable_capabilities() -> AgyCapabilities {
    AgyCapabilities {
        headless_print: CapabilityState::NotInstalled,
        structured_json_output: CapabilityState::NotInstalled,
        public_session_resume: CapabilityState::NotInstalled,
        terminal_sandbox: CapabilityState::NotInstalled,
        auto_mode: CapabilityState::NotInstalled,
        filesystem_write_scope: CapabilityState::NotTested,
        safe_unattended_write: CapabilityState::NotTested,
    }
}

fn failed_probe(executable: PathBuf, message: &str) -> AgyProbe {
    AgyProbe {
        status: ProbeStatus::ProbeFailed,
        executable: Some(executable),
        version: None,
        capabilities: unavailable_capabilities(),
        note: Some(message.to_owned()),
    }
}

fn run_probe(
    executable: &Path,
    argument: &str,
    project_root: &Path,
) -> std::io::Result<std::process::Output> {
    Command::new(executable)
        .arg(argument)
        .current_dir(project_root)
        .output()
}

fn first_line(stdout: &[u8], stderr: &[u8]) -> Option<String> {
    [stdout, stderr]
        .into_iter()
        .flat_map(|bytes| {
            String::from_utf8_lossy(bytes)
                .lines()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .map(|line| line.trim().to_owned())
        .find(|line| !line.is_empty())
        .map(|line| line.chars().take(160).collect())
}

fn find_executable(name: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    env::split_paths(&path)
        .filter(|directory| !directory.as_os_str().is_empty())
        .map(|directory| directory.join(name))
        .find(|candidate| is_executable(candidate))
        .and_then(|candidate| candidate.canonicalize().ok())
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    path.metadata()
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

#[cfg(test)]
mod tests {
    use super::{CapabilityState, parse_capabilities};

    #[test]
    fn advertised_cli_flags_do_not_claim_verified_write_safety() {
        let capabilities = parse_capabilities(
            "--print Run headless\n--output-format text|json\n--conversation ID\n--sandbox terminal restrictions\n--mode accept-edits|auto|plan",
        );

        assert!(matches!(
            capabilities.headless_print,
            CapabilityState::Advertised
        ));
        assert!(matches!(
            capabilities.structured_json_output,
            CapabilityState::Advertised
        ));
        assert!(matches!(
            capabilities.public_session_resume,
            CapabilityState::Advertised
        ));
        assert!(matches!(
            capabilities.terminal_sandbox,
            CapabilityState::Advertised
        ));
        assert!(matches!(
            capabilities.auto_mode,
            CapabilityState::Advertised
        ));
        assert!(matches!(
            capabilities.filesystem_write_scope,
            CapabilityState::NotTested
        ));
        assert!(matches!(
            capabilities.safe_unattended_write,
            CapabilityState::NotTested
        ));
    }
}
