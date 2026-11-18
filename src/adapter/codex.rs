use std::{path::PathBuf, time::Duration};

use anyhow::{Context, Result};
use serde_json::Value;
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncWriteExt, BufReader},
    process::Command,
    time,
};

use crate::adapter::agy_process::TerminationReason;
use crate::process_group::ProcessGroupGuard;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodexSandboxMode {
    ReadOnly,
    WorkspaceWrite,
}

#[derive(Clone, Debug)]
pub struct CodexRunConfig {
    pub executable: PathBuf,
    pub project_root: PathBuf,
    pub prompt: String,
    pub sandbox: CodexSandboxMode,
    pub model: Option<String>,
    pub session_id: Option<String>,
    pub timeout: Duration,
}

#[derive(Clone, Debug)]
pub struct CodexRunResult {
    pub process_id: u32,
    pub exit_code: Option<i32>,
    pub termination: TerminationReason,
    pub session_id: Option<String>,
    pub response: Option<String>,
    pub error: Option<String>,
    pub stderr_excerpt: String,
}

pub fn resolve_codex_executable() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("CODEX_PATH") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Some(path);
        }
    }
    let default_candidates = [
        "/Users/AVP-Dev/.local/bin/codex",
        "/usr/local/bin/codex",
        "/opt/homebrew/bin/codex",
    ];
    for candidate in default_candidates {
        let path = PathBuf::from(candidate);
        if path.is_file() {
            return Some(path);
        }
    }
    // Search PATH
    if let Some(path_var) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join("codex");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

pub async fn run_codex(
    config: CodexRunConfig,
    on_session_id: impl FnOnce(&str) -> Result<()> + Send + 'static,
) -> Result<CodexRunResult> {
    let mut cmd = Command::new(&config.executable);
    cmd.arg("exec");

    match config.sandbox {
        CodexSandboxMode::ReadOnly => {
            cmd.arg("--sandbox").arg("read-only");
        }
        CodexSandboxMode::WorkspaceWrite => {
            cmd.arg("--sandbox").arg("workspace-write");
        }
    }

    cmd.arg("--json");
    cmd.arg("-C").arg(&config.project_root);

    if let Some(model) = &config.model {
        cmd.arg("-m").arg(model);
    }

    cmd.current_dir(&config.project_root);
    cmd.stdin(std::process::Stdio::piped());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    #[cfg(unix)]
    cmd.process_group(0);

    let mut child = cmd.spawn().context("Cannot spawn Codex process")?;
    let process_id = child
        .id()
        .context("Codex process has no OS process identifier")?;
    let mut guard = ProcessGroupGuard::new(process_id)?;

    let mut stdin = child.stdin.take().context("Codex stdin was not piped")?;
    let stdout = child.stdout.take().context("Codex stdout was not piped")?;
    let stderr = child.stderr.take().context("Codex stderr was not piped")?;

    // Send prompt through stdin
    let prompt_bytes = config.prompt.into_bytes();
    tokio::spawn(async move {
        let _ = stdin.write_all(&prompt_bytes).await;
        let _ = stdin.shutdown().await;
    });

    let (session_sender, mut session_receiver) = tokio::sync::mpsc::channel::<String>(1);
    let stdout_task = tokio::spawn(parse_codex_stdout(stdout, session_sender));
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
            return Ok(CodexRunResult {
                process_id,
                exit_code: None,
                termination: TerminationReason::TimedOut,
                session_id: observed_session_id,
                response: None,
                error: Some("Codex process timed out".to_owned()),
                stderr_excerpt: String::new(),
            });
        }
    };

    guard.disarm();

    let exit_status = wait_outcome.context("Cannot wait for Codex process")?;
    let (stdout_response, stdout_session, stdout_error) = stdout_task
        .await
        .context("Codex stdout processing failed")??;
    let stderr_excerpt = stderr_task
        .await
        .context("Codex stderr processing failed")??;

    let session_id = observed_session_id.or(stdout_session);
    let termination = TerminationReason::Exited;

    Ok(CodexRunResult {
        process_id,
        exit_code: exit_status.code(),
        termination,
        session_id,
        response: stdout_response,
        error: stdout_error,
        stderr_excerpt,
    })
}

async fn parse_codex_stdout(
    stdout: impl AsyncRead + Unpin,
    session_sender: tokio::sync::mpsc::Sender<String>,
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
            let event_type = event.get("type").and_then(Value::as_str).unwrap_or("");
            match event_type {
                "thread.started" => {
                    if let Some(id) = event.get("thread_id").and_then(Value::as_str) {
                        let id_str = id.to_owned();
                        session_id = Some(id_str.clone());
                        let _ = session_sender.send(id_str).await;
                    }
                }
                "item.completed" => {
                    if let Some(item) = event.get("item") {
                        let item_type = item.get("type").and_then(Value::as_str).unwrap_or("");
                        if item_type == "agent_message" {
                            if let Some(text) = item.get("text").and_then(Value::as_str) {
                                final_response = Some(text.to_owned());
                            }
                        } else if item_type == "error"
                            && let Some(msg) = item.get("message").and_then(Value::as_str)
                            && !msg.contains("is ignored")
                        {
                            error_message = Some(msg.to_owned());
                        }
                    }
                }
                _ => {}
            }
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
