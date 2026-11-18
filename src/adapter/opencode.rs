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
pub enum OpenCodeRoleMode {
    Critic,
    Writer,
}

#[derive(Clone, Debug)]
pub struct OpenCodeRunConfig {
    pub executable: PathBuf,
    pub project_root: PathBuf,
    pub prompt: String,
    pub role: OpenCodeRoleMode,
    pub model: Option<String>,
    pub session_id: Option<String>,
    pub timeout: Duration,
}

#[derive(Clone, Debug)]
pub struct OpenCodeRunResult {
    pub process_id: u32,
    pub exit_code: Option<i32>,
    pub termination: TerminationReason,
    pub session_id: Option<String>,
    pub response: Option<String>,
    pub error: Option<String>,
    pub stderr_excerpt: String,
}

pub fn resolve_opencode_executable() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("OPENCODE_PATH") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Some(path);
        }
    }
    let default_candidates = [
        "/Users/AVP-Dev/.opencode/bin/opencode",
        "/Users/AVP-Dev/.local/bin/opencode",
        "/usr/local/bin/opencode",
        "/opt/homebrew/bin/opencode",
    ];
    for candidate in default_candidates {
        let path = PathBuf::from(candidate);
        if path.is_file() {
            return Some(path);
        }
    }
    if let Some(path_var) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join("opencode");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

pub async fn run_opencode(
    config: OpenCodeRunConfig,
    on_session_id: impl FnOnce(&str) -> Result<()> + Send + 'static,
) -> Result<OpenCodeRunResult> {
    let mut cmd = Command::new(&config.executable);
    cmd.arg("run");
    cmd.arg("--format").arg("json");

    match config.role {
        OpenCodeRoleMode::Critic => {
            // Read-only inspection
        }
        OpenCodeRoleMode::Writer => {
            cmd.arg("--auto");
        }
    }

    if let Some(model) = &config.model {
        cmd.arg("-m").arg(model);
    }

    if let Some(session_id) = &config.session_id {
        cmd.arg("-s").arg(session_id);
    }

    // Pass prompt as argument to run
    cmd.arg(&config.prompt);

    cmd.current_dir(&config.project_root);
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    #[cfg(unix)]
    cmd.process_group(0);

    let mut child = cmd.spawn().context("Cannot spawn OpenCode process")?;
    let process_id = child
        .id()
        .context("OpenCode process has no OS process identifier")?;
    let mut guard = ProcessGroupGuard::new(process_id)?;

    let stdout = child
        .stdout
        .take()
        .context("OpenCode stdout was not piped")?;
    let stderr = child
        .stderr
        .take()
        .context("OpenCode stderr was not piped")?;

    let (session_sender, mut session_receiver) = tokio::sync::mpsc::channel::<String>(1);
    let stdout_task = tokio::spawn(parse_opencode_stdout(stdout, session_sender));
    let stderr_task = tokio::spawn(capture_stderr(stderr));

    let mut on_session_id = Some(on_session_id);
    let mut observed_session_id: Option<String> = None;

    let wait_future = async {
        loop {
            tokio::select! {
                Some(session_id) = session_receiver.recv() => {
                    observed_session_id = Some(session_id.clone());
                    if let Some(callback) = on_session_id.take() {
                        let _ = callback(&session_id);
                    }
                }
                exit_result = child.wait() => {
                    return exit_result;
                }
            }
        }
    };

    let wait_outcome = match time::timeout(config.timeout, wait_future).await {
        Ok(exit_result) => exit_result,
        Err(_) => {
            let _ = guard.terminate_and_wait(&mut child).await;
            return Ok(OpenCodeRunResult {
                process_id,
                exit_code: None,
                termination: TerminationReason::TimedOut,
                session_id: observed_session_id,
                response: None,
                error: Some("OpenCode process timed out".to_owned()),
                stderr_excerpt: String::new(),
            });
        }
    };

    guard.disarm();

    let exit_status = wait_outcome.context("Cannot wait for OpenCode process")?;
    let (stdout_response, stdout_session, stdout_error) = stdout_task
        .await
        .context("OpenCode stdout processing failed")??;
    let stderr_excerpt = stderr_task
        .await
        .context("OpenCode stderr processing failed")??;

    let session_id = observed_session_id.or(stdout_session);
    let termination = TerminationReason::Exited;

    Ok(OpenCodeRunResult {
        process_id,
        exit_code: exit_status.code(),
        termination,
        session_id,
        response: stdout_response,
        error: stdout_error,
        stderr_excerpt,
    })
}

async fn parse_opencode_stdout(
    stdout: impl AsyncRead + Unpin,
    session_sender: tokio::sync::mpsc::Sender<String>,
) -> Result<(Option<String>, Option<String>, Option<String>)> {
    let mut reader = BufReader::new(stdout).lines();
    let mut accumulated = Vec::new();

    while let Some(line) = reader.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        accumulated.push(line);
    }

    let full_text = accumulated.join("\n");
    let mut response: Option<String> = None;
    let mut session_id: Option<String> = None;
    let mut error_message: Option<String> = None;

    if let Ok(val) = serde_json::from_str::<Value>(&full_text) {
        if let Some(id) = val
            .get("session_id")
            .or_else(|| val.get("sessionId"))
            .and_then(Value::as_str)
        {
            let id_str = id.to_owned();
            session_id = Some(id_str.clone());
            let _ = session_sender.send(id_str).await;
        }
        if let Some(res) = val
            .get("result")
            .or_else(|| val.get("response"))
            .and_then(Value::as_str)
        {
            response = Some(res.to_owned());
        } else if let Some(content) = val.get("content").and_then(Value::as_str) {
            response = Some(content.to_owned());
        }
        if let Some(err) = val.get("error").and_then(Value::as_str) {
            error_message = Some(err.to_owned());
        }
    } else {
        response = Some(full_text);
    }

    Ok((response, session_id, error_message))
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
