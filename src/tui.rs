use std::{
    fs::{File, OpenOptions},
    io::{self, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    process::{Child, Command},
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
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Tabs, Wrap},
};
use std::os::unix::fs::OpenOptionsExt;
use unicode_segmentation::UnicodeSegmentation;

use crate::{
    adapter::AdapterId as AdapterKind,
    app::{
        GitStatusPaths, adapter_available_for_cockpit, config_fingerprint,
        current_project_revision_id, ensure_talos_ignored, git_status_paths,
    },
    checks::ProjectCheck,
    config::{OrchestrationMode, ProjectConfig},
    db::{
        COCKPIT_CANCELLED_REASON, CriticReviewRecord, Database, QueueSummary, RecentTask,
        StopRequestOutcome, TaskEvent, TaskRecord, TaskStatus,
    },
    project::{DiscoveryMethod, Project},
    tmux::{TmuxClient, project_session_name},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActiveTab {
    Work = 0,
    Dialogue = 1,
    Diff = 2,
    Logs = 3,
    History = 4,
}

impl ActiveTab {
    const ALL: [Self; 5] = [
        Self::Work,
        Self::Dialogue,
        Self::Diff,
        Self::Logs,
        Self::History,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Self::Work => "1 Work",
            Self::Dialogue => "2 Review",
            Self::Diff => "3 Changes",
            Self::Logs => "4 Logs",
            Self::History => "5 History",
        }
    }

    pub fn next(self) -> Self {
        Self::ALL[(self as usize + 1) % Self::ALL.len()]
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Screen {
    #[default]
    Main,
    Settings,
    WriterModel,
    CriticModel,
    ChooseProject,
    Plan,
}

struct TaskDraft {
    title: String,
    criteria: String,
    config: ProjectConfig,
}

#[derive(Clone, Copy, Debug)]
pub struct SlashCommandItem {
    pub cmd: &'static str,
    pub description: &'static str,
}

pub const PALETTE_COMMANDS: &[SlashCommandItem] = &[
    SlashCommandItem {
        cmd: "/cli",
        description: "Switch CLI adapters: /cli <writer> [critic]",
    },
    SlashCommandItem {
        cmd: "/mode",
        description: "Set mode: adaptive | pair | team",
    },
    SlashCommandItem {
        cmd: "/task:test",
        description: "Preset: generate tests for <path>",
    },
    SlashCommandItem {
        cmd: "/task:refactor",
        description: "Preset: refactor code in <path>",
    },
    SlashCommandItem {
        cmd: "/task:fix",
        description: "Preset: fix issue in <path> <desc>",
    },
    SlashCommandItem {
        cmd: "/task:bench",
        description: "Preset: benchmark target <target>",
    },
    SlashCommandItem {
        cmd: "/diff",
        description: "Open Changes",
    },
    SlashCommandItem {
        cmd: "/work",
        description: "Open Work",
    },
    SlashCommandItem {
        cmd: "/review",
        description: "Open Review",
    },
    SlashCommandItem {
        cmd: "/logs",
        description: "Open Logs",
    },
    SlashCommandItem {
        cmd: "/config",
        description: "Open Settings",
    },
    SlashCommandItem {
        cmd: "/plan",
        description: "Open Goal & Subtasks Plan inspector",
    },
    SlashCommandItem {
        cmd: "/copy",
        description: "Copy active view to system clipboard",
    },
    SlashCommandItem {
        cmd: "/continue",
        description: "Continue task from last confirmed stage (Context Pack)",
    },
    SlashCommandItem {
        cmd: "/commit",
        description: "Commit working tree changes",
    },
    SlashCommandItem {
        cmd: "/history",
        description: "Open task history",
    },
    SlashCommandItem {
        cmd: "/new",
        description: "Start new task / reset session",
    },
    SlashCommandItem {
        cmd: "/stop",
        description: "Request task cancellation",
    },
    SlashCommandItem {
        cmd: "/pause",
        description: "Pause running supervisor",
    },
    SlashCommandItem {
        cmd: "/resume",
        description: "Resume paused supervisor",
    },
    SlashCommandItem {
        cmd: "/clear",
        description: "Clear the input",
    },
    SlashCommandItem {
        cmd: "/help",
        description: "Show command and keybinding help",
    },
    SlashCommandItem {
        cmd: "/exit",
        description: "Exit TALOS Cockpit",
    },
];

pub struct TuiApp {
    project: Project,
    config: ProjectConfig,
    active_tab: ActiveTab,
    input: String,
    cursor: usize,
    pub input_history: Vec<String>,
    pub history_index: Option<usize>,
    pub saved_draft: String,
    pub palette_index: usize,
    screen: Screen,
    draft: Option<TaskDraft>,
    settings: Option<ProjectConfig>,
    task_input: Option<(String, usize)>,
    selection: usize,
    available: Vec<AdapterKind>,
    custom_adapters: bool,
    history: Vec<RecentTask>,
    history_before: Option<i64>,
    history_selection: usize,
    task_view: Option<i64>,
    event_baseline: i64,
    log_baseline: u64,
    child: Option<(i64, Child)>,
    run_notice: Option<String>,
    task_status: Option<TaskStatus>,
    task_title: String,
    task_cancelled: bool,
    verification_status: Option<String>,
    result_diff: Option<String>,
    status_message: Option<(String, Instant)>,
    scroll_offset: usize,

    // Database metrics
    queue: QueueSummary,
    queue_paused: bool,
    supervisor_running: bool,
    active_tasks: Vec<TaskRecord>,
    recent_events: Vec<TaskEvent>,
    latest_review: Option<CriticReviewRecord>,
    verification_checks: Vec<ProjectCheck>,
    diff_lines: Vec<String>,
    supervisor_logs: Vec<String>,
    pub diff_file_index: usize,
    pub modified_files: Vec<String>,
    dirty_paths: GitStatusPaths,
    is_in_tmux: bool,
    pub should_exit: bool,
}

impl TuiApp {
    pub fn new(project: Project) -> Result<Self> {
        let available = available_adapters(&project);
        Self::create(project, available, false)
    }

    #[cfg(test)]
    fn with_adapters(project: Project, available: Vec<AdapterKind>) -> Result<Self> {
        Self::create(project, available, true)
    }

    fn create(
        project: Project,
        available: Vec<AdapterKind>,
        custom_adapters: bool,
    ) -> Result<Self> {
        let mut config = if matches!(project.discovery, DiscoveryMethod::Git) {
            ProjectConfig::load(&project)?
        } else {
            ProjectConfig::default()
        };
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
            active_tab: ActiveTab::Work,
            input: String::new(),
            cursor: 0,
            input_history: Vec::new(),
            history_index: None,
            saved_draft: String::new(),
            palette_index: 0,
            screen: if matches!(project.discovery, DiscoveryMethod::Git) {
                Screen::Main
            } else {
                Screen::ChooseProject
            },
            project,
            draft: None,
            settings: None,
            task_input: None,
            selection: 0,
            available,
            custom_adapters,
            history: Vec::new(),
            history_before: None,
            history_selection: 0,
            task_view: None,
            event_baseline: 0,
            log_baseline: 0,
            child: None,
            run_notice: None,
            task_status: None,
            task_title: String::new(),
            task_cancelled: false,
            verification_status: None,
            result_diff: None,
            status_message: None,
            scroll_offset: 0,
            queue,
            queue_paused: false,
            supervisor_running: false,
            active_tasks: Vec::new(),
            recent_events: Vec::new(),
            latest_review: None,
            verification_checks: Vec::new(),
            diff_lines: Vec::new(),
            supervisor_logs: Vec::new(),
            diff_file_index: 0,
            modified_files: Vec::new(),
            dirty_paths,
            is_in_tmux,
            should_exit: false,
        };

        if matches!(app.project.discovery, DiscoveryMethod::Git) {
            app.refresh_state()?;
        }
        Ok(app)
    }

    pub fn refresh_state(&mut self) -> Result<()> {
        if !matches!(self.project.discovery, DiscoveryMethod::Git) {
            return Ok(());
        }
        if let Some((task_id, child)) = &mut self.child
            && let Some(status) = child.try_wait()?
        {
            let task_id = *task_id;
            self.child = None;
            if self.task_view == Some(task_id) {
                self.status_message = None;
                if !status.success() {
                    let log = recent_log_lines(&self.task_log(task_id), 0);
                    self.run_notice = Some(run_recovery_hint(&log, &self.config));
                }
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

        self.queue_paused = database.project_control_state()?.queue_paused;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_millis() as i64);
        self.supervisor_running = self.child.is_some()
            || database
                .current_supervisor_lease()?
                .is_some_and(|lease| lease.expires_at_ms > now);

        self.active_tasks = database.active_tasks()?;
        if self.task_view.is_none() {
            self.task_view = self.active_tasks.first().map(|task| task.id);
        }
        if let Some(id) = self.task_view {
            let task = database.get_task(id)?;
            self.task_status = task.as_ref().map(|task| task.status.clone());
            self.task_title = task.map_or_else(String::new, |task| task.title);
            self.task_cancelled = self.task_status == Some(TaskStatus::Failed)
                && database
                    .task_events_page(Some(id), None, 20)?
                    .events
                    .iter()
                    .find(|event| event.event_type == "task.status_changed")
                    .is_some_and(|event| event.reason == COCKPIT_CANCELLED_REASON);
            self.latest_review = database.latest_critic_review(id)?;
            self.verification_status = database
                .latest_verification_summary(id)?
                .map(|summary| summary.run.status);
            self.verification_checks = database.task_verification_checks(id)?;
        } else {
            self.latest_review = None;
            self.verification_checks = self.config.checks.clone();
        }

        if self.active_tab == ActiveTab::Logs {
            self.recent_events = database.task_events_page(self.task_view, None, 50)?.events;
            self.recent_events.reverse();
            if self.task_view.is_none() {
                self.recent_events
                    .retain(|event| event.id > self.event_baseline);
            }
            self.read_supervisor_logs();
        }
        self.dirty_paths = git_status_paths(&self.project)?;
        if self.active_tab == ActiveTab::Diff && self.result_diff.is_none() {
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
                self.status_message =
                    Some(("Modes: adaptive  pair  team".to_string(), Instant::now()));
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
                let modes = ["adaptive", "pair", "team"];
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
        if self.is_palette_active()
            && !PALETTE_COMMANDS
                .iter()
                .any(|item| item.cmd.eq_ignore_ascii_case(&text))
        {
            let matches = self.palette_matches();
            if let Some(chosen) =
                matches.get(self.palette_index.min(matches.len().saturating_sub(1)))
            {
                self.input = format!("{} ", chosen.cmd);
                self.cursor = self.input.len();
                self.palette_index = 0;
                return Ok(());
            }
        }

        if !text.is_empty() && self.screen == Screen::Main {
            if self.input_history.last().map(|s| s.as_str()) != Some(&text) {
                self.input_history.push(text.clone());
            }
            self.history_index = None;
            self.saved_draft.clear();
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
            Screen::Main => {
                if self.draft.is_some() && self.active_tab != ActiveTab::Work {
                    self.select_tab(ActiveTab::Work);
                } else if self.draft.is_some() {
                    self.run_draft()?;
                } else if text.starts_with('/') {
                    self.execute_slash_command(&text)?;
                    self.clear_input();
                } else if !text.is_empty() {
                    let first_line = text.lines().next().unwrap_or_default().trim();
                    let end = first_line.floor_char_boundary(first_line.len().min(160));
                    self.prepare_draft(first_line[..end].to_owned(), text);
                    if self.publication_target().is_none() {
                        let result = self.run_draft();
                        self.draft = None;
                        result?;
                    }
                } else if self.active_tab == ActiveTab::History
                    && let Some(task) = self.history.get(self.history_selection)
                {
                    self.task_view = Some(task.task.id);
                    self.run_notice = None;
                    self.result_diff = None;
                    self.select_tab(ActiveTab::Work);
                    self.refresh_state()?;
                }
                return Ok(());
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
                self.restore_task_input();
                return Ok(());
            }
            Screen::Settings => {
                let settings = self.settings.as_ref().context("Open Settings first")?;
                settings.save(&self.project)?;
                self.config = settings.clone();
                self.settings = None;
                self.screen = Screen::Main;
                self.notice("Settings saved.");
                return Ok(());
            }
            Screen::Plan => {
                self.screen = Screen::Main;
                return Ok(());
            }
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
        self.history_index = None;
        self.saved_draft.clear();
        self.palette_index = 0;
    }

    pub fn palette_matches(&self) -> Vec<&'static SlashCommandItem> {
        if !self.input.starts_with('/') || self.input.contains(' ') {
            return Vec::new();
        }
        let query = &self.input[1..].to_ascii_lowercase();
        if query.is_empty() {
            return PALETTE_COMMANDS.iter().collect();
        }
        let mut prefix_matches: Vec<&'static SlashCommandItem> = PALETTE_COMMANDS
            .iter()
            .filter(|item| item.cmd[1..].starts_with(query))
            .collect();
        let contains_matches: Vec<&'static SlashCommandItem> = PALETTE_COMMANDS
            .iter()
            .filter(|item| !item.cmd[1..].starts_with(query) && item.cmd.contains(query))
            .collect();
        prefix_matches.extend(contains_matches);
        prefix_matches
    }

    pub fn is_palette_active(&self) -> bool {
        self.screen == Screen::Main
            && self.history_index.is_none()
            && self.input.starts_with('/')
            && !self.input.contains(' ')
            && !self.palette_matches().is_empty()
    }

    fn prepare_draft(&mut self, title: String, criteria: String) {
        self.draft = Some(TaskDraft {
            title,
            criteria,
            config: self.config.clone(),
        });
        self.select_tab(ActiveTab::Work);
        self.status_message = None;
    }

    fn publication_target(&self) -> Option<&str> {
        self.config
            .github
            .as_ref()
            .filter(|policy| policy.allow_push && policy.allow_pull_request)
            .map(|policy| policy.repository.as_str())
    }

    fn run_draft(&mut self) -> Result<()> {
        self.refresh_state()?;
        let draft = self.draft.as_ref().context("Create a task first")?;
        anyhow::ensure!(
            draft.config == self.config,
            "Project settings changed. Press Esc, then submit the task again."
        );
        if let Some(blocker) = self.run_blocker(None) {
            anyhow::bail!("{blocker}");
        }
        anyhow::ensure!(
            crate::revision::git_head(&self.project.root)?.is_some(),
            "This Git project has no commits yet. Make an initial commit with your project files, or choose an existing project."
        );
        let mut database = Database::open(&self.project)?;
        let task =
            database.add_task_with_checks(&draft.title, &draft.criteria, &draft.config.checks)?;
        self.draft = None;
        self.clear_input();
        self.task_view = Some(task.id);
        self.task_title = task.title;
        self.task_status = Some(TaskStatus::Queued);
        self.task_cancelled = false;
        self.verification_status = None;
        self.result_diff = None;
        self.run_notice = None;
        self.start_selected_task(task.id)
    }

    fn run_blocker(&self, retry: Option<i64>) -> Option<String> {
        if self.supervisor_running || self.active_tasks.iter().any(|task| Some(task.id) != retry) {
            return Some(
                "A task is active. Wait for it to finish or request Stop for its Writer.".into(),
            );
        }
        if self.queue_paused {
            return Some("The queue is paused. Use /resume, then try again.".into());
        }
        let retry_in_progress =
            retry.is_some_and(|id| self.active_tasks.iter().any(|task| task.id == id));
        if !retry_in_progress && !self.dirty_paths.is_empty() {
            return Some(format!(
                "Commit or stash your uncommitted changes before running. Alt+3 opens Changes. {}",
                self.dirty_paths.display_preview()
            ));
        }
        for adapter in [self.config.writer, self.config.critic] {
            if !self.available.contains(&adapter) {
                return Some(format!(
                    "{} was not found or failed its capability check. Press F2 to choose an available CLI.",
                    adapter.as_str()
                ));
            }
        }
        None
    }

    fn can_retry_task(&self) -> bool {
        !self.supervisor_running
            && matches!(
                self.task_status,
                Some(TaskStatus::Queued | TaskStatus::Reviewing | TaskStatus::Verifying)
            )
    }

    fn restore_task_input(&mut self) {
        if let Some((text, cursor)) = self.task_input.take() {
            self.input = text;
            self.cursor = cursor;
        }
    }

    fn refresh_available(&mut self) {
        if self.custom_adapters {
            return;
        }
        let discovered = available_adapters(&self.project);
        if !discovered.is_empty() || self.available.is_empty() {
            self.available = discovered;
        }
    }

    fn open_settings(&mut self) {
        self.refresh_available();
        self.draft = None;
        self.settings = Some(self.config.clone());
        self.selection = 0;
        self.screen = Screen::Settings;
        self.status_message = None;
    }

    pub fn open_plan(&mut self) {
        self.screen = Screen::Plan;
        self.scroll_offset = 0;
        self.status_message = None;
    }

    pub fn has_active_or_recent_task(&self) -> bool {
        self.task_view.is_some() || self.draft.is_some() || self.supervisor_running
    }

    pub fn copy_active_view_to_clipboard(&mut self) {
        let text = self.get_active_view_text();
        if text.trim().is_empty() {
            self.notice("Nothing to copy in current view.");
            return;
        }
        if copy_text_to_clipboard(&text) {
            self.notice("✓ Copied to clipboard!");
        } else {
            self.notice("Failed to copy to clipboard.");
        }
    }

    pub fn continue_selected_task(&mut self, task_id: Option<i64>) -> Result<()> {
        self.refresh_available();
        self.refresh_state()?;
        let id = task_id
            .or_else(|| {
                if self.active_tab == ActiveTab::History {
                    self.history
                        .get(self.history_selection)
                        .map(|entry| entry.task.id)
                } else {
                    self.task_view
                }
            })
            .context("No task selected to continue.")?;

        let database = Database::open(&self.project)?;
        let resume_context = crate::summary::assemble_resume_context(&self.project, &database, id)?;

        let mut db = Database::open(&self.project)?;
        match &resume_context.next_step {
            crate::summary::ResumeNextStep::FixCriticDefects { open_remarks, .. } => {
                let mut prompt = format!(
                    "{}\n\n[CONTINUE TASK - FIX CRITIC DEFECTS]\nResolve the following open defects from peer review:\n",
                    resume_context.acceptance_criteria
                );
                for r in open_remarks {
                    prompt.push_str(&format!("- {r}\n"));
                }
                let _ = db.update_task_acceptance_criteria(id, &prompt);
                let _ = db.transition_task(
                    id,
                    TaskStatus::Implementing,
                    "talos_continue",
                    "Resume task to fix Critic defects",
                );
                let _ = db.record_task_event(
                    id,
                    "talos_continue",
                    "task.continue",
                    &format!(
                        "Resumed task #{id} to fix {} open Critic defect(s)",
                        open_remarks.len()
                    ),
                    None,
                );
            }
            crate::summary::ResumeNextStep::FixVerificationFailure {
                failing_check_id,
                exit_code,
                ..
            } => {
                let _ = db.transition_task(
                    id,
                    TaskStatus::Implementing,
                    "talos_continue",
                    "Resume task to fix verification failure",
                );
                let _ = db.record_task_event(
                    id,
                    "talos_continue",
                    "task.continue",
                    &format!(
                        "Resumed task #{id} to fix failing check '{failing_check_id}' (exit code {exit_code})"
                    ),
                    None,
                );
            }
            crate::summary::ResumeNextStep::ReviewPending { .. } => {
                let _ = db.transition_task(
                    id,
                    TaskStatus::Reviewing,
                    "talos_continue",
                    "Resume task for Critic review",
                );
                let _ = db.record_task_event(
                    id,
                    "talos_continue",
                    "task.continue",
                    &format!("Resumed task #{id} for Critic review"),
                    None,
                );
            }
            crate::summary::ResumeNextStep::ReconcileGitDrift { .. } => {
                let _ = db.transition_task(
                    id,
                    TaskStatus::Queued,
                    "talos_continue",
                    "Resume task with git drift reconciliation",
                );
                let _ = db.record_task_event(
                    id,
                    "talos_continue",
                    "task.continue",
                    &format!("Resumed task #{id} with git drift reconciliation"),
                    None,
                );
            }
            crate::summary::ResumeNextStep::RetryTask { reason } => {
                let _ = db.transition_task(
                    id,
                    TaskStatus::Queued,
                    "talos_continue",
                    "Retry task from History",
                );
                let _ = db.record_task_event(
                    id,
                    "talos_continue",
                    "task.continue",
                    &format!("Retried task #{id}: {reason}"),
                    None,
                );
            }
        }

        self.task_view = Some(id);
        self.task_title = resume_context.title.clone();
        self.task_status = Some(TaskStatus::Queued);
        self.task_cancelled = false;
        self.verification_status = None;
        self.result_diff = None;
        self.select_tab(ActiveTab::Work);
        self.start_selected_task(id)?;
        self.notice(format!(
            "✓ Continuing Task #{id}: {}",
            resume_context.next_step.description()
        ));
        Ok(())
    }

    pub fn get_active_view_text(&self) -> String {
        if self.screen == Screen::Plan {
            let (body, _) = self.build_plan_view_content();
            return body;
        }
        match self.active_tab {
            ActiveTab::Diff => {
                if let Some(diff) = &self.result_diff {
                    diff.clone()
                } else if !self.diff_lines.is_empty() {
                    self.diff_lines.join("\n")
                } else {
                    "No diff available.".to_string()
                }
            }
            ActiveTab::Dialogue => {
                let mut out = Vec::new();
                if let Some(ref rev) = self.latest_review {
                    out.push(format!(
                        "Critic Review Verdict: {} (Revision R{})",
                        rev.decision.to_uppercase(),
                        rev.revision_id
                    ));
                    out.push(format!("Summary: {}", rev.summary));
                    if !rev.result_json.trim().is_empty() && rev.result_json != "{}" {
                        out.push(format!("Findings:\n{}", rev.result_json));
                    }
                }
                if let Some(status) = &self.verification_status {
                    out.push(format!("Project Checks: {}", status));
                }
                out.join("\n\n")
            }
            ActiveTab::Logs => {
                let mut out = Vec::new();
                for event in &self.recent_events {
                    out.push(format!(
                        "[{}] #{}: {} - {}",
                        event.actor, event.task_id, event.event_type, event.reason
                    ));
                }
                for line in &self.supervisor_logs {
                    out.push(line.clone());
                }
                out.join("\n")
            }
            ActiveTab::Work => {
                let mut out = Vec::new();
                if let Some(draft) = &self.draft {
                    out.push(format!("Draft Task: {}\n{}", draft.title, draft.criteria));
                } else if let Some(id) = self.task_view {
                    out.push(format!("Task #{id}: {}", self.task_title));
                    if let Some(status) = &self.task_status {
                        out.push(format!("Status: {:?}", status));
                    }
                    if let Some(notice) = &self.run_notice {
                        out.push(notice.clone());
                    }
                }
                if out.is_empty() {
                    "Ready · Describe a change to make in this project.".to_string()
                } else {
                    out.join("\n\n")
                }
            }
            ActiveTab::History => {
                let mut out = Vec::new();
                for entry in &self.history {
                    out.push(format!(
                        "#{}: {} ({})",
                        entry.task.id,
                        entry.task.title,
                        entry.task.status.as_str()
                    ));
                }
                if out.is_empty() {
                    "No task history recorded yet.".to_string()
                } else {
                    out.join("\n")
                }
            }
        }
    }

    pub fn build_plan_view_content(&self) -> (String, String) {
        let mut body = String::new();
        if let Some(id) = self.task_view {
            body.push_str(&format!("🎯 TARGET TASK #{id}: {}\n", self.task_title));
            if let Some(status) = &self.task_status {
                body.push_str(&format!("Status: {}\n", status.as_str()));
            }
            if let Ok(database) = Database::open(&self.project) {
                if let Ok(Some(task)) = database.get_task(id) {
                    body.push_str("\n--- ORIGINAL REQUIREMENTS & CRITERIA ---\n");
                    body.push_str(&task.acceptance_criteria);
                    body.push('\n');
                }
                if let Ok(subtasks) = database.team_subtasks_for_parent(id) {
                    if !subtasks.is_empty() {
                        body.push_str("\n--- DECOMPOSED SUBTASKS & PROGRESS ---\n");
                        for (i, sub) in subtasks.iter().enumerate() {
                            let icon = match sub.status.as_str() {
                                "completed" => "[✓]",
                                "in_progress" | "running" => "[▶]",
                                "failed" | "blocked" => "[✗]",
                                _ => "[○]",
                            };
                            let role_adapter = format!("Role: {} ({})", sub.role, sub.adapter);
                            let title = database
                                .get_task(sub.subtask_id)
                                .ok()
                                .flatten()
                                .map(|t| t.title)
                                .unwrap_or_else(|| format!("Subtask #{}", sub.subtask_id));
                            body.push_str(&format!(
                                "{} {}. {} | {}\n",
                                icon,
                                i + 1,
                                title,
                                role_adapter
                            ));
                            if let Some(summary) = &sub.result_summary {
                                body.push_str(&format!(
                                    "   Summary: {}\n",
                                    summary.lines().next().unwrap_or("")
                                ));
                            }
                        }
                    } else {
                        body.push_str("\n--- EXECUTION PIPELINE (Pair Mode) ---\n");
                        let status_label = self
                            .task_status
                            .as_ref()
                            .map(|s| s.as_str())
                            .unwrap_or("unknown");
                        body.push_str(&format!(
                            "1. Plan & Triage: Completed\n2. Implementation (Writer: {}): {}\n3. Verification (Critic: {}): {}\n",
                            self.config.writer.as_str(),
                            if matches!(
                                self.task_status,
                                Some(TaskStatus::Queued | TaskStatus::Planning)
                            ) {
                                "Pending"
                            } else {
                                "Done"
                            },
                            self.config.critic.as_str(),
                            status_label
                        ));
                    }
                }
            }
            if let Some(ref rev) = self.latest_review {
                body.push_str("\n--- CRITIC REVIEW VERDICT ---\n");
                body.push_str(&format!(
                    "Verdict: {} (Revision R{})\n",
                    rev.decision.to_uppercase(),
                    rev.revision_id
                ));
                body.push_str(&format!("Summary: {}\n", rev.summary));
                if !rev.result_json.trim().is_empty() && rev.result_json != "{}" {
                    body.push_str(&format!("Findings:\n{}\n", rev.result_json));
                }
            }
            if let Some(checks) = &self.verification_status {
                body.push_str(&format!("\nProject Checks: {}\n", checks));
            }
        } else if let Some(draft) = &self.draft {
            body.push_str(&format!(
                "🎯 UNCONFIRMED DRAFT: {}\n\nCriteria:\n{}",
                draft.title, draft.criteria
            ));
        } else {
            body.push_str("No active or selected task.\nDescribe a change in the Cockpit or choose a task from History.");
        }
        let actions = "Esc / p  Close   y  Copy plan   d  View diff   Enter  Save/New".to_string();
        (body, actions)
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
        self.task_title.clear();
        self.task_cancelled = false;
        self.verification_status = None;
        self.result_diff = None;
        self.recent_events.clear();
        self.supervisor_logs.clear();
        self.run_notice = None;
        self.scroll_offset = 0;
        self.select_tab(ActiveTab::Work);
        self.screen = Screen::Main;
        self.notice("New session.");
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
        self.history_selection = 0;
        self.select_tab(ActiveTab::History);
        self.status_message = None;
        Ok(())
    }

    fn open_changes(&mut self) -> Result<()> {
        if self.task_status == Some(TaskStatus::Complete) && self.dirty_paths.is_empty() {
            return self.open_result_changes();
        }
        self.show_working_diff();
        Ok(())
    }

    fn show_working_diff(&mut self) {
        self.select_tab(ActiveTab::Diff);
        self.result_diff = None;
        self.load_git_diff();
    }

    fn open_result_changes(&mut self) -> Result<()> {
        let id = self.task_view.context("Select a task in History")?;
        let database = Database::open(&self.project)?;
        let event = database
            .task_events_page(Some(id), None, 20)?
            .events
            .into_iter()
            .find(|event| event.event_type == "task.commit_saved");
        let Some(event) = event else {
            self.select_tab(ActiveTab::Diff);
            self.result_diff = Some("No saved Git changes for this task.".to_owned());
            return Ok(());
        };
        let data: serde_json::Value = serde_json::from_str(&event.data_json)?;
        let revision = |key| {
            data.get(key)
                .and_then(|value| value.as_str())
                .filter(|value| {
                    matches!(value.len(), 40 | 64)
                        && value.bytes().all(|byte| byte.is_ascii_hexdigit())
                })
                .context("The task has an invalid Git revision")
        };
        let mut child = std::process::Command::new("git")
            .args([
                "diff",
                "--no-ext-diff",
                "--no-textconv",
                revision("base")?,
                revision("commit")?,
                "--",
            ])
            .current_dir(&self.project.root)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .context("Cannot open saved changes")?;
        const PREVIEW_LIMIT: usize = 64 * 1024;
        let mut bytes = Vec::new();
        let result = child
            .stdout
            .take()
            .context("Cannot read Git diff")?
            .take((PREVIEW_LIMIT + 1) as u64)
            .read_to_end(&mut bytes);
        if result.is_err() || bytes.len() > PREVIEW_LIMIT {
            let _ = child.kill();
        }
        let status = child.wait()?;
        result?;
        anyhow::ensure!(
            status.success() || bytes.len() > PREVIEW_LIMIT,
            "Git could not read the saved revisions. They may be unavailable in this checkout."
        );
        let mut diff = crate::secrets::redact_sensitive(&String::from_utf8_lossy(
            &bytes[..bytes.len().min(PREVIEW_LIMIT)],
        ));
        if bytes.len() > PREVIEW_LIMIT {
            diff.push_str("\n\nPreview ended after 64 KiB. Open Git to inspect the full changes.");
        }
        self.select_tab(ActiveTab::Diff);
        self.result_diff = Some(diff);
        self.status_message = None;
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
                    "Commands: /config, /new, /history, /cli <writer> <critic>, /task:test <file>, /task:refactor <file>, /task:fix <path> <issue>, /task:bench <path>, /stop, /pause, /resume, /run, /commit <msg>, /work, /diff, /working-diff, /review, /logs, /writer, /critic, /model, /mode, /exit".to_string(),
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
            "/work" | "/feed" | "/фид" | "/лента" | "/overview" | "/обзор" => {
                self.select_tab(ActiveTab::Work);
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
                self.select_tab(ActiveTab::Dialogue);
            }
            "/plan" | "/план" => {
                self.open_plan();
            }
            "/copy" | "/копировать" | "/yank" => {
                self.copy_active_view_to_clipboard();
            }
            "/continue" | "/продолжить" | "/resume-task" => {
                let target_id = if arg.is_empty() {
                    None
                } else {
                    arg.parse::<i64>().ok()
                };
                self.continue_selected_task(target_id)?;
            }
            "/diff" | "/дифф" => {
                self.open_changes()?;
            }
            "/working-diff" => {
                self.show_working_diff();
            }
            "/review" | "/ревью" => {
                self.select_tab(ActiveTab::Dialogue);
            }
            "/logs" | "/audit" | "/аудит" => {
                self.select_tab(ActiveTab::Logs);
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
            "/resume" | "/резюм" => {
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
                "adaptive" | "auto" | "адаптивный" | "авто" => {
                    self.config.orchestration.mode = OrchestrationMode::Adaptive;
                    self.config.save(&self.project)?;
                    self.notice("Mode set to Adaptive (smart auto-routing).");
                }
                "team" | "команда" => {
                    self.config.orchestration.mode = OrchestrationMode::Team;
                    self.config.save(&self.project)?;
                    self.status_message = Some(("Mode set to TEAM.".to_string(), Instant::now()));
                }
                "pair" | "пара" => {
                    self.config.orchestration.mode = OrchestrationMode::Pair;
                    self.config.save(&self.project)?;
                    self.status_message = Some(("Mode set to PAIR.".to_string(), Instant::now()));
                }
                _ => {
                    self.status_message = Some((
                        "Use /mode adaptive, /mode pair, or /mode team.".to_string(),
                        Instant::now(),
                    ));
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
        self.child = Some((id, child));
        self.supervisor_running = true;
        self.run_notice = None;
        self.status_message = None;
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
    if log.iter().any(|line| {
        line.starts_with("Error:") && crate::adapter::mentions_authentication_failure(line)
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
                "Open Logs for the failing role. Sign in from a shell with {} where applicable.",
                commands.join(" or ")
            )
        };
        format!(
            "The agent could not authenticate. {actions} Then retry the task or edit it as a new task. Press F2 to choose another CLI."
        )
    } else {
        "Run stopped. Open Logs for details.".to_owned()
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
                self.task_input = Some((std::mem::take(&mut self.input), self.cursor));
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
                let modes = [
                    OrchestrationMode::Adaptive,
                    OrchestrationMode::Pair,
                    OrchestrationMode::Team,
                ];
                let index = modes
                    .iter()
                    .position(|mode| *mode == settings.orchestration.mode)
                    .expect("all orchestration modes are listed");
                settings.orchestration.mode =
                    modes[(index + if forward { 1 } else { 2 }) % modes.len()];
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
        if matches!(key.code, KeyCode::Char('y' | 'Y')) && ctrl && !repeat {
            self.copy_active_view_to_clipboard();
            return Ok(());
        }

        if key.code == KeyCode::Enter {
            return self.handle_submit();
        }
        if key.code == KeyCode::Esc {
            match self.screen {
                Screen::Plan => {
                    self.screen = Screen::Main;
                    self.scroll_offset = 0;
                }
                Screen::Settings => {
                    self.settings = None;
                    self.screen = Screen::Main;
                }
                Screen::WriterModel | Screen::CriticModel => {
                    self.restore_task_input();
                    self.screen = Screen::Settings;
                }
                Screen::Main if self.draft.take().is_some() => {}
                Screen::Main if self.input.is_empty() => self.select_tab(ActiveTab::Work),
                _ => self.clear_input(),
            }
            self.status_message = None;
            return Ok(());
        }

        match self.screen {
            Screen::Main => self.handle_work_key(key, repeat),
            Screen::Plan => {
                match key.code {
                    KeyCode::Esc | KeyCode::Char('p' | 'P') | KeyCode::Enter => {
                        self.screen = Screen::Main;
                        self.scroll_offset = 0;
                    }
                    KeyCode::Char('y' | 'Y') => {
                        self.copy_active_view_to_clipboard();
                    }
                    KeyCode::Char('d' | 'D') => {
                        self.screen = Screen::Main;
                        let _ = self.open_changes();
                    }
                    KeyCode::Up => self.scroll_offset = self.scroll_offset.saturating_sub(1),
                    KeyCode::Down => self.scroll_offset = self.scroll_offset.saturating_add(1),
                    KeyCode::PageUp => self.scroll_offset = self.scroll_offset.saturating_sub(10),
                    KeyCode::PageDown => self.scroll_offset = self.scroll_offset.saturating_add(10),
                    KeyCode::Home => self.scroll_offset = 0,
                    KeyCode::End => self.scroll_offset = usize::MAX / 2,
                    _ => {}
                }
                Ok(())
            }
            Screen::Settings => {
                match key.code {
                    KeyCode::Up => self.selection = (self.selection + 4) % 5,
                    KeyCode::Down | KeyCode::Tab => self.selection = (self.selection + 1) % 5,
                    KeyCode::Left => self.change_setting(false)?,
                    KeyCode::Right | KeyCode::Char('e' | 'E') if !repeat => {
                        self.change_setting(true)?
                    }
                    KeyCode::Char('r' | 'R') if !repeat => {
                        self.refresh_available();
                        self.notice("Checked for installed agent CLIs.");
                    }
                    _ => {}
                }
                Ok(())
            }
            _ => {
                self.edit_input(key);
                Ok(())
            }
        }
    }

    fn handle_work_key(&mut self, key: KeyEvent, repeat: bool) -> Result<()> {
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Char('p' | 'P') if self.input.is_empty() && !repeat => {
                self.open_plan();
                return Ok(());
            }
            KeyCode::Char('c' | 'C')
                if self.active_tab == ActiveTab::History && self.input.is_empty() && !repeat =>
            {
                if let Err(e) = self.continue_selected_task(None) {
                    self.notice(format!("Continue failed: {e}"));
                }
                return Ok(());
            }
            KeyCode::Char('y' | 'Y') if self.input.is_empty() && !repeat => {
                self.copy_active_view_to_clipboard();
                return Ok(());
            }
            KeyCode::Char('d' | 'D') if self.input.is_empty() && !repeat => {
                self.open_changes()?;
                return Ok(());
            }
            KeyCode::F(2) => self.open_settings(),
            KeyCode::F(3) => self.open_history(None)?,
            KeyCode::F(4) => self.new_session()?,
            KeyCode::F(5) if self.can_retry_task() => self.retry_selected_task()?,
            KeyCode::F(6)
                if !self.supervisor_running && self.task_status == Some(TaskStatus::Queued) =>
            {
                if let Some(id) = self.task_view {
                    Database::open(&self.project)?.cancel_queued_task(id)?;
                    self.run_notice = None;
                    self.refresh_state()?;
                }
            }
            KeyCode::F(6)
                if !self.supervisor_running
                    && matches!(
                        self.task_status,
                        Some(TaskStatus::Failed | TaskStatus::Blocked)
                    )
                    && !self.task_cancelled =>
            {
                if self.input.is_empty()
                    && let Some(id) = self.task_view
                    && let Some(task) = Database::open(&self.project)?.get_task(id)?
                {
                    self.input = task.acceptance_criteria;
                    self.cursor = self.input.len();
                    self.draft = None;
                    self.select_tab(ActiveTab::Work);
                }
            }
            KeyCode::Char(ch @ '1'..='5') if alt && !repeat => {
                self.open_tab(ActiveTab::ALL[ch as usize - '1' as usize])?;
            }
            KeyCode::BackTab => {
                let previous =
                    (self.active_tab as usize + ActiveTab::ALL.len() - 1) % ActiveTab::ALL.len();
                self.open_tab(ActiveTab::ALL[previous])?;
            }
            KeyCode::Tab if self.is_palette_active() => {
                let matches = self.palette_matches();
                if !matches.is_empty() {
                    let idx = self.palette_index.min(matches.len() - 1);
                    let chosen = matches[idx];
                    self.input = format!("{} ", chosen.cmd);
                    self.cursor = self.input.len();
                    self.palette_index = 0;
                    self.draft = None;
                }
            }
            KeyCode::Tab if self.input.is_empty() => self.open_tab(self.active_tab.next())?,
            KeyCode::Tab => {
                self.complete_input();
                self.cursor = self.input.len();
                self.draft = None;
            }
            KeyCode::Up if self.is_palette_active() => {
                self.palette_index = self.palette_index.saturating_sub(1);
            }
            KeyCode::Down if self.is_palette_active() => {
                let len = self.palette_matches().len();
                if len > 0 {
                    self.palette_index = (self.palette_index + 1).min(len - 1);
                }
            }
            KeyCode::Up | KeyCode::Down | KeyCode::PageUp | KeyCode::PageDown
                if self.active_tab == ActiveTab::History =>
            {
                match key.code {
                    KeyCode::Up => {
                        self.history_selection = self.history_selection.saturating_sub(1)
                    }
                    KeyCode::Down => {
                        self.history_selection =
                            (self.history_selection + 1).min(self.history.len().saturating_sub(1))
                    }
                    KeyCode::PageDown if self.history.len() == 50 => {
                        self.open_history(self.history.last().map(|entry| entry.task.id))?;
                    }
                    KeyCode::PageUp if self.history_before.is_some() => self.open_history(None)?,
                    _ => {}
                }
            }
            KeyCode::Up
                if alt && self.active_tab == ActiveTab::Diff && self.result_diff.is_none() =>
            {
                self.diff_file_index = self.diff_file_index.saturating_sub(1);
                self.scroll_offset = 0;
            }
            KeyCode::Down
                if alt && self.active_tab == ActiveTab::Diff && self.result_diff.is_none() =>
            {
                self.diff_file_index =
                    (self.diff_file_index + 1).min(self.modified_files.len().saturating_sub(1));
                self.scroll_offset = 0;
            }
            KeyCode::Up
                if (self.active_tab == ActiveTab::Work || !self.input.is_empty())
                    && self.draft.is_none()
                    && !self.input_history.is_empty() =>
            {
                if self.history_index.is_none() {
                    self.saved_draft = self.input.clone();
                    let idx = self.input_history.len() - 1;
                    self.history_index = Some(idx);
                    self.input = self.input_history[idx].clone();
                    self.cursor = self.input.len();
                } else if let Some(idx) = self.history_index
                    && idx > 0
                {
                    let new_idx = idx - 1;
                    self.history_index = Some(new_idx);
                    self.input = self.input_history[new_idx].clone();
                    self.cursor = self.input.len();
                }
            }
            KeyCode::Down if self.history_index.is_some() && self.draft.is_none() => {
                let next = self.history_index.map_or(0, |index| index + 1);
                if let Some(entry) = self.input_history.get(next) {
                    self.history_index = Some(next);
                    self.input = entry.clone();
                } else {
                    self.history_index = None;
                    self.input = std::mem::take(&mut self.saved_draft);
                }
                self.cursor = self.input.len();
            }
            KeyCode::Up => self.scroll_tab(false, 1),
            KeyCode::Down => self.scroll_tab(true, 1),
            KeyCode::PageUp => self.scroll_tab(false, 5),
            KeyCode::PageDown => self.scroll_tab(true, 5),
            KeyCode::Home if self.input.is_empty() => self.scroll_tab(false, usize::MAX),
            KeyCode::End if self.input.is_empty() => self.scroll_tab(true, usize::MAX),
            _ => self.edit_input(key),
        }
        Ok(())
    }

    fn open_tab(&mut self, tab: ActiveTab) -> Result<()> {
        match tab {
            ActiveTab::Diff => self.open_changes()?,
            ActiveTab::History => self.open_history(None)?,
            _ => self.select_tab(tab),
        }
        Ok(())
    }

    fn retry_selected_task(&mut self) -> Result<()> {
        self.refresh_available();
        self.refresh_state()?;
        anyhow::ensure!(self.can_retry_task(), "The task status changed.");
        if let Some(blocker) = self.run_blocker(self.task_view) {
            anyhow::bail!("{blocker}");
        }
        if let Some(id) = self.task_view {
            self.select_tab(ActiveTab::Work);
            self.start_selected_task(id)?;
        }
        Ok(())
    }

    fn select_tab(&mut self, tab: ActiveTab) {
        self.active_tab = tab;
        self.scroll_offset = 0;
    }

    fn edit_input(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let previous = self.input[..self.cursor]
            .char_indices()
            .next_back()
            .map_or(0, |(index, _)| index);
        let next = self.input[self.cursor..]
            .chars()
            .next()
            .map_or(self.cursor, |ch| self.cursor + ch.len_utf8());
        if matches!(
            key.code,
            KeyCode::Backspace | KeyCode::Delete | KeyCode::Char(_)
        ) && self.screen == Screen::Main
        {
            self.draft = None;
            self.status_message = None;
            self.history_index = None;
        }
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
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.input.len(),
            KeyCode::Char(ch) if !ctrl && !alt && !ch.is_control() && self.input.len() < 8_000 => {
                self.input.insert(self.cursor, ch);
                self.cursor += ch.len_utf8();
            }
            _ => {}
        }
    }

    fn scroll_tab(&mut self, forward: bool, rows: usize) {
        self.scroll_offset = if forward == (self.active_tab != ActiveTab::Logs) {
            self.scroll_offset.saturating_add(rows)
        } else {
            self.scroll_offset.saturating_sub(rows)
        };
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
        if self.screen == Screen::Main && !text.is_empty() {
            self.draft = None;
            self.status_message = None;
            self.history_index = None;
        }
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

fn ui(frame: &mut Frame, app: &mut TuiApp) {
    let size = frame.area();
    if app.screen != Screen::Main {
        render_workflow(frame, app, size);
        return;
    }

    let input = InputView::new(app, size);
    let notice = app
        .status_message
        .as_ref()
        .map(|(message, _)| Paragraph::new(safe_display(message)).wrap(Wrap { trim: true }));
    let notice_rows = notice.as_ref().map_or(0, |paragraph| {
        measure_rows(paragraph, size.width, (size.height / 4).max(2))
    });

    let goal_height = if app.has_active_or_recent_task() && size.height >= 18 {
        2
    } else {
        0
    };

    let chunks = Layout::vertical([
        Constraint::Length(1),            // Header
        Constraint::Length(goal_height),  // Pinned Goal & Stage Tracker
        Constraint::Length(1),            // Tabs navigation
        Constraint::Min(3),               // Content Canvas
        Constraint::Length(notice_rows),  // Notice Toast
        Constraint::Length(input.height), // Input Composer
    ])
    .split(size);

    render_header(frame, app, chunks[0]);
    if goal_height > 0 {
        render_goal_tracker(frame, app, chunks[1]);
    }
    render_tabs(frame, app, chunks[2]);
    match app.active_tab {
        ActiveTab::Work => render_work_tab(frame, app, chunks[3]),
        ActiveTab::Dialogue => render_dialogue_tab(frame, app, chunks[3]),
        ActiveTab::Diff => render_diff_tab(frame, app, chunks[3]),
        ActiveTab::Logs => render_unified_feed(frame, app, chunks[3]),
        ActiveTab::History => render_history_tab(frame, app, chunks[3]),
    }
    if let Some(notice) = notice {
        frame.render_widget(notice, chunks[4]);
    }
    input.render(frame, chunks[5]);
    if app.is_palette_active() {
        render_command_palette(frame, app, chunks[5]);
    }
}

fn render_header(frame: &mut Frame, app: &TuiApp, area: Rect) {
    let project = app
        .project
        .root
        .file_name()
        .unwrap_or(app.project.root.as_os_str());
    let mode_str = match app.config.orchestration.mode {
        OrchestrationMode::Adaptive => "Adaptive",
        OrchestrationMode::Pair => "Pair",
        OrchestrationMode::Team => "Team",
    };
    let header_line = Line::from(vec![
        Span::styled("TALOS ", Style::default().bold().fg(Color::Cyan)),
        Span::styled(
            format!("· {} ", project.to_string_lossy()),
            Style::default().bold().fg(Color::White),
        ),
        Span::styled("│ ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!("Mode: {mode_str} "),
            Style::default().fg(Color::Yellow),
        ),
        Span::styled("│ ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!("Writes: {} ", app.config.writer.as_str()),
            Style::default().fg(Color::Green),
        ),
        Span::styled("│ ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!("Reviews: {} ", app.config.critic.as_str()),
            Style::default().fg(Color::Magenta),
        ),
        Span::styled("│ ", Style::default().fg(Color::DarkGray)),
        Span::styled("F2 Settings", Style::default().fg(Color::DarkGray)),
    ]);
    frame.render_widget(Paragraph::new(header_line), area);
}

fn render_goal_tracker(frame: &mut Frame, app: &TuiApp, area: Rect) {
    if area.height == 0 {
        return;
    }
    let mut lines = Vec::new();
    if let Some(id) = app.task_view {
        let title_clean = safe_display(&app.task_title);
        let goal_text = if title_clean.is_empty() {
            "Active Task".to_string()
        } else {
            title_clean
        };
        lines.push(Line::from(vec![
            Span::styled("🎯 GOAL ", Style::default().fg(Color::Cyan).bold()),
            Span::styled(format!("#{id}: "), Style::default().fg(Color::White).bold()),
            Span::styled(goal_text, Style::default().fg(Color::White).bold()),
        ]));

        if area.height > 1 {
            let (stage_label, stage_color) = if app.task_cancelled {
                ("Cancelled", Color::Red)
            } else {
                match app.task_status {
                    Some(TaskStatus::Queued) => ("1/4 Queued · Waiting to run", Color::Yellow),
                    Some(TaskStatus::Planning) => ("1/4 Planning & Triage", Color::Cyan),
                    Some(TaskStatus::Implementing) => ("2/4 Implementing code", Color::Yellow),
                    Some(TaskStatus::Reviewing) => ("3/4 Adversarial review", Color::Magenta),
                    Some(TaskStatus::Verifying) => ("4/4 Running verification checks", Color::Cyan),
                    Some(TaskStatus::Complete) => ("Completed · All checks passed", Color::Green),
                    Some(TaskStatus::Blocked) => ("Blocked", Color::Red),
                    Some(TaskStatus::Failed) => ("Failed", Color::Red),
                    None => ("Ready", Color::Green),
                }
            };

            lines.push(Line::from(vec![
                Span::styled("📍 STAGE: ", Style::default().fg(Color::DarkGray)),
                Span::styled(stage_label, Style::default().fg(stage_color).bold()),
                Span::styled(" │ ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    "[p] Plan  [d] Diff  [y] Copy",
                    Style::default().fg(Color::DarkGray),
                ),
            ]));
        }
    } else if let Some(draft) = &app.draft {
        lines.push(Line::from(vec![
            Span::styled("🎯 DRAFT: ", Style::default().fg(Color::Yellow).bold()),
            Span::styled(
                safe_display(&draft.title),
                Style::default().fg(Color::White).bold(),
            ),
        ]));
        if area.height > 1 {
            lines.push(Line::from(vec![Span::styled(
                "Enter Confirm run · Esc Edit · [p] Plan",
                Style::default().fg(Color::Cyan),
            )]));
        }
    }

    frame.render_widget(Paragraph::new(lines), area);
}

fn render_command_palette(frame: &mut Frame, app: &mut TuiApp, input_area: Rect) {
    let matches = app.palette_matches();
    if matches.is_empty() {
        return;
    }
    let max_visible = 6.min(matches.len());
    let popup_height = (max_visible + 2) as u16;
    let popup_width = input_area.width.saturating_sub(4).min(76);
    let popup_y = input_area.y.saturating_sub(popup_height);
    let popup_area = Rect {
        x: input_area.x + 2,
        y: popup_y,
        width: popup_width,
        height: popup_height,
    };

    frame.render_widget(Clear, popup_area);

    let selected = app.palette_index.min(matches.len().saturating_sub(1));
    let start_idx = if selected >= max_visible {
        selected + 1 - max_visible
    } else {
        0
    };
    let end_idx = (start_idx + max_visible).min(matches.len());

    let items: Vec<ListItem> = matches[start_idx..end_idx]
        .iter()
        .enumerate()
        .map(|(rel_idx, item)| {
            let actual_idx = start_idx + rel_idx;
            let is_selected = actual_idx == selected;
            let (cmd_style, desc_style, bg_style) = if is_selected {
                (
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                    Style::default().fg(Color::White),
                    Style::default().bg(Color::Rgb(30, 45, 70)),
                )
            } else {
                (
                    Style::default().fg(Color::Cyan),
                    Style::default().fg(Color::DarkGray),
                    Style::default(),
                )
            };
            let prefix = if is_selected { " ▶ " } else { "   " };
            let line = Line::from(vec![
                Span::styled(prefix, cmd_style),
                Span::styled(format!("{:<16}", item.cmd), cmd_style),
                Span::styled(item.description, desc_style),
            ]);
            ListItem::new(line).style(bg_style)
        })
        .collect();

    let block = Block::bordered()
        .title(" Slash Commands (↑/↓ to navigate, Tab/Enter to choose) ")
        .title_style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )
        .border_style(Style::default().fg(Color::Yellow));

    frame.render_widget(List::new(items).block(block), popup_area);
}

fn render_workflow(frame: &mut Frame, app: &mut TuiApp, area: Rect) {
    let editing = matches!(
        app.screen,
        Screen::ChooseProject | Screen::WriterModel | Screen::CriticModel
    );
    let input = editing.then(|| InputView::new(app, area));
    let (title, body, actions) = match app.screen {
        Screen::ChooseProject => (
            "Work",
            "Enter the path to an existing Git project. If you have just installed TALOS, you can also open a terminal inside your project and run talos there.".into(),
            "Enter  Open project   Esc  Clear path".to_owned(),
        ),
        Screen::Settings | Screen::WriterModel | Screen::CriticModel => {
            let config = app.settings.as_ref().unwrap_or(&app.config);
            let writer = adapter_label(config.writer, &app.available);
            let critic = adapter_label(config.critic, &app.available);
            let rows = [
                format!("Writes code        {writer}"),
                format!("Writer model       {}", config.writer_model.as_deref().unwrap_or("CLI default")),
                format!("Reviews changes    {critic}"),
                format!("Reviewer model     {}", config.critic_model.as_deref().unwrap_or("CLI default")),
                format!("Mode               {}", match config.orchestration.mode {
                    OrchestrationMode::Adaptive => "Adaptive · select Pair or Team for each task",
                    OrchestrationMode::Pair => "Pair · write, review, verify",
                    OrchestrationMode::Team => "Team · parallel writers and review",
                }),
            ];
            let mut body = rows.iter().enumerate().map(|(index, row)| {
                format!("{} {row}", if index == app.selection { ">" } else { " " })
            }).collect::<Vec<_>>().join("\n");
            let discovered = app.available.iter().map(|adapter| adapter.as_str()).collect::<Vec<_>>().join(", ");
            if discovered.is_empty() {
                body.push_str("\n\nNo agent CLI found. Install one in a separate terminal, sign in if prompted, then press R here.");
                if cfg!(target_os = "macos") {
                    body.push_str("\nCodex: brew install --cask codex\nClaude Code: brew install --cask claude-code");
                } else {
                    body.push_str("\nCodex: https://github.com/openai/codex\nClaude Code: https://code.claude.com/docs/en/setup");
                }
            } else {
                body.push_str(&format!("\n\nFound locally: {discovered}\nSign in or configure credentials in the agent's own CLI if required."));
            }
            body.push_str("\n\nProject checks");
            if config.checks.is_empty() {
                body.push_str("\nNo checks configured.");
            } else {
                for check in &config.checks {
                    body.push_str(&format!("\n{} · {}", check.id, check.argv.join(" ")));
                }
            }
            if let Some(repository) = app.publication_target() {
                body.push_str(&format!("\n\nGitHub draft PR enabled for {repository}."));
            }
            let actions = if editing {
                "Enter  Apply model   Esc  Cancel edit"
            } else {
                "↑ ↓  Select   ← →  Change / edit   Enter  Save   R  Check again   Esc  Back"
            };
            ("Settings", body, actions.to_owned())
        }
        Screen::Plan => {
            let (body, actions) = app.build_plan_view_content();
            ("Plan & Tasks", body, actions)
        }
        Screen::Main => unreachable!(),
    };
    let project = app.project.root.display();
    let title =
        Paragraph::new(safe_display(&format!("{title} · {project}"))).wrap(Wrap { trim: true });
    let title_rows = measure_rows(&title, area.width.saturating_sub(2), 4) + 2;
    let message = app
        .status_message
        .as_ref()
        .map_or("", |(message, _)| message.as_str());
    let footer = if message.is_empty() {
        actions
    } else {
        format!("{actions}\n{message}")
    };
    let footer = Paragraph::new(safe_display(&footer)).wrap(Wrap { trim: true });
    let footer_rows = measure_rows(&footer, area.width, (area.height / 3).max(2));
    let chunks = Layout::vertical([
        Constraint::Length(title_rows),
        Constraint::Min(4),
        Constraint::Length(input.as_ref().map_or(0, |input| input.height)),
        Constraint::Length(footer_rows),
    ])
    .split(area);

    frame.render_widget(title.block(Block::bordered()), chunks[0]);
    let body = safe_display(&body);
    let block = if app.screen == Screen::Plan {
        Block::default()
            .borders(Borders::TOP)
            .border_style(Style::default().fg(Color::DarkGray))
            .title(" Plan & Task Tree · [y] Copy · Esc Back ")
    } else {
        Block::bordered()
    };
    let inner = block.inner(chunks[1]);
    let paragraph = Paragraph::new(body.as_str()).wrap(Wrap { trim: false });
    let max_scroll = wrapped_scroll_limit(&paragraph, inner).min(u16::MAX as usize) as u16;
    let scroll = match app.screen {
        Screen::Settings | Screen::WriterModel | Screen::CriticModel => {
            let lines = body
                .lines()
                .take(app.selection.saturating_add(1))
                .collect::<Vec<_>>()
                .join("\n");
            let selected_end = Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .line_count(inner.width);
            selected_end
                .saturating_sub(usize::from(inner.height))
                .min(usize::from(max_scroll)) as u16
        }
        Screen::Plan => app.scroll_offset.min(usize::from(max_scroll)) as u16,
        _ => 0,
    };
    frame.render_widget(paragraph.block(block).scroll((scroll, 0)), chunks[1]);
    if let Some(input) = input {
        input.render(frame, chunks[2]);
    }
    frame.render_widget(footer, chunks[3]);
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

fn copy_text_to_clipboard(text: &str) -> bool {
    let mut success = false;
    #[cfg(target_os = "macos")]
    {
        if let Ok(mut child) = Command::new("pbcopy")
            .stdin(std::process::Stdio::piped())
            .spawn()
        {
            if let Some(mut stdin) = child.stdin.take() {
                use std::io::Write;
                let _ = stdin.write_all(text.as_bytes());
            }
            if child.wait().is_ok() {
                success = true;
            }
        }
    }

    #[cfg(target_os = "linux")]
    {
        if let Ok(mut child) = Command::new("wl-copy")
            .stdin(std::process::Stdio::piped())
            .spawn()
        {
            if let Some(mut stdin) = child.stdin.take() {
                use std::io::Write;
                let _ = stdin.write_all(text.as_bytes());
            }
            if child.wait().is_ok() {
                success = true;
            }
        } else if let Ok(mut child) = Command::new("xclip")
            .args(["-selection", "clipboard"])
            .stdin(std::process::Stdio::piped())
            .spawn()
        {
            if let Some(mut stdin) = child.stdin.take() {
                use std::io::Write;
                let _ = stdin.write_all(text.as_bytes());
            }
            if child.wait().is_ok() {
                success = true;
            }
        }
    }

    let b64 = base64_encode(text.as_bytes());
    print!("\x1b]52;c;{}\x07", b64);
    use std::io::Write;
    let _ = std::io::stdout().flush();
    success || !b64.is_empty()
}

fn base64_encode(data: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);
        out.push(CHARS[(b0 >> 2) as usize] as char);
        out.push(CHARS[(((b0 & 3) << 4) | (b1 >> 4)) as usize] as char);
        if chunk.len() > 1 {
            out.push(CHARS[(((b1 & 15) << 2) | (b2 >> 6)) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(CHARS[(b2 & 63) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

fn render_tabs(frame: &mut Frame, app: &TuiApp, area: Rect) {
    let titles = if area.width < 49 {
        ["1 Work", "2 Rev", "3 Diff", "4 Log", "5 Hist"]
    } else {
        ActiveTab::ALL.map(ActiveTab::title)
    };
    let tabs = Tabs::new(titles)
        .select(app.active_tab as usize)
        .padding("", "")
        .divider("  ")
        .highlight_style(Style::default().fg(Color::Cyan).bold());
    frame.render_widget(tabs, area);
}

fn render_work_tab(frame: &mut Frame, app: &mut TuiApp, area: Rect) {
    let mut lines = Vec::new();
    let blocker = app.run_blocker(None);
    if let Some(draft) = &app.draft {
        lines.push(Line::from("Ready to confirm").style(Style::default().bold()));
        if let Some(repository) = app.publication_target() {
            lines.push(Line::from(safe_display(&format!(
                "This task can open a draft PR in {repository}."
            ))));
        }
        lines.push(Line::from("Enter Confirm · Esc Back"));
        lines.push(Line::default());
        lines.extend(
            safe_display(&draft.criteria)
                .lines()
                .map(|line| Line::from(line.to_owned())),
        );
    } else if let (Some(status), Some(id)) = (&app.task_status, app.task_view) {
        let label = if app.task_cancelled {
            "Cancelled"
        } else {
            match status {
                TaskStatus::Queued if app.child.as_ref().is_some_and(|(task, _)| *task == id) => {
                    "Starting"
                }
                TaskStatus::Queued => "Waiting to run",
                TaskStatus::Planning => "Planning",
                TaskStatus::Implementing => "Making changes",
                TaskStatus::Reviewing => "Reviewing changes",
                TaskStatus::Verifying => "Running checks",
                TaskStatus::Complete => "Completed",
                TaskStatus::Blocked => "Blocked",
                TaskStatus::Failed => "Failed",
            }
        };
        lines.push(Line::from(format!("{label} · Task #{id}")).style(Style::default().bold()));
        lines.push(Line::from(safe_display(&app.task_title)));
        if let Some(notice) = &app.run_notice {
            lines.push(Line::default());
            lines.push(Line::from(safe_display(notice)));
        }
        if *status == TaskStatus::Complete {
            let checks = match app.verification_status.as_deref() {
                Some("passed") => "Project checks passed.",
                _ if app.verification_checks.is_empty() => "No project checks configured.",
                _ => "Open Review for recorded checks.",
            };
            lines.push(Line::from(checks));
            lines.push(Line::from("Alt+3 View changes"));
        } else if app.can_retry_task() {
            lines.push(Line::from(if *status == TaskStatus::Queued {
                "F5 Retry · F6 Cancel queued task"
            } else {
                "F5 Retry this stage"
            }));
        } else if !app.supervisor_running
            && !app.task_cancelled
            && matches!(status, TaskStatus::Failed | TaskStatus::Blocked)
        {
            lines.push(Line::from("F6 Edit as new task · Alt+4 Logs"));
        } else if app.active_tasks.iter().any(|task| task.id == id) {
            lines.push(Line::from("/stop Request stop"));
        }
    } else {
        let status = if blocker.is_some() {
            "Blocked"
        } else {
            "Ready"
        };
        lines.push(Line::from(status).style(Style::default().bold()));
        lines.push(Line::from("Describe a change to make in this project."));
    }

    if let Some(blocker) = blocker
        && !app.supervisor_running
        && app.active_tasks.is_empty()
        && app.status_message.is_none()
    {
        lines.push(Line::default());
        lines.push(Line::from(safe_display(&blocker)));
    }

    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(" Work · [p] Plan · [d] Diff · [y] Copy ");
    render_reader(
        frame,
        Paragraph::new(lines),
        block,
        area,
        &mut app.scroll_offset,
    );
}

fn render_history_tab(frame: &mut Frame, app: &TuiApp, area: Rect) {
    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(" History · Enter Open · [c] Continue · ↑↓ Select · [y] Copy ");
    if app.history.is_empty() {
        frame.render_widget(Paragraph::new("No tasks yet.").block(block), area);
        return;
    }

    let selected_entry = app.history.get(app.history_selection);

    if area.height >= 12 && selected_entry.is_some() {
        let card_height = 6.min(area.height.saturating_sub(6));
        let chunks =
            Layout::vertical([Constraint::Min(4), Constraint::Length(card_height)]).split(area);

        let mut state =
            ratatui::widgets::ListState::default().with_selected(Some(app.history_selection));
        let items = app.history.iter().map(|entry| {
            let task = &entry.task;
            let status = if entry.cancelled {
                "cancelled"
            } else {
                task.status.as_str()
            };
            ListItem::new(safe_display(&format!(
                "#{} · {status} · {}",
                task.id, task.title
            )))
        });
        frame.render_stateful_widget(
            List::new(items)
                .block(block)
                .highlight_symbol("› ")
                .highlight_style(Style::default().fg(Color::Cyan).bold()),
            chunks[0],
            &mut state,
        );

        // Render Resume Summary Card
        if let Some(entry) = selected_entry
            && let Ok(database) = Database::open(&app.project)
            && let Ok(ctx) =
                crate::summary::assemble_resume_context(&app.project, &database, entry.task.id)
        {
            let mut card_lines = Vec::new();
            card_lines.push(Line::from(vec![
                Span::styled("🎯 Task ", Style::default().fg(Color::Cyan).bold()),
                Span::styled(
                    format!("#{}: ", ctx.task_id),
                    Style::default().fg(Color::White).bold(),
                ),
                Span::styled(
                    safe_display(&ctx.title),
                    Style::default().fg(Color::White).bold(),
                ),
            ]));
            card_lines.push(Line::from(vec![
                Span::styled("📍 Stopped at: ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    safe_display(&ctx.where_stopped),
                    Style::default().fg(Color::Yellow),
                ),
            ]));
            if let Some(diff) = &ctx.landed_diff_summary {
                card_lines.push(Line::from(vec![
                    Span::styled("📄 Changes: ", Style::default().fg(Color::DarkGray)),
                    Span::styled(diff.as_str(), Style::default().fg(Color::Green)),
                ]));
            }
            if !ctx.open_critic_remarks.is_empty() {
                let preview = ctx
                    .open_critic_remarks
                    .first()
                    .map(|s| s.as_str())
                    .unwrap_or("");
                card_lines.push(Line::from(vec![
                    Span::styled(
                        format!("⚠️ Open remark ({}): ", ctx.open_critic_remarks.len()),
                        Style::default().fg(Color::Red),
                    ),
                    Span::styled(safe_display(preview), Style::default().fg(Color::White)),
                ]));
            }
            if let Some(check) = &ctx.failing_check {
                card_lines.push(Line::from(vec![
                    Span::styled("❌ Check: ", Style::default().fg(Color::Red)),
                    Span::styled(check.as_str(), Style::default().fg(Color::White)),
                ]));
            }
            if let Some(drift) = &ctx.git_drift {
                card_lines.push(Line::from(vec![
                    Span::styled("🔀 Drift: ", Style::default().fg(Color::Yellow)),
                    Span::styled(drift.as_str(), Style::default().fg(Color::White)),
                ]));
            }
            card_lines.push(Line::from(vec![
                Span::styled("⚡ Action: ", Style::default().fg(Color::Cyan).bold()),
                Span::styled(
                    ctx.next_step.description(),
                    Style::default().fg(Color::Cyan),
                ),
                Span::styled(
                    " │ Press [c] to continue",
                    Style::default().fg(Color::DarkGray),
                ),
            ]));

            let card_block = Block::default()
                .borders(Borders::TOP)
                .border_style(Style::default().fg(Color::DarkGray))
                .title(" Task Resume Card · [c] Continue Task · [y] Copy ");
            frame.render_widget(Paragraph::new(card_lines).block(card_block), chunks[1]);
        }
    } else {
        let mut state =
            ratatui::widgets::ListState::default().with_selected(Some(app.history_selection));
        let items = app.history.iter().map(|entry| {
            let task = &entry.task;
            let status = if entry.cancelled {
                "cancelled"
            } else {
                task.status.as_str()
            };
            ListItem::new(safe_display(&format!(
                "#{} · {status} · {}",
                task.id, task.title
            )))
        });
        frame.render_stateful_widget(
            List::new(items)
                .block(block)
                .highlight_symbol("› ")
                .highlight_style(Style::default().fg(Color::Cyan).bold()),
            area,
            &mut state,
        );
    }
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

        let reason_clean = safe_display(&event.reason).replace('\n', " ");
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

fn render_unified_feed(frame: &mut Frame, app: &mut TuiApp, area: Rect) {
    let feed = Paragraph::new(build_unified_feed_lines(app)).wrap(Wrap { trim: true });
    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(Color::DarkGray));
    let max_scroll = wrapped_scroll_limit(&feed, block.inner(area)).min(u16::MAX as usize);
    app.scroll_offset = app.scroll_offset.min(max_scroll);
    let start = (max_scroll - app.scroll_offset) as u16;
    let title = if app.scroll_offset > 0 {
        " Logs · End to follow · [y] Copy "
    } else {
        " Logs · Following · [y] Copy "
    };
    frame.render_widget(feed.block(block.title(title)).scroll((start, 0)), area);
}

fn render_reader(
    frame: &mut Frame,
    paragraph: Paragraph<'_>,
    block: Block<'_>,
    area: Rect,
    offset: &mut usize,
) {
    let paragraph = paragraph.wrap(Wrap { trim: false });
    let maximum = wrapped_scroll_limit(&paragraph, block.inner(area)).min(u16::MAX as usize);
    *offset = (*offset).min(maximum);
    frame.render_widget(paragraph.block(block).scroll((*offset as u16, 0)), area);
}

fn render_dialogue_tab(frame: &mut Frame, app: &mut TuiApp, area: Rect) {
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

    if let Some(id) = app.task_view {
        lines.push(Line::default());
        let checks = app
            .verification_status
            .as_deref()
            .unwrap_or("No run recorded");
        lines.push(Line::from(format!("Project checks · {checks}")));
        let report = crate::summary::task_summary_path(&app.project.root, id);
        if report.is_file() {
            lines.push(Line::from(safe_display(&format!(
                "Saved report · .talos/reports/task-{id}.md"
            ))));
        }
    }

    let block = Block::default()
        .borders(Borders::TOP)
        .title(" Review · [y] Copy ")
        .border_style(Style::default().fg(Color::DarkGray));

    render_reader(
        frame,
        Paragraph::new(lines),
        block,
        area,
        &mut app.scroll_offset,
    );
}

fn render_diff_tab(frame: &mut Frame, app: &mut TuiApp, area: Rect) {
    if let Some(diff) = &app.result_diff {
        let block = Block::default()
            .borders(Borders::TOP)
            .border_style(Style::default().fg(Color::DarkGray))
            .title(" Saved changes · [y] Copy ");
        render_reader(
            frame,
            Paragraph::new(diff.as_str()),
            block,
            area,
            &mut app.scroll_offset,
        );
        return;
    }
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
            .borders(Borders::TOP | Borders::BOTTOM)
            .title(" Changed files · Alt+↑/↓ Select ")
            .border_style(Style::default().fg(Color::DarkGray));

        frame.render_widget(List::new(file_items).block(files_block), chunks[0]);

        render_diff_view(frame, app, chunks[1]);
    } else {
        render_diff_view(frame, app, area);
    }
}

fn render_diff_view(frame: &mut Frame, app: &mut TuiApp, area: Rect) {
    let mut items = Vec::new();

    if app.diff_lines.is_empty() {
        items.push(Line::from(Span::styled(
            "No uncommitted code changes in current working tree or active worktree.",
            Style::default().fg(Color::DarkGray),
        )));
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

            items.push(Line::from(Span::styled(line.clone(), style)));
        }
    }

    let title = if let Some(selected) = app.selected_modified_file() {
        format!(
            " 📄 Live Git Diff: Selected [{selected}] ({} diff lines) · [y] Copy ",
            app.diff_lines.len()
        )
    } else {
        format!(
            " 📄 Live Code Diff ({} lines changed) · [y] Copy ",
            app.diff_lines.len()
        )
    };

    let block = Block::default()
        .borders(Borders::TOP)
        .title(title)
        .border_style(Style::default().fg(Color::DarkGray));

    render_reader(
        frame,
        Paragraph::new(items),
        block,
        area,
        &mut app.scroll_offset,
    );
}

struct InputView {
    lines: Vec<Line<'static>>,
    cursor: (usize, usize),
    height: u16,
    title: &'static str,
    hint: Option<Paragraph<'static>>,
    hint_rows: u16,
}

impl InputView {
    fn new(app: &TuiApp, area: Rect) -> Self {
        let (short_title, controls) = match app.screen {
            Screen::ChooseProject => (" Project folder ", ""),
            Screen::WriterModel | Screen::CriticModel => (" Model name ", ""),
            Screen::Main if app.draft.is_some() => (" Task ", " Enter Confirm · Esc Back "),
            Screen::Main if app.active_tab == ActiveTab::History && app.input.is_empty() => {
                (" Describe a task ", " Enter Open task · Tab Switch view ")
            }
            _ => (
                " Describe a task ",
                " Enter Run · Tab Switch view · Esc Clear ",
            ),
        };
        let width = area.width.saturating_sub(2).max(1);
        let (title, hint) = if controls.is_empty() {
            (short_title, None)
        } else if Line::raw(controls).width() <= usize::from(width) {
            (controls, None)
        } else {
            (
                short_title,
                Some(Paragraph::new(controls.trim()).wrap(Wrap { trim: true })),
            )
        };
        let hint_rows = hint.as_ref().map_or(0, |hint| measure_rows(hint, width, 4));
        let (lines, cursor) = wrap_input(&app.input, app.cursor, usize::from(width));
        let visible = lines
            .len()
            .max(2)
            .min(usize::from((area.height / 4).max(2))) as u16;
        Self {
            lines,
            cursor,
            height: visible + hint_rows + 2,
            title,
            hint,
            hint_rows,
        }
    }

    fn render(self, frame: &mut Frame, area: Rect) {
        let block = Block::bordered()
            .title(self.title)
            .border_style(Style::default().fg(Color::Cyan));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        if inner.is_empty() {
            return;
        }
        let hint_rows = self.hint_rows.min(inner.height.saturating_sub(1));
        let input = Rect {
            height: inner.height - hint_rows,
            ..inner
        };
        let (row, column) = self.cursor;
        let start = row
            .saturating_add(1)
            .saturating_sub(usize::from(input.height));
        frame.render_widget(Paragraph::new(self.lines).scroll((start as u16, 0)), input);
        frame.set_cursor_position((
            input.x + column.min(usize::from(input.width - 1)) as u16,
            input.y + (row - start) as u16,
        ));
        if let Some(hint) = self.hint {
            frame.render_widget(
                hint.style(Style::default().fg(Color::Gray)),
                Rect {
                    y: input.bottom(),
                    height: hint_rows,
                    ..inner
                },
            );
        }
    }
}

fn wrap_input(input: &str, cursor: usize, width: usize) -> (Vec<Line<'static>>, (usize, usize)) {
    let mut lines = Vec::new();
    let mut line = String::new();
    let mut column = 0;
    let mut position = (0, 0);
    let mut at_word_start = true;
    for (offset, grapheme) in input.grapheme_indices(true) {
        let whitespace = grapheme.chars().all(char::is_whitespace);
        let symbol = if grapheme == "\t" {
            &"    "[..width.min(4)]
        } else {
            grapheme
        };
        let cells = Span::raw(symbol).width();
        let word = if at_word_start && !whitespace {
            Span::raw(
                input[offset..]
                    .split_whitespace()
                    .next()
                    .unwrap_or(grapheme),
            )
            .width()
        } else {
            0
        };
        at_word_start = whitespace;
        let fits_on_next_line = word <= width && column + word > width;
        if grapheme != "\n" && column > 0 && (column + cells > width || fits_on_next_line) {
            lines.push(Line::raw(std::mem::take(&mut line)));
            column = 0;
        }
        if (offset..offset + grapheme.len()).contains(&cursor) {
            position = (lines.len(), column);
        }
        if grapheme == "\n" {
            lines.push(Line::raw(std::mem::take(&mut line)));
            column = 0;
        } else {
            line.push_str(symbol);
            column += cells;
        }
    }
    if cursor == input.len() {
        if column >= width {
            lines.push(Line::raw(std::mem::take(&mut line)));
            column = 0;
        }
        position = (lines.len(), column);
    }
    lines.push(Line::raw(line));
    (lines, position)
}

fn measure_rows(paragraph: &Paragraph<'_>, width: u16, limit: u16) -> u16 {
    paragraph.line_count(width).min(usize::from(limit)) as u16
}

fn wrapped_scroll_limit(paragraph: &Paragraph<'_>, inner: Rect) -> usize {
    paragraph
        .line_count(inner.width)
        .saturating_sub(usize::from(inner.height))
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
    fn cockpit_preserves_blocked_input_and_enqueues_only_the_submitted_task() {
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

        let prompt = "Проверка проекта\nKeep the change small.";
        app.paste(prompt);
        assert_eq!(app.screen, Screen::Main);
        assert_eq!(database.list_tasks().expect("list tasks").len(), 1);
        let mut terminal =
            Terminal::new(ratatui::backend::TestBackend::new(90, 25)).expect("terminal");
        terminal
            .draw(|frame| ui(frame, &mut app))
            .expect("render Cyrillic input");

        std::fs::write(project.root.join("local-edit.txt"), "user change").expect("dirty project");
        assert!(
            app.handle_submit()
                .unwrap_err()
                .to_string()
                .contains("uncommitted")
        );
        assert_eq!(app.input, prompt);
        assert!(app.draft.is_none());
        assert_eq!(database.list_tasks().expect("list tasks").len(), 1);
        std::fs::remove_file(project.root.join("local-edit.txt")).expect("clean test project");

        app.handle_submit().expect("submit with one Enter");
        assert_eq!(app.screen, Screen::Main);
        assert_eq!(app.active_tab, ActiveTab::Work);
        assert!(app.input.is_empty());
        let queued = database.list_tasks().expect("list tasks");
        assert_eq!(queued.len(), 2);
        let submitted = queued
            .iter()
            .find(|task| task.id != old.id)
            .expect("new task");
        assert_eq!(submitted.acceptance_criteria, prompt);
        assert_eq!(app.task_view, Some(submitted.id));
        assert_eq!(
            database
                .get_task(old.id)
                .expect("query original")
                .expect("original")
                .status,
            TaskStatus::Queued
        );
        // Unit tests run under the test harness executable, which cannot run a TALOS task.
        if let Some((_, mut child)) = app.child.take() {
            child.wait().expect("reap test harness child");
        }
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
        assert!(app.history.iter().any(|entry| entry.task.id == old.id));

        app.config.github = Some(crate::config::GithubPolicy {
            repository: "fixture/example".into(),
            remote: "origin".into(),
            base_branch: "main".into(),
            feature_branch_prefix: "talos/".into(),
            allow_push: true,
            allow_pull_request: true,
        });
        app.config
            .save(&project)
            .expect("save publication settings");
        app.paste("Prepare a draft PR");
        app.handle_submit()
            .expect("request publication confirmation");
        assert_eq!(app.screen, Screen::Main);
        assert_eq!(app.active_tab, ActiveTab::Work);
        assert_eq!(app.publication_target(), Some("fixture/example"));
        assert!(app.draft.is_some());
        assert!(app.child.is_none());
        assert_eq!(database.list_tasks().expect("list tasks").len(), 2);
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
            .expect("cancel confirmation");
        assert!(app.draft.is_none());
        assert_eq!(app.input, "Prepare a draft PR");
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

    fn buffer_text(buffer: &ratatui::buffer::Buffer) -> String {
        buffer
            .content()
            .chunks(usize::from(buffer.area.width))
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn narrow_cockpit_shows_wrapped_input_and_keeps_controls_visible() {
        let project = test_tui_project();
        let mut app = TuiApp::with_adapters(
            project.clone(),
            vec![AdapterKind::Codex, AdapterKind::Claude],
        )
        .expect("open cockpit");
        app.config.writer = AdapterKind::Codex;
        app.config.critic = AdapterKind::Claude;
        app.screen = Screen::Main;
        app.paste(&format!("{}\nФИНАЛ", "Длинный русский текст ".repeat(9)));
        let mut terminal =
            Terminal::new(ratatui::backend::TestBackend::new(48, 24)).expect("terminal");
        terminal
            .draw(|frame| ui(frame, &mut app))
            .expect("draw cockpit");
        let buffer = buffer_text(terminal.backend().buffer());
        assert!(buffer.contains("ФИНАЛ"), "{buffer}");
        assert!(
            buffer.contains("Esc") && buffer.contains("Clear"),
            "{buffer}"
        );
        app.handle_key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE))
            .expect("move to beginning");
        terminal
            .draw(|frame| ui(frame, &mut app))
            .expect("draw beginning");
        assert!(buffer_text(terminal.backend().buffer()).contains("Длинный русский текст"));
        std::fs::remove_dir_all(&project.root).expect("remove test project");
    }

    #[test]
    fn live_feed_wraps_to_its_pane_and_follows_the_last_wrapped_row() {
        let project = test_tui_project();
        let mut app = TuiApp::with_adapters(project.clone(), Vec::new()).expect("open cockpit");
        app.screen = Screen::Main;
        app.supervisor_logs = vec![format!(
            "FIRST-MARKER {} LAST-MARKER",
            "The Live Feed entry wraps inside its own panel. ".repeat(30)
        )];
        let mut terminal =
            Terminal::new(ratatui::backend::TestBackend::new(108, 26)).expect("terminal");
        terminal
            .draw(|frame| ui(frame, &mut app))
            .expect("draw work");
        let work = buffer_text(terminal.backend().buffer());
        assert!(!work.contains("LAST-MARKER"), "{work}");
        app.select_tab(ActiveTab::Logs);
        for width in [70, 108] {
            let mut terminal =
                Terminal::new(ratatui::backend::TestBackend::new(width, 26)).expect("terminal");
            terminal
                .draw(|frame| ui(frame, &mut app))
                .expect("draw live feed");
            assert!(buffer_text(terminal.backend().buffer()).contains("LAST-MARKER"));
            app.handle_key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE))
                .expect("go to beginning");
            terminal
                .draw(|frame| ui(frame, &mut app))
                .expect("draw feed beginning");
            assert!(buffer_text(terminal.backend().buffer()).contains("FIRST-MARKER"));
            app.handle_key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE))
                .expect("follow live feed");
            terminal
                .draw(|frame| ui(frame, &mut app))
                .expect("draw feed end");
            assert!(buffer_text(terminal.backend().buffer()).contains("LAST-MARKER"));
        }
        std::fs::remove_dir_all(&project.root).expect("remove test project");
    }

    #[test]
    fn wrapped_work_preview_and_diff_scroll_to_the_end_and_back() {
        let project = test_tui_project();
        let mut app = TuiApp::with_adapters(project.clone(), Vec::new()).expect("open cockpit");
        let content = format!("{} КОНЕЦ", "Длинный текст для проверки ".repeat(48));
        app.prepare_draft("Check wrap".into(), content.clone());
        let mut terminal =
            Terminal::new(ratatui::backend::TestBackend::new(48, 24)).expect("terminal");
        for tab in [ActiveTab::Work, ActiveTab::Diff] {
            app.screen = Screen::Main;
            app.select_tab(tab);
            app.diff_lines = vec![content.clone()];
            app.handle_key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE))
                .expect("scroll to end");
            terminal
                .draw(|frame| ui(frame, &mut app))
                .expect("draw end");
            let buffer = buffer_text(terminal.backend().buffer());
            assert!(buffer.contains("КОНЕЦ"), "{tab:?}\n{buffer}");
            assert!(app.scroll_offset < usize::MAX);
            app.handle_key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE))
                .expect("scroll to start");
            terminal
                .draw(|frame| ui(frame, &mut app))
                .expect("draw start");
            assert!(!buffer_text(terminal.backend().buffer()).contains("КОНЕЦ"));
        }
        std::fs::remove_dir_all(&project.root).expect("remove test project");
    }

    #[test]
    fn input_wraps_on_terminal_cells_and_keeps_graphemes_intact() {
        let input = "Ж界e\u{301}\nкрай";
        let (lines, cursor) = wrap_input(input, "Ж界".len(), 3);
        let text: Vec<_> = lines.iter().map(ToString::to_string).collect();
        assert_eq!(text, ["Ж界", "e\u{301}", "кра", "й"]);
        assert_eq!(cursor, (1, 0));
        assert_eq!(wrap_input("abc", 3, 3).1, (1, 0));
    }

    #[test]
    fn active_tab_navigation_and_titles() {
        assert_eq!(ActiveTab::Work.title(), "1 Work");
        assert_eq!(ActiveTab::Work.next(), ActiveTab::Dialogue);
        assert_eq!(ActiveTab::Dialogue.next(), ActiveTab::Diff);
        assert_eq!(ActiveTab::Diff.next(), ActiveTab::Logs);
        assert_eq!(ActiveTab::Logs.next(), ActiveTab::History);
        assert_eq!(ActiveTab::History.next(), ActiveTab::Work);
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
        assert_eq!(app.active_tab, ActiveTab::Work);

        app.active_tab = ActiveTab::Logs;
        app.execute_slash_command("/фид").expect("execute /фид");
        assert_eq!(app.active_tab, ActiveTab::Work);

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
            assert_eq!(app.active_tab, ActiveTab::Work);
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

    #[test]
    fn tui_input_history_navigation_with_arrows() {
        let project = test_tui_project();
        let mut app = TuiApp::with_adapters(project.clone(), Vec::new()).expect("create TuiApp");
        app.screen = Screen::Main;

        // 1. Submit multiple distinct inputs
        app.input = "/feed".to_string();
        app.handle_submit().expect("submit first");

        app.input = "/diff".to_string();
        app.handle_submit().expect("submit second");

        // Consecutive duplicate is ignored
        app.input = "/diff".to_string();
        app.handle_submit().expect("submit duplicate");

        assert_eq!(app.input_history.len(), 2);
        assert_eq!(app.input_history[0], "/feed");
        assert_eq!(app.input_history[1], "/diff");

        // 2. Start typing an unsaved draft
        app.input = "draft in progress".to_string();
        app.cursor = app.input.len();

        // 3. Up arrow recalls previous input (/diff)
        app.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))
            .expect("press Up");
        assert_eq!(app.input, "/diff");
        assert_eq!(app.history_index, Some(1));
        assert_eq!(app.saved_draft, "draft in progress");

        // 4. Up arrow recalls earlier input (/feed)
        app.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))
            .expect("press Up again");
        assert_eq!(app.input, "/feed");
        assert_eq!(app.history_index, Some(0));

        // 5. Up at top remains at top
        app.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))
            .expect("press Up at top");
        assert_eq!(app.input, "/feed");
        assert_eq!(app.history_index, Some(0));

        // 6. Down arrow advances forward to "/diff"
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))
            .expect("press Down");
        assert_eq!(app.input, "/diff");
        assert_eq!(app.history_index, Some(1));

        // 7. Down arrow at bottom restores the original unsaved draft
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))
            .expect("press Down to bottom");
        assert_eq!(app.input, "draft in progress");
        assert_eq!(app.history_index, None);

        let _ = std::fs::remove_dir_all(&project.root);
    }

    #[test]
    fn interactive_slash_command_palette_filtering_and_selection() {
        let project = test_tui_project();
        let mut app = TuiApp::with_adapters(project.clone(), Vec::new()).expect("create TuiApp");
        app.screen = Screen::Main;

        // 1. Initially no palette
        assert!(!app.is_palette_active());
        assert!(app.palette_matches().is_empty());

        // 2. Typing '/' activates palette with all commands
        app.input = "/".to_string();
        app.cursor = 1;
        assert!(app.is_palette_active());
        let all_matches = app.palette_matches();
        assert_eq!(all_matches.len(), PALETTE_COMMANDS.len());

        // 3. Typing '/m' filters to /mode
        app.input = "/m".to_string();
        app.cursor = 2;
        assert!(app.is_palette_active());
        let m_matches = app.palette_matches();
        assert!(m_matches.iter().any(|item| item.cmd == "/mode"));

        // 4. Arrow navigation updates palette_index
        assert_eq!(app.palette_index, 0);
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))
            .expect("palette Down");
        // If there's only 1 match (/mode), clamped to 0; if multiple, advances
        let expected_idx = if m_matches.len() > 1 { 1 } else { 0 };
        assert_eq!(app.palette_index, expected_idx);

        // 5. Tab on filtered match autocompletes the command with trailing space
        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))
            .expect("palette Tab");
        assert_eq!(app.input, format!("{} ", m_matches[expected_idx].cmd));
        assert_eq!(app.palette_index, 0);
        assert!(!app.is_palette_active()); // space terminates palette

        // 6. Typing '/task:t' filters to /task:test and Enter selects it
        app.input = "/task:t".to_string();
        app.cursor = app.input.len();
        assert!(app.is_palette_active());
        app.handle_submit().expect("submit palette selection");
        assert_eq!(app.input, "/task:test ");

        let _ = std::fs::remove_dir_all(&project.root);
    }

    #[test]
    fn mode_slash_command_supports_adaptive_and_aliases() {
        let project = test_tui_project();
        let mut app = TuiApp::with_adapters(project.clone(), Vec::new()).expect("create TuiApp");

        // Default mode is Adaptive
        assert_eq!(app.config.orchestration.mode, OrchestrationMode::Adaptive);

        // /mode team
        app.execute_slash_command("/mode team").expect("set team");
        assert_eq!(app.config.orchestration.mode, OrchestrationMode::Team);

        // /mode pair
        app.execute_slash_command("/mode pair").expect("set pair");
        assert_eq!(app.config.orchestration.mode, OrchestrationMode::Pair);

        // /mode adaptive
        app.execute_slash_command("/mode adaptive")
            .expect("set adaptive");
        assert_eq!(app.config.orchestration.mode, OrchestrationMode::Adaptive);

        // Russian alias: /mode авто
        app.execute_slash_command("/mode авто").expect("set auto");
        assert_eq!(app.config.orchestration.mode, OrchestrationMode::Adaptive);

        // Russian alias: /mode команда
        app.execute_slash_command("/mode команда")
            .expect("set team ru");
        assert_eq!(app.config.orchestration.mode, OrchestrationMode::Team);

        let _ = std::fs::remove_dir_all(&project.root);
    }

    #[test]
    fn plan_view_navigation_and_shortcuts() {
        let project = test_tui_project();
        let mut app = TuiApp::with_adapters(project.clone(), Vec::new()).expect("create TuiApp");
        app.screen = Screen::Main;

        // 1. Initial screen is Main
        assert_eq!(app.screen, Screen::Main);

        // 2. Open plan via method or 'p' key in Work tab
        app.active_tab = ActiveTab::Work;
        app.handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE))
            .expect("open plan with p");
        assert_eq!(app.screen, Screen::Plan);

        // 3. Plan content generation is non-empty
        let (body, actions) = app.build_plan_view_content();
        assert!(!body.is_empty());
        assert!(actions.contains("Esc"));

        // 4. Esc returns to Main
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
            .expect("close plan with Esc");
        assert_eq!(app.screen, Screen::Main);

        // 5. Slash command /plan opens plan
        app.execute_slash_command("/plan").expect("run /plan");
        assert_eq!(app.screen, Screen::Plan);

        // 6. Enter closes plan
        app.handle_submit().expect("submit in plan screen");
        assert_eq!(app.screen, Screen::Main);

        let _ = std::fs::remove_dir_all(&project.root);
    }

    #[test]
    fn clipboard_copy_view_and_shortcuts() {
        let project = test_tui_project();
        let mut app = TuiApp::with_adapters(project.clone(), Vec::new()).expect("create TuiApp");
        app.screen = Screen::Main;
        app.task_view = Some(1);
        app.task_title = "Implement feature X".to_string();

        // 1. Work view text extraction
        app.active_tab = ActiveTab::Work;
        let work_text = app.get_active_view_text();
        assert!(
            work_text.contains("Implement feature X")
                || work_text.contains("Task #1")
                || work_text.contains("Ready")
        );

        // 2. Copy active view
        app.copy_active_view_to_clipboard();
        assert!(
            app.status_message
                .as_ref()
                .is_some_and(|(msg, _)| { msg.contains("Copied") && msg.contains("clipboard") })
        );

        // 3. 'y' key in Work tab triggers copy
        app.handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE))
            .expect("copy with y");
        assert!(
            app.status_message
                .as_ref()
                .is_some_and(|(msg, _)| msg.contains("Copied"))
        );

        // 4. Ctrl+Y global copy works from any tab
        app.active_tab = ActiveTab::History;
        app.handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL))
            .expect("copy with Ctrl+Y");
        assert!(
            app.status_message
                .as_ref()
                .is_some_and(|(msg, _)| msg.contains("Copied"))
        );

        let _ = std::fs::remove_dir_all(&project.root);
    }

    #[test]
    fn goal_tracker_and_borderless_rendering() {
        let project = test_tui_project();
        let mut app = TuiApp::with_adapters(project.clone(), Vec::new()).expect("create TuiApp");
        app.screen = Screen::Main;
        app.task_view = Some(42);
        app.task_title = "Refactor cockpit UI".to_string();
        app.task_status = Some(TaskStatus::Implementing);

        assert!(app.has_active_or_recent_task());

        let mut terminal =
            Terminal::new(ratatui::backend::TestBackend::new(100, 30)).expect("terminal");
        terminal
            .draw(|frame| ui(frame, &mut app))
            .expect("render ui with goal tracker");

        let buffer = terminal.backend().buffer();
        let rendered_text = buffer
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<Vec<_>>()
            .join("");

        assert!(rendered_text.contains("GOAL"));
        assert!(rendered_text.contains("#42"));
        assert!(rendered_text.contains("Refactor cockpit UI"));
        assert!(rendered_text.contains("STAGE"));

        let _ = std::fs::remove_dir_all(&project.root);
    }

    #[test]
    fn continue_task_action_and_card_rendering() {
        let project = test_tui_project();
        let mut database = Database::open(&project).expect("open test database");
        let task = database
            .add_task("Fix parser bug", "Ensure valid ast generated")
            .expect("add task");
        let mut app = TuiApp::with_adapters(project.clone(), Vec::new()).expect("create TuiApp");
        app.screen = Screen::Main;
        app.refresh_state().expect("refresh state");

        // 1. Switch to History tab
        app.open_history(None).expect("open history");
        assert_eq!(app.history.len(), 1);

        // 2. Render History UI with Resume Card
        let mut terminal =
            Terminal::new(ratatui::backend::TestBackend::new(100, 30)).expect("terminal");
        terminal
            .draw(|frame| ui(frame, &mut app))
            .expect("render ui with history resume card");

        let buffer = terminal.backend().buffer();
        let rendered_text = buffer
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<Vec<_>>()
            .join("");

        assert!(rendered_text.contains("History"));
        assert!(rendered_text.contains("Task Resume Card"));
        assert!(rendered_text.contains("[c] Continue Task"));

        // 3. Slash command /continue executes and switches to Work
        app.execute_slash_command(&format!("/continue {}", task.id))
            .expect("run /continue");
        assert_eq!(app.active_tab, ActiveTab::Work);
        assert_eq!(app.task_view, Some(task.id));
        assert!(
            app.status_message
                .as_ref()
                .is_some_and(|(msg, _)| msg.contains("Continuing Task"))
        );

        let _ = std::fs::remove_dir_all(&project.root);
    }
}
