pub mod agy;
pub mod agy_process;
pub mod claude;
pub mod codex;
pub mod kilo;
pub mod omp;
pub mod opencode;
pub mod pi;

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
