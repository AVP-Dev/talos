use std::{
    env,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use anyhow::{Context, Result, bail};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::{adapter::AdapterId, config::ProjectConfig, secrets::redact_sensitive};

const WORKSPACE_PANES_OPTION: &str = "@talos_pair_panes";
const COLUMNS_LAYOUT_OPTION: &str = "@talos_columns_layout";
const NATIVE: &str = "\"$1\"; exec \"${SHELL:-/bin/sh}\"";

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum NativeRole {
    Writer,
    Critic,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum NativeLayout {
    Columns,
    Stacked,
}

impl NativeRole {
    fn index(self) -> usize {
        match self {
            Self::Writer => 0,
            Self::Critic => 1,
        }
    }
}

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
    /// Start native, operator-controlled terminals. No agent prompts or permission
    /// overrides are injected. Existing sessions are never restarted.
    pub fn ensure_pair_workspace(&self, root: &Path, config: &ProjectConfig) -> Result<String> {
        let root = root.canonicalize().context("Cannot resolve project root")?;
        let name = project_session_name(&root);
        if let Some(session_id) = self.find_session_id(&name)? {
            self.validate_pair_workspace(&name).with_context(|| {
                format!(
                    "Workspace {name} is incomplete; inspect it with tmux attach-session -t '{session_id}'. Existing panes were not changed"
                )
            })?;
            return Ok(name);
        }
        let writer = native_executable(config.writer)?;
        let critic = native_executable(config.critic)?;
        let talos = env::current_exe().context("Cannot resolve TALOS executable")?;
        self.create_pair_workspace(&root, &name, &writer, &critic, &talos, config)
            .with_context(|| {
                format!("Workspace {name} setup failed; inspect existing panes before retrying")
            })?;
        Ok(name)
    }

    fn create_pair_workspace(
        &self,
        root: &Path,
        name: &str,
        writer: &Path,
        critic: &Path,
        talos: &Path,
        config: &ProjectConfig,
    ) -> Result<()> {
        // Multi-argument tmux commands preserve paths with spaces without shell
        // interpolation. The shell fallback keeps a pane alive after CLI exit.
        let first = self.checked(
            self.command()
                .args([
                    "new-session",
                    "-d",
                    "-P",
                    "-F",
                    "#{pane_id}",
                    "-s",
                    name,
                    "-n",
                    "pair",
                    "-x",
                    "120",
                    "-y",
                    "40",
                    "-c",
                ])
                .arg(root)
                .args(["sh", "-c", NATIVE, "talos-native"])
                .arg(writer),
        )?;
        let writer_id = pane_id(&first)?;
        // Split bottom first, then split the original top pane horizontally.
        let bottom = self.checked(
            self.command()
                .args([
                    "split-window",
                    "-d",
                    "-v",
                    "-l",
                    "7",
                    "-P",
                    "-F",
                    "#{pane_id}",
                    "-t",
                    writer_id,
                    "-c",
                ])
                .arg(root)
                .arg(talos)
                .arg("workspace-status"),
        )?;
        let bottom_id = pane_id(&bottom)?;
        let right = self.checked(
            self.command()
                .args([
                    "split-window",
                    "-d",
                    "-h",
                    "-l",
                    "50%",
                    "-P",
                    "-F",
                    "#{pane_id}",
                    "-t",
                    writer_id,
                    "-c",
                ])
                .arg(root)
                .args(["sh", "-c", NATIVE, "talos-native"])
                .arg(critic),
        )?;
        let critic_id = pane_id(&right)?;
        for (id, title) in [
            (writer_id, format!("Writer | {}", config.writer.as_str())),
            (critic_id, format!("Critic | {}", config.critic.as_str())),
            (bottom_id, "TALOS | Cockpit".to_owned()),
        ] {
            self.checked(self.command().args(["select-pane", "-t", id, "-T", &title]))?;
        }
        self.checked(self.command().args([
            "set-option",
            "-w",
            "-t",
            writer_id,
            "pane-border-status",
            "top",
        ]))?;
        self.checked(self.command().args([
            "set-option",
            "-w",
            "-t",
            writer_id,
            "pane-border-format",
            "#{pane_title}",
        ]))?;
        self.checked(self.command().args(["select-pane", "-t", writer_id]))?;
        // Publish topology only after all pane creation and configuration succeeded.
        // A failed or interrupted setup is never mistaken for a complete workspace.
        let target = self.session_target(name)?;
        let pane_ids = format!("{writer_id} {critic_id} {bottom_id}");
        self.checked(self.command().args([
            "set-option",
            "-t",
            &target,
            WORKSPACE_PANES_OPTION,
            &pane_ids,
        ]))?;
        self.validate_pair_workspace(name)?;
        Ok(())
    }

    fn validate_pair_workspace(&self, name: &str) -> Result<()> {
        let target = self.session_target(name)?;
        let recorded = self.checked(self.command().args([
            "show-options",
            "-qv",
            "-t",
            &target,
            WORKSPACE_PANES_OPTION,
        ]))?;
        let expected = parse_workspace_panes(&recorded)?;
        let window_target = format!("{target}:");
        let observed = self.checked(self.command().args([
            "list-panes",
            "-s",
            "-t",
            &window_target,
            "-F",
            "#{pane_id} #{pane_dead}",
        ]))?;
        validate_live_panes(&expected, &observed)
    }

    /// Change geometry without restarting the native terminals.
    pub fn set_native_layout(&self, root: &Path, layout: NativeLayout) -> Result<()> {
        let name = project_session_name(&root.canonicalize()?);
        self.validate_pair_workspace(&name)?;
        let target = self.session_target(&name)?;
        let recorded = self.checked(self.command().args([
            "show-options", "-qv", "-t", &target, WORKSPACE_PANES_OPTION,
        ]))?;
        let panes = parse_workspace_panes(&recorded)?;
        let listed = self.checked(self.command().args([
            "list-panes", "-t", panes[0], "-F", "#{pane_id}",
        ]))?;
        if listed.lines().collect::<Vec<_>>() != panes {
            bail!("Pair window must contain exactly its original three panes in role order");
        }
        let current = self.checked(self.command().args([
            "display-message", "-p", "-t", panes[0], "#{window_layout}",
        ]))?;
        let saved = self.checked(self.command().args([
            "show-options", "-qv", "-t", &target, COLUMNS_LAYOUT_OPTION,
        ]))?;
        match layout {
            NativeLayout::Stacked => {
                if saved.trim().is_empty() {
                    let geometry = self.checked(self.command().args([
                        "list-panes", "-t", panes[0], "-F", "#{pane_top} #{pane_left}",
                    ]))?;
                    let rows: Vec<Vec<&str>> = geometry.lines().map(|line| line.split_whitespace().collect()).collect();
                    if rows.len() != 3 || rows.iter().any(|row| row.len() != 2) || rows[0][0] != rows[1][0] || rows[0][1] == rows[1][1] || rows[2][0] == rows[0][0] {
                        bail!("Restore the original columns layout before saving it");
                    }
                    self.checked(self.command().args([
                        "set-option", "-t", &target, COLUMNS_LAYOUT_OPTION, current.trim(),
                    ]))?;
                }
                self.checked(self.command().args([
                    "select-layout", "-t", panes[0], "even-vertical",
                ]))?;
                self.checked(self.command().args([
                    "resize-pane", "-t", panes[2], "-y", "7",
                ]))?;
            }
            NativeLayout::Columns => {
                if !saved.trim().is_empty() {
                    self.checked(self.command().args([
                        "select-layout", "-t", panes[0], saved.trim(),
                    ]))?;
                }
            }
        }
        self.validate_pair_workspace(&name)
    }

    /// Replace only the explicitly selected role, preserving the other panes.
    pub fn restart_native_role(
        &self,
        root: &Path,
        role: NativeRole,
        config: &ProjectConfig,
    ) -> Result<()> {
        let adapter = match role {
            NativeRole::Writer => config.writer,
            NativeRole::Critic => config.critic,
        };
        let executable = native_executable(adapter)?;
        self.restart_role_with_executable(root, role, &executable, adapter)
    }

    fn restart_role_with_executable(
        &self,
        root: &Path,
        role: NativeRole,
        executable: &Path,
        adapter: AdapterId,
    ) -> Result<()> {
        let root = root.canonicalize()?;
        let name = project_session_name(&root);
        self.validate_pair_workspace(&name)?;
        let target = self.session_target(&name)?;
        let record = self.checked(self.command().args([
            "show-options",
            "-qv",
            "-t",
            &target,
            WORKSPACE_PANES_OPTION,
        ]))?;
        let panes = parse_workspace_panes(&record)?;
        let pane = panes[role.index()];
        self.checked(
            self.command()
                .args(["respawn-pane", "-k", "-t", pane, "-c"])
                .arg(&root)
                .args(["sh", "-c", NATIVE, "talos-native"])
                .arg(executable),
        )?;
        let label = match role {
            NativeRole::Writer => "Writer",
            NativeRole::Critic => "Critic",
        };
        let title = format!("{label} | {}", adapter.as_str());
        self.checked(
            self.command()
                .args(["select-pane", "-t", pane, "-T", &title]),
        )?;
        self.validate_pair_workspace(&name)
    }

    fn checked(&self, command: &mut Command) -> Result<String> {
        let output = command.output().context("Cannot execute tmux command")?;
        if !output.status.success() {
            bail!("tmux command failed: {}", safe_stderr(&output));
        }
        String::from_utf8(output.stdout).context("Invalid tmux output")
    }

    fn command(&self) -> Command {
        let mut command = Command::new(&self.executable);
        if let Some(socket) = &self.server_socket {
            command.arg("-S").arg(socket).args(["-f", "/dev/null"]);
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

    pub fn attach_project_session(&self, session_name: &str) -> Result<()> {
        let target = self.session_target(session_name)?;
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
        Ok(self.find_session_id(session_name)?.is_some())
    }

    fn session_target(&self, session_name: &str) -> Result<String> {
        self.find_session_id(session_name)?
            .context("TALOS tmux session no longer exists")
    }

    fn find_session_id(&self, session_name: &str) -> Result<Option<String>> {
        validate_session_name(session_name)?;
        let output = self
            .command()
            .args(["list-sessions", "-F", "#{session_id} #{session_name}"])
            .output()
            .context("Cannot run tmux; install tmux or use TALOS in the foreground")?;
        if !output.status.success() {
            let error = safe_stderr(&output);
            if error.contains("no server running") || error.contains("No such file or directory") {
                return Ok(None);
            }
            bail!("Cannot list tmux sessions: {error}");
        }
        let sessions = String::from_utf8(output.stdout).context("Invalid tmux session list")?;
        Ok(find_exact_session_id(&sessions, session_name)?.map(str::to_owned))
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

fn pane_id(output: &str) -> Result<&str> {
    let id = output.trim();
    if !id
        .strip_prefix('%')
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
    {
        bail!("Invalid tmux pane identity");
    }
    Ok(id)
}

fn parse_workspace_panes(recorded: &str) -> Result<[&str; 3]> {
    let mut ids = recorded.split_whitespace();
    let writer = pane_id(ids.next().context("Missing Writer pane")?)?;
    let critic = pane_id(ids.next().context("Missing Critic pane")?)?;
    let cockpit = pane_id(ids.next().context("Missing Cockpit pane")?)?;
    if ids.next().is_some() || writer == critic || writer == cockpit || critic == cockpit {
        bail!("Invalid workspace pane identities");
    }
    Ok([writer, critic, cockpit])
}

fn validate_live_panes(expected: &[&str; 3], observed: &str) -> Result<()> {
    for id in expected {
        if !observed.lines().any(|line| {
            let mut fields = line.split_whitespace();
            fields.next() == Some(*id) && fields.next() == Some("0") && fields.next().is_none()
        }) {
            bail!("Workspace pane {id} is missing or has exited");
        }
    }
    Ok(())
}

pub(crate) fn native_executable(adapter: AdapterId) -> Result<PathBuf> {
    let executable = match adapter {
        AdapterId::Agy => crate::adapter::agy::resolve_agy_executable(),
        AdapterId::Codex => crate::adapter::codex::resolve_codex_executable(),
        AdapterId::Claude => crate::adapter::claude::resolve_claude_executable(),
        AdapterId::Omp => crate::adapter::omp::resolve_omp_executable(),
        AdapterId::Pi => crate::adapter::pi::resolve_pi_executable(),
        AdapterId::Kilo => crate::adapter::kilo::resolve_kilo_executable(),
        AdapterId::OpenCode => crate::adapter::opencode::resolve_opencode_executable(),
    };
    executable.with_context(|| {
        format!(
            "{} is not installed; configure the corresponding role with talos config set",
            adapter.as_str()
        )
    })
}

pub fn project_session_name(project_root: &Path) -> String {
    let digest = Sha256::digest(project_root.to_string_lossy().as_bytes());
    let mut suffix = String::with_capacity(16);
    for byte in digest.iter().take(8) {
        suffix.push_str(&format!("{byte:02x}"));
    }
    format!("talos-pair-{suffix}")
}

fn validate_session_name(session_name: &str) -> Result<()> {
    if !session_name
        .strip_prefix("talos-pair-")
        .is_some_and(|suffix| {
            suffix.len() == 16 && suffix.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
    {
        bail!("TALOS tmux session name is invalid");
    }
    Ok(())
}

fn find_exact_session_id<'a>(sessions: &'a str, name: &str) -> Result<Option<&'a str>> {
    for line in sessions.lines() {
        if let Some((id, observed_name)) = line.split_once(' ')
            && observed_name == name
        {
            if !id.strip_prefix('$').is_some_and(|digits| {
                !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
            }) {
                bail!("Invalid tmux session identity");
            }
            return Ok(Some(id));
        }
    }
    Ok(None)
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

    use super::{
        TmuxClient, find_exact_session_id, pane_id, parse_workspace_panes, project_session_name,
        validate_live_panes, validate_session_name,
    };
    use crate::config::ProjectConfig;

    static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

    fn test_socket() -> PathBuf {
        let id = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("talos-tmux-test-{}-{id}.sock", std::process::id()))
    }

    struct IsolatedServer {
        socket: PathBuf,
        root: PathBuf,
    }

    impl Drop for IsolatedServer {
        fn drop(&mut self) {
            let _ = Command::new("tmux")
                .arg("-S")
                .arg(&self.socket)
                .arg("kill-server")
                .output();
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn fake_tmux() -> (PathBuf, PathBuf) {
        use std::os::unix::fs::PermissionsExt;

        let id = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("talos-fake-tmux-{}-{id}", std::process::id()));
        fs::create_dir_all(&root).expect("create fake tmux directory");
        let executable = root.join("tmux");
        let state_file = root.join("sessions");
        let script = format!(
            "#!/bin/sh\nSTATE='{}'\ncase \"$1\" in\n  has-session) target=\"${{3#=}}\"; grep -Fx -- \"$target\" \"$STATE\" >/dev/null 2>&1 ;;\n  new-session) shift; while [ \"$#\" -gt 0 ]; do case \"$1\" in -s) session=\"$2\"; shift 2 ;; *) shift ;; esac; done; printf '%s\\n' \"$session\" >> \"$STATE\"; printf '%%0\\n' ;;\n  split-window) case \"$3\" in -v) printf '%%1\\n' ;; -h) printf '%%2\\n' ;; *) exit 2 ;; esac ;;\n  select-pane) exit 0 ;;\n  set-option) if [ \"$4\" = '@talos_pair_panes' ]; then printf '%s\\n' \"$5\" > \"$STATE.panes\"; fi ;;\n  show-options) if [ -f \"$STATE.panes\" ]; then cat \"$STATE.panes\"; fi ;;\n  list-panes) printf '%%0 0\\n%%1 0\\n%%2 0\\n' ;;\n  list-sessions) if [ ! -f \"$STATE\" ]; then printf 'no server running\\n' >&2; exit 1; fi; if [ \"$#\" -gt 1 ]; then while IFS= read -r session; do printf '$0 %s\\n' \"$session\"; done < \"$STATE\"; else cat \"$STATE\"; fi ;;\n  *) exit 2 ;;\nesac\n",
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
        assert!(validate_session_name(&name).is_ok());
        assert!(validate_session_name("talos-bad/name").is_err());
        let sessions = format!("$0 {name}-other\n$1 {name}\n");
        assert_eq!(find_exact_session_id(&sessions, &name).unwrap(), Some("$1"));
        assert_eq!(find_exact_session_id(&sessions, "other").unwrap(), None);
        assert_eq!(pane_id("%12\n").unwrap(), "%12");
        assert!(pane_id("%12:other").is_err());
    }

    #[test]
    fn rejects_incomplete_or_dead_workspace_topology() {
        assert!(parse_workspace_panes("").is_err());
        assert!(parse_workspace_panes("%0 %1").is_err());
        assert!(parse_workspace_panes("%0 %1 %1").is_err());
        assert!(parse_workspace_panes("%0 %1 %2 %3").is_err());
        let expected = parse_workspace_panes("%0 %1 %2\n").unwrap();
        assert!(validate_live_panes(&expected, "%0 0\n%1 0\n%2 0\n%3 0\n").is_ok());
        assert!(validate_live_panes(&expected, "%0 0\n%1 0\n").is_err());
        assert!(validate_live_panes(&expected, "%0 0\n%1 0\n%2 1\n").is_err());
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

        let first = project_session_name(&project_root.canonicalize().unwrap());
        let fixture = std::path::Path::new("/bin/sh");
        let config = ProjectConfig::default();
        client
            .create_pair_workspace(&project_root, &first, fixture, fixture, fixture, &config)
            .expect("create project session");
        let second = client
            .ensure_pair_workspace(&project_root, &config)
            .expect("reuse project session");
        assert_eq!(first, second);
        fs::remove_file(temporary_root.join("sessions.panes")).expect("simulate incomplete setup");
        let error = client
            .ensure_pair_workspace(&project_root, &config)
            .expect_err("refuse incomplete workspace");
        assert!(
            error
                .to_string()
                .contains("Existing panes were not changed")
        );
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
        assert!(
            probe.available,
            "Install tmux before running this acceptance test"
        );
        let root = std::env::temp_dir().join(format!(
            "talos-tmux-project-{}-{}",
            std::process::id(),
            NEXT_DIR.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).expect("create isolated project root");
        let socket = test_socket();
        let _server = IsolatedServer {
            socket: socket.clone(),
            root: root.clone(),
        };
        let client = TmuxClient {
            executable: PathBuf::from("tmux"),
            server_socket: Some(socket.clone()),
        };

        let first = project_session_name(&root.canonicalize().unwrap());
        let fixture = root.join("fixture cli");
        use std::os::unix::fs::PermissionsExt;
        fs::write(&fixture, "#!/bin/sh\nexec sleep 120\n").unwrap();
        fs::set_permissions(&fixture, fs::Permissions::from_mode(0o700)).unwrap();
        let config = ProjectConfig::default();
        client
            .create_pair_workspace(&root, &first, &fixture, &fixture, &fixture, &config)
            .expect("create isolated control session");
        let identity = || {
            client
                .checked(client.command().args([
                    "list-panes",
                    "-t",
                    &first,
                    "-F",
                    "#{pane_id} #{pane_pid}",
                ]))
                .expect("record pane identities")
        };
        let before = identity();
        let second = client
            .ensure_pair_workspace(&root, &config)
            .expect("reuse existing control session");
        assert_eq!(first, second);
        assert_eq!(before, identity(), "reconnect must preserve pane processes");
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
        let panes = client
            .command()
            .args([
                "list-panes",
                "-t",
                &first,
                "-F",
                "#{pane_id} #{pane_top} #{pane_left} #{pane_height}",
            ])
            .output()
            .expect("list panes");
        assert!(panes.status.success());
        let geometry = String::from_utf8(panes.stdout).unwrap();
        assert_eq!(geometry.lines().count(), 3);
        let mut rows: Vec<Vec<&str>> = geometry
            .lines()
            .map(|line| line.split_whitespace().collect())
            .collect();
        rows.sort_by_key(|row| row[1].parse::<u32>().unwrap());
        assert_eq!(rows[0][1], rows[1][1]);
        assert_ne!(rows[0][2], rows[1][2]);
        assert!((6..=8).contains(&rows[2][3].parse::<u32>().unwrap()));
        client.set_native_layout(&root, super::NativeLayout::Stacked).expect("stack native panes");
        assert_eq!(before, identity(), "layout must preserve pane identities and processes");
        client.set_native_layout(&root, super::NativeLayout::Columns).expect("restore native columns");
        assert_eq!(before, identity(), "restoring layout must preserve pane identities and processes");
        let target = client.session_target(&first).unwrap();
        let record = client
            .checked(client.command().args([
                "show-options",
                "-qv",
                "-t",
                &target,
                super::WORKSPACE_PANES_OPTION,
            ]))
            .unwrap();
        let ids = parse_workspace_panes(&record).unwrap();
        let before_restart = identity();
        client
            .restart_role_with_executable(
                &root,
                super::NativeRole::Writer,
                &fixture,
                crate::adapter::AdapterId::Agy,
            )
            .expect("restart only Writer");
        let after_restart = identity();
        for id in ids {
            let old = before_restart
                .lines()
                .find(|line| line.split_whitespace().next() == Some(id))
                .unwrap();
            let new = after_restart
                .lines()
                .find(|line| line.split_whitespace().next() == Some(id))
                .unwrap();
            if id == ids[0] {
                assert_ne!(old, new);
            } else {
                assert_eq!(old, new);
            }
        }
    }
}
