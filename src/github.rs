use std::{
    collections::BTreeMap,
    io,
    process::{ExitStatus, Stdio},
    time::Duration,
};

use serde::{Deserialize, Serialize};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::Command,
    time::timeout,
};

const COMMAND_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_CAPTURE_BYTES: usize = 32 * 1024;

#[derive(Clone, Debug, Serialize)]
pub struct GithubProbe {
    pub status: &'static str,
    pub version: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct GithubStatusReport {
    pub gh: GithubProbe,
    pub auth_status: &'static str,
    pub auth_check_requested: bool,
    pub auth_check_performed: bool,
}

enum CommandResult {
    Output {
        status: ExitStatus,
        stdout: Vec<u8>,
        output_truncated: bool,
    },
    NotInstalled,
    TimedOut,
    Failed,
}

#[derive(Deserialize)]
struct AuthStatusOutput {
    hosts: BTreeMap<String, Vec<AuthHostEntry>>,
}

#[derive(Deserialize)]
struct AuthHostEntry {
    state: String,
}

pub async fn probe_local() -> GithubProbe {
    match run_gh(&["--version"]).await {
        CommandResult::Output {
            status,
            stdout,
            output_truncated: false,
        } if status.success() => match parse_version(&stdout) {
            Some(version) => GithubProbe {
                status: "available",
                version: Some(version),
            },
            None => GithubProbe {
                status: "probe_failed",
                version: None,
            },
        },
        CommandResult::NotInstalled => GithubProbe {
            status: "unavailable",
            version: None,
        },
        CommandResult::TimedOut => GithubProbe {
            status: "timeout",
            version: None,
        },
        _ => GithubProbe {
            status: "probe_failed",
            version: None,
        },
    }
}

pub async fn status_report(check_auth: bool) -> GithubStatusReport {
    let gh = probe_local().await;
    if !check_auth {
        return GithubStatusReport {
            gh,
            auth_status: "not_checked",
            auth_check_requested: false,
            auth_check_performed: false,
        };
    }

    if gh.status != "available" {
        return GithubStatusReport {
            gh,
            auth_status: "gh_unavailable",
            auth_check_requested: true,
            auth_check_performed: false,
        };
    }

    let auth_status = match run_gh(&["auth", "status", "--active", "--json", "hosts"]).await {
        CommandResult::Output {
            status,
            stdout,
            output_truncated: false,
        } => parse_auth_status(&stdout, status.success()),
        CommandResult::TimedOut => "timeout",
        CommandResult::NotInstalled | CommandResult::Failed => "unknown",
        CommandResult::Output {
            output_truncated: true,
            ..
        } => "unknown",
    };

    GithubStatusReport {
        gh,
        auth_status,
        auth_check_requested: true,
        auth_check_performed: true,
    }
}

async fn run_gh(arguments: &[&str]) -> CommandResult {
    let mut command = Command::new("gh");
    command
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return CommandResult::NotInstalled;
        }
        Err(_) => return CommandResult::Failed,
    };

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let result = timeout(COMMAND_TIMEOUT, async {
        let (stdout, stderr_truncated, status) = tokio::join!(
            capture_bounded(stdout),
            discard_output(stderr),
            child.wait(),
        );
        let (stdout, stdout_truncated) = stdout?;
        let stderr_truncated = stderr_truncated?;
        let status = status?;
        Ok::<_, io::Error>((status, stdout, stdout_truncated || stderr_truncated))
    })
    .await;

    match result {
        Ok(Ok((status, stdout, output_truncated))) => CommandResult::Output {
            status,
            stdout,
            output_truncated,
        },
        Ok(Err(_)) => CommandResult::Failed,
        Err(_) => {
            let _ = child.kill().await;
            CommandResult::TimedOut
        }
    }
}

async fn capture_bounded<R: AsyncRead + Unpin>(reader: Option<R>) -> io::Result<(Vec<u8>, bool)> {
    let Some(mut reader) = reader else {
        return Ok((Vec::new(), false));
    };

    let mut captured = Vec::with_capacity(512);
    let mut truncated = false;
    let mut buffer = [0_u8; 4096];
    loop {
        let bytes_read = reader.read(&mut buffer).await?;
        if bytes_read == 0 {
            break;
        }
        let remaining = MAX_CAPTURE_BYTES.saturating_sub(captured.len());
        let bytes_to_keep = remaining.min(bytes_read);
        captured.extend_from_slice(&buffer[..bytes_to_keep]);
        truncated |= bytes_to_keep < bytes_read;
    }
    Ok((captured, truncated))
}

async fn discard_output<R: AsyncRead + Unpin>(reader: Option<R>) -> io::Result<bool> {
    let Some(mut reader) = reader else {
        return Ok(false);
    };

    let mut total = 0_usize;
    let mut buffer = [0_u8; 4096];
    loop {
        let bytes_read = reader.read(&mut buffer).await?;
        if bytes_read == 0 {
            break;
        }
        total = total.saturating_add(bytes_read);
    }
    Ok(total > MAX_CAPTURE_BYTES)
}

fn parse_version(stdout: &[u8]) -> Option<String> {
    let first_line = std::str::from_utf8(stdout).ok()?.lines().next()?.trim();
    let version = first_line
        .strip_prefix("gh version ")?
        .split_whitespace()
        .next()?;
    if version.is_empty()
        || !version
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'+'))
    {
        return None;
    }
    Some(version.to_owned())
}

fn parse_auth_status(stdout: &[u8], command_succeeded: bool) -> &'static str {
    let Ok(output) = serde_json::from_slice::<AuthStatusOutput>(stdout) else {
        return "unknown";
    };

    let entries = output.hosts.values().flatten().collect::<Vec<_>>();
    if entries.is_empty() {
        return if command_succeeded {
            "not_authenticated"
        } else {
            "unknown"
        };
    }

    let mut has_failure = false;
    for entry in entries {
        match entry.state.as_str() {
            "success" => {}
            "error" | "timeout" | "refresh_expired" => has_failure = true,
            _ => return "unknown",
        }
    }
    if has_failure {
        "needs_attention"
    } else if command_succeeded {
        "authenticated"
    } else {
        "unknown"
    }
}
