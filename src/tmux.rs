use std::{
    env,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use anyhow::{Context, Result, bail};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::secrets::redact_sensitive;

#[derive(Clone, Debug, Serialize)]
pub struct TmuxProbe {
    pub available: bool,
    pub version: Option<String>,
}

#[derive(Clone, Debug)]
pub struct TmuxClient {
    executable: PathBuf,
    server_socket: Option<PathBuf>,
}

impl Default for TmuxClient {
    fn default() -> Self {
        Self {
            executable: PathBuf::from("tmux"),
            server_socket: None,
        }
    }
}

impl TmuxClient {
    fn command(&self) -> Command {
        let mut command = Command::new(&self.executable);
        if let Some(socket) = &self.server_socket {
            command.arg("-S").arg(socket);
        }
        command
    }

    pub fn probe(&self) -> TmuxProbe {
        match self.command().arg("-V").output() {
            Ok(output) if output.status.success() => TmuxProbe {
                available: true,
                version: String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .next()
                    .map(str::trim)
                    .filter(|line| !line.is_empty())
                    .map(redact_sensitive),
            },
            _ => TmuxProbe {
                available: false,
                version: None,
            },
        }
    }

    pub fn ensure_project_session(&self, project_root: &Path) -> Result<String> {
        let project_root = project_root
            .canonicalize()
            .with_context(|| format!("Cannot resolve project root {}", project_root.display()))?;
        let session_name = project_session_name(&project_root);
        if self.has_session(&session_name)? {
            return Ok(session_name);
        }

        let output = self
            .command()
            .arg("new-session")
            .arg("-d")
            .arg("-s")
            .arg(&session_name)
            .arg("-c")
            .arg(&project_root)
            .output()
            .with_context(|| {
                format!(
                    "Cannot start tmux session {session_name}; install tmux or use TALOS in the foreground"
                )
            })?;
        if output.status.success() || self.has_session(&session_name)? {
            return Ok(session_name);
        }
        bail!(
            "Cannot create tmux control session {session_name}: {}",
            safe_stderr(&output)
        )
    }

    pub fn attach_project_session(&self, session_name: &str) -> Result<()> {
        let target = exact_session_target(session_name)?;
        if env::var_os("TMUX").is_some() {
            let current = self
                .command()
                .arg("display-message")
                .arg("-p")
                .arg("#{session_name}")
                .output()
                .context("Cannot inspect the current tmux client")?;
            if current.status.success()
                && String::from_utf8_lossy(&current.stdout).trim() == session_name
            {
                return Ok(());
            }
            self.run_interactive("switch-client", &target)
        } else {
            self.run_interactive("attach-session", &target)
        }
    }

    pub fn has_session(&self, session_name: &str) -> Result<bool> {
        let target = exact_session_target(session_name)?;
        let output = self
            .command()
            .arg("has-session")
            .arg("-t")
            .arg(&target)
            .output()
            .context("Cannot run tmux; install tmux or use TALOS in the foreground")?;
        Ok(output.status.success())
    }

    fn run_interactive(&self, subcommand: &str, target: &str) -> Result<()> {
        let status = self
            .command()
            .arg(subcommand)
            .arg("-t")
            .arg(target)
            .status()
            .with_context(|| format!("Cannot run tmux {subcommand}"))?;
        if !status.success() {
            bail!("tmux {subcommand} exited with {status}");
        }
        Ok(())
    }
}

pub fn project_session_name(project_root: &Path) -> String {
    let digest = Sha256::digest(project_root.to_string_lossy().as_bytes());
    let mut suffix = String::with_capacity(16);
    for byte in digest.iter().take(8) {
        suffix.push_str(&format!("{byte:02x}"));
    }
    format!("talos-{suffix}")
}

fn exact_session_target(session_name: &str) -> Result<String> {
    if session_name.len() != 22
        || !session_name.starts_with("talos-")
        || !session_name[6..]
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        bail!("TALOS tmux session name is invalid");
    }
    Ok(format!("={session_name}"))
}

fn safe_stderr(output: &Output) -> String {
    let text = String::from_utf8_lossy(&output.stderr);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        format!("tmux exited with {}", output.status)
    } else {
        redact_sensitive(trimmed.chars().take(512).collect::<String>().as_str())
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::PathBuf,
        process::Command,
        sync::atomic::{AtomicU64, Ordering},
    };

    use super::{TmuxClient, exact_session_target, project_session_name};

    static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

    fn test_socket() -> PathBuf {
        let id = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
        PathBuf::from(format!(
            "/private/tmp/talos-tmux-test-{}-{id}.sock",
            std::process::id()
        ))
    }

    fn fake_tmux() -> (PathBuf, PathBuf) {
        use std::os::unix::fs::PermissionsExt;

        let id = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
        let root = PathBuf::from(format!(
            "/private/tmp/talos-fake-tmux-{}-{id}",
            std::process::id()
        ));
        fs::create_dir_all(&root).expect("create fake tmux directory");
        let executable = root.join("tmux");
        let state_file = root.join("sessions");
        let script = format!(
            "#!/bin/sh\nSTATE='{}'\ncase \"$1\" in\n  has-session) target=\"${{3#=}}\"; grep -Fx -- \"$target\" \"$STATE\" >/dev/null 2>&1 ;;\n  new-session) shift; while [ \"$#\" -gt 0 ]; do case \"$1\" in -s) session=\"$2\"; shift 2 ;; *) shift ;; esac; done; printf '%s\\n' \"$session\" >> \"$STATE\" ;;\n  list-sessions) cat \"$STATE\" ;;\n  *) exit 2 ;;\nesac\n",
            state_file.display()
        );
        fs::write(&executable, script).expect("write fake tmux executable");
        let mut permissions = fs::metadata(&executable)
            .expect("read fake tmux permissions")
            .permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&executable, permissions).expect("make fake tmux executable");
        (root, executable)
    }

    #[test]
    fn derives_stable_project_scoped_names_and_exact_targets() {
        let project = PathBuf::from("/tmp/example");
        let name = project_session_name(&project);
        assert_eq!(name, project_session_name(&project));
        assert!(name.starts_with("talos-"));
        assert_eq!(exact_session_target(&name).unwrap(), format!("={name}"));
        assert!(exact_session_target("talos-bad/name").is_err());
    }

    #[test]
    fn reuses_one_project_session_with_fake_tmux() {
        let (temporary_root, executable) = fake_tmux();
        let project_root = temporary_root.join("project");
        fs::create_dir(&project_root).expect("create fake project root");
        let client = TmuxClient {
            executable,
            server_socket: None,
        };

        let first = client
            .ensure_project_session(&project_root)
            .expect("create project session");
        let second = client
            .ensure_project_session(&project_root)
            .expect("reuse project session");
        assert_eq!(first, second);
        let listed = client
            .command()
            .arg("list-sessions")
            .output()
            .expect("list fake tmux sessions");
        assert_eq!(
            String::from_utf8_lossy(&listed.stdout)
                .lines()
                .collect::<Vec<_>>(),
            [first.as_str()]
        );
        let _ = fs::remove_dir_all(temporary_root);
    }

    #[test]
    #[ignore = "Requires tmux Unix-socket creation outside the restricted sandbox"]
    fn creates_only_one_control_session_for_a_project() {
        let probe = TmuxClient::default().probe();
        if !probe.available {
            return;
        }
        let root = std::env::temp_dir().join(format!(
            "talos-tmux-project-{}-{}",
            std::process::id(),
            NEXT_DIR.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).expect("create isolated project root");
        let socket = test_socket();
        let client = TmuxClient {
            executable: PathBuf::from("tmux"),
            server_socket: Some(socket.clone()),
        };

        let first = client
            .ensure_project_session(&root)
            .expect("create isolated control session");
        let second = client
            .ensure_project_session(&root)
            .expect("reuse existing control session");
        assert_eq!(first, second);
        assert!(client.has_session(&first).expect("find project session"));

        let listed = Command::new("tmux")
            .arg("-S")
            .arg(&socket)
            .arg("list-sessions")
            .arg("-F")
            .arg("#{session_name}")
            .output()
            .expect("list isolated tmux server sessions");
        assert!(listed.status.success());
        let names = String::from_utf8_lossy(&listed.stdout);
        assert_eq!(names.lines().collect::<Vec<_>>(), [first.as_str()]);

        let _ = Command::new("tmux")
            .arg("-S")
            .arg(&socket)
            .arg("kill-server")
            .status();
        let _ = fs::remove_dir_all(root);
    }
}
