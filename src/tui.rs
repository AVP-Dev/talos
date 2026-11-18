use std::{
    io,
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
    widgets::{Block, Borders, List, ListItem, Paragraph},
};

use crate::{
    app::{GitStatusPaths, current_project_revision_id, ensure_talos_ignored, git_status_paths},
    config::{OrchestrationMode, ProjectConfig},
    db::{Database, QueueSummary, TaskEvent, TaskRecord, TaskStatus, TaskWorktreeRecord},
    project::Project,
};

pub struct TuiApp {
    project: Project,
    config: ProjectConfig,
    input: String,
    status_message: Option<(String, Instant)>,
    scroll_offset: usize,
    auto_scroll: bool,

    // Database metrics
    queue: QueueSummary,
    queue_paused: bool,
    pause_reason: Option<String>,
    supervisor_running: bool,
    supervisor_owner: Option<String>,
    active_tasks: Vec<TaskRecord>,
    worktrees: Vec<TaskWorktreeRecord>,
    recent_events: Vec<TaskEvent>,
    dirty_paths: GitStatusPaths,
    reconnect_notice: Option<String>,
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

        let mut app = Self {
            project,
            config,
            input: String::new(),
            status_message: None,
            scroll_offset: 0,
            auto_scroll: true,
            queue,
            queue_paused: false,
            pause_reason: None,
            supervisor_running: false,
            supervisor_owner: None,
            active_tasks: Vec::new(),
            worktrees: Vec::new(),
            recent_events: Vec::new(),
            dirty_paths,
            reconnect_notice: None,
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
        self.dirty_paths = git_status_paths(&self.project).unwrap_or(GitStatusPaths {
            total_count: 0,
            preview: Vec::new(),
            preview_values_truncated: false,
        });

        Ok(())
    }

    pub fn submit_task(&mut self) -> Result<()> {
        let task_text = self.input.trim().to_string();
        if task_text.is_empty() {
            return Ok(());
        }

        let mut database = Database::open(&self.project)?;
        let task = database.add_task(&task_text, &task_text)?;
        self.status_message = Some((
            format!("Task #{} enqueued: {}", task.id, task.title),
            Instant::now(),
        ));
        self.input.clear();

        // If supervisor is not currently running, spawn it in background
        if !self.supervisor_running {
            self.spawn_background_worker();
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
            cmd.stdout(std::process::Stdio::null());
            cmd.stderr(std::process::Stdio::null());

            #[cfg(unix)]
            {
                use std::os::unix::process::CommandExt;
                cmd.process_group(0);
            }

            match cmd.spawn() {
                Ok(child) => {
                    self.supervisor_running = true;
                    self.supervisor_owner = Some(format!("pid-{}", child.id()));
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
                KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    let _ = app.toggle_pause();
                }
                KeyCode::Char('q')
                    if app.input.is_empty() && !key.modifiers.contains(KeyModifiers::CONTROL) =>
                {
                    return Ok(());
                }
                KeyCode::Enter => {
                    let _ = app.submit_task();
                }
                KeyCode::Backspace => {
                    app.input.pop();
                }
                KeyCode::Esc => {
                    app.input.clear();
                }
                KeyCode::Up => {
                    app.auto_scroll = false;
                    if app.scroll_offset > 0 {
                        app.scroll_offset -= 1;
                    }
                }
                KeyCode::Down => {
                    app.scroll_offset = app.scroll_offset.saturating_add(1);
                }
                KeyCode::PageUp => {
                    app.auto_scroll = false;
                    app.scroll_offset = app.scroll_offset.saturating_sub(5);
                }
                KeyCode::PageDown => {
                    app.scroll_offset = app.scroll_offset.saturating_add(5);
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
            Constraint::Length(8), // Team / Agents panel
            Constraint::Min(6),    // Activity log
            Constraint::Length(3), // Input bar
        ])
        .split(size);

    render_header(frame, app, chunks[0]);

    if !app.dirty_paths.is_empty() || app.reconnect_notice.is_some() {
        render_alerts(frame, app, chunks[1]);
    }

    render_agents(frame, app, chunks[2]);
    render_activity(frame, app, chunks[3]);
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

    let project_name = app
        .project
        .root
        .file_name()
        .map(|n| n.to_string_lossy())
        .unwrap_or_default();

    let header_text = vec![Line::from(vec![
        Span::styled(
            "TALOS Orchestrator",
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
                " ⚠️ Warning: {} uncommitted file(s) in repository. Commit or clean before running new worktrees! ",
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

fn render_agents(frame: &mut Frame, app: &TuiApp, area: Rect) {
    let agent_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);

    let active_task = app.active_tasks.first();

    // Left card: Writer Team
    let writer_profile = app.config.writer.as_str();
    let mut writer_lines = Vec::new();
    writer_lines.push(Line::from(vec![
        Span::styled("Adapter Profile: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            writer_profile,
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
    ]));

    if let Some(task) = active_task {
        writer_lines.push(Line::from(vec![
            Span::styled("Current Task: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("#{} {}", task.id, task.title),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
        ]));
        writer_lines.push(Line::from(vec![
            Span::styled("Status: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("● {}", task.status.as_str()),
                Style::default().fg(Color::Green),
            ),
        ]));
    } else {
        writer_lines.push(Line::from(vec![Span::styled(
            "Status: ○ Idle (Waiting for runnable tasks)",
            Style::default().fg(Color::DarkGray),
        )]));
    }

    let active_worktrees = app.worktrees.len();
    writer_lines.push(Line::from(vec![
        Span::styled("Worktrees: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!("{active_worktrees} active"),
            Style::default().fg(Color::Cyan),
        ),
    ]));

    let writer_block = Block::default()
        .borders(Borders::ALL)
        .title(" 🖊  Writer Team / Worker ")
        .border_style(Style::default().fg(Color::Yellow));

    frame.render_widget(
        Paragraph::new(writer_lines).block(writer_block),
        agent_chunks[0],
    );

    // Right card: Critic Team
    let critic_profile = app.config.critic.as_str();
    let mut critic_lines = Vec::new();
    critic_lines.push(Line::from(vec![
        Span::styled("Adapter Profile: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            critic_profile,
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        ),
    ]));

    if let Some(task) = active_task {
        let critic_status = match task.status {
            TaskStatus::Reviewing => "● Auditing & Reviewing",
            TaskStatus::Verifying => "● Running Verification Checks",
            _ => "○ Waiting for revision",
        };
        critic_lines.push(Line::from(vec![
            Span::styled("Status: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                critic_status,
                Style::default().fg(if task.status == TaskStatus::Reviewing {
                    Color::Magenta
                } else {
                    Color::DarkGray
                }),
            ),
        ]));
    } else {
        critic_lines.push(Line::from(vec![Span::styled(
            "Status: ○ Ready for audit",
            Style::default().fg(Color::DarkGray),
        )]));
    }

    let checks_count = app.config.checks.len();
    critic_lines.push(Line::from(vec![
        Span::styled("Checks Configured: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!("{checks_count} verification checks"),
            Style::default().fg(Color::White),
        ),
    ]));

    let critic_block = Block::default()
        .borders(Borders::ALL)
        .title(" 🛡  Critic Audit Team ")
        .border_style(Style::default().fg(Color::Magenta));

    frame.render_widget(
        Paragraph::new(critic_lines).block(critic_block),
        agent_chunks[1],
    );
}

fn render_activity(frame: &mut Frame, app: &TuiApp, area: Rect) {
    let mut items = Vec::new();

    if app.recent_events.is_empty() {
        items.push(ListItem::new(Line::from(Span::styled(
            "No events recorded yet. Type a task below to begin.",
            Style::default().fg(Color::DarkGray),
        ))));
    } else {
        for event in &app.recent_events {
            let actor_color = match event.actor.as_str() {
                "writer" | "writer_lead" => Color::Yellow,
                "critic" => Color::Magenta,
                "supervisor" => Color::Green,
                "git" | "worktree" => Color::Cyan,
                _ => Color::White,
            };

            let reason_preview = if event.reason.len() > 80 {
                format!("{}...", &event.reason[..77])
            } else {
                event.reason.clone()
            };

            items.push(ListItem::new(Line::from(vec![
                Span::styled(
                    format!("[Task #{}] ", event.task_id),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(
                    format!("[{}] ", event.actor),
                    Style::default()
                        .fg(actor_color)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("{}: ", event.event_type),
                    Style::default().fg(Color::White),
                ),
                Span::styled(reason_preview, Style::default().fg(Color::DarkGray)),
            ])));
        }
    }

    let log_block = Block::default()
        .borders(Borders::ALL)
        .title(" 📜 Activity & Supervision Log (Auto-updates) ")
        .border_style(Style::default().fg(Color::White));

    let list = List::new(items).block(log_block);
    frame.render_widget(list, area);
}

fn render_input(frame: &mut Frame, app: &TuiApp, area: Rect) {
    let title = if let Some((ref msg, _)) = app.status_message {
        format!(" Notice: {msg} ")
    } else {
        " Describe next task (Enter: Enqueue & Run | Ctrl+P: Pause/Resume | Ctrl+C/q: Exit) "
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
