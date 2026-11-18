use std::{path::PathBuf, time::Duration};

use anyhow::{Context, Result};
use serde_json::Value;
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, BufReader},
    process::Command,
    time,
};

use crate::adapter::agy_process::TerminationReason;
use crate::process_group::ProcessGroupGuard;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OmpRole {
    Writer,
    Critic,
}

#[derive(Clone, Debug)]
pub struct OmpRunConfig {
    pub executable: PathBuf,
    pub project_root: PathBuf,
    pub prompt: String,
    pub role: OmpRole,
    pub model: Option<String>,
    pub timeout: Duration,
}

#[derive(Clone, Debug)]
pub struct OmpRunResult {
    pub process_id: u32,
    pub exit_code: Option<i32>,
    pub termination: TerminationReason,
    pub session_id: Option<String>,
    pub response: Option<String>,
    pub error: Option<String>,
    pub stderr_excerpt: String,
}

pub fn resolve_omp_executable() -> Option<PathBuf> {
    super::resolve_installed_cli("omp", "OMP_PATH", &[".bun/bin", ".local/bin"])
}

pub async fn run_omp(config: OmpRunConfig) -> Result<OmpRunResult> {
    let mut cmd = Command::new(&config.executable);
    cmd.arg("--print");
    cmd.arg("--mode").arg("json");
    cmd.arg("--cwd").arg(&config.project_root);

    if let Some(model) = &config.model {
        cmd.arg("--model").arg(model);
    }

    match config.role {
        OmpRole::Critic => {
            cmd.arg("--tools").arg("read,grep,glob");
        }
        OmpRole::Writer => {
            cmd.arg("--tools").arg("read,bash,edit,write,grep,glob");
        }
    }

    cmd.arg(&config.prompt);
    cmd.current_dir(&config.project_root);
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    #[cfg(unix)]
    cmd.process_group(0);

    let mut child = cmd.spawn().context("Cannot spawn OMP process")?;
    let process_id = child
        .id()
        .context("OMP process has no OS process identifier")?;
    let mut guard = ProcessGroupGuard::new(process_id)?;

    let stdout = child.stdout.take().context("OMP stdout was not piped")?;
    let stderr = child.stderr.take().context("OMP stderr was not piped")?;

    let stdout_task = tokio::spawn(parse_omp_stdout(stdout));
    let stderr_task = tokio::spawn(capture_stderr(stderr));

    let wait_outcome = match time::timeout(config.timeout, child.wait()).await {
        Ok(exit_result) => exit_result,
        Err(_) => {
            let _ = guard.terminate_and_wait(&mut child).await;
            return Ok(OmpRunResult {
                process_id,
                exit_code: None,
                termination: TerminationReason::TimedOut,
                session_id: None,
                response: None,
                error: Some("OMP process timed out".to_owned()),
                stderr_excerpt: String::new(),
            });
        }
    };

    guard.disarm();

    let exit_status = wait_outcome.context("Cannot wait for OMP process")?;
    let (stdout_response, stdout_session, stdout_error) = stdout_task
        .await
        .context("OMP stdout processing failed")??;
    let stderr_excerpt = stderr_task
        .await
        .context("OMP stderr processing failed")??;

    Ok(OmpRunResult {
        process_id,
        exit_code: exit_status.code(),
        termination: TerminationReason::Exited,
        session_id: stdout_session,
        response: stdout_response,
        error: stdout_error,
        stderr_excerpt,
    })
}

async fn parse_omp_stdout(
    stdout: impl AsyncRead + Unpin,
) -> Result<(Option<String>, Option<String>, Option<String>)> {
    let mut reader = BufReader::new(stdout).lines();
    let mut final_response: Option<String> = None;
    let mut session_id: Option<String> = None;
    let mut error_message: Option<String> = None;

    while let Some(line) = reader.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(event) = serde_json::from_str::<Value>(&line) {
            if let Some(sid) = event.get("session_id").and_then(Value::as_str) {
                session_id = Some(sid.to_owned());
            }
            if let Some(text) = event.get("response").and_then(Value::as_str) {
                final_response = Some(text.to_owned());
            } else if let Some(content) = event.get("content").and_then(Value::as_str) {
                final_response = Some(content.to_owned());
            }
            if let Some(err) = event.get("error").and_then(Value::as_str) {
                error_message = Some(err.to_owned());
            }
        } else if !line.starts_with('{') {
            final_response = Some(line);
        }
    }

    Ok((final_response, session_id, error_message))
}

async fn capture_stderr(stderr: impl AsyncRead + Unpin) -> Result<String> {
    let mut reader = BufReader::new(stderr).lines();
    let mut lines = Vec::new();
    while let Some(line) = reader.next_line().await? {
        lines.push(line);
        if lines.len() > 50 {
            lines.remove(0);
        }
    }
    Ok(lines.join("\n"))
}
