use std::{
    path::{Component, Path},
    process::{Command, Output},
};

use anyhow::{Context, Result, bail};
use serde::Serialize;
use sha2::{Digest, Sha256};

const MAX_REVIEW_PATCH_BYTES: usize = 64 * 1024;
const PROJECT_PATHS: [&str; 3] = [".", ":(exclude).git", ":(exclude).talos"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RevisionSnapshot {
    pub base_commit: String,
    pub head_commit: String,
    pub revision_id: String,
    pub patch_sha256: String,
    pub changed_paths: Vec<String>,
    pub patch: String,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct RevisionFacts {
    pub base_commit: String,
    pub head_commit: String,
    pub revision_id: String,
    pub patch_sha256: String,
    pub changed_paths: Vec<String>,
}

/// Capture a Git base revision for a new Writer task. Existing changes are rejected so that
/// the later Critic review can attribute the complete diff to TALOS's task lifecycle.
pub fn clean_base_commit(project_root: &Path) -> Result<String> {
    let status = git_output(
        project_root,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    )?;
    if !status.stdout.is_empty() {
        bail!(
            "A new Writer task must start from a clean Git worktree so its review has an exact base revision"
        );
    }
    git_head(project_root)?.context("Critic review requires a Git project with at least one commit")
}

pub fn git_head(project_root: &Path) -> Result<Option<String>> {
    let output =
        git_output_allow_failure(project_root, &["rev-parse", "--verify", "HEAD^{commit}"])?;
    if !output.status.success() {
        return Ok(None);
    }
    let value = String::from_utf8(output.stdout).context("Git returned a non-UTF-8 commit ID")?;
    let commit = value.trim();
    validate_commit_id(commit)?;
    Ok(Some(commit.to_owned()))
}

impl RevisionSnapshot {
    pub fn capture(project_root: &Path, base_commit: &str) -> Result<Self> {
        validate_commit_id(base_commit)?;

        let mut patch = git_output(
            project_root,
            &[
                "diff",
                "--binary",
                "--no-ext-diff",
                "--no-renames",
                base_commit,
                "--",
                PROJECT_PATHS[0],
                PROJECT_PATHS[1],
                PROJECT_PATHS[2],
            ],
        )?
        .stdout;
        if patch.len() > MAX_REVIEW_PATCH_BYTES {
            bail!(
                "Review patch exceeds the {MAX_REVIEW_PATCH_BYTES}-byte limit; TALOS will not truncate it"
            );
        }
        let tracked_paths = git_output(
            project_root,
            &[
                "diff",
                "--name-only",
                "-z",
                "--no-ext-diff",
                "--no-renames",
                base_commit,
                "--",
                PROJECT_PATHS[0],
                PROJECT_PATHS[1],
                PROJECT_PATHS[2],
            ],
        )?
        .stdout;
        let mut changed_paths = decode_paths(&tracked_paths)?;

        let untracked = git_output(
            project_root,
            &[
                "ls-files",
                "--others",
                "--exclude-standard",
                "-z",
                "--",
                PROJECT_PATHS[0],
                PROJECT_PATHS[1],
                PROJECT_PATHS[2],
            ],
        )?
        .stdout;
        let mut untracked_paths = decode_paths(&untracked)?;
        untracked_paths.sort();
        for path in untracked_paths {
            validate_relative_path(&path)?;
            let output = git_output_allow_failure(
                project_root,
                &[
                    "diff",
                    "--no-index",
                    "--binary",
                    "--no-ext-diff",
                    "--",
                    "/dev/null",
                    &path,
                ],
            )?;
            match output.status.code() {
                Some(1) => {}
                Some(0) => continue,
                _ => bail!("Git could not produce a review patch for untracked path {path:?}"),
            }
            append_patch(&mut patch, &output.stdout)?;
            changed_paths.push(path);
        }

        changed_paths.sort();
        changed_paths.dedup();
        let patch = String::from_utf8(patch)
            .context("The Git patch is not valid UTF-8 and cannot be sent to the Critic")?;
        let head_commit = git_head(project_root)?
            .context("Critic revision facts require a committed Git HEAD")?;
        let mut digest = Sha256::new();
        digest.update(base_commit.as_bytes());
        digest.update([0]);
        digest.update(patch.as_bytes());
        let revision_id = format!("sha256:{}", lower_hex(&digest.finalize()));
        let patch_sha256 = format!("sha256:{}", lower_hex(&Sha256::digest(patch.as_bytes())));

        Ok(Self {
            base_commit: base_commit.to_owned(),
            head_commit,
            revision_id,
            patch_sha256,
            changed_paths,
            patch,
        })
    }

    pub fn facts(&self) -> RevisionFacts {
        RevisionFacts {
            base_commit: self.base_commit.clone(),
            head_commit: self.head_commit.clone(),
            revision_id: self.revision_id.clone(),
            patch_sha256: self.patch_sha256.clone(),
            changed_paths: self.changed_paths.clone(),
        }
    }
}

fn append_patch(target: &mut Vec<u8>, addition: &[u8]) -> Result<()> {
    let new_length = target
        .len()
        .checked_add(addition.len())
        .context("Review patch size overflowed")?;
    if new_length > MAX_REVIEW_PATCH_BYTES {
        bail!(
            "Review patch exceeds the {MAX_REVIEW_PATCH_BYTES}-byte limit; TALOS will not truncate it"
        );
    }
    target.extend_from_slice(addition);
    Ok(())
}

fn decode_paths(bytes: &[u8]) -> Result<Vec<String>> {
    bytes
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| {
            let path = String::from_utf8(path.to_vec())
                .context("Git returned a non-UTF-8 path that cannot be reviewed safely")?;
            validate_relative_path(&path)?;
            Ok(path)
        })
        .collect()
}

fn validate_relative_path(value: &str) -> Result<()> {
    let path = Path::new(value);
    if value.is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
        || value.starts_with(".talos/")
        || value == ".talos"
        || value.starts_with(".git/")
        || value == ".git"
    {
        bail!("Review path is not a safe project-relative path: {value:?}");
    }
    Ok(())
}

fn validate_commit_id(value: &str) -> Result<()> {
    if !matches!(value.len(), 40 | 64)
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!("Git base revision must be a full hexadecimal commit ID");
    }
    Ok(())
}

fn git_output(project_root: &Path, arguments: &[&str]) -> Result<Output> {
    let output = git_output_allow_failure(project_root, arguments)?;
    if !output.status.success() {
        bail!(
            "Git command failed while preparing a Critic revision (exit code {})",
            output.status.code().unwrap_or(-1)
        );
    }
    Ok(output)
}

fn git_output_allow_failure(project_root: &Path, arguments: &[&str]) -> Result<Output> {
    let mut command = Command::new("git");
    command
        .args(arguments)
        .current_dir(project_root)
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

fn lower_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::PathBuf,
        process::Command,
        sync::atomic::{AtomicU64, Ordering},
    };

    use super::{RevisionSnapshot, clean_base_commit};

    static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn captures_tracked_and_untracked_files_and_binds_revision_to_base() {
        let root = temp_git_project();
        fs::write(root.join("tracked.txt"), "base\n").expect("write tracked baseline");
        git(&root, &["add", "tracked.txt"]);
        git(
            &root,
            &[
                "-c",
                "user.name=TALOS",
                "-c",
                "user.email=talos@example.invalid",
                "commit",
                "-m",
                "base",
            ],
        );
        let base = clean_base_commit(&root).expect("capture clean base");
        fs::write(root.join("tracked.txt"), "changed\n").expect("change tracked file");
        fs::write(root.join("new.txt"), "new file\n").expect("create untracked file");

        let snapshot = RevisionSnapshot::capture(&root, &base).expect("capture complete diff");
        assert_eq!(snapshot.changed_paths, ["new.txt", "tracked.txt"]);
        assert_eq!(snapshot.head_commit, base);
        assert!(snapshot.patch_sha256.starts_with("sha256:"));
        assert!(snapshot.patch.contains("tracked.txt"));
        assert!(snapshot.patch.contains("new.txt"));
        assert!(snapshot.patch.contains("new file"));
        assert!(snapshot.revision_id.starts_with("sha256:"));
        assert_eq!(
            snapshot,
            RevisionSnapshot::capture(&root, &base).expect("repeat capture")
        );

        fs::write(root.join("new.txt"), "changed again\n").expect("change untracked file");
        let changed = RevisionSnapshot::capture(&root, &base).expect("capture changed revision");
        assert_ne!(snapshot.revision_id, changed.revision_id);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn clean_base_refuses_preexisting_changes_and_uncommitted_repository() {
        let root = temp_git_project();
        let error = clean_base_commit(&root).expect_err("repository has no base commit");
        assert!(format!("{error:#}").contains("at least one commit"));

        fs::write(root.join("baseline.txt"), "base\n").expect("write file");
        git(&root, &["add", "baseline.txt"]);
        git(
            &root,
            &[
                "-c",
                "user.name=TALOS",
                "-c",
                "user.email=talos@example.invalid",
                "commit",
                "-m",
                "base",
            ],
        );
        fs::write(root.join("baseline.txt"), "dirty\n").expect("dirty file");
        let error = clean_base_commit(&root).expect_err("dirty worktree must be refused");
        assert!(format!("{error:#}").contains("clean Git worktree"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn refuses_a_tracked_patch_over_the_review_budget_without_truncating() {
        let root = temp_git_project();
        fs::write(root.join("tracked.txt"), "base\n").expect("write tracked baseline");
        git(&root, &["add", "tracked.txt"]);
        git(
            &root,
            &[
                "-c",
                "user.name=TALOS",
                "-c",
                "user.email=talos@example.invalid",
                "commit",
                "-m",
                "base",
            ],
        );
        let base = clean_base_commit(&root).expect("capture clean base");
        fs::write(root.join("tracked.txt"), "x".repeat(70 * 1024)).expect("write oversized diff");

        let error = RevisionSnapshot::capture(&root, &base)
            .expect_err("oversized tracked patch must be refused");
        assert!(format!("{error:#}").contains("will not truncate"));
        let _ = fs::remove_dir_all(root);
    }

    fn temp_git_project() -> PathBuf {
        let id = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("talos-revision-test-{}-{id}", std::process::id()));
        fs::create_dir_all(&path).expect("create temp project");
        git(&path, &["init", "-q"]);
        path
    }

    fn git(root: &PathBuf, arguments: &[&str]) {
        let status = Command::new("git")
            .args(arguments)
            .current_dir(root)
            .status()
            .expect("run git");
        assert!(status.success(), "git {arguments:?} failed");
    }
}
