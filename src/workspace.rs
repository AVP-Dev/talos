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
    app::ensure_talos_ignored, db::Database, project::Project, secrets::redact_sensitive,
};

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
        "TALOS | Native Pair (manual)\n{task}\nChecks: {checks} | Queue: {} waiting / {} complete / {} blocked | {}\nNative CLI permissions apply; do not run a managed Writer concurrently.\n[c] Full cockpit | tmux prefix + arrows: switch / z: zoom / d: detach",
        queue.queued, queue.complete, queue.blocked_or_failed,
        if control.queue_paused { "paused" } else { "ready" },
    ))
}

fn display_line(value: &str) -> String {
    redact_sensitive(value)
        .chars()
        .filter(|ch| !ch.is_control())
        .take(120)
        .collect()
}

fn open_cockpit(project: &Project) -> Result<()> {
    let executable = env::current_exe()?;
    let pane = env::var("TMUX_PANE").context("The status pane is not inside tmux")?;
    ensure!(numeric_id(&pane, '%'), "Invalid tmux pane identity");
    let session_output = Command::new("tmux")
        .args(["display-message", "-p", "-t", &pane, "#{session_id}"])
        .output()?;
    ensure!(session_output.status.success(), "Cannot locate the workspace session");
    let session = String::from_utf8(session_output.stdout)?;
    let session = session.trim();
    ensure!(numeric_id(session, '$'), "Invalid tmux session identity");
    let windows = Command::new("tmux")
        .args(["list-windows", "-t", session, "-F", "#{window_id} #{window_name}"])
        .output()?;
    ensure!(windows.status.success(), "Cannot inspect workspace windows");
    let windows = String::from_utf8(windows.stdout)?;
    if let Some(window) = existing_cockpit(&windows) {
        let target = format!("{session}:{window}");
        let status = Command::new("tmux")
            .args(["select-window", "-t", &target])
            .status()?;
        ensure!(status.success(), "Cannot select the full cockpit");
        return Ok(());
    }
    let target = format!("{session}:");
    let status = Command::new("tmux")
        .args(["new-window", "-t", &target, "-n", "talos-cockpit", "-c"])
        .arg(&project.root)
        .arg(executable)
        .arg("tui")
        .status()?;
    ensure!(status.success(), "Cannot open the full cockpit");
    Ok(())
}

fn numeric_id(value: &str, prefix: char) -> bool {
    value.strip_prefix(prefix).is_some_and(|digits| {
        !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
    })
}

fn existing_cockpit(windows: &str) -> Option<&str> {
    windows.lines().find_map(|line| {
        let (id, name) = line.split_once(' ')?;
        (name == "talos-cockpit" && numeric_id(id, '@')).then_some(id)
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
                && key.code == KeyCode::Char('c')
            {
                notice = open_cockpit(&project).err().map(|_| "Cannot open cockpit".to_owned());
            }
        }
    })();
    let _ = execute!(io::stdout(), LeaveAlternateScreen, crossterm::cursor::Show);
    let _ = disable_raw_mode();
    result
}

#[cfg(test)]
mod tests {
    use super::{display_line, existing_cockpit, numeric_id};

    #[test]
    fn status_text_removes_control_characters_and_bounds_title() {
        assert_eq!(display_line("hello\nworld\u{1b}"), "helloworld");
        assert_eq!(display_line(&"a".repeat(500)).len(), 120);
    }

    #[test]
    fn cockpit_reuse_requires_exact_name_and_valid_window_id() {
        assert_eq!(existing_cockpit("@1 pair\n@2 talos-cockpit\n"), Some("@2"));
        assert_eq!(existing_cockpit("@2 talos-cockpit-old\n@abc talos-cockpit"), None);
        assert!(numeric_id("$1", '$'));
        assert!(!numeric_id("$1:other", '$'));
    }
}
