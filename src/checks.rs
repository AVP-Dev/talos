use std::{
    path::{Component, Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::Command,
    task::JoinHandle,
    time,
};

use crate::secrets::{contains_sensitive, redact_sensitive};

const MAX_CHECKS: usize = 32;
const MAX_ARGUMENTS: usize = 128;
const MAX_ARGUMENT_BYTES: usize = 8 * 1024;
const MAX_CAPTURE_BYTES: usize = 128 * 1024;
const CAPTURE_JOIN_TIMEOUT: Duration = Duration::from_secs(2);
const VERSION_TIMEOUT: Duration = Duration::from_secs(10);
const CHECK_TIMEOUT: Duration = Duration::from_secs(30 * 60);

const APPROVED_CHECK_TOOLS: &[&str] = &[
    "bun",
    "cargo",
    "cmake",
    "composer",
    "ctest",
    "deno",
    "dotnet",
    "go",
    "gradle",
    "make",
    "mvn",
    "node",
    "npm",
    "php",
    "pnpm",
    "pytest",
    "python3",
    "rake",
    "ruby",
    "rustc",
    "swift",
    "xcodebuild",
    "yarn",
];

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProjectCheck {
    pub id: String,
    pub argv: Vec<String>,
    pub version_argv: Vec<String>,
    pub working_directory: String,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct CheckExecution {
    pub check_id: String,
    pub argv: Vec<String>,
    pub version_argv: Vec<String>,
    pub working_directory: String,
    pub tool_version: Option<String>,
    pub version_duration_ms: Option<i64>,
    pub version_exit_code: Option<i32>,
    pub duration_ms: Option<i64>,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub failure_code: Option<String>,
    pub stdout_sha256: String,
    pub stderr_sha256: String,
}

impl CheckExecution {
    pub fn passed(&self) -> bool {
        self.version_exit_code == Some(0)
            && self.exit_code == Some(0)
            && self.version_duration_ms.is_some()
            && self.duration_ms.is_some()
            && !self.timed_out
            && self.failure_code.is_none()
    }
}

#[derive(Debug)]
struct CapturedStream {
    bytes: Vec<u8>,
    sha256: String,
}

#[derive(Debug)]
struct ProcessResult {
    exit_code: Option<i32>,
    timed_out: bool,
    failure_code: Option<String>,
    stdout: CapturedStream,
    stderr: CapturedStream,
}

pub fn validate_project_checks(checks: &[ProjectCheck]) -> Result<()> {
    if checks.len() > MAX_CHECKS {
        bail!("Project config may define at most {MAX_CHECKS} checks");
    }
    let mut ids = std::collections::HashSet::new();
    for check in checks {
        validate_check(check)?;
        if !ids.insert(check.id.as_str()) {
            bail!("Project check IDs must be unique");
        }
    }
    Ok(())
}

fn validate_check(check: &ProjectCheck) -> Result<()> {
    if check.id.is_empty()
        || check.id.len() > 64
        || !check
            .id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"_-".contains(&byte))
    {
        bail!("Project check ID must use 1 to 64 lowercase letters, digits, '_' or '-'");
    }
    validate_argv("check command", &check.argv)?;
    validate_argv("version command", &check.version_argv)?;
    if check.argv[0] != check.version_argv[0] {
        bail!("Check and version commands must use the same approved executable");
    }
    if !APPROVED_CHECK_TOOLS.contains(&check.argv[0].as_str()) {
        bail!(
            "Project check executable {:?} is not in TALOS's approved check-tool allowlist",
            check.argv[0]
        );
    }
    let cwd = Path::new(&check.working_directory);
    if check.working_directory.is_empty()
        || cwd.is_absolute()
        || cwd
            .components()
            .any(|component| !matches!(component, Component::Normal(_) | Component::CurDir))
        || contains_sensitive(&check.working_directory)
    {
        bail!("Project check working_directory must be a safe project-relative path");
    }
    Ok(())
}

fn validate_argv(label: &str, argv: &[String]) -> Result<()> {
    if argv.is_empty() || argv.len() > MAX_ARGUMENTS {
        bail!("{label} must contain 1 to {MAX_ARGUMENTS} arguments");
    }
    let total = argv.iter().try_fold(0_usize, |length, argument| {
        if argument.is_empty()
            || argument.len() > 2 * 1024
            || argument.chars().any(char::is_control)
            || contains_sensitive(argument)
        {
            bail!("{label} contains an empty, oversized, control, or secret-like argument");
        }
        length
            .checked_add(argument.len())
            .context("Project check argument length overflowed")
    })?;
    if total > MAX_ARGUMENT_BYTES {
        bail!("{label} exceeds the {MAX_ARGUMENT_BYTES}-byte argument budget");
    }
    let executable = Path::new(&argv[0]);
    if argv[0].contains('/') || argv[0].contains('\\') || executable.components().count() != 1 {
        bail!("{label} executable must be an approved command name without a path");
    }
    Ok(())
}

pub fn check_by_id(checks: &[ProjectCheck], id: &str) -> Option<ProjectCheck> {
    checks.iter().find(|check| check.id == id).cloned()
}

pub fn validate_selected_checks(
    checks: &[ProjectCheck],
    selected: &[String],
) -> Result<Vec<ProjectCheck>> {
    if selected.len() > MAX_CHECKS {
        bail!("A task may select at most {MAX_CHECKS} project checks");
    }
    let mut result = Vec::with_capacity(selected.len());
    let mut seen = std::collections::HashSet::new();
    for id in selected {
        if !seen.insert(id) {
            bail!("A project check may only be selected once for a task");
        }
        let check = check_by_id(checks, id).with_context(|| {
            format!("Project check {id:?} is not defined in .talos/config.json")
        })?;
        result.push(check);
    }
    Ok(result)
}

pub async fn run_project_check(
    project_root: &Path,
    check: &ProjectCheck,
    timeout: Duration,
) -> Result<CheckExecution> {
    validate_check(check)?;
    if timeout.is_zero() {
        bail!("Project check timeout must be greater than zero");
    }
    let cwd = canonical_check_directory(project_root, &check.working_directory)?;
    let check_started = tokio::time::Instant::now();
    let version_started = tokio::time::Instant::now();
    let version = run_process(&check.version_argv, &cwd, VERSION_TIMEOUT.min(timeout)).await?;
    let version_duration_ms = elapsed_milliseconds(version_started.elapsed())?;
    let tool_version = if version.exit_code == Some(0) && !version.timed_out {
        let text = String::from_utf8_lossy(&version.stdout.bytes);
        let first_line = text.lines().next().unwrap_or_default().trim();
        if first_line.is_empty() {
            None
        } else {
            Some(redact_sensitive(first_line).chars().take(200).collect())
        }
    } else {
        None
    };
    let mut stdout_digest = version.stdout.sha256.clone();
    let mut stderr_digest = version.stderr.sha256.clone();
    let mut exit_code = None;
    let mut duration_ms = None;
    let mut timed_out = version.timed_out;
    let mut failure_code = version.failure_code.clone();
    if version.exit_code == Some(0) && !version.timed_out && failure_code.is_none() {
        let remaining = timeout.saturating_sub(check_started.elapsed());
        if remaining.is_zero() {
            failure_code = Some("verification_budget_exhausted".to_owned());
        } else {
            let execution_started = tokio::time::Instant::now();
            let execution = run_process(&check.argv, &cwd, remaining.min(CHECK_TIMEOUT)).await?;
            duration_ms = Some(elapsed_milliseconds(execution_started.elapsed())?);
            stdout_digest = execution.stdout.sha256;
            stderr_digest = execution.stderr.sha256;
            exit_code = execution.exit_code;
            timed_out = execution.timed_out;
            failure_code = execution.failure_code;
        }
    } else if failure_code.is_none() {
        failure_code = Some("version_probe_failed".to_owned());
    }
    Ok(CheckExecution {
        check_id: check.id.clone(),
        argv: check.argv.clone(),
        version_argv: check.version_argv.clone(),
        working_directory: check.working_directory.clone(),
        tool_version,
        version_duration_ms: Some(version_duration_ms),
        version_exit_code: version.exit_code,
        duration_ms,
        exit_code,
        timed_out,
        failure_code,
        stdout_sha256: stdout_digest,
        stderr_sha256: stderr_digest,
    })
}

fn elapsed_milliseconds(duration: Duration) -> Result<i64> {
    i64::try_from(duration.as_millis()).context("Project check duration is out of range")
}

fn canonical_check_directory(project_root: &Path, relative: &str) -> Result<PathBuf> {
    let root = project_root
        .canonicalize()
        .context("Cannot resolve project root for a verification command")?;
    let candidate = root.join(relative);
    let cwd = candidate
        .canonicalize()
        .with_context(|| format!("Project check directory {relative:?} does not exist"))?;
    if !cwd.starts_with(&root) || !cwd.is_dir() {
        bail!("Project check directory must remain inside the project root");
    }
    Ok(cwd)
}

async fn run_process(argv: &[String], cwd: &Path, timeout: Duration) -> Result<ProcessResult> {
    let mut command = Command::new(&argv[0]);
    command
        .args(&argv[1..])
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(_) => {
            return Ok(ProcessResult {
                exit_code: None,
                timed_out: false,
                failure_code: Some("command_unavailable".to_owned()),
                stdout: empty_capture(),
                stderr: empty_capture(),
            });
        }
    };
    let stdout = child
        .stdout
        .take()
        .context("Cannot capture verification command stdout")?;
    let stderr = child
        .stderr
        .take()
        .context("Cannot capture verification command stderr")?;
    let stdout_task = tokio::spawn(capture_stream(stdout));
    let stderr_task = tokio::spawn(capture_stream(stderr));
    let (exit_code, timed_out, failure_code) = match time::timeout(timeout, child.wait()).await {
        Ok(Ok(status)) => (status.code(), false, None),
        Ok(Err(_)) => (None, false, Some("process_wait_failed".to_owned())),
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            (None, true, Some("timeout".to_owned()))
        }
    };
    let stdout = finish_capture(stdout_task).await?;
    let stderr = finish_capture(stderr_task).await?;
    Ok(ProcessResult {
        exit_code,
        timed_out,
        failure_code,
        stdout,
        stderr,
    })
}

async fn capture_stream<R>(mut reader: R) -> std::io::Result<CapturedStream>
where
    R: AsyncRead + Unpin,
{
    let mut bytes = Vec::with_capacity(MAX_CAPTURE_BYTES.min(16 * 1024));
    let mut hasher = Sha256::new();
    let mut chunk = [0_u8; 8192];
    loop {
        let count = reader.read(&mut chunk).await?;
        if count == 0 {
            break;
        }
        hasher.update(&chunk[..count]);
        let remaining = MAX_CAPTURE_BYTES.saturating_sub(bytes.len());
        bytes.extend_from_slice(&chunk[..count.min(remaining)]);
    }
    Ok(CapturedStream {
        bytes,
        sha256: lower_hex(&hasher.finalize()),
    })
}

async fn finish_capture(
    mut task: JoinHandle<std::io::Result<CapturedStream>>,
) -> Result<CapturedStream> {
    match time::timeout(CAPTURE_JOIN_TIMEOUT, &mut task).await {
        Ok(joined) => joined
            .context("Verification output capture task failed")?
            .context("Cannot read verification command output"),
        Err(_) => {
            task.abort();
            bail!("Verification command output stream did not close after process exit")
        }
    }
}

fn empty_capture() -> CapturedStream {
    CapturedStream {
        bytes: Vec::new(),
        sha256: lower_hex(&Sha256::digest([])),
    }
}

fn lower_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut result = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        result.push(char::from(HEX[usize::from(byte >> 4)]));
        result.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    result
}

#[cfg(test)]
mod tests {
    use super::{ProjectCheck, validate_project_checks, validate_selected_checks};

    fn sample_check() -> ProjectCheck {
        ProjectCheck {
            id: "unit".to_owned(),
            argv: vec!["cargo".to_owned(), "test".to_owned(), "--locked".to_owned()],
            version_argv: vec!["cargo".to_owned(), "--version".to_owned()],
            working_directory: ".".to_owned(),
        }
    }

    #[test]
    fn accepts_allowlisted_argv_and_rejects_shell_paths_or_secret_arguments() {
        assert!(validate_project_checks(&[sample_check()]).is_ok());
        let mut shell = sample_check();
        shell.argv[0] = "sh".to_owned();
        shell.version_argv[0] = "sh".to_owned();
        assert!(validate_project_checks(&[shell]).is_err());

        let mut path = sample_check();
        path.working_directory = "../outside".to_owned();
        assert!(validate_project_checks(&[path]).is_err());

        let mut secret = sample_check();
        secret.argv.push("token=abcdefghijklmnop".to_owned());
        assert!(validate_project_checks(&[secret]).is_err());
    }

    #[test]
    fn task_check_selection_requires_known_unique_ids() {
        let checks = vec![sample_check()];
        assert!(validate_selected_checks(&checks, &["unit".to_owned()]).is_ok());
        assert!(validate_selected_checks(&checks, &["missing".to_owned()]).is_err());
        assert!(
            validate_selected_checks(&checks, &["unit".to_owned(), "unit".to_owned()]).is_err()
        );
    }

    #[tokio::test]
    async fn records_the_version_and_exit_for_a_real_allowlisted_check_command() {
        let root = std::env::current_dir().expect("resolve test project root");
        let check = ProjectCheck {
            id: "tool-version".to_owned(),
            argv: vec!["cargo".to_owned(), "--version".to_owned()],
            version_argv: vec!["cargo".to_owned(), "--version".to_owned()],
            working_directory: ".".to_owned(),
        };
        let result = super::run_project_check(&root, &check, std::time::Duration::from_secs(10))
            .await
            .expect("run allowlisted local check");
        assert!(result.passed());
        assert!(
            result
                .tool_version
                .as_deref()
                .is_some_and(|version| version.starts_with("cargo "))
        );
        assert_eq!(result.version_exit_code, Some(0));
        assert_eq!(result.exit_code, Some(0));
        assert!(result.version_duration_ms.is_some());
        assert!(result.duration_ms.is_some());
        assert_eq!(result.stdout_sha256.len(), 64);
        assert_eq!(result.stderr_sha256.len(), 64);
    }
}
