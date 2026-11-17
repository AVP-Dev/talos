use clap::Parser;
use tracing_subscriber::EnvFilter;

use talos::{cli::Cli, run};

#[tokio::main]
async fn main() {
    let filter = EnvFilter::builder()
        .with_default_directive(tracing::Level::WARN.into())
        .from_env_lossy();
    let _ = tracing_subscriber::fmt().with_env_filter(filter).try_init();

    if let Err(error) = run(Cli::parse()).await {
        eprintln!("Error: {error:#}");
        std::process::exit(1);
    }
}
