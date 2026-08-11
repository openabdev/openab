//! Standalone control-plane binary.

use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;
use tracing::info;

use openab_cp::config::CpConfig;
use openab_cp::server::{app, run_sweeper, AppState};

#[derive(Parser)]
#[command(name = "openab-cp", about = "OpenAB Agent Control Plane")]
struct Cli {
    /// Path to the CP config file (TOML).
    #[arg(short, long, default_value = "cp.toml")]
    config: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let cli = Cli::parse();
    let cfg = CpConfig::load(&cli.config)?;
    let listen = cfg.listen.clone();
    if cfg.agents.is_empty() {
        tracing::warn!("no [[agents]] identities configured — every connection will be rejected");
    }
    info!(
        listen = %listen,
        identities = cfg.agents.len(),
        namespaces = cfg.namespaces.len(),
        "starting openab-cp"
    );

    let state = Arc::new(AppState::new(cfg));
    let sweeper = tokio::spawn(run_sweeper(state.clone()));

    let listener = tokio::net::TcpListener::bind(&listen)
        .await
        .with_context(|| format!("binding {listen}"))?;
    axum::serve(listener, app(state))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
            info!("shutdown signal received");
        })
        .await?;

    sweeper.abort();
    Ok(())
}
