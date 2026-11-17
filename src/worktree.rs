use std::{
    fs,
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Command, Output},
};

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

use crate::{
    db::{Database, TaskStatus, TaskWorktreeRecord, TaskWorktreeState},
    project::Project,
    revision::clean_base_commit,
};

pub fn create(
    project: &Project,
    database: &mut Database,
    task_id: i64,
) -> Result<TaskWorktreeRecord> {
    let task = database
        .list_tasks()?
        .into_iter()
        .find(|task| task.id == task_id)
        .with_context(|| format!("Task #{task_id} does not exist"))?;
    if task.status != TaskStatus::Queued {
        bail!("A task worktree can only be created for a queued task");
    }
    let base_commit = clean_base_commit(&project.root)?;
    let (managed_root, worktree_path, branch_name) = managed_identity(&project.root, task_id)?;
    if inspect_path(&worktree_path)? {
        bail!(
            "Task worktree path {} already exists; inspect it before changing anything",
            worktree_path.display()
        );
    }
    if branch_exists(&project.root, &branch_name)? {
        bail!("Task worktree branch {branch_name} already exists; inspect it before reusing it");
    }
    ensure_private_directory(&project.root, &managed_root)?;

    let path_text = worktree_path
        .to_str()
        .context("Worktree path is not valid UTF-8")?;
    database.begin_task_worktree(task_id, path_text, &branch_name, &base_commit)?;

    let add = git_output(
        &project.root,
        &[
            "worktree",
            "add",
            "--branch",
            &branch_name,
            path_text,
            &base_commit,
        ],
    );
    if let Err(error) = add {
        database.update_task_worktree_state(
            task_id,
            TaskWorktreeState::Failed,
            "Git could not create the requested task worktree; inspect the recorded path and branch",
        )?;
        return Err(error)
            .context("Task worktree creation failed; no automatic cleanup was attempted");
    }

    if let Err(error) = verify_worktree(project, &worktree_path, &branch_name, Some(&base_commit)) {
        database.update_task_worktree_state(
            task_id,
            TaskWorktreeState::NeedsAttention,
            "Git created a worktree that did not match its recorded identity; no files were removed",
        )?;
        return Err(error).context("Created worktree needs operator inspection");
    }
    database.update_task_worktree_state(
        task_id,
        TaskWorktreeState::Active,
        "Git created and TALOS verified the explicit task worktree",
    )
}

pub fn list(database: &Database) -> Result<Vec<TaskWorktreeRecord>> {
    database.task_worktrees()
}

pub fn reconcile(
    project: &Project,
    database: &mut Database,
    task_id: i64,
) -> Result<TaskWorktreeRecord> {
    let record = database
        .task_worktree(task_id)?
        .with_context(|| format!("Task #{task_id} has no worktree record"))?;
    match record.state {
        TaskWorktreeState::Creating => {
            let path = Path::new(&record.worktree_path);
            if !inspect_path(path)? {
                return database.update_task_worktree_state(
                    task_id,
                    TaskWorktreeState::Failed,
                    "Creation record has no worktree directory; no cleanup was needed",
                );
            }
            match verify_worktree(
                project,
                path,
                &record.branch_name,
                Some(&record.base_commit),
            ) {
                Ok(()) => database.update_task_worktree_state(
                    task_id,
                    TaskWorktreeState::Active,
                    "Reconciled a registered worktree after interrupted creation",
                ),
                Err(_) => database.update_task_worktree_state(
                    task_id,
                    TaskWorktreeState::NeedsAttention,
                    "Creation record and filesystem state differ; no files were changed",
                ),
            }
        }
        TaskWorktreeState::Active => {
            let path = Path::new(&record.worktree_path);
            match verify_worktree(project, path, &record.branch_name, None) {
                Ok(()) => Ok(record),
                Err(_) => database.update_task_worktree_state(
                    task_id,
                    TaskWorktreeState::NeedsAttention,
                    "Active worktree path or Git identity changed; no files were changed",
                ),
            }
        }
        _ => Ok(record),
    }
}

pub fn remove(
    project: &Project,
    database: &mut Database,
    task_id: i64,
) -> Result<TaskWorktreeRecord> {
    let record = database
        .task_worktree(task_id)?
        .with_context(|| format!("Task #{task_id} has no worktree record"))?;
    if record.state != TaskWorktreeState::Active {
        bail!(
            "Task #{task_id} worktree is {}, not active; reconcile or inspect it first",
            record.state.as_str()
        );
    }
    let path = Path::new(&record.worktree_path);
    let (managed_root, expected_path, expected_branch) = managed_identity(&project.root, task_id)?;
    if expected_path != path || expected_branch != record.branch_name {
        bail!("Recorded task worktree does not match its project-scoped managed path");
    }
    ensure_path_is_within(path, &managed_root)?;
    verify_worktree(project, path, &record.branch_name, None)?;

    let status = git_output(
        path,
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--ignored=matching",
        ],
    )?;
    if !status.stdout.is_empty() {
        bail!(
            "Task #{task_id} worktree contains changes or ignored files; TALOS left it intact at {}",
            path.display()
        );
    }

    let path_text = path.to_str().context("Worktree path is not valid UTF-8")?;
    git_output(&project.root, &["worktree", "remove", path_text])
        .context("Git refused to remove the clean worktree; TALOS did not force removal")?;
    database.update_task_worktree_state(
        task_id,
        TaskWorktreeState::Retired,
        "Operator explicitly removed the verified clean task worktree; its branch remains",
    )
}

fn managed_identity(project_root: &Path, task_id: i64) -> Result<(PathBuf, PathBuf, String)> {
    if task_id <= 0 {
        bail!("Task ID must be positive");
    }
    let project_root = project_root
        .canonicalize()
        .context("Cannot resolve project root for worktree management")?;
    let parent = project_root
        .parent()
        .context("Project root has no parent directory for managed worktrees")?;
    let root_hash = hex(&Sha256::digest(project_root.as_os_str().as_encoded_bytes()));
    let namespace = &root_hash[..16];
    let managed_root = parent.join(".talos-worktrees").join(namespace);
    let worktree_path = managed_root.join(format!("task-{task_id}"));
    let branch_name = format!("talos/task/{namespace}/{task_id}");
    Ok((managed_root, worktree_path, branch_name))
}

fn ensure_private_directory(project_root: &Path, managed_root: &Path) -> Result<()> {
    let parent = project_root
        .parent()
        .context("Project root has no parent directory")?;
    let worktree_parent = parent.join(".talos-worktrees");
    ensure_one_private_directory(&worktree_parent)?;
    ensure_one_private_directory(managed_root)?;
    ensure_path_is_within(managed_root, parent)?;
    Ok(())
}

fn ensure_one_private_directory(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                bail!(
                    "Managed worktree directory {} is not a real directory",
                    path.display()
                );
            }
            if metadata.permissions().mode() & 0o077 != 0 {
                bail!(
                    "Managed worktree directory {} is accessible to other users; inspect its permissions manually",
                    path.display()
                );
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::DirBuilder::new()
                .recursive(false)
                .mode(0o700)
                .create(path)
                .with_context(|| {
                    format!(
                        "Cannot create private worktree directory {}",
                        path.display()
                    )
                })?;
        }
        Err(error) => {
            return Err(error).with_context(|| format!("Cannot inspect {}", path.display()));
        }
    }
    Ok(())
}

fn ensure_path_is_within(path: &Path, parent: &Path) -> Result<()> {
    let parent = parent.canonicalize().with_context(|| {
        format!(
            "Cannot resolve managed worktree parent {}",
            parent.display()
        )
    })?;
    let path_parent = path
        .parent()
        .context("Managed worktree path has no parent")?
        .canonicalize()
        .with_context(|| {
            format!(
                "Cannot resolve managed worktree parent for {}",
                path.display()
            )
        })?;
    if !path_parent.starts_with(&parent) {
        bail!("Managed worktree path escapes its project-scoped storage directory");
    }
    Ok(())
}

fn inspect_path(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).with_context(|| format!("Cannot inspect {}", path.display())),
    }
}

fn branch_exists(project_root: &Path, branch_name: &str) -> Result<bool> {
    let output = git_output_allow_failure(
        project_root,
        &[
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch_name}"),
        ],
    )?;
    match output.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => bail!("Git could not check whether the managed task branch already exists"),
    }
}

fn verify_worktree(
    project: &Project,
    worktree_path: &Path,
    expected_branch: &str,
    expected_head: Option<&str>,
) -> Result<()> {
    let metadata = fs::symlink_metadata(worktree_path)
        .context("Task worktree path does not exist or cannot be inspected")?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!("Task worktree path is not a real directory");
    }
    let path = worktree_path
        .canonicalize()
        .context("Task worktree path does not exist")?;
    if path != worktree_path {
        bail!("Task worktree path no longer resolves to its recorded location");
    }
    let top_level = git_text(&path, &["rev-parse", "--show-toplevel"])?;
    let top_level = PathBuf::from(top_level);
    if top_level.canonicalize()? != path {
        bail!("Git worktree root does not match its recorded path");
    }
    let branch = git_text(&path, &["symbolic-ref", "--quiet", "--short", "HEAD"])?;
    if branch != expected_branch {
        bail!("Git worktree branch does not match its recorded branch");
    }
    let main_common_dir = git_common_dir(&project.root)?;
    let worktree_common_dir = git_common_dir(&path)?;
    if main_common_dir != worktree_common_dir {
        bail!("Task worktree is attached to a different Git repository");
    }
    if let Some(expected_head) = expected_head {
        let head = git_text(&path, &["rev-parse", "--verify", "HEAD^{commit}"])?;
        if head != expected_head {
            bail!("New task worktree does not start at the recorded base commit");
        }
    }
    Ok(())
}

fn git_common_dir(root: &Path) -> Result<PathBuf> {
    let value = git_text(root, &["rev-parse", "--git-common-dir"])?;
    let path = PathBuf::from(value);
    let path = if path.is_absolute() {
        path
    } else {
        root.join(path)
    };
    path.canonicalize()
        .context("Cannot resolve Git common directory")
}

fn git_text(root: &Path, arguments: &[&str]) -> Result<String> {
    let output = git_output(root, arguments)?;
    let value = String::from_utf8(output.stdout).context("Git returned non-UTF-8 output")?;
    Ok(value.trim().to_owned())
}

fn git_output(root: &Path, arguments: &[&str]) -> Result<Output> {
    let output = git_output_allow_failure(root, arguments)?;
    if !output.status.success() {
        bail!(
            "Git command {:?} failed with exit code {}",
            arguments.first().unwrap_or(&""),
            output.status.code().unwrap_or(-1)
        );
    }
    Ok(output)
}

fn git_output_allow_failure(root: &Path, arguments: &[&str]) -> Result<Output> {
    let mut command = Command::new("git");
    command
        .args(arguments)
        .current_dir(root)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0");
    for (key, _) in std::env::vars_os().filter(|(key, _)| key.to_string_lossy().starts_with("GIT_"))
    {
        command.env_remove(key);
    }
    command
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()
        .with_context(|| {
            format!(
                "Cannot run Git command {:?}",
                arguments.first().unwrap_or(&"")
            )
        })
}

fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
}
