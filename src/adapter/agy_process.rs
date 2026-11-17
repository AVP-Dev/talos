use std::{fs, io, path::PathBuf, process::ExitStatus, time::Duration};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::{Child, Command},
    task::JoinHandle,
    time::{self, Instant},
};

const MAX_CAPTURE_BYTES: usize = 2 * 1024 * 1024;
const MAX_EVENT_LINE_BYTES: usize = 64 * 1024;
const STREAM_JOIN_TIMEOUT: Duration = Duration::from_secs(2);
#[cfg(unix)]
const TERMINATION_GRACE: Duration = Duration::from_millis(750);

#[derive(Clone, Debug)]
pub struct AgyRunConfig {
    pub executable: PathBuf,
    pub project_root: PathBuf,
    pub settings_file: PathBuf,
    pub prompt: String,
    pub conversation_id: Option<String>,
    pub timeout: Duration,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerminationReason {
    Exited,
    TimedOut,
    InterruptedByCtrlC,
    InterruptedByTerminateSignal,
}

#[derive(Clone, Debug)]
pub struct AgyRunResult {
    pub process_id: u32,
    pub exit_code: Option<i32>,
    pub termination: TerminationReason,
    pub conversation_id: Option<String>,
    pub agent_status: Option<String>,
    pub response: Option<String>,
    pub agent_error: Option<String>,
    pub stderr_excerpt: String,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub input_error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgyProcessEvent {
    Started(u32),
    ConversationId(String),
}

#[derive(Serialize)]
struct UserInput<'a> {
    event: &'static str,
    message: UserMessage<'a>,
}

#[derive(Serialize)]
struct UserMessage<'a> {
    content: &'a str,
}

#[derive(Deserialize)]
struct OutputEvent {
    event: String,
    conversation_id: Option<String>,
    result: Option<ResultPayload>,
}

#[derive(Deserialize)]
struct ResultPayload {
    conversation_id: Option<String>,
    status: Option<String>,
    response: Option<String>,
    error: Option<String>,
}

struct CapturedOutput {
    bytes: Vec<u8>,
    truncated: bool,
}

enum WaitOutcome {
    Exited(io::Result<ExitStatus>),
    TimedOut,
    InterruptedByCtrlC,
    InterruptedByTerminateSignal,
}

/// Run one bounded Antigravity turn without putting its prompt in the process argv.
///
/// The caller must create and persist an explicit session-only settings file before launch.
/// The command always enables the CLI sandbox and `accept-edits` mode; this function never
/// uses Antigravity's `auto` mode or its dangerous permission bypass.
pub async fn run<F>(config: AgyRunConfig, mut on_event: F) -> Result<AgyRunResult>
where
    F: FnMut(AgyProcessEvent) -> Result<()>,
{
    validate_config(&config)?;

    let mut command = Command::new(&config.executable);
    command
        .arg("--input-format")
        .arg("stream-json")
        .arg("--output-format")
        .arg("stream-json")
        .arg("--mode")
        .arg("accept-edits")
        .arg("--sandbox")
        .arg("--disable-slash-commands")
        .arg("--settings")
        .arg(&config.settings_file)
        .current_dir(&config.project_root)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);

    if let Some(conversation_id) = &config.conversation_id {
        if conversation_id.is_empty()
            || conversation_id.len() > 200
            || !conversation_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
        {
            bail!("Conversation ID contains unsupported characters");
        }
        command.arg("--conversation").arg(conversation_id);
    }

    #[cfg(unix)]
    command.process_group(0);

    let mut terminate_signal = create_terminate_signal()?;
    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);

    let mut child = command.spawn().with_context(|| {
        format!(
            "Cannot start Antigravity CLI {}",
            config.executable.display()
        )
    })?;
    let process_id = child
        .id()
        .context("Antigravity did not report a process ID")?;
    let mut process_group_guard = ProcessGroupGuard::new(process_id)?;
    if let Err(error) = on_event(AgyProcessEvent::Started(process_id)) {
        stop_process_group(&mut child, process_id)
            .await
            .context("Cannot stop Antigravity after recording its PID failed")?;
        return Err(error).context("Cannot persist Antigravity process identity");
    }
    let stdin = child
        .stdin
        .take()
        .context("Cannot open Antigravity standard input")?;
    let stdout = child
        .stdout
        .take()
        .context("Cannot open Antigravity standard output")?;
    let stderr = child
        .stderr
        .take()
        .context("Cannot open Antigravity standard error")?;

    let prompt = config.prompt;
    let (conversation_tx, mut conversation_rx) = tokio::sync::mpsc::unbounded_channel();
    let stdout_task = tokio::spawn(capture_output(stdout, Some(conversation_tx)));
    let stderr_task = tokio::spawn(capture_output(stderr, None));
    let deadline = time::sleep(config.timeout);
    tokio::pin!(deadline);

    let mut session_recorded = false;
    let input_task = if let Some(expected_id) = config.conversation_id.as_deref() {
        let handshake = tokio::select! {
            status = child.wait() => {
                let _ = status.context("Cannot wait for Antigravity before continuation handshake")?;
                bail!("Antigravity exited before confirming the requested conversation ID; the attempt remains unresolved")
            },
            event = conversation_rx.recv() => event,
            _ = &mut deadline => {
                let _ = stop_process_group(&mut child, process_id).await
                    .context("Cannot stop Antigravity after continuation handshake timeout")?;
                bail!("Antigravity did not confirm the requested conversation ID before timeout; the attempt remains unresolved")
            },
            _ = &mut ctrl_c => {
                let _ = stop_process_group(&mut child, process_id).await
                    .context("Cannot stop Antigravity after continuation cancellation")?;
                bail!("Antigravity continuation was interrupted before the conversation ID was confirmed; the attempt remains unresolved")
            },
            _ = wait_for_terminate_signal(&mut terminate_signal) => {
                let _ = stop_process_group(&mut child, process_id).await
                    .context("Cannot stop Antigravity after termination signal")?;
                bail!("Antigravity continuation was terminated before the conversation ID was confirmed; the attempt remains unresolved")
            },
        };
        let Some(Some(reported_id)) = handshake else {
            bail!(
                "Antigravity did not emit a valid init conversation ID; the attempt remains unresolved"
            );
        };
        if reported_id != expected_id {
            let _ = stop_process_group(&mut child, process_id)
                .await
                .context("Cannot stop Antigravity after a continuation session mismatch")?;
            bail!(
                "Antigravity reported a different conversation ID; no continuation prompt was sent and the attempt remains unresolved"
            );
        }
        on_event(AgyProcessEvent::ConversationId(reported_id.clone()))
            .context("Cannot persist the resumed Antigravity conversation ID")?;
        session_recorded = true;
        tokio::spawn(async move { write_prompt(stdin, &prompt).await })
    } else {
        tokio::spawn(async move { write_prompt(stdin, &prompt).await })
    };

    let outcome = loop {
        let completed = tokio::select! {
            status = child.wait() => Some(WaitOutcome::Exited(status)),
            event = conversation_rx.recv(), if !session_recorded => {
                match event {
                    Some(Some(conversation_id)) => {
                        on_event(AgyProcessEvent::ConversationId(conversation_id))
                            .context("Cannot persist the Antigravity conversation ID")?;
                        session_recorded = true;
                        None
                    }
                    Some(None) | None => {
                        session_recorded = true;
                        None
                    }
                }
            },
            _ = &mut deadline => Some(WaitOutcome::TimedOut),
            _ = &mut ctrl_c => Some(WaitOutcome::InterruptedByCtrlC),
            _ = wait_for_terminate_signal(&mut terminate_signal) => Some(WaitOutcome::InterruptedByTerminateSignal),
        };
        if let Some(completed) = completed {
            break completed;
        }
    };

    let (status, termination) = match outcome {
        WaitOutcome::Exited(status) => (
            status.context("Cannot wait for Antigravity process")?,
            TerminationReason::Exited,
        ),
        WaitOutcome::TimedOut => (
            stop_process_group(&mut child, process_id)
                .await
                .context("Cannot stop timed-out Antigravity process")?,
            TerminationReason::TimedOut,
        ),
        WaitOutcome::InterruptedByCtrlC => (
            stop_process_group(&mut child, process_id)
                .await
                .context("Cannot stop interrupted Antigravity process")?,
            TerminationReason::InterruptedByCtrlC,
        ),
        WaitOutcome::InterruptedByTerminateSignal => (
            stop_process_group(&mut child, process_id)
                .await
                .context("Cannot stop Antigravity after termination signal")?,
            TerminationReason::InterruptedByTerminateSignal,
        ),
    };

    let input_error = finish_task(input_task)
        .await
        .err()
        .map(|error| error.to_string());
    let stdout = finish_capture(stdout_task).await?;
    let stderr = finish_capture(stderr_task).await?;
    let stream = parse_output_stream(&stdout.bytes);
    cleanup_remaining_process_group(process_id).await?;
    process_group_guard.disarm();

    Ok(AgyRunResult {
        process_id,
        exit_code: status.code(),
        termination,
        conversation_id: stream.conversation_id,
        agent_status: stream.status,
        response: stream.response,
        agent_error: stream.error,
        stderr_excerpt: excerpt(&stderr.bytes),
        stdout_truncated: stdout.truncated,
        stderr_truncated: stderr.truncated,
        input_error,
    })
}

fn validate_config(config: &AgyRunConfig) -> Result<()> {
    if !config.project_root.is_absolute() || !config.project_root.is_dir() {
        bail!("Antigravity project root must be an existing absolute directory");
    }
    if config.timeout.is_zero() {
        bail!("Antigravity process timeout must be greater than zero");
    }
    let settings = fs::symlink_metadata(&config.settings_file).with_context(|| {
        format!(
            "Cannot inspect Antigravity settings {}",
            config.settings_file.display()
        )
    })?;
    if settings.file_type().is_symlink() || !settings.is_file() {
        bail!("Antigravity settings path must be a regular, non-symlink file");
    }
    Ok(())
}

async fn write_prompt(mut stdin: tokio::process::ChildStdin, prompt: &str) -> io::Result<()> {
    let input = UserInput {
        event: "user",
        message: UserMessage { content: prompt },
    };
    let mut bytes = serde_json::to_vec(&input).map_err(io::Error::other)?;
    bytes.push(b'\n');
    stdin.write_all(&bytes).await?;
    stdin.shutdown().await
}

async fn capture_output<R>(
    mut reader: R,
    conversation_tx: Option<tokio::sync::mpsc::UnboundedSender<Option<String>>>,
) -> io::Result<CapturedOutput>
where
    R: AsyncRead + Unpin,
{
    let mut bytes = Vec::with_capacity(MAX_CAPTURE_BYTES.min(64 * 1024));
    let mut chunk = [0_u8; 8192];
    let mut truncated = false;
    let mut event_line = Vec::new();
    let mut event_line_truncated = false;
    let mut init_event_sent = false;
    loop {
        let read = reader.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        let remaining = MAX_CAPTURE_BYTES.saturating_sub(bytes.len());
        let retained = remaining.min(read);
        bytes.extend_from_slice(&chunk[..retained]);
        truncated |= retained < read;
        if !init_event_sent && let Some(conversation_tx) = &conversation_tx {
            for byte in &chunk[..read] {
                if *byte == b'\n' {
                    if !event_line_truncated
                        && let Some(conversation_id) = parse_init_conversation_id(&event_line)
                    {
                        let _ = conversation_tx.send(conversation_id);
                        init_event_sent = true;
                        break;
                    }
                    event_line.clear();
                    event_line_truncated = false;
                } else if !event_line_truncated {
                    if event_line.len() < MAX_EVENT_LINE_BYTES {
                        event_line.push(*byte);
                    } else {
                        event_line_truncated = true;
                    }
                }
            }
        }
    }
    Ok(CapturedOutput { bytes, truncated })
}

fn parse_init_conversation_id(line: &[u8]) -> Option<Option<String>> {
    let event = serde_json::from_slice::<OutputEvent>(line).ok()?;
    if event.event != "init" {
        return None;
    }
    Some(event.conversation_id.filter(|value| {
        !value.is_empty()
            && value.len() <= 200
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    }))
}

async fn finish_capture(
    mut task: JoinHandle<io::Result<CapturedOutput>>,
) -> Result<CapturedOutput> {
    match time::timeout(STREAM_JOIN_TIMEOUT, &mut task).await {
        Ok(joined) => joined
            .context("Antigravity output reader task failed")?
            .context("Cannot read Antigravity output"),
        Err(_) => {
            task.abort();
            bail!("Antigravity output stream did not close after the process exited")
        }
    }
}

async fn finish_task(mut task: JoinHandle<io::Result<()>>) -> io::Result<()> {
    match time::timeout(STREAM_JOIN_TIMEOUT, &mut task).await {
        Ok(Ok(result)) => result,
        Ok(Err(error)) => Err(io::Error::other(format!("input task failed: {error}"))),
        Err(_) => {
            task.abort();
            let _ = task.await;
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Antigravity input pipe did not close",
            ))
        }
    }
}

struct ParsedStream {
    conversation_id: Option<String>,
    status: Option<String>,
    response: Option<String>,
    error: Option<String>,
}

fn parse_output_stream(bytes: &[u8]) -> ParsedStream {
    let mut parsed = ParsedStream {
        conversation_id: None,
        status: None,
        response: None,
        error: None,
    };
    for line in bytes.split(|byte| *byte == b'\n') {
        let Ok(event) = serde_json::from_slice::<OutputEvent>(line) else {
            continue;
        };
        if parsed.conversation_id.is_none() {
            parsed.conversation_id = event.conversation_id;
        }
        if event.event == "result"
            && let Some(result) = event.result
        {
            parsed.conversation_id = result.conversation_id.or(parsed.conversation_id);
            parsed.status = result.status;
            parsed.response = result.response;
            parsed.error = result.error;
        }
    }
    parsed
}

fn excerpt(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let text = text.trim();
    text.chars().take(4096).collect()
}

#[cfg(unix)]
fn create_terminate_signal() -> Result<Option<tokio::signal::unix::Signal>> {
    use tokio::signal::unix::{SignalKind, signal};

    signal(SignalKind::terminate())
        .context("Cannot subscribe to the termination signal")
        .map(Some)
}

#[cfg(not(unix))]
fn create_terminate_signal() -> Result<Option<()>> {
    Ok(None)
}

#[cfg(unix)]
async fn wait_for_terminate_signal(signal: &mut Option<tokio::signal::unix::Signal>) {
    if let Some(signal) = signal {
        let _ = signal.recv().await;
    } else {
        std::future::pending::<()>().await;
    }
}

#[cfg(not(unix))]
async fn wait_for_terminate_signal(_signal: &mut Option<()>) {
    std::future::pending::<()>().await;
}

#[cfg(unix)]
async fn stop_process_group(child: &mut Child, process_id: u32) -> io::Result<ExitStatus> {
    let group_id = i32::try_from(process_id)
        .map_err(|_| io::Error::other("Antigravity process ID is out of range"))?;
    let grace_deadline = Instant::now() + TERMINATION_GRACE;
    signal_process_group(group_id, libc::SIGTERM);

    let status = match time::timeout(TERMINATION_GRACE, child.wait()).await {
        Ok(status) => status?,
        Err(_) => {
            signal_process_group(group_id, libc::SIGKILL);
            return child.wait().await;
        }
    };

    time::sleep_until(grace_deadline).await;
    signal_process_group(group_id, libc::SIGKILL);
    Ok(status)
}

#[cfg(unix)]
fn signal_process_group(group_id: i32, signal: i32) {
    let result = unsafe { libc::kill(-group_id, signal) };
    if result != 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::ESRCH) {
            tracing::warn!(%error, group_id, signal, "Could not signal Antigravity process group");
        }
    }
}

#[cfg(unix)]
async fn cleanup_remaining_process_group(process_id: u32) -> Result<()> {
    let group_id = i32::try_from(process_id)
        .context("Antigravity process ID is out of range for its Unix process group")?;
    if !process_group_exists(group_id) {
        return Ok(());
    }

    signal_process_group(group_id, libc::SIGTERM);
    time::sleep(TERMINATION_GRACE).await;
    signal_process_group(group_id, libc::SIGKILL);
    Ok(())
}

#[cfg(unix)]
fn process_group_exists(group_id: i32) -> bool {
    let result = unsafe { libc::kill(-group_id, 0) };
    result == 0 || io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

#[cfg(not(unix))]
async fn cleanup_remaining_process_group(_process_id: u32) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
struct ProcessGroupGuard {
    group_id: i32,
    armed: bool,
}

#[cfg(unix)]
impl ProcessGroupGuard {
    fn new(process_id: u32) -> Result<Self> {
        let group_id = i32::try_from(process_id)
            .context("Antigravity process ID is out of range for its Unix process group")?;
        Ok(Self {
            group_id,
            armed: true,
        })
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

#[cfg(unix)]
impl Drop for ProcessGroupGuard {
    fn drop(&mut self) {
        if self.armed {
            signal_process_group(self.group_id, libc::SIGKILL);
        }
    }
}

#[cfg(not(unix))]
struct ProcessGroupGuard;

#[cfg(not(unix))]
impl ProcessGroupGuard {
    fn new(_process_id: u32) -> Result<Self> {
        Ok(Self)
    }

    fn disarm(&mut self) {}
}

#[cfg(not(unix))]
async fn stop_process_group(child: &mut Child, _process_id: u32) -> io::Result<ExitStatus> {
    child.kill().await?;
    child.wait().await
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        os::unix::fs::PermissionsExt,
        path::PathBuf,
        process::Command,
        sync::atomic::{AtomicU64, Ordering},
        time::{Duration, Instant},
    };

    use super::{AgyRunConfig, TerminationReason, parse_output_stream, run};

    static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

    fn test_dir() -> PathBuf {
        let sequence = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "talos-agy-process-test-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("create temporary process test directory");
        path
    }

    #[test]
    fn parses_final_result_and_conversation_id_from_ndjson() {
        let input = concat!(
            "{\"event\":\"init\",\"conversation_id\":\"conversation-1\"}\n",
            "{\"event\":\"step_update\",\"step_update\":{}}\n",
            "{\"event\":\"result\",\"result\":{\"conversation_id\":\"conversation-1\",\"status\":\"SUCCESS\",\"response\":\"done\"}}\n"
        );
        let parsed = parse_output_stream(input.as_bytes());

        assert_eq!(parsed.conversation_id.as_deref(), Some("conversation-1"));
        assert_eq!(parsed.status.as_deref(), Some("SUCCESS"));
        assert_eq!(parsed.response.as_deref(), Some("done"));
        assert_eq!(parsed.error, None);
    }

    #[test]
    fn ignores_partial_or_non_json_stream_lines() {
        let input = b"not json\n{\"event\":\"result\",\"result\":{\"status\":\"ERROR\",\"error\":\"failed\"}}\n{";
        let parsed = parse_output_stream(input);

        assert_eq!(parsed.status.as_deref(), Some("ERROR"));
        assert_eq!(parsed.error.as_deref(), Some("failed"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn sends_prompt_through_stdin_and_parses_the_result() {
        let root = test_dir();
        let settings_file = root.join("settings.json");
        let executable = root.join("fake-agy");
        fs::write(&settings_file, "{}\n").expect("write temporary settings");
        fs::write(
            &executable,
            "#!/bin/sh\nfor arg in \"$@\"; do case \"$arg\" in *SECRET_PROMPT*) exit 90;; esac; done\ncat >/dev/null\nprintf '%s\\n' '{\"event\":\"result\",\"result\":{\"conversation_id\":\"test-conversation\",\"status\":\"SUCCESS\",\"response\":\"ready\"}}'\n",
        )
        .expect("write fake CLI");
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700))
            .expect("make fake CLI executable");

        let result = run(
            AgyRunConfig {
                executable,
                project_root: root.clone(),
                settings_file,
                prompt: "SECRET_PROMPT must stay off argv".to_owned(),
                conversation_id: None,
                timeout: Duration::from_secs(5),
            },
            |_| Ok(()),
        )
        .await
        .expect("run fake Antigravity CLI");

        assert_eq!(result.termination, TerminationReason::Exited);
        assert_eq!(result.exit_code, Some(0));
        assert_eq!(result.conversation_id.as_deref(), Some("test-conversation"));
        assert_eq!(result.agent_status.as_deref(), Some("SUCCESS"));
        assert_eq!(result.response.as_deref(), Some("ready"));
        assert_eq!(result.input_error, None);
        fs::remove_dir_all(root).expect("remove temporary test directory");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn continuation_waits_for_the_matching_init_id_before_sending_prompt() {
        let root = test_dir();
        let settings_file = root.join("settings.json");
        let executable = root.join("fake-agy");
        let callback_marker = root.join("session-persisted");
        fs::write(&settings_file, "{}\n").expect("write temporary settings");
        fs::write(
            &executable,
            format!(
                "#!/bin/sh\nfound=0\nwhile [ \"$#\" -gt 0 ]; do\n  if [ \"$1\" = \"--conversation\" ]; then shift; [ \"$1\" = \"saved-session\" ] || exit 91; found=1; fi\n  shift\ndone\n[ \"$found\" -eq 1 ] || exit 92\nprintf '%s\\n' '{{\"event\":\"init\",\"conversation_id\":\"saved-session\"}}'\nIFS= read -r input || exit 93\n[ -f '{}' ] || exit 94\ncase \"$input\" in *'Continue without replaying changes'*) ;; *) exit 95;; esac\nprintf '%s\\n' '{{\"event\":\"result\",\"result\":{{\"conversation_id\":\"saved-session\",\"status\":\"SUCCESS\",\"response\":\"Continued safely.\"}}}}'\n",
                callback_marker.display()
            ),
        )
        .expect("write fake CLI");
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700))
            .expect("make fake CLI executable");

        let mut persisted_id = None;
        let result = run(
            AgyRunConfig {
                executable,
                project_root: root.clone(),
                settings_file,
                prompt: "Continue without replaying changes".to_owned(),
                conversation_id: Some("saved-session".to_owned()),
                timeout: Duration::from_secs(5),
            },
            |event| match event {
                super::AgyProcessEvent::Started(_) => Ok(()),
                super::AgyProcessEvent::ConversationId(conversation_id) => {
                    fs::write(&callback_marker, &conversation_id)
                        .expect("persist test session marker");
                    persisted_id = Some(conversation_id);
                    Ok(())
                }
            },
        )
        .await
        .expect("continue fake Antigravity conversation");

        assert_eq!(persisted_id.as_deref(), Some("saved-session"));
        assert_eq!(result.conversation_id.as_deref(), Some("saved-session"));
        assert_eq!(result.agent_status.as_deref(), Some("SUCCESS"));
        assert_eq!(result.response.as_deref(), Some("Continued safely."));
        fs::remove_dir_all(root).expect("remove temporary test directory");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn continuation_id_mismatch_never_sends_prompt() {
        let root = test_dir();
        let settings_file = root.join("settings.json");
        let executable = root.join("fake-agy");
        let prompt_received = root.join("unexpected-prompt");
        fs::write(&settings_file, "{}\n").expect("write temporary settings");
        fs::write(
            &executable,
            format!(
                "#!/bin/sh\nprintf '%s\\n' '{{\"event\":\"init\",\"conversation_id\":\"different-session\"}}'\nIFS= read -r input && printf '%s' \"$input\" > '{}'\nexec sleep 30\n",
                prompt_received.display()
            ),
        )
        .expect("write fake CLI");
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700))
            .expect("make fake CLI executable");

        let result = run(
            AgyRunConfig {
                executable,
                project_root: root.clone(),
                settings_file,
                prompt: "Must not be sent to a different conversation".to_owned(),
                conversation_id: Some("saved-session".to_owned()),
                timeout: Duration::from_secs(5),
            },
            |_| Ok(()),
        )
        .await;

        assert!(result.is_err());
        assert!(!prompt_received.exists());
        fs::remove_dir_all(root).expect("remove temporary test directory");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn timeout_terminates_and_reaps_the_process_group() {
        let root = test_dir();
        let settings_file = root.join("settings.json");
        let executable = root.join("fake-agy");
        fs::write(&settings_file, "{}\n").expect("write temporary settings");
        fs::write(&executable, "#!/bin/sh\nexec sleep 30\n").expect("write fake CLI");
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700))
            .expect("make fake CLI executable");

        let result = run(
            AgyRunConfig {
                executable,
                project_root: root.clone(),
                settings_file,
                prompt: "Stop after the timeout".to_owned(),
                conversation_id: None,
                timeout: Duration::from_millis(100),
            },
            |_| Ok(()),
        )
        .await
        .expect("run fake Antigravity CLI");

        assert_eq!(result.termination, TerminationReason::TimedOut);
        assert_eq!(result.exit_code, None);
        fs::remove_dir_all(root).expect("remove temporary test directory");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn successful_exit_cleans_up_a_detached_descendant_in_the_process_group() {
        let root = test_dir();
        let settings_file = root.join("settings.json");
        let executable = root.join("fake-agy");
        let descendant_marker = root.join("descendant-survived");
        fs::write(&settings_file, "{}\n").expect("write temporary settings");
        fs::write(
            &executable,
            format!(
                "#!/bin/sh\n(sleep 2; printf alive > '{}') </dev/null >/dev/null 2>&1 &\nprintf '%s\\n' '{{\"event\":\"result\",\"result\":{{\"status\":\"SUCCESS\",\"response\":\"done\"}}}}'\n",
                descendant_marker.display()
            ),
        )
        .expect("write fake CLI");
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700))
            .expect("make fake CLI executable");

        let result = run(
            AgyRunConfig {
                executable,
                project_root: root.clone(),
                settings_file,
                prompt: "Finish and leave no child process".to_owned(),
                conversation_id: None,
                timeout: Duration::from_secs(5),
            },
            |_| Ok(()),
        )
        .await
        .expect("run fake Antigravity CLI");
        assert_eq!(result.termination, TerminationReason::Exited);
        assert_eq!(result.agent_status.as_deref(), Some("SUCCESS"));
        tokio::time::sleep(Duration::from_millis(1300)).await;
        assert!(!descendant_marker.exists());

        fs::remove_dir_all(root).expect("remove temporary test directory");
    }

    #[cfg(unix)]
    #[test]
    fn ctrl_c_and_sigterm_interrupt_the_supervisor_and_reap_the_writer() {
        for signal in ["interrupt", "terminate"] {
            let root = test_dir();
            let settings_file = root.join("settings.json");
            let executable = root.join("fake-agy");
            let ready_file = root.join("writer-started");
            fs::write(&settings_file, "{}\n").expect("write temporary settings");
            fs::write(
                &executable,
                format!(
                    "#!/bin/sh\nprintf started > '{}'\nexec sleep 30\n",
                    ready_file.display()
                ),
            )
            .expect("write fake CLI");
            fs::set_permissions(&executable, fs::Permissions::from_mode(0o700))
                .expect("make fake CLI executable");

            let mut helper = Command::new(std::env::current_exe().expect("current test binary"))
                .args([
                    "--exact",
                    "adapter::agy_process::tests::signal_helper_subprocess",
                    "--ignored",
                    "--nocapture",
                ])
                .env("TALOS_SIGNAL_TEST_ROOT", &root)
                .env("TALOS_SIGNAL_TEST_KIND", signal)
                .spawn()
                .expect("start signal helper process");

            let deadline = Instant::now() + Duration::from_secs(5);
            while !ready_file.exists() && Instant::now() < deadline {
                assert!(helper.try_wait().expect("check helper process").is_none());
                std::thread::sleep(Duration::from_millis(20));
            }
            assert!(
                ready_file.exists(),
                "fake Writer did not start for {signal}"
            );

            let os_signal = if signal == "interrupt" {
                libc::SIGINT
            } else {
                libc::SIGTERM
            };
            let signal_result = unsafe { libc::kill(helper.id() as i32, os_signal) };
            assert_eq!(signal_result, 0, "could not send {signal}");

            let deadline = Instant::now() + Duration::from_secs(5);
            let status = loop {
                if let Some(status) = helper.try_wait().expect("check helper result") {
                    break status;
                }
                if Instant::now() >= deadline {
                    let _ = helper.kill();
                    panic!("supervisor did not finish after {signal}");
                }
                std::thread::sleep(Duration::from_millis(20));
            };
            assert!(status.success(), "signal helper failed for {signal}");
            fs::remove_dir_all(root).expect("remove temporary test directory");
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    #[ignore = "Started by the signal-forwarding test"]
    async fn signal_helper_subprocess() {
        let Ok(root) = std::env::var("TALOS_SIGNAL_TEST_ROOT") else {
            return;
        };
        let root = PathBuf::from(root);
        let signal = std::env::var("TALOS_SIGNAL_TEST_KIND").expect("signal kind");
        let result = run(
            AgyRunConfig {
                executable: root.join("fake-agy"),
                project_root: root.clone(),
                settings_file: root.join("settings.json"),
                prompt: "Signal test".to_owned(),
                conversation_id: None,
                timeout: Duration::from_secs(20),
            },
            |_| Ok(()),
        )
        .await
        .expect("run signal test process");

        let expected = if signal == "interrupt" {
            TerminationReason::InterruptedByCtrlC
        } else {
            TerminationReason::InterruptedByTerminateSignal
        };
        assert_eq!(result.termination, expected);
    }
}
