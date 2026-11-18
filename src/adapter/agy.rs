use std::{
    env,
    io::{self, Read},
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use serde::Serialize;

const MAX_PROBE_OUTPUT_BYTES: usize = 64 * 1024;
const PROBE_TIMEOUT: Duration = Duration::from_secs(15);

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
    /// Advertised only when the CLI exposes the public native invocation tool.
    pub native_subagents: CapabilityState,
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

        Self {
            status: ProbeStatus::Available,
            executable: Some(executable),
            version,
            capabilities: parse_capabilities_streams(
                &help_output.stdout,
                &help_output.stderr,
            ),
            note: Some(
                "Advertised CLI flags are based on local help output. Native subagent invocation remains not tested until the configured agent roster and a harmless live delegation are verified. This probe does not evaluate session-specific file permissions; explicit writes are enabled only for the Agy version covered by TALOS's disposable-project boundary probe.".to_owned(),
            ),
        }
    }
}

#[cfg(test)]
fn parse_capabilities(help: &str) -> AgyCapabilities {
    parse_capabilities_streams(help.as_bytes(), &[])
}

fn parse_capabilities_streams(stdout: &[u8], stderr: &[u8]) -> AgyCapabilities {
    AgyCapabilities {
        headless_print: state_for(
            probe_output_contains(stdout, stderr, "--print")
                || probe_output_contains(stdout, stderr, "-p "),
        ),
        structured_json_output: state_for(
            probe_output_contains(stdout, stderr, "--output-format")
                && probe_output_contains(stdout, stderr, "json"),
        ),
        public_session_resume: state_for(
            probe_output_contains(stdout, stderr, "--conversation")
                || probe_output_contains(stdout, stderr, "--continue"),
        ),
        terminal_sandbox: state_for(probe_output_contains(stdout, stderr, "--sandbox")),
        auto_mode: state_for(
            probe_output_contains(stdout, stderr, "--mode")
                && probe_output_contains(stdout, stderr, "auto"),
        ),
        filesystem_write_scope: CapabilityState::NotTested,
        safe_unattended_write: CapabilityState::NotTested,
        native_subagents: if probe_output_contains(stdout, stderr, "invoke_subagent") {
            CapabilityState::Advertised
        } else {
            CapabilityState::NotTested
        },
    }
}

fn probe_output_contains(stdout: &[u8], stderr: &[u8], needle: &str) -> bool {
    [stdout, stderr].into_iter().any(|output| {
        output
            .windows(needle.len())
            .any(|window| window.eq_ignore_ascii_case(needle.as_bytes()))
    })
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
        native_subagents: CapabilityState::NotInstalled,
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

fn run_probe(executable: &Path, argument: &str, project_root: &Path) -> io::Result<Output> {
    let mut child = Command::new(executable)
        .arg(argument)
        .current_dir(project_root)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let Some(stdout) = child.stdout.take() else {
        stop_probe_child(&mut child);
        return Err(io::Error::other("Agy probe stdout was not piped"));
    };
    let Some(stderr) = child.stderr.take() else {
        stop_probe_child(&mut child);
        return Err(io::Error::other("Agy probe stderr was not piped"));
    };

    let stop_capture = Arc::new(AtomicBool::new(false));
    let output_exceeded = Arc::new(AtomicBool::new(false));
    let stdout_reader = spawn_probe_capture(
        stdout,
        Arc::clone(&stop_capture),
        Arc::clone(&output_exceeded),
    );
    let stderr_reader = spawn_probe_capture(
        stderr,
        Arc::clone(&stop_capture),
        Arc::clone(&output_exceeded),
    );
    let started = Instant::now();
    let status = loop {
        if stop_capture.load(Ordering::Acquire) {
            stop_probe_child(&mut child);
            let stdout_result = join_probe_capture(stdout_reader);
            let stderr_result = join_probe_capture(stderr_reader);
            stdout_result?;
            stderr_result?;
            if output_exceeded.load(Ordering::Acquire) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("Agy probe output exceeds the {MAX_PROBE_OUTPUT_BYTES}-byte limit"),
                ));
            }
            return Err(io::Error::other("Cannot read Agy probe output"));
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < PROBE_TIMEOUT => {
                thread::sleep(Duration::from_millis(10));
            }
            Ok(None) => {
                stop_probe_child(&mut child);
                let _ = join_probe_capture(stdout_reader);
                let _ = join_probe_capture(stderr_reader);
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "Agy probe exceeded its time limit",
                ));
            }
            Err(error) => {
                stop_probe_child(&mut child);
                let _ = join_probe_capture(stdout_reader);
                let _ = join_probe_capture(stderr_reader);
                return Err(error);
            }
        }
    };
    let stdout_result = join_probe_capture(stdout_reader);
    let stderr_result = join_probe_capture(stderr_reader);
    let stdout = stdout_result?;
    let stderr = stderr_result?;
    if output_exceeded.load(Ordering::Acquire) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Agy probe output exceeds the {MAX_PROBE_OUTPUT_BYTES}-byte limit"),
        ));
    }
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

fn spawn_probe_capture<R>(
    reader: R,
    stop_capture: Arc<AtomicBool>,
    output_exceeded: Arc<AtomicBool>,
) -> thread::JoinHandle<io::Result<Vec<u8>>>
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        let mut reader = reader;
        let mut captured = Vec::with_capacity(MAX_PROBE_OUTPUT_BYTES);
        let mut buffer = [0_u8; 8 * 1024];
        loop {
            let bytes_read = match reader.read(&mut buffer) {
                Ok(bytes_read) => bytes_read,
                Err(error) => {
                    stop_capture.store(true, Ordering::Release);
                    return Err(error);
                }
            };
            if bytes_read == 0 {
                return Ok(captured);
            }
            if bytes_read > MAX_PROBE_OUTPUT_BYTES.saturating_sub(captured.len()) {
                output_exceeded.store(true, Ordering::Release);
                stop_capture.store(true, Ordering::Release);
                return Ok(captured);
            }
            captured.extend_from_slice(&buffer[..bytes_read]);
        }
    })
}

fn join_probe_capture(reader: thread::JoinHandle<io::Result<Vec<u8>>>) -> io::Result<Vec<u8>> {
    reader
        .join()
        .map_err(|_| io::Error::other("Agy probe output reader stopped unexpectedly"))?
}

fn stop_probe_child(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn first_line(stdout: &[u8], stderr: &[u8]) -> Option<String> {
    for bytes in [stdout, stderr] {
        if let Some(line) = String::from_utf8_lossy(bytes)
            .lines()
            .find(|line| !line.trim().is_empty())
        {
            return Some(line.trim().chars().take(160).collect());
        }
    }
    None
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
        assert!(matches!(
            capabilities.native_subagents,
            CapabilityState::NotTested
        ));
    }

    #[test]
    fn explicit_native_subagent_tool_is_reported_as_advertised() {
        let capabilities = parse_capabilities("Available tools: invoke_subagent");

        assert!(matches!(
            capabilities.native_subagents,
            CapabilityState::Advertised
        ));
    }
}
