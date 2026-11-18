use std::{
    fs::{File, OpenOptions},
    io::{self, BufRead, BufReader},
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph, Tabs},
};

use crate::{
    adapter::AdapterId as AdapterKind,
    app::{GitStatusPaths, current_project_revision_id, ensure_talos_ignored, git_status_paths},
    checks::ProjectCheck,
    config::{OrchestrationMode, ProjectConfig},
    db::{
        CriticReviewRecord, Database, QueueSummary, StopRequestOutcome, TaskEvent, TaskRecord,
        TaskStatus, TaskWorktreeRecord,
    },
    project::Project,
    tmux::{TmuxClient, project_session_name},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActiveTab {
    Feed = 0,
    Dialogue = 1,
    Diff = 2,
    Audit = 3,
}

impl ActiveTab {
    pub fn title(self) -> &'static str {
        match self {
            Self::Feed => " 1: Unified Feed ",
            Self::Dialogue => " 2: Dialogue & Review ",
            Self::Diff => " 3: Git Diff ",
            Self::Audit => " 4: Audit & Checks ",
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::Feed => Self::Dialogue,
            Self::Dialogue => Self::Diff,
            Self::Diff => Self::Audit,
            Self::Audit => Self::Feed,
        }
    }
}

pub struct TuiApp {
    project: Project,
    config: ProjectConfig,
    active_tab: ActiveTab,
    input: String,
    status_message: Option<(String, Instant)>,
    scroll_offset: usize,

    // Database metrics
    queue: QueueSummary,
    queue_paused: bool,
    pause_reason: Option<String>,
    supervisor_running: bool,
    supervisor_owner: Option<String>,
    active_tasks: Vec<TaskRecord>,
    worktrees: Vec<TaskWorktreeRecord>,
    recent_events: Vec<TaskEvent>,
    latest_review: Option<CriticReviewRecord>,
    verification_checks: Vec<ProjectCheck>,
    diff_lines: Vec<String>,
    supervisor_logs: Vec<String>,
    dirty_paths: GitStatusPaths,
    reconnect_notice: Option<String>,
    is_in_tmux: bool,
    pub should_exit: bool,
}

impl TuiApp {
    pub fn new(project: Project) -> Result<Self> {
        let config = ProjectConfig::load(&project).unwrap_or_default();
        let queue = QueueSummary {
            total: 0,
            queued: 0,
            active: 0,
            complete: 0,
            blocked_or_failed: 0,
        };
        let dirty_paths = GitStatusPaths {
            total_count: 0,
            preview: Vec::new(),
            preview_values_truncated: false,
        };

        let is_in_tmux = std::env::var_os("TMUX").is_some();

        let mut app = Self {
            project,
            config,
            active_tab: ActiveTab::Feed,
            input: String::new(),
            status_message: None,
            scroll_offset: 0,
            queue,
            queue_paused: false,
            pause_reason: None,
            supervisor_running: false,
            supervisor_owner: None,
            active_tasks: Vec::new(),
            worktrees: Vec::new(),
            recent_events: Vec::new(),
            latest_review: None,
            verification_checks: Vec::new(),
            diff_lines: Vec::new(),
            supervisor_logs: Vec::new(),
            dirty_paths,
            reconnect_notice: None,
            is_in_tmux,
            should_exit: false,
        };

        app.refresh_state()?;
        if let Some(task) = app.active_tasks.first() {
            app.reconnect_notice = Some(format!(
                "Active session reconnected for Task #{} ({})",
                task.id,
                task.status.as_str()
            ));
        }

        Ok(app)
    }

    pub fn refresh_state(&mut self) -> Result<()> {
        if let Ok(cfg) = ProjectConfig::load(&self.project) {
            self.config = cfg;
        }

        let database = Database::open(&self.project)?;
        self.queue = database.queue_summary().unwrap_or(QueueSummary {
            total: 0,
            queued: 0,
            active: 0,
            complete: 0,
            blocked_or_failed: 0,
        });

        let control = database.project_control_state().ok();
        if let Some(ctrl) = control {
            self.queue_paused = ctrl.queue_paused;
            self.pause_reason = ctrl.pause_reason;
        }

        if let Ok(Some(lease)) = database.current_supervisor_lease() {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);
            if lease.expires_at_ms > now {
                self.supervisor_running = true;
                self.supervisor_owner = Some(lease.owner_id);
            } else {
                self.supervisor_running = false;
                self.supervisor_owner = None;
            }
        } else {
            self.supervisor_running = false;
            self.supervisor_owner = None;
        }

        self.active_tasks = database.active_tasks().unwrap_or_default();
        self.worktrees = database.task_worktrees().unwrap_or_default();
        self.recent_events = database.recent_events(50).unwrap_or_default();

        if let Some(active) = self.active_tasks.first() {
            self.latest_review = database.latest_critic_review(active.id).ok().flatten();
            self.verification_checks = database
                .task_verification_checks(active.id)
                .unwrap_or_default();
        } else {
            self.latest_review = None;
            self.verification_checks = self.config.checks.clone();
        }

        self.dirty_paths = git_status_paths(&self.project).unwrap_or(GitStatusPaths {
            total_count: 0,
            preview: Vec::new(),
            preview_values_truncated: false,
        });

        self.read_supervisor_logs();
        self.load_git_diff();
        Ok(())
    }

    fn read_supervisor_logs(&mut self) {
        let log_path = self.project.root.join(".talos").join("supervisor.log");
        if let Ok(file) = File::open(&log_path) {
            let reader = BufReader::new(file);
            let lines: Vec<String> = reader
                .lines()
                .map_while(Result::ok)
                .filter(|l| !l.trim().is_empty())
                .collect();
            let start = lines.len().saturating_sub(500);
            self.supervisor_logs = lines[start..].to_vec();
        }
    }

    fn load_git_diff(&mut self) {
        let mut cmd = std::process::Command::new("git");
        cmd.args(["diff", "HEAD"]).current_dir(&self.project.root);
        if let Ok(out) = cmd.output() {
            let text = String::from_utf8_lossy(&out.stdout);
            if text.trim().is_empty() {
                // If HEAD has no diff, check untracked diff or normal diff
                let mut fallback_cmd = std::process::Command::new("git");
                fallback_cmd.arg("diff").current_dir(&self.project.root);
                if let Ok(fb_out) = fallback_cmd.output() {
                    let fb_text = String::from_utf8_lossy(&fb_out.stdout);
                    self.diff_lines = fb_text.lines().map(|s| s.to_string()).collect();
                }
            } else {
                self.diff_lines = text.lines().map(|s| s.to_string()).collect();
            }
        }
    }

    pub fn handle_submit(&mut self) -> Result<()> {
        let input_text = self.input.trim().to_string();
        if input_text.is_empty() {
            return Ok(());
        }

        if input_text.starts_with('/') {
            self.execute_slash_command(&input_text)?;
        } else {
            self.submit_task(&input_text)?;
        }
        self.input.clear();
        Ok(())
    }

    pub fn submit_task(&mut self, task_text: &str) -> Result<()> {
        let mut database = Database::open(&self.project)?;
        let task = database.add_task(task_text, task_text)?;
        self.status_message = Some((
            format!("Task #{} enqueued: {}", task.id, task.title),
            Instant::now(),
        ));

        if !self.supervisor_running {
            self.spawn_background_worker();
        }

        self.refresh_state()?;
        Ok(())
    }

    pub fn execute_slash_command(&mut self, command: &str) -> Result<()> {
        let parts: Vec<&str> = command.splitn(2, ' ').collect();
        let cmd = parts[0].to_ascii_lowercase();
        let arg = parts.get(1).copied().unwrap_or("").trim();

        match cmd.as_str() {
            "/help" | "/помощь" | "/справка" => {
                self.status_message = Some((
                    "Commands: /cli <name>, /stop, /pause, /resume, /run, /commit <msg>, /feed, /diff, /dialogue, /audit, /writer, /critic, /model, /mode, /exit".to_string(),
                    Instant::now(),
                ));
            }
            "/feed" | "/фид" | "/лента" | "/overview" | "/обзор" => {
                self.active_tab = ActiveTab::Feed;
            }
            "/cli" | "/кли" | "/adapter" | "/адаптер" => {
                let tokens: Vec<&str> = arg.split_whitespace().collect();
                if tokens.is_empty() {
                    self.status_message = Some((
                        format!(
                            "Current CLI: Writer='{}', Critic='{}'. Valid: agy, codex, claude, omp, pi, kilo, opencode. Use /cli <name> to set both.",
                            self.config.writer.as_str(),
                            self.config.critic.as_str()
                        ),
                        Instant::now(),
                    ));
                } else if tokens.len() == 1 {
                    if let Some(adapter) = AdapterKind::parse(tokens[0]) {
                        self.config.writer = adapter;
                        self.config.critic = adapter;
                        self.config.save(&self.project)?;
                        self.status_message = Some((
                            format!(
                                "Active CLI adapter updated to: {} (Writer & Critic)",
                                adapter.as_str()
                            ),
                            Instant::now(),
                        ));
                    } else {
                        self.status_message = Some((
                            format!(
                                "Unknown adapter '{}'. Valid adapters: agy, codex, claude, omp, pi, kilo, opencode",
                                tokens[0]
                            ),
                            Instant::now(),
                        ));
                    }
                } else {
                    let w_opt = AdapterKind::parse(tokens[0]);
                    let c_opt = AdapterKind::parse(tokens[1]);
                    match (w_opt, c_opt) {
                        (Some(w), Some(c)) => {
                            self.config.writer = w;
                            self.config.critic = c;
                            self.config.save(&self.project)?;
                            self.status_message = Some((
                                format!(
                                    "CLI adapters updated: Writer={}, Critic={}",
                                    w.as_str(),
                                    c.as_str()
                                ),
                                Instant::now(),
                            ));
                        }
                        (None, _) => {
                            self.status_message = Some((
                                format!(
                                    "Unknown writer adapter '{}'. Valid: agy, codex, claude, omp, pi, kilo, opencode",
                                    tokens[0]
                                ),
                                Instant::now(),
                            ));
                        }
                        (_, None) => {
                            self.status_message = Some((
                                format!(
                                    "Unknown critic adapter '{}'. Valid: agy, codex, claude, omp, pi, kilo, opencode",
                                    tokens[1]
                                ),
                                Instant::now(),
                            ));
                        }
                    }
                }
            }
            "/dialogue" | "/chat" | "/диалог" | "/чат" => {
                self.active_tab = ActiveTab::Dialogue;
            }
            "/diff" | "/дифф" => {
                self.active_tab = ActiveTab::Diff;
                self.load_git_diff();
            }
            "/audit" | "/review" | "/аудит" | "/ревью" => {
                self.active_tab = ActiveTab::Audit;
            }
            "/stop" | "/стоп" => {
                let mut database = Database::open(&self.project)?;
                match database.request_writer_stop()? {
                    StopRequestOutcome::NoActiveWriter => {
                        self.status_message = Some((
                            "No active Writer process running to stop.".to_string(),
                            Instant::now(),
                        ));
                    }
                    StopRequestOutcome::Requested { attempt_id } => {
                        self.status_message = Some((
                            format!("Graceful stop requested for Writer attempt #{attempt_id}."),
                            Instant::now(),
                        ));
                    }
                }
            }
            "/pause" | "/пауза" => {
                let mut database = Database::open(&self.project)?;
                let revision_id = current_project_revision_id(&self.project);
                let _ = database
                    .pause_project("Operator paused queue via /pause", revision_id.as_deref());
                self.queue_paused = true;
                self.status_message = Some((
                    "Queue paused. New tasks will wait.".to_string(),
                    Instant::now(),
                ));
            }
            "/resume" | "/continue" | "/резюм" | "/продолжить" => {
                let mut database = Database::open(&self.project)?;
                let revision_id = current_project_revision_id(&self.project);
                let _ = database.resume_project(revision_id.as_deref(), false);
                self.queue_paused = false;
                self.status_message = Some(("Queue resumed.".to_string(), Instant::now()));
                if !self.supervisor_running {
                    self.spawn_background_worker();
                }
            }
            "/quit" | "/exit" | "/выход" | "/q" => {
                self.should_exit = true;
            }
            "/clear" | "/очистить" => {
                self.status_message = None;
                self.input.clear();
            }
            "/stash" | "/стэш" => {
                let mut stash_cmd = std::process::Command::new("git");
                stash_cmd.arg("stash").current_dir(&self.project.root);
                if let Ok(out) = stash_cmd.output() {
                    let msg = String::from_utf8_lossy(&out.stdout);
                    self.status_message =
                        Some((format!("Git stash: {}", msg.trim()), Instant::now()));
                    self.refresh_state()?;
                }
            }
            "/status" | "/статус" => {
                self.status_message = Some((
                    format!(
                        "Queue: {} active, {} queued, {} complete. Supervisor: {}",
                        self.queue.active,
                        self.queue.queued,
                        self.queue.complete,
                        if self.supervisor_running {
                            "running"
                        } else {
                            "idle"
                        }
                    ),
                    Instant::now(),
                ));
            }
            "/writer" | "/писатель" => {
                if arg.is_empty() {
                    self.status_message = Some((
                        format!(
                            "Current Writer: {}. Use /writer <codex|claude|agy|omp|pi|kilo|opencode>",
                            self.config.writer.as_str()
                        ),
                        Instant::now(),
                    ));
                } else if let Some(adapter) = AdapterKind::parse(arg) {
                    self.config.writer = adapter;
                    self.config.save(&self.project)?;
                    self.status_message = Some((
                        format!("Writer updated to: {}", adapter.as_str()),
                        Instant::now(),
                    ));
                } else {
                    self.status_message = Some((
                        format!(
                            "Unknown adapter '{arg}'. Valid: agy, codex, claude, omp, pi, kilo, opencode"
                        ),
                        Instant::now(),
                    ));
                }
            }
            "/critic" => {
                if arg.is_empty() {
                    self.status_message = Some((
                        format!(
                            "Current Critic: {}. Use /critic <codex|claude|agy|omp|pi|kilo|opencode>",
                            self.config.critic.as_str()
                        ),
                        Instant::now(),
                    ));
                } else if let Some(adapter) = AdapterKind::parse(arg) {
                    self.config.critic = adapter;
                    self.config.save(&self.project)?;
                    self.status_message = Some((
                        format!("Critic updated to: {}", adapter.as_str()),
                        Instant::now(),
                    ));
                } else {
                    self.status_message = Some((
                        format!(
                            "Unknown adapter '{arg}'. Valid: agy, codex, claude, omp, pi, kilo, opencode"
                        ),
                        Instant::now(),
                    ));
                }
            }
            "/model" => {
                if arg.is_empty() {
                    self.config.writer_model = None;
                    self.config.critic_model = None;
                    self.config.save(&self.project)?;
                    self.status_message = Some((
                        "Model override cleared (using adapter defaults).".to_string(),
                        Instant::now(),
                    ));
                } else {
                    self.config.writer_model = Some(arg.to_string());
                    self.config.critic_model = Some(arg.to_string());
                    self.config.save(&self.project)?;
                    self.status_message =
                        Some((format!("Model override set to: {arg}"), Instant::now()));
                }
            }
            "/mode" => match arg.to_ascii_lowercase().as_str() {
                "team" => {
                    self.config.orchestration.mode = OrchestrationMode::Team;
                    self.config.save(&self.project)?;
                    self.status_message = Some(("Mode set to TEAM.".to_string(), Instant::now()));
                }
                "pair" => {
                    self.config.orchestration.mode = OrchestrationMode::Pair;
                    self.config.save(&self.project)?;
                    self.status_message = Some(("Mode set to PAIR.".to_string(), Instant::now()));
                }
                _ => {
                    self.status_message =
                        Some(("Use /mode pair or /mode team.".to_string(), Instant::now()));
                }
            },
            "/commit" => {
                let msg = if arg.is_empty() {
                    "checkpoint before talos run"
                } else {
                    arg
                };
                let mut add_cmd = std::process::Command::new("git");
                add_cmd.args(["add", "-A"]).current_dir(&self.project.root);
                let _ = add_cmd.output();

                let mut commit_cmd = std::process::Command::new("git");
                commit_cmd
                    .args(["commit", "-m", msg])
                    .current_dir(&self.project.root);
                match commit_cmd.output() {
                    Ok(out) if out.status.success() => {
                        self.status_message = Some((
                            format!("Git commit created: {msg}. Worktree is now clean!"),
                            Instant::now(),
                        ));
                        self.refresh_state()?;
                        if !self.supervisor_running {
                            self.spawn_background_worker();
                        }
                    }
                    Ok(out) => {
                        let err = String::from_utf8_lossy(&out.stderr);
                        self.status_message =
                            Some((format!("Git commit failed: {err}"), Instant::now()));
                    }
                    Err(e) => {
                        self.status_message = Some((format!("Git error: {e}"), Instant::now()));
                    }
                }
            }
            "/run" => {
                self.spawn_background_worker();
                self.status_message = Some((
                    "Started background supervisor daemon.".to_string(),
                    Instant::now(),
                ));
            }
            "/tmux" => {
                let tmux = TmuxClient::default();
                let probe = tmux.probe();
                let session = project_session_name(&self.project.root);
                if self.is_in_tmux {
                    self.status_message = Some((
                        format!(
                            "Running inside tmux session '{session}' (tmux v{}).",
                            probe.version.unwrap_or_default()
                        ),
                        Instant::now(),
                    ));
                } else if probe.available {
                    self.status_message = Some((
                        "Outside tmux. To launch inside persistent session: run 'talos --tmux' or 'talos attach'".to_string(),
                        Instant::now(),
                    ));
                } else {
                    self.status_message = Some((
                        "tmux is not installed on this machine.".to_string(),
                        Instant::now(),
                    ));
                }
            }
            _ => {
                self.status_message = Some((
                    format!("Unknown command '{cmd}'. Type /help for command list."),
                    Instant::now(),
                ));
            }
        }

        self.refresh_state()?;
        Ok(())
    }

    pub fn spawn_background_worker(&mut self) {
        if let Ok(exe) = std::env::current_exe() {
            let mut cmd = std::process::Command::new(exe);
            cmd.arg("up").arg("--auto");
            cmd.current_dir(&self.project.root);
            cmd.stdin(std::process::Stdio::null());

            // Pipe output to .talos/supervisor.log for real-time visibility
            let _ = std::fs::create_dir_all(self.project.root.join(".talos"));
            let log_path = self.project.root.join(".talos").join("supervisor.log");
            if let Ok(file) = OpenOptions::new().create(true).append(true).open(&log_path)
                && let Ok(err_file) = file.try_clone()
            {
                cmd.stdout(file);
                cmd.stderr(err_file);
            }

            #[cfg(unix)]
            {
                use std::os::unix::process::CommandExt;
                cmd.process_group(0);
            }

            match cmd.spawn() {
                Ok(child) => {
                    self.supervisor_running = true;
                    self.supervisor_owner = Some(format!("pid-{}", child.id()));
                    self.status_message = Some((
                        format!("Background worker spawned (PID: {}).", child.id()),
                        Instant::now(),
                    ));
                }
                Err(err) => {
                    self.status_message = Some((
                        format!("Failed to spawn background supervisor: {err}"),
                        Instant::now(),
                    ));
                }
            }
        }
    }

    pub fn toggle_pause(&mut self) -> Result<()> {
        let mut database = Database::open(&self.project)?;
        if self.queue_paused {
            let revision_id = current_project_revision_id(&self.project);
            let _ = database.resume_project(revision_id.as_deref(), false);
            self.queue_paused = false;
            self.status_message = Some(("Queue resumed.".to_string(), Instant::now()));
        } else {
            let revision_id = current_project_revision_id(&self.project);
            let _ = database.pause_project("Operator toggled pause in TUI", revision_id.as_deref());
            self.queue_paused = true;
            self.status_message = Some(("Queue paused.".to_string(), Instant::now()));
        }
        self.refresh_state()?;
        Ok(())
    }
}

pub async fn run_tui(project: Project) -> Result<()> {
    ensure_talos_ignored(&project)?;

    // Setup terminal
    enable_raw_mode().context("Failed to enable raw mode")?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen).context("Failed to enter alternate screen")?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).context("Failed to initialize terminal")?;

    let mut app = TuiApp::new(project)?;
    let mut last_tick = Instant::now();
    let tick_rate = Duration::from_millis(250);

    let res = run_app_loop(&mut terminal, &mut app, tick_rate, &mut last_tick).await;

    // Restore terminal
    let _ = disable_raw_mode();
    let _ = execute!(terminal.backend_mut(), LeaveAlternateScreen);
    let _ = terminal.show_cursor();

    res
}

async fn run_app_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut TuiApp,
    tick_rate: Duration,
    last_tick: &mut Instant,
) -> Result<()> {
    loop {
        if app.should_exit {
            return Ok(());
        }

        terminal.draw(|f| ui(f, app))?;

        let timeout = tick_rate
            .checked_sub(last_tick.elapsed())
            .unwrap_or_else(|| Duration::from_millis(0));

        if crossterm::event::poll(timeout)?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            match key.code {
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    return Ok(());
                }
                KeyCode::Char('q')
                    if app.input.is_empty() && !key.modifiers.contains(KeyModifiers::CONTROL) =>
                {
                    return Ok(());
                }
                KeyCode::Tab => {
                    app.active_tab = app.active_tab.next();
                    if app.active_tab == ActiveTab::Diff {
                        app.load_git_diff();
                    }
                }
                KeyCode::Char('1') if app.input.is_empty() => {
                    app.active_tab = ActiveTab::Feed;
                }
                KeyCode::Char('2') if app.input.is_empty() => {
                    app.active_tab = ActiveTab::Dialogue;
                }
                KeyCode::Char('3') if app.input.is_empty() => {
                    app.active_tab = ActiveTab::Diff;
                    app.load_git_diff();
                }
                KeyCode::Char('4') if app.input.is_empty() => {
                    app.active_tab = ActiveTab::Audit;
                }
                KeyCode::Enter => {
                    let _ = app.handle_submit();
                }
                KeyCode::Backspace => {
                    app.input.pop();
                }
                KeyCode::Esc => {
                    app.input.clear();
                }
                KeyCode::Up => {
                    app.scroll_offset = app.scroll_offset.saturating_add(1);
                }
                KeyCode::Down => {
                    app.scroll_offset = app.scroll_offset.saturating_sub(1);
                }
                KeyCode::PageUp => {
                    app.scroll_offset = app.scroll_offset.saturating_add(5);
                }
                KeyCode::PageDown => {
                    app.scroll_offset = app.scroll_offset.saturating_sub(5);
                }
                KeyCode::Home => {
                    app.scroll_offset = usize::MAX;
                }
                KeyCode::End => {
                    app.scroll_offset = 0;
                }
                KeyCode::Char(c) => {
                    app.input.push(c);
                }
                _ => {}
            }
        }

        if last_tick.elapsed() >= tick_rate {
            let _ = app.refresh_state();
            *last_tick = Instant::now();
        }
    }
}

fn ui(frame: &mut Frame, app: &TuiApp) {
    let size = frame.area();

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Header
            Constraint::Length(
                if !app.dirty_paths.is_empty() || app.reconnect_notice.is_some() {
                    2
                } else {
                    0
                },
            ), // Alerts banner
            Constraint::Length(3), // Tabs navigation bar
            Constraint::Min(8),    // Active Tab View Content
            Constraint::Length(3), // Input bar
        ])
        .split(size);

    render_header(frame, app, chunks[0]);

    if !app.dirty_paths.is_empty() || app.reconnect_notice.is_some() {
        render_alerts(frame, app, chunks[1]);
    }

    render_tabs(frame, app, chunks[2]);

    match app.active_tab {
        ActiveTab::Feed => render_feed_tab(frame, app, chunks[3]),
        ActiveTab::Dialogue => render_dialogue_tab(frame, app, chunks[3]),
        ActiveTab::Diff => render_diff_tab(frame, app, chunks[3]),
        ActiveTab::Audit => render_audit_tab(frame, app, chunks[3]),
    }

    render_input(frame, app, chunks[4]);
}

fn render_header(frame: &mut Frame, app: &TuiApp, area: Rect) {
    let mode_str = match app.config.orchestration.mode {
        OrchestrationMode::Pair => "PAIR",
        OrchestrationMode::Team => "TEAM",
    };

    let status_span = if app.queue_paused {
        Span::styled(
            " [⏸ PAUSED] ",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )
    } else if app.supervisor_running {
        Span::styled(
            " [● RUNNING] ",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        Span::styled(
            " [○ IDLE] ",
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
    };

    let tmux_span = if app.is_in_tmux {
        Span::styled(
            " [tmux: attached] ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        Span::styled(" [tmux: direct] ", Style::default().fg(Color::DarkGray))
    };

    let project_name = app
        .project
        .root
        .file_name()
        .map(|n| n.to_string_lossy())
        .unwrap_or_default();

    let header_text = vec![Line::from(vec![
        Span::styled(
            "TALOS",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" | Project: "),
        Span::styled(
            project_name.to_string(),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" | Mode: "),
        Span::styled(
            mode_str,
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" | Queue: "),
        Span::styled(
            format!(
                "{} active, {} queued, {} complete",
                app.queue.active, app.queue.queued, app.queue.complete
            ),
            Style::default().fg(Color::White),
        ),
        status_span,
        tmux_span,
    ])];

    let header_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));

    let paragraph = Paragraph::new(header_text)
        .block(header_block)
        .alignment(Alignment::Left);

    frame.render_widget(paragraph, area);
}

fn render_alerts(frame: &mut Frame, app: &TuiApp, area: Rect) {
    let mut alert_spans = Vec::new();

    if let Some(ref notice) = app.reconnect_notice {
        alert_spans.push(Span::styled(
            format!(" 🔄 {notice} "),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ));
    }

    if !app.dirty_paths.is_empty() {
        alert_spans.push(Span::styled(
            format!(
                " ⚠️ Warning: {} uncommitted file(s) in repository. Type '/commit <msg>' to clean before worktree execution! ",
                app.dirty_paths.total_count
            ),
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        ));
    }

    let alert_p = Paragraph::new(Line::from(alert_spans)).block(
        Block::default()
            .borders(Borders::BOTTOM)
            .border_style(Style::default().fg(Color::DarkGray)),
    );
    frame.render_widget(alert_p, area);
}

fn render_tabs(frame: &mut Frame, app: &TuiApp, area: Rect) {
    let titles = vec![
        ActiveTab::Feed.title(),
        ActiveTab::Dialogue.title(),
        ActiveTab::Diff.title(),
        ActiveTab::Audit.title(),
    ];

    let tabs = Tabs::new(titles)
        .select(app.active_tab as usize)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray)),
        )
        .style(Style::default().fg(Color::DarkGray))
        .highlight_style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        );

    frame.render_widget(tabs, area);
}

fn render_feed_tab(frame: &mut Frame, app: &TuiApp, area: Rect) {
    if area.width >= 100 {
        // Horizontal split: Left Cockpit Sidebar (34 cols), Right Unified Feed
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(34), Constraint::Min(40)])
            .split(area);

        render_cockpit_sidebar(frame, app, chunks[0]);
        render_unified_feed(frame, app, chunks[1]);
    } else {
        // Narrow vertical split: Top Compact Cockpit (7 rows), Bottom Unified Feed
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(7), Constraint::Min(8)])
            .split(area);

        render_cockpit_compact(frame, app, chunks[0]);
        render_unified_feed(frame, app, chunks[1]);
    }
}

fn render_cockpit_sidebar(frame: &mut Frame, app: &TuiApp, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(8), // Agents & Model
            Constraint::Length(7), // Active Task
            Constraint::Min(6),    // Queue & Repository
        ])
        .split(area);

    let active_task = app.active_tasks.first();

    // 1. Agents & Model Block
    let writer_profile = app.config.writer.as_str();
    let critic_profile = app.config.critic.as_str();
    let writer_model_str = app.config.writer_model.as_deref().unwrap_or("default");
    let critic_model_str = app.config.critic_model.as_deref().unwrap_or("default");
    let (mode_str, mode_color) = match app.config.orchestration.mode {
        OrchestrationMode::Pair => ("PAIR", Color::Cyan),
        OrchestrationMode::Team => ("TEAM", Color::Magenta),
    };

    let agent_lines = vec![
        Line::from(vec![
            Span::styled("Writer: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                writer_profile,
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" ({})", writer_model_str),
                Style::default().fg(Color::DarkGray),
            ),
        ]),
        Line::from(vec![
            Span::styled("Critic: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                critic_profile,
                Style::default()
                    .fg(Color::Magenta)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" ({})", critic_model_str),
                Style::default().fg(Color::DarkGray),
            ),
        ]),
        Line::from(vec![
            Span::styled("Mode:   ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                mode_str,
                Style::default().fg(mode_color).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(
                    " (max {} workers)",
                    app.config.orchestration.max_writer_workers
                ),
                Style::default().fg(Color::DarkGray),
            ),
        ]),
        Line::from(Span::styled(
            "Tip: Use /cli <name> to switch",
            Style::default().fg(Color::DarkGray),
        )),
    ];

    let agent_block = Block::default()
        .borders(Borders::ALL)
        .title(" 🤖 AI Agents ")
        .border_style(Style::default().fg(Color::Yellow));

    frame.render_widget(Paragraph::new(agent_lines).block(agent_block), chunks[0]);

    // 2. Active Task Block
    let task_lines = if let Some(task) = active_task {
        let (status_color, status_icon) = match task.status {
            TaskStatus::Queued => (Color::DarkGray, "○"),
            TaskStatus::Planning => (Color::Cyan, "◐"),
            TaskStatus::Implementing => (Color::Yellow, "●"),
            TaskStatus::Reviewing => (Color::Magenta, "●"),
            TaskStatus::Verifying => (Color::Blue, "●"),
            TaskStatus::Complete => (Color::Green, "✔"),
            TaskStatus::Blocked | TaskStatus::Failed => (Color::Red, "✖"),
        };

        let title_clean = if task.title.len() > 30 {
            format!("{}...", &task.title[..27])
        } else {
            task.title.clone()
        };

        vec![
            Line::from(vec![
                Span::styled(
                    format!("#{} ", task.id),
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("{status_icon} {}", task.status.as_str()),
                    Style::default()
                        .fg(status_color)
                        .add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(Span::styled(title_clean, Style::default().fg(Color::White))),
            Line::from(vec![
                Span::styled("Worktrees: ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    format!("{} active", app.worktrees.len()),
                    Style::default().fg(Color::Cyan),
                ),
            ]),
        ]
    } else {
        vec![
            Line::from(Span::styled(
                "○ Idle",
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                "Waiting for runnable tasks",
                Style::default().fg(Color::DarkGray),
            )),
            Line::from(vec![
                Span::styled("Enqueued: ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    format!("{} waiting", app.queue.queued),
                    Style::default().fg(Color::White),
                ),
            ]),
        ]
    };

    let task_block = Block::default()
        .borders(Borders::ALL)
        .title(" 🎯 Active Task ")
        .border_style(Style::default().fg(if active_task.is_some() {
            Color::Cyan
        } else {
            Color::DarkGray
        }));

    frame.render_widget(Paragraph::new(task_lines).block(task_block), chunks[1]);

    // 3. Queue & Repository State Block
    let git_status_line = if app.dirty_paths.is_empty() {
        Line::from(vec![
            Span::styled("Git: ", Style::default().fg(Color::DarkGray)),
            Span::styled("Clean (HEAD)", Style::default().fg(Color::Green)),
        ])
    } else {
        Line::from(vec![
            Span::styled("Git: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("⚠️ {} dirty files", app.dirty_paths.total_count),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
        ])
    };

    let queue_lines = vec![
        Line::from(vec![
            Span::styled("Active:   ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{}", app.queue.active),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" | Queued: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{}", app.queue.queued),
                Style::default().fg(Color::White),
            ),
        ]),
        Line::from(vec![
            Span::styled("Complete: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{}", app.queue.complete),
                Style::default().fg(Color::Green),
            ),
            Span::styled(" | Failed: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{}", app.queue.blocked_or_failed),
                Style::default().fg(if app.queue.blocked_or_failed > 0 {
                    Color::Red
                } else {
                    Color::DarkGray
                }),
            ),
        ]),
        git_status_line,
        Line::from(vec![
            Span::styled("Checks:   ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{} configured", app.config.checks.len()),
                Style::default().fg(Color::White),
            ),
        ]),
    ];

    let queue_block = Block::default()
        .borders(Borders::ALL)
        .title(" 📊 Repository & Queue ")
        .border_style(Style::default().fg(Color::DarkGray));

    frame.render_widget(Paragraph::new(queue_lines).block(queue_block), chunks[2]);
}

fn render_cockpit_compact(frame: &mut Frame, app: &TuiApp, area: Rect) {
    let mode_str = match app.config.orchestration.mode {
        OrchestrationMode::Pair => "PAIR",
        OrchestrationMode::Team => "TEAM",
    };

    let active_task_summary = if let Some(task) = app.active_tasks.first() {
        format!("#{} {} (● {})", task.id, task.title, task.status.as_str())
    } else {
        "Idle (No active task)".to_string()
    };

    let lines = vec![
        Line::from(vec![
            Span::styled("Mode: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                mode_str,
                Style::default()
                    .fg(Color::Magenta)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" | Writer: "),
            Span::styled(
                app.config.writer.as_str(),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" | Critic: "),
            Span::styled(
                app.config.critic.as_str(),
                Style::default()
                    .fg(Color::Magenta)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled("Active: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                active_task_summary,
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" | Worktrees: "),
            Span::styled(
                format!("{}", app.worktrees.len()),
                Style::default().fg(Color::Cyan),
            ),
        ]),
        Line::from(vec![Span::styled(
            format!(
                "Queue: {} active, {} queued, {} complete | Git: {} dirty files",
                app.queue.active, app.queue.queued, app.queue.complete, app.dirty_paths.total_count
            ),
            Style::default().fg(Color::Gray),
        )]),
    ];

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" 🎛 Cockpit Summary ")
        .border_style(Style::default().fg(Color::DarkGray));

    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn build_unified_feed_lines(app: &TuiApp) -> Vec<Line<'static>> {
    let mut feed = Vec::new();

    // 1. Convert recent SQLite task events to styled feed lines
    for event in &app.recent_events {
        let (actor_color, badge) = match event.actor.as_str() {
            "writer" | "writer_lead" => (Color::Yellow, "[WRITER]"),
            "critic" | "critic_lead" => (Color::Magenta, "[CRITIC]"),
            "supervisor" => (Color::Green, "[SUPERVISOR]"),
            "git" => (Color::LightCyan, "[GIT]"),
            "worktree" => (Color::Cyan, "[WORKTREE]"),
            _ => (Color::White, "[TASK]"),
        };

        let time_str = {
            let secs = (event.occurred_at_ms / 1000) % 86400;
            let h = secs / 3600;
            let m = (secs % 3600) / 60;
            let s = secs % 60;
            format!("{:02}:{:02}:{:02}", h, m, s)
        };

        let reason_clean = if event.reason.len() > 72 {
            format!("{}...", &event.reason[..69])
        } else {
            event.reason.clone()
        };

        feed.push(Line::from(vec![
            Span::styled(format!("{time_str} "), Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{:<13} ", badge),
                Style::default()
                    .fg(actor_color)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("#{} {}: ", event.task_id, event.event_type),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(reason_clean, Style::default().fg(Color::Gray)),
        ]));
    }

    // 2. Convert supervisor logs to styled feed lines
    for line in &app.supervisor_logs {
        let l = line.trim();
        if l.is_empty() {
            continue;
        }

        if l.starts_with(">>> [TALOS Team]") || l.starts_with(">>> [TALOS Auto]") {
            let style = if l.contains("Phase 1") {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else if l.contains("Phase 2") {
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else if l.contains("Phase 3") {
                Style::default()
                    .fg(Color::Magenta)
                    .add_modifier(Modifier::BOLD)
            } else if l.contains("Phase 4") {
                Style::default()
                    .fg(Color::Blue)
                    .add_modifier(Modifier::BOLD)
            } else if l.contains("APPROVED")
                || l.contains("Deliverable Summary")
                || l.contains("complete")
            {
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD)
            } else if l.contains("request_changes") || l.contains("requested changes") {
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Cyan)
            };

            feed.push(Line::from(vec![
                Span::styled("LIVE  ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    "[PHASE]       ",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(l.to_string(), style),
            ]));
        } else if l.to_ascii_lowercase().contains("rate limit") || l.contains("backoff") {
            feed.push(Line::from(vec![
                Span::styled("LIVE  ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    "[RATE-LIMIT]  ",
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(l.to_string(), Style::default().fg(Color::Yellow)),
            ]));
        } else if l.to_ascii_lowercase().contains("error")
            || l.to_ascii_lowercase().contains("fail")
        {
            feed.push(Line::from(vec![
                Span::styled("LIVE  ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    "[ERROR]       ",
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                ),
                Span::styled(l.to_string(), Style::default().fg(Color::Red)),
            ]));
        } else if l.starts_with("Task #") || l.starts_with("Subtask #") {
            feed.push(Line::from(vec![
                Span::styled("LIVE  ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    "[TASK]        ",
                    Style::default()
                        .fg(Color::LightCyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(l.to_string(), Style::default().fg(Color::White)),
            ]));
        } else {
            feed.push(Line::from(vec![
                Span::styled("LOG   ", Style::default().fg(Color::DarkGray)),
                Span::styled("[SYS]         ", Style::default().fg(Color::DarkGray)),
                Span::styled(l.to_string(), Style::default().fg(Color::DarkGray)),
            ]));
        }
    }

    if feed.is_empty() {
        feed.push(Line::from(vec![
            Span::styled(
                "⚡ TALOS Unified Feed initialized. ",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "Type a task below (e.g. 'Implement binary search in calc.py') or /help for commands.",
                Style::default().fg(Color::Gray),
            ),
        ]));
    }

    feed
}

fn render_unified_feed(frame: &mut Frame, app: &TuiApp, area: Rect) {
    let feed_lines = build_unified_feed_lines(app);
    let total_lines = feed_lines.len();

    let inner_height = area.height.saturating_sub(2) as usize;

    let (visible_lines, title) = if total_lines <= inner_height {
        (
            feed_lines,
            " ⚡ Live Unified Feed (Auto-scroll active) ".to_string(),
        )
    } else {
        let max_scroll = total_lines.saturating_sub(inner_height);
        let actual_scroll = app.scroll_offset.min(max_scroll);

        let end_idx = total_lines.saturating_sub(actual_scroll);
        let start_idx = end_idx.saturating_sub(inner_height);

        let title = if actual_scroll > 0 {
            format!(
                " ⚡ Live Unified Feed (Viewing earlier lines: -{} | Press 'End' to resume live stream) ",
                actual_scroll
            )
        } else {
            " ⚡ Live Unified Feed (Live stream • Auto-scroll) ".to_string()
        };

        (feed_lines[start_idx..end_idx].to_vec(), title)
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(Style::default().fg(Color::Cyan));

    let paragraph = Paragraph::new(visible_lines).block(block);
    frame.render_widget(paragraph, area);
}

fn render_dialogue_tab(frame: &mut Frame, app: &TuiApp, area: Rect) {
    let mut lines = Vec::new();

    lines.push(Line::from(vec![Span::styled(
        "=== Agent Dialogue & Peer Review ===",
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    )]));
    lines.push(Line::from(Span::raw("")));

    if let Some(ref review) = app.latest_review {
        let decision_color = if review.decision == "approve" {
            Color::Green
        } else {
            Color::Red
        };
        lines.push(Line::from(vec![
            Span::styled(
                "Critic Review Verdict: ",
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(
                review.decision.to_uppercase(),
                Style::default()
                    .fg(decision_color)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!(" (Revision R{})", review.revision_id)),
        ]));
        lines.push(Line::from(vec![
            Span::styled("Review Summary: ", Style::default().fg(Color::DarkGray)),
            Span::styled(&review.summary, Style::default().fg(Color::White)),
        ]));
        lines.push(Line::from(Span::raw("")));

        lines.push(Line::from(Span::styled(
            "Critic Findings / Defect Analysis:",
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        )));
        if review.result_json.trim().is_empty() || review.result_json == "{}" {
            lines.push(Line::from(Span::styled(
                "  • No defects found. Clean review.",
                Style::default().fg(Color::Green),
            )));
        } else {
            lines.push(Line::from(Span::styled(
                format!("  {}", review.result_json),
                Style::default().fg(Color::Yellow),
            )));
        }
    } else {
        lines.push(Line::from(Span::styled(
            "No Critic reviews recorded yet for current task.",
            Style::default().fg(Color::DarkGray),
        )));
        lines.push(Line::from(Span::styled(
            "When Writer submits code for review, Critic findings and discussion will appear here.",
            Style::default().fg(Color::DarkGray),
        )));
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" 💬 Peer Review & Decision Log ")
        .border_style(Style::default().fg(Color::Magenta));

    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn render_diff_tab(frame: &mut Frame, app: &TuiApp, area: Rect) {
    let mut items = Vec::new();

    if app.diff_lines.is_empty() {
        items.push(ListItem::new(Line::from(Span::styled(
            "No uncommitted code changes in current working tree or active worktree.",
            Style::default().fg(Color::DarkGray),
        ))));
    } else {
        for line in &app.diff_lines {
            let style = if line.starts_with('+') && !line.starts_with("+++") {
                Style::default().fg(Color::Green)
            } else if line.starts_with('-') && !line.starts_with("---") {
                Style::default().fg(Color::Red)
            } else if line.starts_with("@@") {
                Style::default().fg(Color::Cyan)
            } else if line.starts_with("diff ") || line.starts_with("index ") {
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };

            items.push(ListItem::new(Line::from(Span::styled(line.clone(), style))));
        }
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(
            " 📄 Live Code Diff ({} lines changed) ",
            app.diff_lines.len()
        ))
        .border_style(Style::default().fg(Color::Green));

    frame.render_widget(List::new(items).block(block), area);
}

fn render_audit_tab(frame: &mut Frame, app: &TuiApp, area: Rect) {
    let mut lines = Vec::new();

    lines.push(Line::from(Span::styled(
        "=== Automated Verification Checks & Test Gate ===",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(Span::raw("")));

    if app.verification_checks.is_empty() {
        lines.push(Line::from(Span::styled(
            "No automated verification checks configured for this project.",
            Style::default().fg(Color::DarkGray),
        )));
        lines.push(Line::from(Span::styled("Tip: Add checks with /config set checks '[{\"id\":\"test\",\"argv\":[\"cargo\",\"test\"]}]'", Style::default().fg(Color::DarkGray))));
    } else {
        for (i, check) in app.verification_checks.iter().enumerate() {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("Check #{}: ", i + 1),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(
                    &check.id,
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(" -> Command: "),
                Span::styled(check.argv.join(" "), Style::default().fg(Color::White)),
            ]));
        }
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" 🧪 Verification Suite & Test Gate ")
        .border_style(Style::default().fg(Color::Cyan));

    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn render_input(frame: &mut Frame, app: &TuiApp, area: Rect) {
    let title = if let Some((ref msg, _)) = app.status_message {
        format!(" Notice: {msg} ")
    } else {
        " Type task or command: /stop, /pause, /resume, /diff, /commit, /exit, /help | Tabs: [1-4] "
            .to_string()
    };

    let input_line = Line::from(vec![
        Span::styled(
            "> ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(&app.input, Style::default().fg(Color::White)),
        Span::styled(
            "█",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::SLOW_BLINK),
        ),
    ]);

    let input_block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(Style::default().fg(Color::Cyan));

    frame.render_widget(Paragraph::new(input_line).block(input_block), area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TUI_DIR: AtomicU64 = AtomicU64::new(0);

    fn test_tui_project() -> Project {
        let seq = NEXT_TUI_DIR.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("talos-tui-test-{}-{seq}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create test project");
        Project {
            state_root: root.clone(),
            root,
            discovery: crate::project::DiscoveryMethod::CurrentDirectory,
        }
    }

    #[test]
    fn active_tab_navigation_and_titles() {
        assert_eq!(ActiveTab::Feed.title(), " 1: Unified Feed ");
        assert_eq!(ActiveTab::Feed.next(), ActiveTab::Dialogue);
        assert_eq!(ActiveTab::Dialogue.next(), ActiveTab::Diff);
        assert_eq!(ActiveTab::Diff.next(), ActiveTab::Audit);
        assert_eq!(ActiveTab::Audit.next(), ActiveTab::Feed);
    }

    #[test]
    fn tui_cli_slash_commands_and_feed() {
        let project = test_tui_project();

        // Create .talos directory with empty database
        let talos_dir = project.root.join(".talos");
        std::fs::create_dir_all(&talos_dir).expect("create .talos");
        let db = Database::open(&project).expect("open db");
        drop(db);

        let mut app = TuiApp::new(project.clone()).expect("create TuiApp");

        // 1. Test /feed and aliases
        app.active_tab = ActiveTab::Diff;
        app.execute_slash_command("/feed").expect("execute /feed");
        assert_eq!(app.active_tab, ActiveTab::Feed);

        app.active_tab = ActiveTab::Audit;
        app.execute_slash_command("/фид").expect("execute /фид");
        assert_eq!(app.active_tab, ActiveTab::Feed);

        // 2. Test /cli without args shows current status
        app.execute_slash_command("/cli").expect("execute /cli");
        assert!(app.status_message.is_some());
        let (msg, _) = app.status_message.as_ref().unwrap();
        assert!(msg.contains("Current CLI:"));

        // 3. Test /cli <name> sets both Writer and Critic
        app.execute_slash_command("/cli codex")
            .expect("set cli codex");
        assert_eq!(app.config.writer, AdapterKind::Codex);
        assert_eq!(app.config.critic, AdapterKind::Codex);
        let (msg, _) = app.status_message.as_ref().unwrap();
        assert!(msg.contains("Active CLI adapter updated to: codex"));

        // 4. Test /cli <writer> <critic> sets distinct roles
        app.execute_slash_command("/cli claude agy")
            .expect("set cli claude agy");
        assert_eq!(app.config.writer, AdapterKind::Claude);
        assert_eq!(app.config.critic, AdapterKind::Agy);

        // 5. Test invalid adapter reports error
        app.execute_slash_command("/cli invalid_name")
            .expect("execute invalid cli");
        let (msg, _) = app.status_message.as_ref().unwrap();
        assert!(msg.contains("Unknown adapter 'invalid_name'"));

        // 6. Test build_unified_feed_lines
        app.supervisor_logs = vec![
            ">>> [TALOS Team] Phase 1: Writer Lead analyzing requirements & planning..."
                .to_string(),
            ">>> [TALOS Team] Critic Review Verdict: APPROVED (Round 1)".to_string(),
            "Task #12 calc.py updated successfully".to_string(),
            "Rate limit backoff 20s".to_string(),
        ];
        let feed = build_unified_feed_lines(&app);
        assert!(feed.len() >= 4);

        let _ = std::fs::remove_dir_all(&project.root);
    }
}
