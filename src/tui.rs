use std::{
    fs::{File, OpenOptions},
    io::{self, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    process::Child,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use crossterm::{
    event::{
        self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEvent, KeyEventKind,
        KeyModifiers,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph, Tabs, Wrap},
};
use std::os::unix::fs::OpenOptionsExt;

use crate::{
    adapter::AdapterId as AdapterKind,
    app::{
        GitStatusPaths, adapter_available_for_cockpit, config_fingerprint,
        current_project_revision_id, ensure_talos_ignored, git_status_paths,
    },
    checks::ProjectCheck,
    config::{OrchestrationMode, ProjectConfig},
    db::{
        CriticReviewRecord, Database, QueueSummary, StopRequestOutcome, TaskEvent, TaskRecord,
        TaskStatus, TaskSummaryRecord, TaskWorktreeRecord,
    },
    project::{DiscoveryMethod, Project},
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Screen {
    #[default]
    Main,
    Settings,
    WriterModel,
    CriticModel,
    Review,
    History,
    Task,
    ChooseProject,
}

struct TaskDraft {
    title: String,
    criteria: String,
    config: ProjectConfig,
}

pub struct TuiApp {
    project: Project,
    config: ProjectConfig,
    active_tab: ActiveTab,
    input: String,
    cursor: usize,
    screen: Screen,
    draft: Option<TaskDraft>,
    settings: Option<ProjectConfig>,
    selection: usize,
    available: Vec<AdapterKind>,
    history: Vec<TaskSummaryRecord>,
    history_before: Option<i64>,
    task_view: Option<i64>,
    detail_scroll: u16,
    event_baseline: i64,
    log_baseline: u64,
    child: Option<Child>,
    run_notice: Option<String>,
    task_status: Option<TaskStatus>,
    task_details: String,
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
    pub diff_file_index: usize,
    pub modified_files: Vec<String>,
    dirty_paths: GitStatusPaths,
    reconnect_notice: Option<String>,
    is_in_tmux: bool,
    pub should_exit: bool,
}

impl TuiApp {
    pub fn new(project: Project) -> Result<Self> {
        let available = available_adapters(&project);
        Self::with_adapters(project, available)
    }

    fn with_adapters(project: Project, available: Vec<AdapterKind>) -> Result<Self> {
        let mut config = ProjectConfig::load(&project)?;
        if matches!(project.discovery, DiscoveryMethod::Git)
            && !crate::config::config_path(&project.state_root).exists()
            && let Some(writer) = available.first().copied()
        {
            config.writer = writer;
            config.critic = available.get(1).copied().unwrap_or(writer);
            config.save(&project)?;
        }
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
            config,
            active_tab: ActiveTab::Feed,
            input: String::new(),
            cursor: 0,
            screen: if matches!(project.discovery, DiscoveryMethod::Git) {
                Screen::Main
            } else {
                Screen::ChooseProject
            },
            project,
            draft: None,
            settings: None,
            selection: 0,
            available,
            history: Vec::new(),
            history_before: None,
            task_view: None,
            detail_scroll: 0,
            event_baseline: 0,
            log_baseline: 0,
            child: None,
            run_notice: None,
            task_status: None,
            task_details: String::new(),
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
            diff_file_index: 0,
            modified_files: Vec::new(),
            dirty_paths,
            reconnect_notice: None,
            is_in_tmux,
            should_exit: false,
        };

        if matches!(app.project.discovery, DiscoveryMethod::Git) {
            app.refresh_state()?;
        }
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
        if !matches!(self.project.discovery, DiscoveryMethod::Git) {
            return Ok(());
        }
        if let Some(child) = &mut self.child
            && let Some(status) = child.try_wait()?
        {
            self.child = None;
            if !status.success()
                && let Some(id) = self.task_view
            {
                let log = recent_log_lines(&self.task_log(id), 0);
                self.run_notice = Some(run_recovery_hint(&log, &self.config));
            }
        }
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

        self.supervisor_running |= self.child.is_some();
        self.active_tasks = database.active_tasks()?;
        if let Some(id) = self.task_view {
            let task = database.get_task(id)?;
            self.task_status = task.as_ref().map(|task| task.status.clone());
            if let Some(task) = task {
                self.update_task_details(&database, &task)?;
            }
        }
        self.worktrees = database.task_worktrees().unwrap_or_default();
        self.recent_events = database.recent_events(50)?;
        self.recent_events
            .retain(|event| event.id > self.event_baseline);

        if let Some(active) = self.active_tasks.first() {
            self.latest_review = database.latest_critic_review(active.id).ok().flatten();
            self.verification_checks = database
                .task_verification_checks(active.id)
                .unwrap_or_default();
        } else {
            self.latest_review = None;
            self.verification_checks = self.config.checks.clone();
        }

        self.dirty_paths = git_status_paths(&self.project)?;
        self.read_supervisor_logs();
        if self.active_tab == ActiveTab::Diff {
            self.load_git_diff();
        }
        Ok(())
    }

    fn read_supervisor_logs(&mut self) {
        let (path, baseline) = match self.task_view {
            Some(id) => (self.task_log(id), 0),
            None => (
                self.project.root.join(".talos").join("supervisor.log"),
                self.log_baseline,
            ),
        };
        self.supervisor_logs = recent_log_lines(&path, baseline);
    }

    fn load_git_diff(&mut self) {
        let mut status_cmd = std::process::Command::new("git");
        status_cmd
            .args(["status", "--porcelain"])
            .current_dir(&self.project.root);
        if let Ok(out) = status_cmd.output() {
            let text = String::from_utf8_lossy(&out.stdout);
            self.modified_files = text
                .lines()
                .filter(|l| l.len() >= 4)
                .map(|l| l[3..].trim().to_string())
                .collect();
        }
        if self.modified_files.is_empty() {
            self.diff_file_index = 0;
        } else if self.diff_file_index >= self.modified_files.len() {
            self.diff_file_index = self.modified_files.len() - 1;
        }

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

    pub fn selected_modified_file(&self) -> Option<&str> {
        self.modified_files
            .get(self.diff_file_index)
            .map(|s| s.as_str())
    }

    pub fn stage_selected_file(&mut self) -> Result<()> {
        if let Some(file) = self.selected_modified_file() {
            let f = file.to_string();
            let mut cmd = std::process::Command::new("git");
            cmd.args(["add", "--", &f]).current_dir(&self.project.root);
            if let Ok(out) = cmd.output()
                && out.status.success()
            {
                self.status_message = Some((format!("Staged: {f}"), Instant::now()));
                self.refresh_state()?;
                self.load_git_diff();
            }
        }
        Ok(())
    }

    pub fn discard_selected_file(&mut self) -> Result<()> {
        if let Some(file) = self.selected_modified_file() {
            let f = file.to_string();
            let path = self.project.root.join(&f);
            let mut checkout_cmd = std::process::Command::new("git");
            checkout_cmd
                .args(["checkout", "--", &f])
                .current_dir(&self.project.root);
            if let Ok(out) = checkout_cmd.output() {
                if out.status.success() {
                    self.status_message =
                        Some((format!("Discarded changes in: {f}"), Instant::now()));
                } else if path.exists() {
                    let _ = std::fs::remove_file(&path);
                    self.status_message =
                        Some((format!("Removed untracked file: {f}"), Instant::now()));
                }
            }
            self.refresh_state()?;
            self.load_git_diff();
        }
        Ok(())
    }

    pub fn find_project_files(&self) -> Vec<String> {
        let mut cmd = std::process::Command::new("git");
        cmd.arg("ls-files").current_dir(&self.project.root);
        if let Ok(out) = cmd.output() {
            let text = String::from_utf8_lossy(&out.stdout);
            text.lines()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        } else {
            Vec::new()
        }
    }

    pub fn complete_input(&mut self) {
        if self.input.is_empty() {
            return;
        }

        let input_text = self.input.clone();
        let tokens: Vec<&str> = input_text.split_whitespace().collect();

        // 1. Completing a slash command
        if input_text.starts_with('/') && !input_text.contains(' ') {
            let slash_commands = [
                "/cli",
                "/config",
                "/new",
                "/history",
                "/commit",
                "/mode",
                "/writer",
                "/critic",
                "/model",
                "/stop",
                "/pause",
                "/resume",
                "/run",
                "/feed",
                "/diff",
                "/dialogue",
                "/audit",
                "/status",
                "/stash",
                "/task:test",
                "/task:refactor",
                "/task:fix",
                "/task:bench",
                "/clear",
                "/help",
                "/exit",
                "/кли",
                "/тест",
                "/рефакторинг",
                "/фикс",
                "/фид",
                "/лента",
                "/дифф",
                "/диалог",
                "/аудит",
                "/статус",
                "/стоп",
                "/пауза",
                "/выход",
            ];
            let matches: Vec<String> = slash_commands
                .iter()
                .filter(|c| c.starts_with(&input_text))
                .map(|s| s.to_string())
                .collect();

            if matches.len() == 1 {
                self.input = format!("{} ", matches[0]);
            } else if matches.len() > 1 {
                let common = longest_common_prefix(&matches);
                if common.len() > self.input.len() {
                    self.input = common;
                } else {
                    self.status_message = Some((
                        format!("Suggestions: {}", matches.join("  ")),
                        Instant::now(),
                    ));
                }
            }
            return;
        }

        // 2. Completing arguments for /cli, /writer, /critic, /mode
        if tokens.len() == 1 && input_text.ends_with(' ') {
            let cmd = tokens[0].to_ascii_lowercase();
            if cmd == "/cli" || cmd == "/writer" || cmd == "/critic" {
                self.status_message = Some((
                    "Adapters: agy  codex  claude  omp  pi  kilo  opencode".to_string(),
                    Instant::now(),
                ));
                return;
            } else if cmd == "/mode" {
                self.status_message = Some(("Modes: pair  team".to_string(), Instant::now()));
                return;
            }
        }

        if tokens.len() >= 2 {
            let cmd = tokens[0].to_ascii_lowercase();
            let last_token = tokens[tokens.len() - 1];

            if (cmd == "/cli" || cmd == "/writer" || cmd == "/critic") && !input_text.ends_with(' ')
            {
                let adapters = ["agy", "codex", "claude", "omp", "pi", "kilo", "opencode"];
                let matches: Vec<String> = adapters
                    .iter()
                    .filter(|a| a.starts_with(last_token))
                    .map(|s| s.to_string())
                    .collect();
                if matches.len() == 1 {
                    let mut prefix = tokens[..tokens.len() - 1].join(" ");
                    prefix.push(' ');
                    prefix.push_str(&matches[0]);
                    prefix.push(' ');
                    self.input = prefix;
                } else if !matches.is_empty() {
                    self.status_message =
                        Some((format!("Adapters: {}", matches.join("  ")), Instant::now()));
                }
                return;
            }

            if cmd == "/mode" && !input_text.ends_with(' ') {
                let modes = ["pair", "team"];
                let matches: Vec<String> = modes
                    .iter()
                    .filter(|m| m.starts_with(last_token))
                    .map(|s| s.to_string())
                    .collect();
                if matches.len() == 1 {
                    self.input = format!("{} {} ", tokens[0], matches[0]);
                }
                return;
            }
        }

        // 3. File path completion for project files
        let last_word = if input_text.ends_with(' ') {
            ""
        } else {
            tokens.last().copied().unwrap_or("")
        };

        if !last_word.is_empty() {
            let files = self.find_project_files();
            let matches: Vec<String> = files
                .into_iter()
                .filter(|f| f.starts_with(last_word))
                .collect();

            if matches.len() == 1 {
                let prefix = if input_text.contains(' ') {
                    let mut p = tokens[..tokens.len() - 1].join(" ");
                    p.push(' ');
                    p
                } else {
                    String::new()
                };
                self.input = format!("{}{}", prefix, matches[0]);
            } else if matches.len() > 1 {
                let common = longest_common_prefix(&matches);
                if common.len() > last_word.len() {
                    let prefix = if input_text.contains(' ') {
                        let mut p = tokens[..tokens.len() - 1].join(" ");
                        p.push(' ');
                        p
                    } else {
                        String::new()
                    };
                    self.input = format!("{}{}", prefix, common);
                } else {
                    let preview = if matches.len() > 5 {
                        format!(
                            "Files: {} ... (+{} more)",
                            matches[..5].join("  "),
                            matches.len() - 5
                        )
                    } else {
                        format!("Files: {}", matches.join("  "))
                    };
                    self.status_message = Some((preview, Instant::now()));
                }
            }
        }
    }

    pub fn handle_submit(&mut self) -> Result<()> {
        let text = self.input.trim().to_owned();
        if self.screen == Screen::Task && text.starts_with('/') {
            self.execute_slash_command(&text)?;
            self.clear_input();
            return Ok(());
        }
        match self.screen {
            Screen::ChooseProject => {
                if text.is_empty() {
                    return Ok(());
                }
                let path = text
                    .strip_prefix("~/")
                    .and_then(|tail| {
                        std::env::var_os("HOME").map(|home| PathBuf::from(home).join(tail))
                    })
                    .unwrap_or_else(|| PathBuf::from(&text));
                let project = Project::discover(&path)?;
                anyhow::ensure!(
                    matches!(project.discovery, DiscoveryMethod::Git),
                    "Choose a folder inside a Git repository."
                );
                ensure_talos_ignored(&project)?;
                *self = Self::new(project)?;
            }
            Screen::Main if !text.is_empty() => {
                if text.starts_with('/') {
                    self.execute_slash_command(&text)?;
                } else {
                    let first_line = text.lines().next().unwrap_or_default().trim();
                    let end = first_line.floor_char_boundary(first_line.len().min(160));
                    self.prepare_draft(first_line[..end].to_owned(), text);
                }
            }
            Screen::WriterModel | Screen::CriticModel => {
                let settings = self.settings.as_mut().context("Open Settings first")?;
                anyhow::ensure!(
                    text.len() <= 200 && !crate::secrets::contains_sensitive(&text),
                    "Enter a model name, up to 200 bytes."
                );
                let model = (!text.is_empty()).then_some(text);
                if self.screen == Screen::WriterModel {
                    settings.writer_model = model;
                } else {
                    settings.critic_model = model;
                }
                self.screen = Screen::Settings;
            }
            Screen::Settings => {
                let settings = self.settings.as_ref().context("Open Settings first")?;
                settings.save(&self.project)?;
                self.config = settings.clone();
                self.settings = None;
                if let Some(draft) = &mut self.draft {
                    draft.config = self.config.clone();
                    self.screen = Screen::Review;
                } else {
                    self.screen = Screen::Main;
                }
                self.notice("Settings saved.");
            }
            Screen::Review => self.run_draft()?,
            Screen::History => {
                if let Some(task) = self.history.get(self.selection) {
                    self.task_view = Some(task.id);
                    self.run_notice = None;
                    self.detail_scroll = 0;
                    self.screen = Screen::Task;
                    self.refresh_state()?;
                }
            }
            _ => {}
        }
        self.clear_input();
        Ok(())
    }

    fn notice(&mut self, message: impl Into<String>) {
        self.status_message = Some((message.into(), Instant::now()));
    }

    fn clear_input(&mut self) {
        self.input.clear();
        self.cursor = 0;
    }

    fn prepare_draft(&mut self, title: String, criteria: String) {
        self.draft = Some(TaskDraft {
            title,
            criteria,
            config: self.config.clone(),
        });
        self.screen = Screen::Review;
        self.detail_scroll = 0;
        self.status_message = None;
    }

    fn run_draft(&mut self) -> Result<()> {
        self.refresh_state()?;
        let draft = self.draft.as_ref().context("Create a task first")?;
        anyhow::ensure!(
            draft.config == self.config,
            "Project settings changed. Open Settings and save to review the new assignments."
        );
        if let Some(blocker) = self.run_blocker() {
            anyhow::bail!("{blocker}");
        }
        let mut database = Database::open(&self.project)?;
        let task =
            database.add_task_with_checks(&draft.title, &draft.criteria, &draft.config.checks)?;
        self.draft = None;
        self.task_view = Some(task.id);
        self.run_notice = None;
        self.detail_scroll = 0;
        self.screen = Screen::Task;
        self.start_selected_task(task.id)
    }

    fn run_blocker(&self) -> Option<String> {
        if self.supervisor_running || !self.active_tasks.is_empty() {
            return Some(
                "A task is active. Wait for it to finish or request Stop for its Writer.".into(),
            );
        }
        if self.queue_paused {
            return Some("The queue is paused. Press U to resume the queue, then retry.".into());
        }
        if !self.dirty_paths.is_empty() {
            return Some(format!(
                "Project has uncommitted changes. Press D to view them. Commit or stash your own changes, then retry. {}",
                self.dirty_paths.display_preview()
            ));
        }
        for adapter in [self.config.writer, self.config.critic] {
            if !self.available.contains(&adapter) {
                return Some(format!(
                    "{} was not found or failed its capability check. Press S to choose an available CLI, or install it and press R to check again.",
                    adapter.as_str()
                ));
            }
        }
        None
    }

    fn open_settings(&mut self) {
        self.available = available_adapters(&self.project);
        self.settings = Some(self.config.clone());
        self.selection = 0;
        self.screen = Screen::Settings;
        self.status_message = None;
    }

    fn new_session(&mut self) -> Result<()> {
        self.refresh_state()?;
        anyhow::ensure!(
            !self.supervisor_running && self.active_tasks.is_empty(),
            "A task is still active. Wait for it to finish or use /stop to stop the active Writer."
        );
        self.event_baseline = Database::open(&self.project)?
            .recent_events(1)?
            .last()
            .map_or(0, |event| event.id);
        self.log_baseline =
            std::fs::metadata(self.project.root.join(".talos").join("supervisor.log"))
                .map_or(0, |metadata| metadata.len());
        self.clear_input();
        self.draft = None;
        self.task_view = None;
        self.task_status = None;
        self.run_notice = None;
        self.reconnect_notice = None;
        self.scroll_offset = 0;
        self.active_tab = ActiveTab::Feed;
        self.screen = Screen::Main;
        self.notice("New session. Describe your next task and press Enter to review.");
        Ok(())
    }

    fn task_log(&self, id: i64) -> PathBuf {
        self.project
            .root
            .join(".talos")
            .join(format!("task-{id}.log"))
    }

    fn open_history(&mut self, before: Option<i64>) -> Result<()> {
        self.history = Database::open(&self.project)?.recent_tasks(before, 50)?;
        self.history_before = before;
        self.selection = 0;
        self.screen = Screen::History;
        self.status_message = None;
        Ok(())
    }

    fn update_task_details(&mut self, database: &Database, task: &TaskRecord) -> Result<()> {
        let report = crate::summary::task_summary_path(&self.project.root, task.id);
        let mut details = format!(
            "Task #{} · {}\n{}\n",
            task.id,
            task.status.as_str(),
            task.title
        );
        if let Some(integration) = database.task_worktree_integration(task.id)? {
            details.push_str("\nChanged files\n");
            details.push_str(&integration.changed_paths.join("\n"));
            details.push('\n');
        }
        let report_text = open_runtime_read(&report).and_then(|file| {
            let mut text = Vec::new();
            file.take(64 * 1024).read_to_end(&mut text)?;
            Ok(String::from_utf8_lossy(&text).into_owned())
        });
        if let Ok(report_text) = report_text {
            details.push('\n');
            details.push_str(&report_text);
        } else {
            let events = database.task_events_page(Some(task.id), None, 20)?;
            for event in events.events.iter().take(5).rev() {
                details.push_str(&format!("\n{}", event.reason));
            }
        }
        if self.screen == Screen::Task {
            let logs = recent_log_lines(&self.task_log(task.id), 0);
            if !logs.is_empty() {
                details.push_str("\n\nRun details\n");
                details.push_str(&logs.join("\n"));
            }
        }
        self.task_details = details;
        Ok(())
    }

    pub fn submit_task(&mut self, task_text: &str) -> Result<()> {
        self.prepare_draft(task_text.to_owned(), task_text.to_owned());
        Ok(())
    }

    pub fn execute_slash_command(&mut self, command: &str) -> Result<()> {
        let parts: Vec<&str> = command.splitn(2, ' ').collect();
        let cmd = parts[0].to_ascii_lowercase();
        let arg = parts.get(1).copied().unwrap_or("").trim();

        match cmd.as_str() {
            "/help" | "/помощь" | "/справка" => {
                self.status_message = Some((
                    "Commands: /config, /new, /history, /cli <writer> <critic>, /task:test <file>, /task:refactor <file>, /task:fix <path> <issue>, /task:bench <path>, /stop, /pause, /resume, /run, /commit <msg>, /feed, /diff, /dialogue, /audit, /writer, /critic, /model, /mode, /exit".to_string(),
                    Instant::now(),
                ));
            }
            "/task:test" | "/тест" => {
                if arg.is_empty() {
                    self.status_message = Some((
                        "Usage: /task:test <file_path> (e.g. /task:test calc.py)".to_string(),
                        Instant::now(),
                    ));
                } else {
                    let title = format!("Add comprehensive unit tests for {arg}");
                    let criteria = format!(
                        "Verify {arg} behavior with thorough unit tests. Production code in {arg} must not be altered. All assertions must pass."
                    );
                    self.prepare_draft(title, criteria);
                }
            }
            "/task:refactor" | "/рефакторинг" => {
                if arg.is_empty() {
                    self.status_message = Some((
                        "Usage: /task:refactor <file_path> (e.g. /task:refactor src/tui.rs)"
                            .to_string(),
                        Instant::now(),
                    ));
                } else {
                    let title = format!("Refactor {arg} for clarity and maintainability");
                    let criteria = format!(
                        "Refactor {arg} to improve code clarity and structure without breaking behavior or altering public API. All existing tests must pass."
                    );
                    self.prepare_draft(title, criteria);
                }
            }
            "/task:fix" | "/фикс" => {
                if arg.is_empty() {
                    self.status_message = Some((
                        "Usage: /task:fix <path> <issue> (e.g. /task:fix calc.py handle negative square root)"
                            .to_string(),
                        Instant::now(),
                    ));
                } else {
                    let title = format!("Fix defect: {arg}");
                    let criteria = format!(
                        "Diagnose and resolve defect: {arg}. Ensure full test suite passes and add regression test coverage."
                    );
                    self.prepare_draft(title, criteria);
                }
            }
            "/task:bench" => {
                if arg.is_empty() {
                    self.status_message = Some((
                        "Usage: /task:bench <path> (e.g. /task:bench calc.py)".to_string(),
                        Instant::now(),
                    ));
                } else {
                    let title = format!("Benchmark and optimize performance for {arg}");
                    let criteria = format!(
                        "Measure execution performance and optimize critical paths in {arg}. Verify functional correctness with unit tests."
                    );
                    self.prepare_draft(title, criteria);
                }
            }
            "/config" | "/settings" => self.open_settings(),
            "/new" | "/session" => self.new_session()?,
            "/history" => self.open_history(None)?,
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
                let outcome = database.resume_project(revision_id.as_deref(), false)?;
                self.refresh_state()?;
                self.notice(if self.queue_paused {
                    format!(
                        "Queue still paused. Inspect project changes before resuming. {outcome:?}"
                    )
                } else {
                    "Queue resumed. Open History to retry a queued task.".to_owned()
                });
            }
            "/quit" | "/exit" | "/выход" | "/q" => {
                self.should_exit = true;
            }
            "/clear" | "/очистить" => {
                self.status_message = None;
                self.clear_input();
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

    fn start_selected_task(&mut self, id: i64) -> Result<()> {
        anyhow::ensure!(self.child.is_none(), "A task is still running.");
        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(self.task_log(id))
            .context("Cannot open the task log. Check that .talos is writable, then retry.")?;
        anyhow::ensure!(
            log.metadata()?.is_file(),
            "The task log must be a regular file."
        );
        use std::os::unix::process::CommandExt;
        let expected_config = config_fingerprint(&self.config)?;
        let child = std::process::Command::new(std::env::current_exe()?)
            .args([
                "up",
                "--auto",
                "--task",
                &id.to_string(),
                "--expected-config",
                &expected_config,
            ])
            .process_group(0)
            .current_dir(&self.project.root)
            .stdin(std::process::Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log)
            .spawn()
            .context("Cannot start TALOS. Check the installation and retry.")?;
        self.child = Some(child);
        self.supervisor_running = true;
        self.run_notice = None;
        self.notice(format!("Starting task #{id}."));
        self.refresh_state()
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
                    if self.status_message.is_none() {
                        self.status_message = Some((
                            format!("Background worker spawned (PID: {}).", child.id()),
                            Instant::now(),
                        ));
                    }
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

fn available_adapters(project: &Project) -> Vec<AdapterKind> {
    if !matches!(project.discovery, DiscoveryMethod::Git) {
        return Vec::new();
    }
    [
        AdapterKind::Codex,
        AdapterKind::Claude,
        AdapterKind::Agy,
        AdapterKind::OpenCode,
        AdapterKind::Pi,
        AdapterKind::Omp,
        AdapterKind::Kilo,
    ]
    .into_iter()
    .filter(|adapter| adapter_available_for_cockpit(*adapter, &project.root))
    .collect()
}

fn run_recovery_hint(log: &[String], config: &ProjectConfig) -> String {
    if log.iter().rev().any(|line| {
        let line = line.to_ascii_lowercase();
        [
            "not logged in",
            "authentication required",
            "unauthorized",
            "status 401",
            "api key is missing",
            "invalid api key",
            "please run /login",
        ]
        .iter()
        .any(|marker| line.contains(marker))
    }) {
        let mut commands = [config.writer, config.critic]
            .into_iter()
            .filter_map(|adapter| match adapter {
                AdapterKind::Codex => Some("codex login"),
                AdapterKind::Claude => Some("claude auth login"),
                _ => None,
            })
            .collect::<Vec<_>>();
        commands.dedup();
        let actions = if commands.is_empty() {
            "Sign in or configure credentials in the selected agent's own CLI.".to_owned()
        } else {
            format!(
                "Check the failing role below. Sign in from a shell with {} where applicable.",
                commands.join(" or ")
            )
        };
        format!(
            "The agent could not authenticate. {actions} Return to TALOS to retry, or press F2 to choose another CLI."
        )
    } else {
        "Run stopped. See Run details below for the error and the next action.".to_owned()
    }
}

fn open_runtime_read(path: &Path) -> io::Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::other("Expected a regular runtime file"));
    }
    Ok(file)
}

fn recent_log_lines(path: &Path, baseline: u64) -> Vec<String> {
    let Ok(mut file) = open_runtime_read(path) else {
        return Vec::new();
    };
    let Ok(metadata) = file.metadata() else {
        return Vec::new();
    };
    let end = metadata.len();
    let start = baseline.min(end).max(end.saturating_sub(48 * 1024));
    if file.seek(SeekFrom::Start(start)).is_err() {
        return Vec::new();
    }
    let mut bytes = Vec::new();
    if file.take(end - start).read_to_end(&mut bytes).is_err() {
        return Vec::new();
    }
    String::from_utf8_lossy(&bytes)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(crate::secrets::redact_sensitive)
        .collect()
}

impl TuiApp {
    fn change_setting(&mut self, forward: bool) -> Result<()> {
        let settings = self.settings.as_mut().context("Open Settings first")?;
        match self.selection {
            0 | 2 => {
                let (adapter, model) = if self.selection == 0 {
                    (&mut settings.writer, &mut settings.writer_model)
                } else {
                    (&mut settings.critic, &mut settings.critic_model)
                };
                if self.available.is_empty() {
                    self.notice(
                        "No compatible CLI was found. Install an agent and press R to check again.",
                    );
                    return Ok(());
                }
                let next = self
                    .available
                    .iter()
                    .position(|candidate| candidate == adapter)
                    .map_or(0, |position| {
                        (position + if forward { 1 } else { self.available.len() - 1 })
                            % self.available.len()
                    });
                if *adapter != self.available[next] {
                    *adapter = self.available[next];
                    *model = None;
                }
            }
            1 | 3 => {
                let writer = self.selection == 1;
                self.input = if writer {
                    settings.writer_model.clone()
                } else {
                    settings.critic_model.clone()
                }
                .unwrap_or_default();
                self.cursor = self.input.len();
                self.screen = if writer {
                    Screen::WriterModel
                } else {
                    Screen::CriticModel
                };
            }
            _ => {
                settings.orchestration.mode = match settings.orchestration.mode {
                    OrchestrationMode::Pair => OrchestrationMode::Team,
                    OrchestrationMode::Team => OrchestrationMode::Pair,
                };
            }
        }
        Ok(())
    }

    fn handle_key(&mut self, key: KeyEvent) -> Result<()> {
        let modifiers = key.modifiers;
        let ctrl = modifiers.contains(KeyModifiers::CONTROL);
        if key.kind == KeyEventKind::Release {
            return Ok(());
        }
        let repeat = key.kind == KeyEventKind::Repeat;
        if repeat
            && !matches!(
                key.code,
                KeyCode::Backspace
                    | KeyCode::Delete
                    | KeyCode::Left
                    | KeyCode::Right
                    | KeyCode::Up
                    | KeyCode::Down
                    | KeyCode::Char(_)
            )
        {
            return Ok(());
        }
        if matches!(key.code, KeyCode::Char('c')) && ctrl && !repeat {
            self.should_exit = true;
            return Ok(());
        }

        let editing = matches!(
            self.screen,
            Screen::Main | Screen::ChooseProject | Screen::WriterModel | Screen::CriticModel
        );
        if key.code == KeyCode::Enter {
            return self.handle_submit();
        }
        if key.code == KeyCode::Esc {
            if self.screen == Screen::Task && !self.input.is_empty() {
                self.clear_input();
                return Ok(());
            }
            self.screen = match self.screen {
                Screen::Main if !self.input.is_empty() => Screen::Main,
                Screen::Main => {
                    self.active_tab = ActiveTab::Feed;
                    Screen::Main
                }
                Screen::Settings if self.draft.is_some() => {
                    self.settings = None;
                    Screen::Review
                }
                Screen::Settings => {
                    self.settings = None;
                    Screen::Main
                }
                Screen::WriterModel | Screen::CriticModel => Screen::Settings,
                Screen::Review => {
                    if let Some(draft) = self.draft.take() {
                        self.input = draft.criteria;
                        self.cursor = self.input.len();
                    }
                    self.screen = Screen::Main;
                    return Ok(());
                }
                Screen::ChooseProject => {
                    self.clear_input();
                    return Ok(());
                }
                _ => Screen::Main,
            };
            self.clear_input();
            self.status_message = None;
            return Ok(());
        }

        if !repeat && self.screen != Screen::ChooseProject {
            match key.code {
                KeyCode::F(2) => {
                    self.clear_input();
                    self.open_settings();
                    return Ok(());
                }
                KeyCode::F(3) => return self.open_history(None),
                KeyCode::F(4) => return self.new_session(),
                _ => {}
            }
        }

        if self.screen == Screen::Task && (!self.input.is_empty() || key.code == KeyCode::Char('/'))
        {
            self.edit_input(key, repeat);
            return Ok(());
        }

        match self.screen {
            Screen::Settings => match key.code {
                KeyCode::Up => self.selection = (self.selection + 4) % 5,
                KeyCode::Down | KeyCode::Tab => self.selection = (self.selection + 1) % 5,
                KeyCode::Left => self.change_setting(false)?,
                KeyCode::Right | KeyCode::Char('e' | 'E') if !repeat => {
                    self.change_setting(true)?
                }
                KeyCode::Char('r' | 'R') if !repeat => {
                    self.available = available_adapters(&self.project);
                    self.notice("Checked for installed agent CLIs.");
                }
                _ => {}
            },
            Screen::Review => match key.code {
                KeyCode::Char('s' | 'S') if !repeat => self.open_settings(),
                KeyCode::Char('d' | 'D') if !repeat => {
                    self.active_tab = ActiveTab::Diff;
                    self.load_git_diff();
                    self.screen = Screen::Main;
                    self.notice("Review your changes. Press F5 to return to the task draft.");
                }
                KeyCode::Char('r' | 'R') if !repeat => {
                    self.available = available_adapters(&self.project);
                    self.refresh_state()?;
                }
                KeyCode::Char('u' | 'U') if !repeat => self.execute_slash_command("/resume")?,
                KeyCode::Up => self.detail_scroll = self.detail_scroll.saturating_sub(1),
                KeyCode::Down => self.detail_scroll = self.detail_scroll.saturating_add(1),
                _ => {}
            },
            Screen::History => match key.code {
                KeyCode::Up => self.selection = self.selection.saturating_sub(1),
                KeyCode::Down => {
                    self.selection = (self.selection + 1).min(self.history.len().saturating_sub(1))
                }
                KeyCode::PageDown if !repeat && self.history.len() == 50 => {
                    self.open_history(self.history.last().map(|task| task.id))?;
                }
                KeyCode::PageUp if !repeat && self.history_before.is_some() => {
                    self.open_history(None)?
                }
                _ => {}
            },
            Screen::Task => match key.code {
                KeyCode::Up => self.detail_scroll = self.detail_scroll.saturating_sub(1),
                KeyCode::Down => self.detail_scroll = self.detail_scroll.saturating_add(1),
                KeyCode::PageUp => self.detail_scroll = self.detail_scroll.saturating_sub(10),
                KeyCode::PageDown => self.detail_scroll = self.detail_scroll.saturating_add(10),
                KeyCode::End => {
                    self.detail_scroll = self
                        .task_details
                        .lines()
                        .count()
                        .saturating_sub(10)
                        .min(u16::MAX as usize) as u16
                }
                KeyCode::Char('s' | 'S')
                    if !repeat
                        && self
                            .active_tasks
                            .iter()
                            .any(|task| Some(task.id) == self.task_view) =>
                {
                    self.execute_slash_command("/stop")?
                }
                KeyCode::Char('r' | 'R')
                    if !repeat && self.task_status == Some(TaskStatus::Queued) =>
                {
                    self.available = available_adapters(&self.project);
                    self.refresh_state()?;
                    if let Some(blocker) = self.run_blocker() {
                        anyhow::bail!("{blocker}");
                    }
                    if let Some(id) = self.task_view {
                        self.start_selected_task(id)?;
                    }
                }
                KeyCode::Char('c' | 'C')
                    if !repeat
                        && !self.supervisor_running
                        && self.task_status == Some(TaskStatus::Queued) =>
                {
                    if let Some(id) = self.task_view {
                        Database::open(&self.project)?.cancel_queued_task(id)?;
                        self.run_notice = Some(format!("Task #{id} cancelled."));
                        self.refresh_state()?;
                    }
                }
                _ => {}
            },
            _ if editing => self.edit_input(key, repeat),
            _ => {}
        }
        Ok(())
    }

    fn edit_input(&mut self, key: KeyEvent, repeat: bool) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let main = self.screen == Screen::Main;
        let previous = self.input[..self.cursor]
            .char_indices()
            .next_back()
            .map_or(0, |(index, _)| index);
        let next = self.input[self.cursor..]
            .chars()
            .next()
            .map_or(self.cursor, |ch| self.cursor + ch.len_utf8());
        match key.code {
            KeyCode::Backspace | KeyCode::Char('w')
                if alt || (key.code == KeyCode::Char('w') && ctrl) =>
            {
                let prefix = self.input[..self.cursor].trim_end();
                let start = prefix.rfind(char::is_whitespace).map_or(0, |index| {
                    index + prefix[index..].chars().next().map_or(0, char::len_utf8)
                });
                self.input.drain(start..self.cursor);
                self.cursor = start;
            }
            KeyCode::Char('u') if ctrl => self.clear_input(),
            KeyCode::Backspace => {
                self.input.drain(previous..self.cursor);
                self.cursor = previous;
            }
            KeyCode::Delete => {
                self.input.drain(self.cursor..next);
            }
            KeyCode::Left => self.cursor = previous,
            KeyCode::Right => self.cursor = next,
            KeyCode::Home if !main || !self.input.is_empty() => self.cursor = 0,
            KeyCode::End if !main || !self.input.is_empty() => self.cursor = self.input.len(),
            KeyCode::F(5) if main && self.draft.is_some() => self.screen = Screen::Review,
            KeyCode::Tab if main && self.input.is_empty() && !repeat => {
                self.active_tab = self.active_tab.next();
                if self.active_tab == ActiveTab::Diff {
                    self.load_git_diff();
                }
            }
            KeyCode::Tab if main => {
                self.complete_input();
                self.cursor = self.input.len();
            }
            KeyCode::Char(ch @ '1'..='4')
                if main && self.input.is_empty() && !repeat && !alt && !ctrl =>
            {
                self.active_tab = [
                    ActiveTab::Feed,
                    ActiveTab::Dialogue,
                    ActiveTab::Diff,
                    ActiveTab::Audit,
                ][ch as usize - '1' as usize];
                if self.active_tab == ActiveTab::Diff {
                    self.load_git_diff();
                }
            }
            KeyCode::Up if main && alt && self.active_tab == ActiveTab::Diff => {
                self.diff_file_index = self.diff_file_index.saturating_sub(1);
                self.load_git_diff();
            }
            KeyCode::Down if main && alt && self.active_tab == ActiveTab::Diff => {
                self.diff_file_index =
                    (self.diff_file_index + 1).min(self.modified_files.len().saturating_sub(1));
                self.load_git_diff();
            }
            KeyCode::Up if main => self.scroll_offset = self.scroll_offset.saturating_add(1),
            KeyCode::Down if main => self.scroll_offset = self.scroll_offset.saturating_sub(1),
            KeyCode::PageUp if main => self.scroll_offset = self.scroll_offset.saturating_add(5),
            KeyCode::PageDown if main => self.scroll_offset = self.scroll_offset.saturating_sub(5),
            KeyCode::Home if main => self.scroll_offset = usize::MAX,
            KeyCode::End if main => self.scroll_offset = 0,
            KeyCode::Char(ch) if !ctrl && !alt && !ch.is_control() && self.input.len() < 8_000 => {
                self.input.insert(self.cursor, ch);
                self.cursor += ch.len_utf8();
            }
            _ => {}
        }
    }

    fn paste(&mut self, text: &str) {
        if !matches!(
            self.screen,
            Screen::Main | Screen::ChooseProject | Screen::WriterModel | Screen::CriticModel
        ) {
            return;
        }
        let remaining = 8_000_usize.saturating_sub(self.input.len());
        let clean: String = text
            .chars()
            .filter(|ch| !ch.is_control() || matches!(*ch, '\n' | '\t'))
            .collect();
        let clean = &clean[..clean.floor_char_boundary(remaining.min(clean.len()))];
        let single_line = clean.replace(['\n', '\t'], " ");
        let text = if self.screen == Screen::Main {
            clean
        } else {
            &single_line
        };
        self.input.insert_str(self.cursor, text);
        self.cursor += text.len();
    }
}

pub async fn run_tui(project: Project) -> Result<()> {
    ensure_talos_ignored(&project)?;
    let mut app = TuiApp::new(project)?;
    enable_raw_mode().context("Failed to enable raw mode")?;
    let mut stdout = io::stdout();
    if let Err(error) = execute!(stdout, EnterAlternateScreen, EnableBracketedPaste) {
        let _ = disable_raw_mode();
        return Err(error.into());
    }
    let mut terminal = match Terminal::new(CrosstermBackend::new(stdout)) {
        Ok(terminal) => terminal,
        Err(error) => {
            let _ = execute!(io::stdout(), DisableBracketedPaste, LeaveAlternateScreen);
            let _ = disable_raw_mode();
            return Err(error.into());
        }
    };
    let result = run_app_loop(&mut terminal, &mut app).await;
    let _ = execute!(
        terminal.backend_mut(),
        DisableBracketedPaste,
        LeaveAlternateScreen
    );
    let _ = disable_raw_mode();
    let _ = terminal.show_cursor();
    result
}

async fn run_app_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut TuiApp,
) -> Result<()> {
    let mut last_tick = Instant::now();
    let tick_rate = Duration::from_millis(300);
    while !app.should_exit {
        terminal.draw(|frame| ui(frame, app))?;
        if event::poll(tick_rate.saturating_sub(last_tick.elapsed()))? {
            match event::read()? {
                Event::Key(key) => {
                    if let Err(error) = app.handle_key(key) {
                        app.notice(crate::secrets::redact_sensitive(&format!("{error:#}")));
                    }
                }
                Event::Paste(text) => app.paste(&text),
                _ => {}
            }
        }
        if last_tick.elapsed() >= tick_rate {
            if let Err(error) = app.refresh_state() {
                app.notice(crate::secrets::redact_sensitive(&format!("{error:#}")));
            }
            last_tick = Instant::now();
        }
    }
    Ok(())
}

fn ui(frame: &mut Frame, app: &TuiApp) {
    let size = frame.area();
    if app.screen != Screen::Main {
        render_workflow(frame, app, size);
        return;
    }

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
            Constraint::Length(4), // Input bar
            Constraint::Length(if app.status_message.is_some() { 2 } else { 0 }),
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
    if let Some((message, _)) = &app.status_message {
        frame.render_widget(
            Paragraph::new(safe_display(message)).wrap(Wrap { trim: true }),
            chunks[5],
        );
    }
}

fn render_workflow(frame: &mut Frame, app: &TuiApp, area: Rect) {
    let editing = matches!(
        app.screen,
        Screen::ChooseProject | Screen::WriterModel | Screen::CriticModel | Screen::Task
    );
    let chunks = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(4),
        Constraint::Length(if editing { 4 } else { 0 }),
        Constraint::Length(4),
    ])
    .split(area);
    let (title, body, actions) = match app.screen {
        Screen::ChooseProject => (
            "Choose project",
            "Enter the path to an existing Git project. If you have just installed TALOS, you can also open a terminal inside your project and run talos there.".into(),
            "Enter  Open project   Esc  Clear path".to_owned(),
        ),
        Screen::Settings => {
            let config = app.settings.as_ref().unwrap_or(&app.config);
            let writer = adapter_label(config.writer, &app.available);
            let critic = adapter_label(config.critic, &app.available);
            let rows = [
                format!("Writes code        {writer}"),
                format!("Writer model       {}", config.writer_model.as_deref().unwrap_or("CLI default")),
                format!("Reviews changes    {critic}"),
                format!("Reviewer model     {}", config.critic_model.as_deref().unwrap_or("CLI default")),
                format!("Mode               {}", match config.orchestration.mode {
                    OrchestrationMode::Pair => "Pair · write, review, verify",
                    OrchestrationMode::Team => "Team · parallel writers and review",
                }),
            ];
            let mut body = rows.iter().enumerate().map(|(index, row)| {
                format!("{} {row}", if index == app.selection { ">" } else { " " })
            }).collect::<Vec<_>>().join("\n");
            let discovered = app.available.iter().map(|adapter| adapter.as_str()).collect::<Vec<_>>().join(", ");
            if discovered.is_empty() {
                body.push_str("\n\nNo compatible agent CLI found. Install one of these tools, then press R:\ncodex, claude, agy, opencode, pi, omp, kilo.");
            } else {
                body.push_str(&format!("\n\nFound locally: {discovered}\nSign in or configure credentials in the agent's own CLI if required."));
            }
            ("Settings", body, "↑ ↓  Select   ← →  Change / edit   Enter  Save   R  Check again   Esc  Back".to_owned())
        }
        Screen::WriterModel | Screen::CriticModel => (
            if app.screen == Screen::WriterModel { "Writer model" } else { "Reviewer model" },
            "Enter a model name supported by this CLI. Leave the field empty to use the CLI default.".into(),
            "Enter  Apply   Esc  Back".to_owned(),
        ),
        Screen::Review => {
            let Some(draft) = &app.draft else { return; };
            let config = &draft.config;
            let checks = if config.checks.is_empty() {
                "No project checks configured".to_owned()
            } else {
                config.checks.iter().map(|check| format!("{} · {}", check.id, check.argv.join(" ")))
                    .collect::<Vec<_>>().join("\n")
            };
            let mut body = format!(
                "Writes code   {} ({})\nReviews       {} ({})\nMode          {}\nChecks        {}\n\n{}",
                config.writer.as_str(), config.writer_model.as_deref().unwrap_or("CLI default"),
                config.critic.as_str(), config.critic_model.as_deref().unwrap_or("CLI default"),
                config.orchestration.mode.as_str(), checks, draft.criteria
            );
            let actions = if let Some(blocker) = app.run_blocker() {
                body = format!("{blocker}\n\n{body}");
                "Esc  Edit   S  Settings   D  View changes   R  Check again   U  Resume queue"
            } else {
                "Enter  Run task   Esc  Edit   S  Settings"
            };
            ("Review task", body, actions.to_owned())
        }
        Screen::History => {
            let body = if app.history.is_empty() {
                "No tasks on this page. Press F4 to start a new task or Page Up for the latest tasks.".to_owned()
            } else {
                let visible = usize::from(chunks[1].height).saturating_sub(2).max(1);
                let start = app.selection.saturating_add(1).saturating_sub(visible);
                app.history.iter().enumerate().skip(start).take(visible).map(|(index, task)| {
                    let title = task.title.lines().next().unwrap_or_default();
                    format!("{} #{} · {} · {}", if index == app.selection { ">" } else { " " }, task.id, task.status.as_str(), title)
                }).collect::<Vec<_>>().join("\n")
            };
            ("History", body, "↑ ↓  Select   Enter  Open result   PgDn  Older   PgUp  Latest   F4  New session   Esc  Back".to_owned())
        }
        Screen::Task => {
            let hint = if app.supervisor_running {
                if app.active_tasks.iter().any(|task| Some(task.id) == app.task_view) {
                    "Running   S  Request stop for Writer   ↑ ↓  Scroll   F3  History"
                } else {
                    "Starting or another task is running   ↑ ↓  Scroll   F3  History"
                }
            } else if app.task_status == Some(TaskStatus::Queued) {
                "R  Retry   C  Cancel queued task   F2  Settings   F3  History   F4  New session"
            } else if !app.active_tasks.is_empty() {
                "An unfinished task needs attention   ↑ ↓  Scroll   F3  History"
            } else {
                "F4  New session   F3  History   ↑ ↓  Scroll result   Esc  Back"
            };
            let body = if let Some(note) = &app.run_notice {
                format!("{note}\n\n{}", app.task_details)
            } else {
                app.task_details.clone()
            };
            ("Task result", body, hint.to_owned())
        }
        Screen::Main => unreachable!(),
    };
    let project = app.project.root.display();
    frame.render_widget(
        Paragraph::new(format!("{title} · {project}")).block(Block::bordered()),
        chunks[0],
    );
    let body = safe_display(&body);
    let body = Paragraph::new(body)
        .wrap(Wrap { trim: false })
        .block(Block::bordered());
    let body = if matches!(app.screen, Screen::Task | Screen::Review) {
        body.scroll((app.detail_scroll, 0))
    } else {
        body
    };
    frame.render_widget(body, chunks[1]);
    if editing {
        render_input(frame, app, chunks[2]);
    }
    let message = app
        .status_message
        .as_ref()
        .map(|(message, _)| message.as_str())
        .unwrap_or("");
    frame.render_widget(
        Paragraph::new(format!("{actions}\n{message}")).wrap(Wrap { trim: true }),
        chunks[3],
    );
}

fn adapter_label(adapter: AdapterKind, available: &[AdapterKind]) -> String {
    format!(
        "{} · {}",
        adapter.as_str(),
        if available.contains(&adapter) {
            "found"
        } else {
            "unavailable"
        }
    )
}

fn safe_display(text: &str) -> String {
    text.chars()
        .filter(|ch| !ch.is_control() || matches!(*ch, '\n' | '\t'))
        .collect()
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
                " {} uncommitted path(s). Press 3 to view changes before running a task. ",
                app.dirty_paths.total_count
            ),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
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
            Span::styled("Writes: ", Style::default().fg(Color::DarkGray)),
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
            Span::styled("Reviews: ", Style::default().fg(Color::DarkGray)),
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
                if app.config.orchestration.mode == OrchestrationMode::Team {
                    format!(
                        " (max {} workers)",
                        app.config.orchestration.max_writer_workers
                    )
                } else {
                    String::new()
                },
                Style::default().fg(Color::DarkGray),
            ),
        ]),
        Line::from(Span::styled(
            "F2 Settings · F4 New session",
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
            format!("{}...", &task.title[..task.title.floor_char_boundary(27)])
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
            format!(
                "{}...",
                &event.reason[..event.reason.floor_char_boundary(69)]
            )
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
    if !app.modified_files.is_empty() {
        let files_height = (app.modified_files.len() as u16 + 2).min(7);
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(files_height), Constraint::Min(4)])
            .split(area);

        let mut file_items = Vec::new();
        for (i, file) in app.modified_files.iter().enumerate() {
            let is_selected = i == app.diff_file_index;
            let (cursor, style) = if is_selected {
                (
                    "▶ ",
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                ("  ", Style::default().fg(Color::White))
            };
            file_items.push(ListItem::new(Line::from(vec![
                Span::styled(
                    cursor,
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(file.clone(), style),
            ])));
        }

        let files_block = Block::default()
            .borders(Borders::ALL)
            .title(" Changed files · Alt+↑/↓ select · F5 back to task draft ")
            .border_style(Style::default().fg(Color::Yellow));

        frame.render_widget(List::new(file_items).block(files_block), chunks[0]);

        render_diff_view(frame, app, chunks[1]);
    } else {
        render_diff_view(frame, app, area);
    }
}

fn render_diff_view(frame: &mut Frame, app: &TuiApp, area: Rect) {
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

    let title = if let Some(selected) = app.selected_modified_file() {
        format!(
            " 📄 Live Git Diff: Selected [{selected}] ({} diff lines) ",
            app.diff_lines.len()
        )
    } else {
        format!(
            " 📄 Live Code Diff ({} lines changed) ",
            app.diff_lines.len()
        )
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
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
        lines.push(Line::from(Span::styled("In a shell, configure checks with talos config set checks '[{\"id\":\"test\",\"argv\":[\"cargo\",\"test\"]}]'", Style::default().fg(Color::DarkGray))));
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
    let title = match app.screen {
        Screen::ChooseProject => " Project folder ",
        Screen::Task => " /new New session · /config Settings · /history History ",
        Screen::WriterModel | Screen::CriticModel => " Model name ",
        _ if !app.available.contains(&app.config.writer)
            || !app.available.contains(&app.config.critic) =>
        {
            " /config Set up agents · F2 Settings · F3 History "
        }
        _ => {
            " Describe task · Enter Review · /config Settings · /new New session · F3 History · Esc Clear "
        }
    };
    let before = &app.input[..app.cursor];
    let after = &app.input[app.cursor..];
    let mut lines: Vec<Line<'_>> = before.split('\n').map(Line::raw).collect();
    let cursor_line = lines.last_mut().expect("split always has a line");
    let mut suffix = after.split('\n');
    let tail = suffix.next().unwrap_or_default();
    let next = tail.chars().next().map_or(0, char::len_utf8);
    let cursor_style = Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::REVERSED);
    cursor_line.spans.push(Span::styled(
        if next == 0 { " " } else { &tail[..next] },
        cursor_style,
    ));
    cursor_line.spans.push(Span::raw(&tail[next..]));
    let row = lines.len() - 1;
    lines.extend(suffix.map(Line::raw));
    let row_offset = row.saturating_sub(1);
    let column = Line::raw(before.rsplit('\n').next().unwrap_or_default()).width();
    let horizontal = column
        .saturating_add(1)
        .saturating_sub(area.width.saturating_sub(2) as usize);
    frame.render_widget(
        Paragraph::new(lines)
            .scroll((row_offset as u16, horizontal as u16))
            .block(
                Block::bordered()
                    .title(title)
                    .border_style(Style::default().fg(Color::Cyan)),
            ),
        area,
    );
}

fn longest_common_prefix(strings: &[String]) -> String {
    if strings.is_empty() {
        return String::new();
    }
    let first = &strings[0];
    let mut len = 0;
    for (i, c) in first.char_indices() {
        if strings
            .iter()
            .all(|s| s.get(i..).is_some_and(|rest| rest.starts_with(c)))
        {
            len += c.len_utf8();
        } else {
            break;
        }
    }
    first[..first.floor_char_boundary(len)].to_string()
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
    fn guided_cockpit_reviews_before_enqueuing_and_resets_the_session() {
        let project = test_tui_project();
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .args(args)
                .current_dir(&project.root)
                .output()
                .expect("run Git");
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        git(&["init", "--quiet"]);
        git(&[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "--allow-empty",
            "-m",
            "Base",
        ]);
        let project = Project::discover(&project.root).expect("discover Git project");
        ensure_talos_ignored(&project).expect("local exclusion");
        let mut app = TuiApp::with_adapters(
            project.clone(),
            vec![AdapterKind::Codex, AdapterKind::Claude],
        )
        .expect("open cockpit");
        let mut database = Database::open(&project).expect("open database");
        let old = database
            .add_task("Earlier task", "Earlier criteria")
            .expect("queue old task");
        app.execute_slash_command("/config").expect("open settings");
        assert_eq!(app.screen, Screen::Settings);
        app.handle_submit().expect("save settings");
        assert_eq!(app.config.writer, AdapterKind::Codex);
        assert_eq!(app.config.critic, AdapterKind::Claude);

        app.paste("Проверка проекта\nKeep the change small.");
        app.handle_submit().expect("prepare draft");
        assert_eq!(app.screen, Screen::Review);
        assert_eq!(database.list_tasks().expect("list tasks").len(), 1);
        let mut terminal =
            Terminal::new(ratatui::backend::TestBackend::new(90, 25)).expect("terminal");
        terminal
            .draw(|frame| ui(frame, &app))
            .expect("render Cyrillic draft");

        std::fs::write(project.root.join("local-edit.txt"), "user change").expect("dirty project");
        assert!(
            app.handle_submit()
                .unwrap_err()
                .to_string()
                .contains("uncommitted")
        );
        assert_eq!(database.list_tasks().expect("list tasks").len(), 1);
        std::fs::remove_file(project.root.join("local-edit.txt")).expect("clean test project");

        app.screen = Screen::Task;
        app.task_view = Some(old.id);
        for key in "/new".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(key), KeyModifiers::NONE))
                .expect("type command");
        }
        app.handle_submit().expect("new session");
        app.refresh_state().expect("refresh");
        assert_eq!(app.screen, Screen::Main);
        assert!(app.input.is_empty() && app.recent_events.is_empty());
        assert!(app.task_view.is_none());
        assert_eq!(app.config.writer, AdapterKind::Codex);
        app.open_history(None).expect("open history");
        assert_eq!(app.history[0].id, old.id);
        std::fs::remove_dir_all(&project.root).expect("remove test project");
    }

    #[test]
    fn cockpit_backspace_accepts_repeat_and_edits_at_a_unicode_boundary() {
        let project = test_tui_project();
        let mut app = TuiApp::with_adapters(project.clone(), Vec::new()).expect("open cockpit");
        app.screen = Screen::Main;
        app.paste("раз два");
        app.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE))
            .expect("move cursor");
        let mut held = KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE);
        held.kind = KeyEventKind::Repeat;
        app.handle_key(held).expect("repeat Backspace");
        app.handle_key(held).expect("repeat Backspace");
        assert_eq!(app.input, "раз а");
        app.handle_key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE))
            .expect("move to end");
        app.handle_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL))
            .expect("delete word");
        assert_eq!(app.input, "раз ");
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
            .expect("clear line");
        assert!(app.input.is_empty());
        std::fs::remove_dir_all(&project.root).expect("remove test project");
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

        let mut app = TuiApp::with_adapters(project.clone(), Vec::new()).expect("create TuiApp");

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

    #[test]
    fn tab_completion_and_task_presets() {
        let project = test_tui_project();

        let talos_dir = project.root.join(".talos");
        std::fs::create_dir_all(&talos_dir).expect("create .talos");
        let db = Database::open(&project).expect("open db");
        drop(db);

        let mut app = TuiApp::with_adapters(project.clone(), Vec::new()).expect("create TuiApp");

        // 1. Longest common prefix helper
        assert_eq!(
            longest_common_prefix(&["/task:test".to_string(), "/task:refactor".to_string(),]),
            "/task:"
        );
        assert_eq!(
            longest_common_prefix(&["/cli".to_string(), "/commit".to_string()]),
            "/c"
        );
        assert_eq!(longest_common_prefix(&[]), "");

        // 2. Tab completion for slash command
        app.input = "/cle".to_string();
        app.complete_input();
        assert_eq!(app.input, "/clear ");

        app.input = "/cli".to_string();
        app.complete_input();
        assert_eq!(app.input, "/cli ");

        app.input = "/task:t".to_string();
        app.complete_input();
        assert_eq!(app.input, "/task:test ");

        app.input = "/тес".to_string();
        app.complete_input();
        assert_eq!(app.input, "/тест ");

        // 3. Tab completion for CLI adapter arguments
        app.input = "/cli cod".to_string();
        app.complete_input();
        assert_eq!(app.input, "/cli codex ");

        app.input = "/cli cla".to_string();
        app.complete_input();
        assert_eq!(app.input, "/cli claude ");

        // 4. Task preset execution without argument prompts usage
        app.execute_slash_command("/task:test")
            .expect("execute empty /task:test");
        assert!(app.status_message.is_some());
        let (msg, _) = app.status_message.as_ref().unwrap();
        assert!(msg.contains("Usage: /task:test"));

        for (command, path) in [
            ("/task:test src/calc.rs", "calc.rs"),
            ("/task:refactor src/storage.rs", "storage.rs"),
            ("/task:fix src/parser.rs", "parser.rs"),
            ("/task:bench src/engine.rs", "engine.rs"),
        ] {
            app.execute_slash_command(command)
                .expect("prepare preset draft");
            let draft = app.draft.as_ref().expect("draft");
            assert!(draft.title.contains(path));
            assert!(draft.criteria.contains(path));
            assert_eq!(app.screen, Screen::Review);
        }
        let database = Database::open(&project).expect("open db");
        assert!(database.list_tasks().expect("list tasks").is_empty());

        let _ = std::fs::remove_dir_all(&project.root);
    }

    #[test]
    fn diff_file_navigation_and_selection() {
        let project = test_tui_project();

        let talos_dir = project.root.join(".talos");
        std::fs::create_dir_all(&talos_dir).expect("create .talos");
        let db = Database::open(&project).expect("open db");
        drop(db);

        let mut app = TuiApp::with_adapters(project.clone(), Vec::new()).expect("create TuiApp");
        app.modified_files = vec![
            "src/main.rs".to_string(),
            "src/tui.rs".to_string(),
            "docs/tasks.md".to_string(),
        ];
        app.diff_file_index = 0;

        assert_eq!(app.selected_modified_file(), Some("src/main.rs"));

        // Navigation
        app.diff_file_index = 1;
        assert_eq!(app.selected_modified_file(), Some("src/tui.rs"));

        app.diff_file_index = 2;
        assert_eq!(app.selected_modified_file(), Some("docs/tasks.md"));

        app.diff_file_index = 10;
        assert_eq!(app.selected_modified_file(), None);

        let _ = std::fs::remove_dir_all(&project.root);
    }
}
