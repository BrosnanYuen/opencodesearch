use anyhow::{Context, Result};
use opencodesearch::config::AppConfig;
use opencodesearch::indexing::IndexingRuntime;
use opencodesearch::mcp::OpenCodeSearchMcpServer;
use opencodesearch::orchestrator::Orchestrator;
use opencodesearch::watchdog::WatchdogProcess;
use std::path::PathBuf;
use tokio::time::{Duration, sleep};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    // Parse command form: `opencodesearch <role> --config <path>`.
    let args = std::env::args().collect::<Vec<_>>();
    let role = args.get(1).map(String::as_str).unwrap_or("orchestrator");
    let config_path = parse_config_path(&args)?;

    match role {
        "orchestrator" => {
            let orchestrator = Orchestrator::new(config_path);
            orchestrator.run().await
        }
        "ingestor" => run_ingestor(config_path).await,
        "mcp" => run_mcp_server(config_path).await,
        "watchdog" => run_watchdog(config_path).await,
        other => anyhow::bail!(
            "unknown role '{}'. expected orchestrator|ingestor|mcp|watchdog",
            other
        ),
    }
}

fn parse_config_path(args: &[String]) -> Result<PathBuf> {
    // Support explicit flag and fallback to local config.json.
    for idx in 0..args.len() {
        if args[idx] == "--config" {
            if let Some(path) = args.get(idx + 1) {
                return Ok(PathBuf::from(path));
            }
            anyhow::bail!("--config provided without path");
        }
    }

    Ok(PathBuf::from("config.json"))
}

async fn run_ingestor(config_path: PathBuf) -> Result<()> {
    let config = AppConfig::from_path(&config_path)?;
    let runtime = IndexingRuntime::from_config(config)?;

    // Run initial full indexing then keep process alive for orchestrator supervision.
    runtime.index_entire_codebase().await?;

    loop {
        sleep(Duration::from_secs(3600)).await;
    }
}

async fn run_mcp_server(config_path: PathBuf) -> Result<()> {
    let config = AppConfig::from_path(&config_path)?;
    let runtime = IndexingRuntime::from_config(config)?;
    let server = OpenCodeSearchMcpServer::new(runtime);
    server.run_stdio().await
}

async fn run_watchdog(config_path: PathBuf) -> Result<()> {
    let config = AppConfig::from_path(&config_path)?;
    let runtime = IndexingRuntime::from_config(config)?;

    let ipc_path = std::env::var("OPENCODESEARCH_IPC_SOCKET")
        .context("OPENCODESEARCH_IPC_SOCKET is required for watchdog process")?;

    let watchdog = WatchdogProcess::new(runtime, PathBuf::from(ipc_path));
    watchdog.run().await
}
