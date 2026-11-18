use std::{
    fs,
    io::Write,
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

use crate::{
    db::{Database, TaskStatus, TaskWorktreeRecord, TaskWorktreeState},
    project::Project,
    revision::{RevisionSnapshot, clean_base_commit, git_head, git_worktree_is_clean},
};

pub fn create(
    project: &Project,
    database: &mut Database,
    task_id: i64,
) -> Result<TaskWorktreeRecord> {
    let task = database
        .get_task(task_id)?
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

pub fn list(database: &Database, after: i64, limit: usize) -> Result<crate::db::TaskWorktreePage> {
    database.task_worktrees_page(after, limit)
}

pub fn execution_project(project: &Project, database: &Database, task_id: i64) -> Result<Project> {
    if let Some(integration) = database.task_worktree_integration(task_id)? {
        return match integration.state.as_str() {
            "pending" | "integrated" => Ok(project.clone()),
            state => bail!(
                "Task #{task_id} worktree integration is {state}; run `talos task worktree reconcile {task_id}` and inspect the result"
            ),
        };
    }
    let Some(record) = database.task_worktree(task_id)? else {
        return Ok(project.clone());
    };
    if record.state == TaskWorktreeState::Active
        && record.writer_routed_at_ms.is_none()
        && database.latest_writer_attempt(task_id)?.is_some()
    {
        return Ok(project.clone());
    }
    if record.state == TaskWorktreeState::Retired
        && database.task_worktree_integration(task_id)?.is_none()
    {
        return Ok(project.clone());
    }
    if record.state != TaskWorktreeState::Active {
        bail!(
            "Task #{task_id} managed worktree is {}; reconcile or inspect it before agent execution",
            record.state.as_str()
        );
    }
    let worktree_path = Path::new(&record.worktree_path);
    let (managed_root, expected_path, expected_branch) = managed_identity(&project.root, task_id)?;
    if expected_path != worktree_path || expected_branch != record.branch_name {
        bail!("Recorded task worktree does not match its project-scoped managed path");
    }
    ensure_path_is_within(worktree_path, &managed_root)?;
    verify_worktree(
        project,
        worktree_path,
        &record.branch_name,
        Some(&record.base_commit),
    )?;
    project.execution_at(worktree_path)
}

pub fn reconcile(
    project: &Project,
    database: &mut Database,
    task_id: i64,
) -> Result<TaskWorktreeRecord> {
    let record = database
        .task_worktree(task_id)?
        .with_context(|| format!("Task #{task_id} has no worktree record"))?;
    if let Some(integration) = database.task_worktree_integration(task_id)? {
        if integration.state == "applying" {
            let primary_head = git_head(&project.root)?;
            let current = RevisionSnapshot::capture(&project.root, &integration.base_commit);
            let exact_and_based = current
                .as_ref()
                .is_ok_and(|snapshot| integration_matches(&integration, snapshot))
                && primary_head.as_deref().is_some_and(|head| {
                    ensure_commit_contains_base(&project.root, &integration.base_commit, head)
                        .is_ok()
                });
            if exact_and_based {
                database.set_task_worktree_integration_state(
                    task_id,
                    "pending",
                    "Reconciliation found the exact recorded task patch in the primary project",
                )?;
            } else if primary_head.as_deref() == Some(integration.base_commit.as_str())
                && clean_base_commit(&project.root)
                    .is_ok_and(|head| head == integration.base_commit)
            {
                let worktree_path = Path::new(&record.worktree_path);
                verify_worktree(
                    project,
                    worktree_path,
                    &record.branch_name,
                    Some(&record.base_commit),
                )?;
                let source = RevisionSnapshot::capture(worktree_path, &integration.base_commit)?;
                if source.head_commit == integration.source_head_commit
                    && source.revision_id == integration.revision_id
                    && source.patch_sha256 == integration.patch_sha256
                    && source.changed_paths == integration.changed_paths
                    && git_apply(&project.root, &source.patch, true).is_ok()
                    && git_apply(&project.root, &source.patch, false).is_ok()
                {
                    let applied =
                        RevisionSnapshot::capture(&project.root, &integration.base_commit)?;
                    if integration_matches(&integration, &applied) {
                        database.set_task_worktree_integration_state(
                            task_id,
                            "pending",
                            "Reconciliation safely reapplied the exact recorded patch to the unchanged primary base",
                        )?;
                    } else {
                        database.set_task_worktree_integration_state(
                            task_id,
                            "needs_attention",
                            "Reconciliation applied a patch but the resulting primary revision differs; inspect both worktrees",
                        )?;
                    }
                } else {
                    database.set_task_worktree_integration_state(
                        task_id,
                        "needs_attention",
                        "Integration intent remains but the source patch could not be safely reapplied; inspect both worktrees",
                    )?;
                }
            } else {
                database.set_task_worktree_integration_state(
                    task_id,
                    "needs_attention",
                    "Primary project differs from both the recorded base and task patch; no files were changed",
                )?;
            }
        } else if integration.state == "needs_attention" {
            let primary_head = git_head(&project.root)?;
            if let Ok(current) = RevisionSnapshot::capture(&project.root, &integration.base_commit)
                && integration_matches(&integration, &current)
                && primary_head.as_deref().is_some_and(|head| {
                    ensure_commit_contains_base(&project.root, &integration.base_commit, head)
                        .is_ok()
                })
            {
                database.set_task_worktree_integration_state(
                    task_id,
                    "pending",
                    "Reconciliation found the exact recorded patch after operator inspection",
                )?;
            }
        }
    }
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
            match verify_worktree(
                project,
                path,
                &record.branch_name,
                Some(&record.base_commit),
            ) {
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
    let task = database
        .get_task(task_id)?
        .with_context(|| format!("Task #{task_id} does not exist"))?;
    if task.status != TaskStatus::Queued
        || database.latest_writer_attempt(task_id)?.is_some()
        || database.task_worktree_integration(task_id)?.is_some()
    {
        bail!("Only an unused queued task worktree with no integration history can be removed");
    }
    let path = Path::new(&record.worktree_path);
    let (managed_root, expected_path, expected_branch) = managed_identity(&project.root, task_id)?;
    if expected_path != path || expected_branch != record.branch_name {
        bail!("Recorded task worktree does not match its project-scoped managed path");
    }
    ensure_path_is_within(path, &managed_root)?;
    verify_worktree(
        project,
        path,
        &record.branch_name,
        Some(&record.base_commit),
    )?;

    if !git_worktree_is_clean(path, true)? {
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

pub fn integrate(
    project: &Project,
    database: &mut Database,
    task_id: i64,
) -> Result<crate::db::TaskWorktreeIntegrationRecord> {
    let worktree = database
        .task_worktree(task_id)?
        .with_context(|| format!("Task #{task_id} has no managed worktree"))?;
    if worktree.state != TaskWorktreeState::Active {
        bail!(
            "Task #{task_id} worktree is {}, not active",
            worktree.state.as_str()
        );
    }
    if database.task_worktree_integration(task_id)?.is_some() {
        bail!("Task #{task_id} already has an integration record; reconcile it before continuing");
    }
    let worktree_path = Path::new(&worktree.worktree_path);
    let (managed_root, expected_path, expected_branch) = managed_identity(&project.root, task_id)?;
    if expected_path != worktree_path || expected_branch != worktree.branch_name {
        bail!("Recorded task worktree does not match its project-scoped managed path");
    }
    ensure_path_is_within(worktree_path, &managed_root)?;
    verify_worktree(
        project,
        worktree_path,
        &worktree.branch_name,
        Some(&worktree.base_commit),
    )?;

    let primary_base = clean_base_commit(&project.root)?;
    if primary_base != worktree.base_commit {
        bail!(
            "Primary project HEAD changed after this task worktree was created; recreate or inspect the task worktree"
        );
    }
    let snapshot = RevisionSnapshot::capture(worktree_path, &worktree.base_commit)?;
    if snapshot.changed_paths.is_empty() {
        bail!("Task #{task_id} worktree has no changes to integrate");
    }
    if let Some(contract) = database.child_task_contract(task_id)?
        && let Some(path) = snapshot
            .changed_paths
            .iter()
            .find(|path| !contract.permits_path(path))
    {
        bail!("Task #{task_id} changed path {path:?} outside its child-task contract");
    }

    git_apply(&project.root, &snapshot.patch, true).context(
        "Git could not validate the task worktree patch against the clean primary project",
    )?;
    let paths_json = serde_json::to_string(&snapshot.changed_paths)?;
    database.begin_task_worktree_integration(
        task_id,
        &snapshot.base_commit,
        &snapshot.head_commit,
        &snapshot.revision_id,
        &snapshot.patch_sha256,
        &paths_json,
    )?;

    if let Err(error) = git_apply(&project.root, &snapshot.patch, false) {
        database.set_task_worktree_integration_state(
            task_id,
            "needs_attention",
            "Git patch application failed after the durable integration intent was recorded; inspect both worktrees",
        )?;
        return Err(error).context(
            "Task worktree integration needs operator inspection; no rollback was attempted",
        );
    }
    let applied = RevisionSnapshot::capture(&project.root, &snapshot.base_commit);
    match applied {
        Ok(applied)
            if applied.revision_id == snapshot.revision_id
                && applied.patch_sha256 == snapshot.patch_sha256
                && applied.changed_paths == snapshot.changed_paths => {}
        _ => {
            database.set_task_worktree_integration_state(
                task_id,
                "needs_attention",
                "Primary project revision after patch application differs from the recorded task patch",
            )?;
            bail!(
                "Task worktree integration needs operator inspection; TALOS left all files intact"
            );
        }
    }
    database.set_task_worktree_integration_state(
        task_id,
        "pending",
        "The exact task worktree patch was applied to the primary project; Critic review and checks are still required",
    )
}

pub fn finalize(
    project: &Project,
    database: &mut Database,
    task_id: i64,
) -> Result<crate::db::TaskWorktreeIntegrationRecord> {
    let worktree = database
        .task_worktree(task_id)?
        .with_context(|| format!("Task #{task_id} has no managed worktree"))?;
    if worktree.state != TaskWorktreeState::Active {
        bail!(
            "Task #{task_id} worktree is {}, not active",
            worktree.state.as_str()
        );
    }
    let integration = database
        .task_worktree_integration(task_id)?
        .with_context(|| format!("Task #{task_id} has no worktree integration record"))?;
    if integration.state != "pending" {
        bail!(
            "Task #{task_id} integration is {}, not pending",
            integration.state
        );
    }

    let worktree_path = Path::new(&worktree.worktree_path);
    verify_worktree(
        project,
        worktree_path,
        &worktree.branch_name,
        Some(&integration.source_head_commit),
    )?;
    let source = RevisionSnapshot::capture(worktree_path, &integration.base_commit)?;
    if source.head_commit != integration.source_head_commit
        || source.revision_id != integration.revision_id
        || source.patch_sha256 != integration.patch_sha256
        || source.changed_paths != integration.changed_paths
    {
        bail!(
            "Task #{task_id} source worktree changed after integration; inspect it before finalization"
        );
    }

    let integration_commit = clean_base_commit(&project.root)?;
    ensure_commit_contains_base(&project.root, &integration.base_commit, &integration_commit)?;
    let current = RevisionSnapshot::capture(&project.root, &integration.base_commit)?;
    if current.head_commit != integration_commit {
        bail!("Primary project HEAD changed while finalization facts were being captured");
    }
    let review = database
        .latest_critic_review(task_id)?
        .with_context(|| format!("Task #{task_id} has no Critic review"))?;
    let paths_json = serde_json::to_string(&current.changed_paths)?;
    if review.decision != "approve"
        || review.base_revision != integration.base_commit
        || review.revision_id != current.revision_id
        || review.patch_sha256.as_deref() != Some(current.patch_sha256.as_str())
        || review.changed_paths_json.as_deref() != Some(paths_json.as_str())
    {
        bail!(
            "Task #{task_id} latest Critic approval does not match the committed primary project revision"
        );
    }
    let verification = database
        .latest_verification_summary(task_id)?
        .with_context(|| format!("Task #{task_id} has no verification evidence"))?;
    if verification.run.status != "passed"
        || verification.run.critic_review_id != review.id
        || verification.base_revision != integration.base_commit
        || verification.run.revision_id != current.revision_id
        || verification.patch_sha256 != current.patch_sha256
        || verification.changed_paths_json != paths_json
        || verification.run.started_at_ms < integration.updated_at_ms
    {
        bail!(
            "Task #{task_id} has no passing post-integration checks for the latest approved revision"
        );
    }

    database.complete_task_worktree_integration(task_id, &integration_commit)
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

fn git_apply(project_root: &Path, patch: &str, check_only: bool) -> Result<()> {
    let mut command = Command::new("git");
    command.arg("apply").arg("--index");
    if check_only {
        command.arg("--check");
    }
    command
        .current_dir(project_root)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    for (key, _) in std::env::vars_os().filter(|(key, _)| key.to_string_lossy().starts_with("GIT_"))
    {
        command.env_remove(key);
    }
    let mut child = command
        .spawn()
        .context("Cannot start Git patch application")?;
    let write_result = child
        .stdin
        .take()
        .context("Git patch input pipe is unavailable")
        .and_then(|mut input| {
            input
                .write_all(patch.as_bytes())
                .context("Cannot send the task patch to Git")
        });
    let output = child
        .wait_with_output()
        .context("Cannot wait for Git patch application")?;
    write_result?;
    if !output.status.success() {
        bail!(
            "Git patch application failed with exit code {}",
            output.status.code().unwrap_or(-1)
        );
    }
    Ok(())
}

fn ensure_commit_contains_base(
    project_root: &Path,
    base_commit: &str,
    head_commit: &str,
) -> Result<()> {
    match git_output_allow_failure(
        project_root,
        &["merge-base", "--is-ancestor", base_commit, head_commit],
    )?
    .status
    .code()
    {
        Some(0) => Ok(()),
        Some(1) => bail!("Primary project commit does not descend from the task worktree base"),
        _ => bail!("Git could not verify ancestry of the task worktree base"),
    }
}

fn integration_matches(
    integration: &crate::db::TaskWorktreeIntegrationRecord,
    snapshot: &RevisionSnapshot,
) -> bool {
    integration.base_commit == snapshot.base_commit
        && integration.revision_id == snapshot.revision_id
        && integration.patch_sha256 == snapshot.patch_sha256
        && integration.changed_paths == snapshot.changed_paths
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
