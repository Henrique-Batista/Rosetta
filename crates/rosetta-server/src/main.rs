use std::sync::Arc;

use clap::Parser;
use tracing::info;

use rosetta_server::cli::Cli;
use rosetta_server::routes::{self, AppState};
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let cfg = Cli::parse().resolve();

    info!("Configured ACP agent: {} {:?}", cfg.acp_command, cfg.acp_args);
    info!("Working directory: {}", cfg.cwd);
    if !cfg.mcp_servers.is_empty() {
        info!("Loaded {} MCP server(s)", cfg.mcp_servers.len());
    }

    let state = Arc::new(AppState {
        acp_command: cfg.acp_command,
        acp_args: cfg.acp_args,
        cwd: cfg.cwd,
        mcp_servers: cfg.mcp_servers,
    });

    let app = routes::router(state);

    info!("Rosetta server listening on {}", cfg.listen);

    let listener = tokio::net::TcpListener::bind(cfg.listen).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
