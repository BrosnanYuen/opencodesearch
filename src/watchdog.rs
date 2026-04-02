use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use tokio::net::UnixDatagram;
use tokio::process::Command;
use tokio::time::{Duration, sleep};

use crate::indexing::IndexingRuntime;
use crate::types::UpdateSignal;

/// Watchdog process state for polling git and driving incremental updates.
pub struct WatchdogProcess {
    runtime: IndexingRuntime,
    ipc_socket_path: PathBuf,
    last_synced_commit: Option<String>,
}

impl WatchdogProcess {
    /// Build watchdog with indexing runtime and IPC socket target.
    pub fn new(runtime: IndexingRuntime, ipc_socket_path: PathBuf) -> Self {
        Self {
            runtime,
            ipc_socket_path,
            last_synced_commit: None,
        }
    }

    /// Main watchdog loop.
    pub async fn run(mut self) -> Result<()> {
        loop {
            let latest = self.current_head().await?;
            if self.last_synced_commit.is_none() {
                self.last_synced_commit = Some(latest);
                sleep(Duration::from_secs(10)).await;
                continue;
            }

            let previous = self
                .last_synced_commit
                .clone()
                .unwrap_or_else(|| latest.clone());

            let delta = self.count_new_commits(&previous).await?;
            if delta >= self.runtime.config.codebase.commit_threshold {
                self.send_signal(UpdateSignal::UpdateStart).await?;

                let changed = self.git_pull_and_changed_files(&previous).await?;
                let deleted_paths = changed
                    .iter()
                    .filter(|(_, status)| status == "D")
                    .map(|(path, _)| path.clone())
                    .collect::<Vec<_>>();

                if !deleted_paths.is_empty() {
                    self.runtime.delete_paths(&deleted_paths).await?;
                }

                let changed_existing = changed
                    .iter()
                    .filter(|(path, status)| {
                        (status == "A" || status == "M") && Path::new(path).exists()
                    })
                    .map(|(path, _)| PathBuf::from(path))
                    .collect::<Vec<_>>();

                if !changed_existing.is_empty() {
                    self.runtime.index_files(&changed_existing).await?;
                }

                self.last_synced_commit = Some(self.current_head().await?);
                self.send_signal(UpdateSignal::UpdateEnd).await?;
            }

            sleep(Duration::from_secs(10)).await;
        }
    }

    async fn send_signal(&self, signal: UpdateSignal) -> Result<()> {
        // Use an unbound datagram socket to send one-way control events.
        let client = UnixDatagram::unbound().context("failed creating unix datagram socket")?;
        client
            .send_to(signal.as_bytes(), &self.ipc_socket_path)
            .await
            .with_context(|| {
                format!(
                    "failed sending IPC signal to {}",
                    self.ipc_socket_path.display()
                )
            })?;
        Ok(())
    }

    async fn current_head(&self) -> Result<String> {
        let output = run_git(
            &self.runtime.config.codebase.directory_path,
            &["rev-parse", "HEAD"],
        )
        .await?;
        Ok(output.trim().to_string())
    }

    async fn count_new_commits(&self, from_commit: &str) -> Result<usize> {
        let range = format!("{}..HEAD", from_commit);
        let output = run_git(
            &self.runtime.config.codebase.directory_path,
            &["rev-list", "--count", &range],
        )
        .await?;

        let parsed = output.trim().parse::<usize>().unwrap_or(0);
        Ok(parsed)
    }

    async fn git_pull_and_changed_files(&self, old_commit: &str) -> Result<Vec<(String, String)>> {
        let branch = self.runtime.config.codebase.git_branch.clone();

        let _ = run_git(
            &self.runtime.config.codebase.directory_path,
            &["pull", "origin", &branch],
        )
        .await?;

        let new_commit = self.current_head().await?;
        let range = format!("{}..{}", old_commit, new_commit);
        let output = run_git(
            &self.runtime.config.codebase.directory_path,
            &["diff", "--name-status", &range],
        )
        .await?;

        let mut changed = Vec::new();
        for line in output.lines() {
            let mut parts = line.split_whitespace();
            if let (Some(status), Some(path)) = (parts.next(), parts.next()) {
                changed.push((
                    self.runtime
                        .config
                        .codebase
                        .directory_path
                        .join(path)
                        .display()
                        .to_string(),
                    status.to_string(),
                ));
            }
        }

        Ok(changed)
    }
}

async fn run_git(repo: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .await
        .with_context(|| format!("failed to execute git {:?}", args))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git {:?} failed: {}", args, stderr);
    }

    let stdout = String::from_utf8(output.stdout).context("git output not utf8")?;
    Ok(stdout)
}
