use std::{
    collections::BTreeMap,
    env, io,
    path::Path,
    process::{Command as StdCommand, ExitStatus, Stdio},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    process::Command,
    time::timeout,
};

use crate::{db::GithubCiCheckRecord, process_group::ProcessGroupGuard};

const COMMAND_TIMEOUT: Duration = Duration::from_secs(15);
const NETWORK_READ_TIMEOUT: Duration = Duration::from_secs(60);
const NETWORK_WRITE_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const PULL_REQUEST_TIMEOUT: Duration = Duration::from_secs(2 * 60);
const CI_QUERY_TIMEOUT: Duration = Duration::from_secs(60);
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
    run_gh_with_timeout(arguments, COMMAND_TIMEOUT).await
}

async fn run_gh_with_timeout(arguments: &[&str], duration: Duration) -> CommandResult {
    run_bounded_command("gh", arguments, None, duration, false, None).await
}

async fn run_bounded_command(
    executable: &str,
    arguments: &[&str],
    working_directory: Option<&Path>,
    duration: Duration,
    clear_git_environment: bool,
    stdin_data: Option<&[u8]>,
) -> CommandResult {
    let mut command = Command::new(executable);
    command
        .args(arguments)
        .stdin(if stdin_data.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(working_directory) = working_directory {
        command.current_dir(working_directory);
    }
    if clear_git_environment {
        command
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_OPTIONAL_LOCKS", "0");
        for (key, _) in env::vars_os().filter(|(key, _)| key.to_string_lossy().starts_with("GIT_"))
        {
            command.env_remove(key);
        }
        command
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_OPTIONAL_LOCKS", "0");
    }
    #[cfg(unix)]
    command.process_group(0);

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return CommandResult::NotInstalled;
        }
        Err(_) => return CommandResult::Failed,
    };
    #[cfg(unix)]
    let mut process_group = match child.id() {
        Some(process_id) => match ProcessGroupGuard::new(process_id) {
            Ok(guard) => Some(guard),
            Err(_) => {
                let _ = child.kill().await;
                return CommandResult::Failed;
            }
        },
        None => None,
    };

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let stdin = child.stdin.take();
    let result = timeout(duration, async {
        let (stdin_result, stdout, stderr_truncated, status) = tokio::join!(
            write_child_stdin(stdin, stdin_data),
            capture_bounded(stdout),
            discard_output(stderr),
            child.wait(),
        );
        stdin_result?;
        let (stdout, stdout_truncated) = stdout?;
        let stderr_truncated = stderr_truncated?;
        let status = status?;
        Ok::<_, io::Error>((status, stdout, stdout_truncated || stderr_truncated))
    })
    .await;

    match result {
        Ok(Ok((status, stdout, output_truncated))) => {
            #[cfg(unix)]
            if let Some(process_group) = &mut process_group {
                process_group.disarm();
            }
            CommandResult::Output {
                status,
                stdout,
                output_truncated,
            }
        }
        Ok(Err(_)) => CommandResult::Failed,
        Err(_) => {
            #[cfg(unix)]
            if let Some(process_group) = &mut process_group {
                let _ = process_group.terminate_and_wait(&mut child).await;
            } else {
                let _ = child.kill().await;
            }
            #[cfg(not(unix))]
            let _ = child.kill().await;
            CommandResult::TimedOut
        }
    }
}

pub fn validate_remote_identity(project_root: &Path, remote: &str, repository: &str) -> Result<()> {
    let fetch_urls = configured_remote_urls(project_root, remote, false)?;
    let push_urls = configured_remote_urls(project_root, remote, true)?;
    if fetch_urls.len() != 1 || push_urls.len() != 1 {
        bail!("Configured GitHub remote must have exactly one fetch URL and one push URL");
    }
    for url in fetch_urls.iter().chain(push_urls.iter()) {
        let identity = parse_github_remote_identity(url)
            .context("Configured Git remote must identify one github.com repository")?;
        if !identity.eq_ignore_ascii_case(repository) {
            bail!("Configured Git remote does not match the GitHub policy repository");
        }
    }
    Ok(())
}

pub async fn repository_default_branch_matches(repository: &str, expected_base: &str) -> bool {
    let arguments = [
        "repo",
        "view",
        repository,
        "--json",
        "nameWithOwner,defaultBranchRef",
    ];
    let CommandResult::Output {
        status,
        stdout,
        output_truncated: false,
    } = run_gh_with_timeout(&arguments, NETWORK_READ_TIMEOUT).await
    else {
        return false;
    };
    if !status.success() {
        return false;
    }
    let Ok(view) = serde_json::from_slice::<RepositoryView>(&stdout) else {
        return false;
    };
    view.name_with_owner.eq_ignore_ascii_case(repository)
        && view
            .default_branch_ref
            .is_some_and(|branch| branch.name == expected_base)
}

pub async fn remote_ref_sha(
    project_root: &Path,
    remote: &str,
    branch: &str,
) -> Result<Option<String>> {
    let full_ref = format!("refs/heads/{branch}");
    let arguments = ["ls-remote", "--heads", remote, &full_ref];
    match run_bounded_command(
        "git",
        &arguments,
        Some(project_root),
        NETWORK_READ_TIMEOUT,
        true,
        None,
    )
    .await
    {
        CommandResult::Output {
            status,
            stdout,
            output_truncated: false,
        } if status.success() => parse_ls_remote(&stdout, &full_ref),
        CommandResult::TimedOut => bail!("GitHub remote branch lookup timed out"),
        _ => bail!("GitHub remote branch lookup failed"),
    }
}

pub async fn create_draft_pull_request(
    repository: &str,
    base: &str,
    head: &str,
    title: &str,
    body: &str,
) -> Option<(u32, String)> {
    let arguments = [
        "pr",
        "create",
        "--repo",
        repository,
        "--base",
        base,
        "--head",
        head,
        "--title",
        title,
        "--body-file",
        "-",
        "--draft",
        "--no-maintainer-edit",
    ];
    let CommandResult::Output {
        status,
        stdout,
        output_truncated: false,
    } = run_bounded_command(
        "gh",
        &arguments,
        None,
        PULL_REQUEST_TIMEOUT,
        false,
        Some(body.as_bytes()),
    )
    .await
    else {
        return None;
    };
    if !status.success() {
        return None;
    }
    parse_pull_request_url(&stdout, repository)
}

pub async fn find_matching_open_pull_request(
    repository: &str,
    head: &str,
    base: &str,
    head_commit: &str,
) -> PullRequestInspection {
    let arguments = [
        "pr",
        "list",
        "--repo",
        repository,
        "--head",
        head,
        "--state",
        "all",
        "--json",
        "number,url,state,headRefOid,baseRefName",
    ];
    let CommandResult::Output {
        status,
        stdout,
        output_truncated: false,
    } = run_gh_with_timeout(&arguments, NETWORK_READ_TIMEOUT).await
    else {
        return PullRequestInspection::Unknown;
    };
    if !status.success() {
        return PullRequestInspection::Unknown;
    }
    let Ok(results) = serde_json::from_slice::<Vec<PullRequestSummary>>(&stdout) else {
        return PullRequestInspection::Unknown;
    };
    if results.is_empty() {
        return PullRequestInspection::None;
    }
    if results.len() != 1 {
        return PullRequestInspection::OtherOrAmbiguous;
    }
    let Some(result) = results.into_iter().next() else {
        return PullRequestInspection::Unknown;
    };
    if result.state != "OPEN" || result.head_ref_oid != head_commit || result.base_ref_name != base
    {
        return PullRequestInspection::OtherOrAmbiguous;
    }
    let Some(number) = parse_repository_pull_request_url(&result.url, repository) else {
        return PullRequestInspection::OtherOrAmbiguous;
    };
    if number == result.number && number > 0 {
        PullRequestInspection::MatchingOpen {
            number,
            url: result.url,
        }
    } else {
        PullRequestInspection::OtherOrAmbiguous
    }
}

pub async fn pull_request_checks(
    repository: &str,
    pull_request_number: u32,
    timeout: Duration,
) -> Option<Vec<GithubCiCheckRecord>> {
    let number = pull_request_number.to_string();
    let arguments = [
        "pr",
        "checks",
        &number,
        "--repo",
        repository,
        "--json",
        "bucket,completedAt,name,startedAt,state,workflow",
    ];
    let CommandResult::Output {
        status,
        stdout,
        output_truncated: false,
    } = run_bounded_command(
        "gh",
        &arguments,
        None,
        timeout.min(CI_QUERY_TIMEOUT),
        false,
        None,
    )
    .await
    else {
        return None;
    };
    if !matches!(status.code(), Some(0 | 8)) {
        return None;
    }
    let checks = serde_json::from_slice::<Vec<GithubCiCheckRecord>>(&stdout).ok()?;
    (checks.len() <= 128).then_some(checks)
}

pub async fn push_process_result(
    project_root: &Path,
    remote: &str,
    branch: &str,
    head_commit: &str,
) -> &'static str {
    let destination = format!("{head_commit}:refs/heads/{branch}");
    match run_bounded_command(
        "git",
        &[
            "push",
            "--porcelain",
            "--no-follow-tags",
            remote,
            &destination,
        ],
        Some(project_root),
        NETWORK_WRITE_TIMEOUT,
        true,
        None,
    )
    .await
    {
        CommandResult::Output {
            status,
            output_truncated: false,
            ..
        } if status.success() => "pushed",
        CommandResult::TimedOut => "timed_out",
        _ => "unknown",
    }
}

#[derive(Deserialize)]
struct RepositoryView {
    #[serde(rename = "nameWithOwner")]
    name_with_owner: String,
    #[serde(rename = "defaultBranchRef")]
    default_branch_ref: Option<DefaultBranchRef>,
}

#[derive(Deserialize)]
struct DefaultBranchRef {
    name: String,
}

#[derive(Deserialize)]
struct PullRequestSummary {
    number: u32,
    url: String,
    state: String,
    #[serde(rename = "headRefOid")]
    head_ref_oid: String,
    #[serde(rename = "baseRefName")]
    base_ref_name: String,
}

pub enum PullRequestInspection {
    None,
    MatchingOpen { number: u32, url: String },
    OtherOrAmbiguous,
    Unknown,
}

fn configured_remote_urls(project_root: &Path, remote: &str, push: bool) -> Result<Vec<String>> {
    let arguments = if push {
        vec!["remote", "get-url", "--push", "--all", remote]
    } else {
        vec!["remote", "get-url", "--all", remote]
    };
    let mut command = StdCommand::new("git");
    command
        .args(&arguments)
        .current_dir(project_root)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0");
    for (key, _) in env::vars_os().filter(|(key, _)| key.to_string_lossy().starts_with("GIT_")) {
        command.env_remove(key);
    }
    let output = command
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()
        .context("Cannot inspect the configured GitHub remote")?;
    if !output.status.success() {
        bail!("Cannot inspect the configured GitHub remote");
    }
    let urls = std::str::from_utf8(&output.stdout)
        .context("Configured Git remote URL is not valid UTF-8")?
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    Ok(urls)
}

fn parse_github_remote_identity(url: &str) -> Option<String> {
    let (host, path) = if let Some(rest) = url.strip_prefix("https://") {
        let (authority, path) = rest.split_once('/')?;
        if authority.contains('@') || authority.contains(':') {
            return None;
        }
        (authority, path)
    } else if let Some(path) = url.strip_prefix("git@github.com:") {
        ("github.com", path)
    } else if let Some(rest) = url.strip_prefix("ssh://git@github.com/") {
        ("github.com", rest)
    } else if let Some(rest) = url.strip_prefix("ssh://github.com/") {
        ("github.com", rest)
    } else {
        return None;
    };
    if host != "github.com"
        || path.contains(['?', '#', '\\'])
        || path.bytes().any(|byte| byte.is_ascii_whitespace())
    {
        return None;
    }
    let path = path.strip_suffix(".git").unwrap_or(path);
    let (owner, repository) = path.split_once('/')?;
    if owner.is_empty()
        || repository.is_empty()
        || repository.contains('/')
        || !owner
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        || !repository
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return None;
    }
    Some(format!("{owner}/{repository}"))
}

fn parse_ls_remote(stdout: &[u8], expected_ref: &str) -> Result<Option<String>> {
    let output = std::str::from_utf8(stdout).context("Git remote returned invalid UTF-8")?;
    let lines = output.lines().collect::<Vec<_>>();
    if lines.is_empty() {
        return Ok(None);
    }
    if lines.len() != 1 {
        bail!("GitHub remote returned an ambiguous branch result");
    }
    let Some((sha, reference)) = lines[0].split_once('\t') else {
        bail!("GitHub remote returned an invalid branch result");
    };
    if reference != expected_ref
        || !matches!(sha.len(), 40 | 64)
        || !sha
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!("GitHub remote returned an invalid branch result");
    }
    Ok(Some(sha.to_owned()))
}

fn parse_pull_request_url(stdout: &[u8], repository: &str) -> Option<(u32, String)> {
    let output = std::str::from_utf8(stdout).ok()?;
    for token in output.split_whitespace() {
        let url = token.trim_matches(|character: char| "()[]<>.,;\"'".contains(character));
        if let Some(number) = parse_repository_pull_request_url(url, repository) {
            return Some((number, url.to_owned()));
        }
    }
    None
}

fn parse_repository_pull_request_url(url: &str, repository: &str) -> Option<u32> {
    let rest = url.strip_prefix("https://")?;
    let (host, path) = rest.split_once('/')?;
    if !host.eq_ignore_ascii_case("github.com") {
        return None;
    }
    let (url_repository, number) = path.split_once("/pull/")?;
    if !url_repository.eq_ignore_ascii_case(repository)
        || number.is_empty()
        || !number.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    let number = number.parse::<u32>().ok()?;
    (number > 0).then_some(number)
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

async fn write_child_stdin<R: AsyncWrite + Unpin>(
    stdin: Option<R>,
    data: Option<&[u8]>,
) -> io::Result<()> {
    match (stdin, data) {
        (Some(mut stdin), Some(data)) => stdin.write_all(data).await,
        (None, None) => Ok(()),
        _ => Err(io::Error::other(
            "Child standard input was not piped as requested",
        )),
    }
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
