use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail};
use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiscoveryMethod {
    Git,
    ExistingTalosDirectory,
    ProjectMarker(String),
    CurrentDirectory,
}

#[derive(Clone, Debug, Serialize)]
pub struct Project {
    pub root: PathBuf,
    pub state_root: PathBuf,
    pub discovery: DiscoveryMethod,
}

impl Project {
    pub fn discover(start: &Path) -> Result<Self> {
        let start = start
            .canonicalize()
            .with_context(|| format!("Cannot resolve working directory {}", start.display()))?;

        if let Some(root) = git_root(&start)? {
            return Ok(Self {
                state_root: root.clone(),
                root,
                discovery: DiscoveryMethod::Git,
            });
        }

        for ancestor in start.ancestors() {
            let talos_dir = ancestor.join(".talos");
            if talos_dir.is_dir() {
                return Ok(Self {
                    root: ancestor.to_path_buf(),
                    state_root: ancestor.to_path_buf(),
                    discovery: DiscoveryMethod::ExistingTalosDirectory,
                });
            }
        }

        const MARKERS: &[&str] = &[
            "Cargo.toml",
            "package.json",
            "pyproject.toml",
            "go.mod",
            "pom.xml",
            "build.gradle",
            "composer.json",
        ];

        for ancestor in start.ancestors() {
            if let Some(marker) = MARKERS
                .iter()
                .find(|marker| ancestor.join(marker).is_file())
            {
                return Ok(Self {
                    root: ancestor.to_path_buf(),
                    state_root: ancestor.to_path_buf(),
                    discovery: DiscoveryMethod::ProjectMarker((*marker).to_owned()),
                });
            }
        }

        Ok(Self {
            state_root: start.clone(),
            root: start,
            discovery: DiscoveryMethod::CurrentDirectory,
        })
    }

    pub fn execution_at(&self, root: &Path) -> Result<Self> {
        let root = root
            .canonicalize()
            .with_context(|| format!("Cannot resolve execution worktree {}", root.display()))?;
        Ok(Self {
            root,
            state_root: self.state_root.clone(),
            discovery: self.discovery.clone(),
        })
    }

    pub fn primary_project(&self) -> Self {
        Self {
            root: self.state_root.clone(),
            state_root: self.state_root.clone(),
            discovery: self.discovery.clone(),
        }
    }

    pub fn runtime_dir(&self) -> PathBuf {
        self.state_root.join(".talos")
    }

    pub fn database_path(&self) -> PathBuf {
        self.runtime_dir().join("state.sqlite3")
    }

    pub fn ensure_runtime_dir(&self) -> Result<()> {
        let path = self.runtime_dir();
        match fs::symlink_metadata(&path) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.is_dir() {
                    bail!("Runtime path {} must be a real directory", path.display());
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                create_private_dir(&path)?;
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("Cannot inspect runtime directory {}", path.display())
                });
            }
        }
        secure_private_dir(&path)?;
        Ok(())
    }
}

fn git_root(start: &Path) -> Result<Option<PathBuf>> {
    let mut has_git_marker = false;
    for ancestor in start.ancestors() {
        match fs::symlink_metadata(ancestor.join(".git")) {
            Ok(_) => {
                has_git_marker = true;
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("Cannot inspect Git metadata under {}", ancestor.display())
                });
            }
        }
    }
    if !has_git_marker {
        return Ok(None);
    }

    let mut command = Command::new("git");
    command
        .current_dir(start)
        .args(["rev-parse", "--show-toplevel"])
        .env("GIT_TERMINAL_PROMPT", "0");

    for (key, _) in std::env::vars_os().filter(|(key, _)| key.to_string_lossy().starts_with("GIT_"))
    {
        command.env_remove(key);
    }
    command.env("GIT_OPTIONAL_LOCKS", "0");

    let output = command
        .output()
        .context("Git metadata exists, but TALOS could not run the Git CLI")?;
    if !output.status.success() {
        bail!(
            "Git metadata exists, but Git could not resolve the project root (exit code {})",
            output.status.code().unwrap_or(-1)
        );
    }

    let path = String::from_utf8(output.stdout).context("Git returned a non-UTF-8 project path")?;
    let path = PathBuf::from(path.trim());
    let path = path
        .canonicalize()
        .with_context(|| format!("Cannot resolve Git project root {}", path.display()))?;
    Ok(Some(path))
}

#[cfg(unix)]
fn create_private_dir(path: &Path) -> Result<()> {
    use std::os::unix::fs::DirBuilderExt;

    let mut builder = fs::DirBuilder::new();
    builder.mode(0o700);
    match builder.create(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let metadata = fs::symlink_metadata(path)
                .with_context(|| format!("Cannot inspect runtime directory {}", path.display()))?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                bail!("Runtime path {} must be a real directory", path.display());
            }
            Ok(())
        }
        Err(error) => Err(error)
            .with_context(|| format!("Cannot create runtime directory {}", path.display())),
    }
}

#[cfg(unix)]
fn secure_private_dir(path: &Path) -> Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("Cannot inspect runtime directory {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!("Runtime path {} must be a real directory", path.display());
    }
    let effective_user = unsafe { libc::geteuid() };
    if metadata.uid() != effective_user {
        bail!(
            "Runtime directory {} is not owned by the current user",
            path.display()
        );
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .with_context(|| format!("Cannot restrict runtime directory {}", path.display()))?;
    Ok(())
}

#[cfg(not(unix))]
fn secure_private_dir(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(not(unix))]
fn create_private_dir(path: &Path) -> Result<()> {
    match fs::create_dir(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let metadata = fs::symlink_metadata(path)
                .with_context(|| format!("Cannot inspect runtime directory {}", path.display()))?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                bail!("Runtime path {} must be a real directory", path.display());
            }
            Ok(())
        }
        Err(error) => Err(error)
            .with_context(|| format!("Cannot create runtime directory {}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };

    use super::{DiscoveryMethod, Project};

    static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

    fn temp_dir() -> PathBuf {
        let sequence = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "talos-project-test-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("create temporary directory");
        path
    }

    #[test]
    fn nested_directory_resolves_existing_talos_root() {
        let root = temp_dir();
        let nested = root.join("src").join("module");
        fs::create_dir_all(root.join(".talos")).expect("create runtime directory");
        fs::create_dir_all(&nested).expect("create nested directory");

        let project = Project::discover(&nested).expect("discover project");

        assert_eq!(
            project.root,
            root.canonicalize().expect("canonicalize test root")
        );
        assert!(matches!(
            project.discovery,
            DiscoveryMethod::ExistingTalosDirectory
        ));
        fs::remove_dir_all(project.root).expect("remove temporary directory");
    }

    #[test]
    fn invalid_git_metadata_does_not_fall_back_to_another_root() {
        let root = temp_dir();
        fs::write(root.join(".git"), "invalid Git metadata").expect("write invalid marker");

        let error = Project::discover(&root).expect_err("invalid Git metadata should be reported");

        assert!(format!("{error:#}").contains("Git metadata exists"));
        fs::remove_dir_all(root).expect("remove temporary directory");
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_runtime_directory_is_rejected() {
        use std::os::unix::fs::symlink;

        let root = temp_dir();
        let external = root.join("external-runtime");
        fs::create_dir(&external).expect("create external runtime target");
        symlink(&external, root.join(".talos")).expect("create runtime symlink");
        let project = Project {
            root: root.clone(),
            state_root: root.clone(),
            discovery: DiscoveryMethod::CurrentDirectory,
        };

        let error = project
            .ensure_runtime_dir()
            .expect_err("runtime symlink should be rejected");
        assert!(format!("{error:#}").contains("must be a real directory"));
        assert!(external.is_dir());
        fs::remove_dir_all(root).expect("remove temporary directory");
    }

    #[cfg(unix)]
    #[test]
    fn runtime_directory_is_private() {
        use std::os::unix::fs::PermissionsExt;

        let root = temp_dir();
        let project = Project {
            root: root.clone(),
            state_root: root.clone(),
            discovery: DiscoveryMethod::CurrentDirectory,
        };
        project
            .ensure_runtime_dir()
            .expect("create runtime directory");
        let mode = fs::metadata(project.runtime_dir())
            .expect("read runtime directory metadata")
            .permissions()
            .mode()
            & 0o777;

        assert_eq!(mode, 0o700);
        fs::remove_dir_all(root).expect("remove temporary directory");
    }
}
