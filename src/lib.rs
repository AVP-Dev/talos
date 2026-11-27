pub mod adapter;
pub mod app;
pub mod checks;
pub mod cli;
pub mod config;
pub mod db;
pub mod github;
pub mod policy;
pub mod process_group;
pub mod project;
pub mod rate_limit;
pub mod review;
pub mod revision;
pub mod secrets;
pub mod summary;
pub mod supervisor;
pub mod team;
pub mod tmux;
pub mod tui;
mod workspace;
pub mod worktree;

mod process_identity;

pub use app::run;
