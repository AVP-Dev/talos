pub mod agy;
pub mod agy_process;
pub mod claude;
pub mod codex;
pub mod kilo;
pub mod omp;
pub mod opencode;
pub mod pi;

pub(crate) fn resolve_installed_cli(
    name: &str,
    override_variable: &str,
    home_directories: &[&str],
) -> Option<std::path::PathBuf> {
    use std::{
        env,
        os::unix::fs::PermissionsExt,
        path::{Path, PathBuf},
    };

    let executable = |candidate: PathBuf| {
        let metadata = candidate.metadata().ok()?;
        if metadata.is_file() && metadata.permissions().mode() & 0o111 != 0 {
            candidate.canonicalize().ok()
        } else {
            None
        }
    };
    if let Some(candidate) =
        env::var_os(override_variable).and_then(|path| executable(PathBuf::from(path)))
    {
        return Some(candidate);
    }
    if let Some(candidate) = env::var_os("PATH").and_then(|paths| {
        env::split_paths(&paths)
            .filter(|dir| dir.is_absolute())
            .find_map(|dir| executable(dir.join(name)))
    }) {
        return Some(candidate);
    }
    if let Some(candidate) = env::var_os("HOME").and_then(|home| {
        let home = PathBuf::from(home);
        home_directories
            .iter()
            .find_map(|dir| executable(home.join(dir).join(name)))
    }) {
        return Some(candidate);
    }
    ["/opt/homebrew/bin", "/usr/local/bin"]
        .into_iter()
        .find_map(|dir| executable(Path::new(dir).join(name)))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AdapterId {
    Agy,
    Codex,
    Omp,
    Pi,
    Kilo,
    OpenCode,
    Claude,
}

impl AdapterId {
    pub const ALL: [Self; 7] = [
        Self::Agy,
        Self::Codex,
        Self::Omp,
        Self::Pi,
        Self::Kilo,
        Self::OpenCode,
        Self::Claude,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Agy => "agy",
            Self::Codex => "codex",
            Self::Omp => "omp",
            Self::Pi => "pi",
            Self::Kilo => "kilo",
            Self::OpenCode => "opencode",
            Self::Claude => "claude",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|id| id.as_str() == value)
    }

    pub fn is_implemented(self) -> bool {
        matches!(
            self,
            Self::Agy
                | Self::Codex
                | Self::Omp
                | Self::Pi
                | Self::Kilo
                | Self::OpenCode
                | Self::Claude
        )
    }
}
