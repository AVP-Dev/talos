//! Compact status view for native, manually operated tmux panes.
use std::{env, io, process::Command, time::Duration};

use anyhow::{Context, Result, ensure};
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend, widgets::Paragraph};

use crate::{
    adapter::AdapterId,
    app::ensure_talos_ignored,
    config::{OrchestrationMode, ProjectConfig},
    db::Database,
    project::Project,
    secrets::redact_sensitive,
    tmux::{NativeLayout, NativeRole, TmuxClient, native_executable},
};

pub(crate) fn restart_role(project: &Project, role: NativeRole) -> Result<()> {
    ensure_talos_ignored(project)?;
    let database = Database::open(project)?;
    ensure!(
        database.queue_summary()?.active == 0,
        "Finish or reconcile active managed tasks first"
    );
    let config = ProjectConfig::load(project)?;
    TmuxClient::default().restart_native_role(&project.root, role, &config)
}

fn status_text(database: &Database) -> Result<String> {
    let queue = database.queue_summary()?;
    let control = database.project_control_state()?;
    let active = database.active_tasks()?;
    let task = active
        .first()
        .map(|task| {
            let title = display_line(&task.title);
            format!("#{} {} | {}", task.id, title, task.status.as_str())
        })
        .unwrap_or_else(|| "No managed task running".to_owned());
    let checks = match active.first() {
        Some(task) => database
            .latest_verification_summary(task.id)?
            .map(|summary| format!("{} ({} recorded)", summary.run.status, summary.check_count))
            .unwrap_or_else(|| "not run".to_owned()),
        None => "—".to_owned(),
    };
    Ok(format!(
        "TALOS | Native Pair (manual)\n{task}\nChecks: {checks} | Queue: {} waiting / {} complete / {} blocked | {}\nNative CLI permissions apply; do not run a managed Writer concurrently.\n[F2] Settings [c] Cockpit [1] Columns [2] Stacked | tmux prefix + arrows / z / d",
        queue.queued,
        queue.complete,
        queue.blocked_or_failed,
        if control.queue_paused {
            "paused"
        } else {
            "ready"
        },
    ))
}

fn display_line(value: &str) -> String {
    redact_sensitive(value)
        .chars()
        .filter(|ch| !ch.is_control())
        .take(120)
        .collect()
}

fn open_window(project: &Project, name: &str, subcommand: &str) -> Result<()> {
    let executable = env::current_exe()?;
    let pane = env::var("TMUX_PANE").context("The status pane is not inside tmux")?;
    ensure!(numeric_id(&pane, '%'), "Invalid tmux pane identity");
    let session_output = Command::new("tmux")
        .args(["display-message", "-p", "-t", &pane, "#{session_id}"])
        .output()?;
    ensure!(
        session_output.status.success(),
        "Cannot locate the workspace session"
    );
    let session = String::from_utf8(session_output.stdout)?;
    let session = session.trim();
    ensure!(numeric_id(session, '$'), "Invalid tmux session identity");
    let windows = Command::new("tmux")
        .args([
            "list-windows",
            "-t",
            session,
            "-F",
            "#{window_id} #{window_name}",
        ])
        .output()?;
    ensure!(windows.status.success(), "Cannot inspect workspace windows");
    let windows = String::from_utf8(windows.stdout)?;
    if let Some(window) = existing_window(&windows, name) {
        let target = format!("{session}:{window}");
        let status = Command::new("tmux")
            .args(["select-window", "-t", &target])
            .status()?;
        ensure!(status.success(), "Cannot select the full cockpit");
        return Ok(());
    }
    let target = format!("{session}:");
    let status = Command::new("tmux")
        .args(["new-window", "-t", &target, "-n", name, "-c"])
        .arg(&project.root)
        .arg(executable)
        .arg(subcommand)
        .status()?;
    ensure!(status.success(), "Cannot open the full cockpit");
    Ok(())
}

fn numeric_id(value: &str, prefix: char) -> bool {
    value.strip_prefix(prefix).is_some_and(|digits| {
        !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
    })
}

fn existing_window<'a>(windows: &'a str, expected: &str) -> Option<&'a str> {
    windows.lines().find_map(|line| {
        let (id, name) = line.split_once(' ')?;
        (name == expected && numeric_id(id, '@')).then_some(id)
    })
}

pub(crate) fn run_status(project: Project) -> Result<()> {
    ensure_talos_ignored(&project)?;
    let database = Database::open(&project)?;
    enable_raw_mode()?;
    let result = (|| -> Result<()> {
        execute!(io::stdout(), EnterAlternateScreen)?;
        let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
        let mut notice: Option<String> = None;
        loop {
            let mut text = status_text(&database)
                .unwrap_or_else(|_| "TALOS | Status temporarily unavailable".to_owned());
            if let Some(message) = &notice {
                text.push('\n');
                text.push_str(message);
            }
            terminal.draw(|frame| {
                frame.render_widget(Paragraph::new(text), frame.area());
            })?;
            if event::poll(Duration::from_secs(1))?
                && let Event::Key(key) = event::read()?
                && key.kind == KeyEventKind::Press
            {
                let result = match key.code {
                    KeyCode::Char('c') => open_window(&project, "talos-cockpit", "tui"),
                    KeyCode::F(2) => open_window(&project, "talos-settings", "settings"),
                    KeyCode::Char('1') => TmuxClient::default()
                        .set_native_layout(&project.root, NativeLayout::Columns),
                    KeyCode::Char('2') => TmuxClient::default()
                        .set_native_layout(&project.root, NativeLayout::Stacked),
                    _ => continue,
                };
                notice = result.err().map(|error| redact_sensitive(&error.to_string()));
            }
        }
    })();
    let _ = execute!(io::stdout(), LeaveAlternateScreen, crossterm::cursor::Show);
    let _ = disable_raw_mode();
    result
}

const SETTING_COUNT: usize = 5;

fn installed_adapters() -> Vec<AdapterId> {
    AdapterId::ALL
        .into_iter()
        .filter(|adapter| native_executable(*adapter).is_ok())
        .collect()
}

fn cycle_adapter(current: AdapterId, available: &[AdapterId], next: bool) -> AdapterId {
    if available.is_empty() {
        return current;
    }
    let offset = if next { 1 } else { available.len() - 1 };
    available
        .iter()
        .position(|adapter| *adapter == current)
        .map_or(available[0], |index| {
            available[(index + offset) % available.len()]
        })
}

fn change_setting(config: &mut ProjectConfig, available: &[AdapterId], row: usize, next: bool) {
    match row {
        0 => {
            let adapter = cycle_adapter(config.writer, available, next);
            if config.writer != adapter {
                config.writer = adapter;
                config.writer_model = None;
            }
        }
        1 => {
            let adapter = cycle_adapter(config.critic, available, next);
            if config.critic != adapter {
                config.critic = adapter;
                config.critic_model = None;
            }
        }
        2 => {
            let modes = [
                OrchestrationMode::Adaptive,
                OrchestrationMode::Pair,
                OrchestrationMode::Team,
            ];
            let index = modes
                .iter()
                .position(|mode| *mode == config.orchestration.mode)
                .unwrap_or(0);
            config.orchestration.mode = modes[(index + if next { 1 } else { 2 }) % modes.len()];
        }
        3 => {
            let rounds = config.orchestration.max_review_rounds;
            config.orchestration.max_review_rounds = (rounds + if next { 0 } else { 3 }) % 5 + 1;
        }
        4 => {
            let workers = config.max_concurrent_writers;
            let workers = (workers + if next { 0 } else { 1 }) % 3 + 1;
            config.max_concurrent_writers = workers;
            config.orchestration.max_writer_workers = workers;
        }
        _ => {}
    }
}

pub(crate) fn run_settings(project: &Project) -> Result<bool> {
    use ratatui::widgets::Wrap;

    ensure_talos_ignored(project)?;
    let mut config = ProjectConfig::load(project)?;
    let mut available = installed_adapters();
    if !crate::config::config_path(&project.state_root).exists() && !available.is_empty() {
        config.writer = available[0];
        config.critic = available.get(1).copied().unwrap_or(available[0]);
    }
    let mut row = 0;
    let mut notice = String::new();
    let mut pending_restart = None;
    enable_raw_mode()?;
    let result = (|| -> Result<bool> {
        execute!(io::stdout(), EnterAlternateScreen)?;
        let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
        loop {
            let rows = [
                format!("Writer CLI: {}", config.writer.as_str()),
                format!("Critic CLI: {}", config.critic.as_str()),
                format!("Managed mode: {}", config.orchestration.mode.as_str()),
                format!(
                    "Managed review rounds: {}",
                    config.orchestration.max_review_rounds
                ),
                format!("Managed writer limit: {}", config.max_concurrent_writers),
            ];
            let discovered = available
                .iter()
                .map(|adapter| adapter.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            let mut body =
                format!("TALOS Settings\n\nAgents / Workspace\nInstalled: {discovered}\n");
            for (index, value) in rows.iter().enumerate() {
                if index == 2 {
                    body.push_str("\nManaged workflow\n");
                }
                body.push_str(&format!(
                    "{} {value}\n",
                    if row == index { ">" } else { " " }
                ));
            }
            body.push_str(&format!(
                "\nProject checks: {} configured | Git publication: {}\n",
                config.checks.len(),
                if config.github.is_some() {
                    "configured"
                } else {
                    "disabled"
                }
            ));
            body.push_str("Models, providers, reasoning and login are configured in the original CLIs.\nEnter saves only; X saves and replaces the selected role after confirmation.\nExit its native CLI before replacing it. Other panes are preserved.\n\nArrows: select/change | R: rescan | Enter: save | X: replace role | Esc: cancel\n");
            body.push_str(&notice);
            terminal.draw(|frame| {
                frame.render_widget(
                    Paragraph::new(body).wrap(Wrap { trim: false }),
                    frame.area(),
                )
            })?;
            if let Event::Key(key) = event::read()?
                && key.kind == KeyEventKind::Press
            {
                if let Some(role) = pending_restart.take() {
                    if matches!(key.code, KeyCode::Char('y' | 'Y')) {
                        config.save(project)?;
                        notice = match restart_role(project, role) {
                            Ok(()) => "Selected native pane restarted.".to_owned(),
                            Err(error) => format!(
                                "Settings saved; pane restart failed: {}",
                                redact_sensitive(&error.to_string())
                            ),
                        };
                    } else {
                        notice = "Pane replacement cancelled.".to_owned();
                    }
                    continue;
                }
                match key.code {
                    KeyCode::Up => row = (row + SETTING_COUNT - 1) % SETTING_COUNT,
                    KeyCode::Down => row = (row + 1) % SETTING_COUNT,
                    KeyCode::Left | KeyCode::Right => {
                        change_setting(&mut config, &available, row, key.code == KeyCode::Right)
                    }
                    KeyCode::Char('r' | 'R') => available = installed_adapters(),
                    KeyCode::Char('x' | 'X') if row < 2 => {
                        if available.contains(&config.writer) && available.contains(&config.critic)
                        {
                            pending_restart = Some(if row == 0 {
                                NativeRole::Writer
                            } else {
                                NativeRole::Critic
                            });
                            notice = "Exit the selected CLI first. Replace its process and terminal contents? Y confirms; any other key cancels.".to_owned();
                        } else {
                            notice =
                                "Select installed CLI roles before replacing a pane.".to_owned();
                        }
                    }
                    KeyCode::Esc => return Ok(false),
                    KeyCode::Enter => {
                        if !available.contains(&config.writer)
                            || !available.contains(&config.critic)
                        {
                            notice = "Select two installed CLI roles before saving.".to_owned();
                        } else {
                            config.save(project)?;
                            return Ok(true);
                        }
                    }
                    _ => {}
                }
            }
        }
    })();
    let _ = execute!(io::stdout(), LeaveAlternateScreen, crossterm::cursor::Show);
    let _ = disable_raw_mode();
    result
}

#[cfg(test)]
mod tests {
    use super::{change_setting, display_line, existing_window, numeric_id};
    use crate::{adapter::AdapterId, config::ProjectConfig};

    #[test]
    fn status_text_removes_control_characters_and_bounds_title() {
        assert_eq!(display_line("hello\nworld\u{1b}"), "helloworld");
        assert_eq!(display_line(&"a".repeat(500)).len(), 120);
    }

    #[test]
    fn cockpit_reuse_requires_exact_name_and_valid_window_id() {
        assert_eq!(
            existing_window("@1 pair\n@2 talos-cockpit\n", "talos-cockpit"),
            Some("@2")
        );
        assert_eq!(
            existing_window("@2 talos-cockpit-old\n@abc talos-cockpit", "talos-cockpit"),
            None
        );
        assert!(numeric_id("$1", '$'));
        assert!(!numeric_id("$1:other", '$'));
    }

    #[test]
    fn native_settings_cycle_only_installed_adapters_and_bounded_limits() {
        let mut config = ProjectConfig::default();
        let available = [AdapterId::Codex, AdapterId::Claude];
        change_setting(&mut config, &available, 0, true);
        assert_eq!(config.writer, AdapterId::Codex);
        change_setting(&mut config, &available, 0, true);
        assert_eq!(config.writer, AdapterId::Claude);
        change_setting(&mut config, &available, 0, false);
        assert_eq!(config.writer, AdapterId::Codex);
        for _ in 0..10 {
            change_setting(&mut config, &available, 3, true);
            change_setting(&mut config, &available, 4, false);
            assert!((1..=5).contains(&config.orchestration.max_review_rounds));
            assert!((1..=3).contains(&config.max_concurrent_writers));
        }
    }
}
