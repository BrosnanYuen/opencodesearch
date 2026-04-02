use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::net::UnixDatagram;
use tokio::process::{Child, Command};
#[cfg(unix)]
use tokio::signal::unix::{SignalKind, signal};
use tokio::time::{Duration, sleep};

use crate::config::AppConfig;
use crate::types::UpdateSignal;

/// Orchestrator state machine modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrchestratorState {
    Spinup,
    Normal,
    Update,
    Closing,
}

/// Child process names managed by orchestrator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum ProcessRole {
    Ingestor,
    Mcp,
    Watchdog,
}

impl ProcessRole {
    fn as_arg(self) -> &'static str {
        match self {
            Self::Ingestor => "ingestor",
            Self::Mcp => "mcp",
            Self::Watchdog => "watchdog",
        }
    }
}

/// Main orchestrator process implementing required lifecycle behavior.
pub struct Orchestrator {
    state: OrchestratorState,
    config_path: PathBuf,
    config: Option<AppConfig>,
    ipc_socket_path: PathBuf,
    ipc_socket: Option<UnixDatagram>,
    children: HashMap<ProcessRole, Child>,
}

impl Orchestrator {
    /// Create orchestrator in SPINUP state.
    pub fn new(config_path: PathBuf) -> Self {
        let ipc_socket_path =
            std::env::temp_dir().join(format!("opencodesearch-{}.sock", std::process::id()));

        Self {
            state: OrchestratorState::Spinup,
            config_path,
            config: None,
            ipc_socket_path,
            ipc_socket: None,
            children: HashMap::new(),
        }
    }

    /// Execute the state machine loop until shutdown.
    pub async fn run(mut self) -> Result<()> {
        let shutdown_requested = Arc::new(AtomicBool::new(false));
        {
            let flag = Arc::clone(&shutdown_requested);
            tokio::spawn(async move {
                let _ = tokio::signal::ctrl_c().await;
                flag.store(true, Ordering::SeqCst);
            });
        }

        #[cfg(unix)]
        {
            let flag = Arc::clone(&shutdown_requested);
            tokio::spawn(async move {
                if let Ok(mut sigterm) = signal(SignalKind::terminate()) {
                    let _ = sigterm.recv().await;
                    flag.store(true, Ordering::SeqCst);
                }
            });
        }

        self.spinup().await?;
        self.transition_to(OrchestratorState::Normal).await?;

        loop {
            if self.state == OrchestratorState::Closing {
                break;
            }

            let should_close =
                self.check_shutdown_signal() || shutdown_requested.load(Ordering::SeqCst);

            if should_close {
                self.transition_to(OrchestratorState::Closing).await?;
                continue;
            }

            self.handle_ipc_signals().await?;
            self.monitor_and_restart().await?;
            sleep(Duration::from_millis(600)).await;
        }

        self.shutdown_all().await?;
        Ok(())
    }

    async fn spinup(&mut self) -> Result<()> {
        // Read config according to SPINUP_STATE requirements.
        let config = AppConfig::from_path(&self.config_path)?;
        self.config = Some(config);

        if self.ipc_socket_path.exists() {
            let _ = std::fs::remove_file(&self.ipc_socket_path);
        }

        // Bind local UNIX datagram for UPDATE_START / UPDATE_END messages.
        let socket = UnixDatagram::bind(&self.ipc_socket_path).with_context(|| {
            format!(
                "failed binding orchestrator ipc socket at {}",
                self.ipc_socket_path.display()
            )
        })?;
        self.ipc_socket = Some(socket);

        Ok(())
    }

    async fn transition_to(&mut self, next: OrchestratorState) -> Result<()> {
        self.state = next;

        match next {
            OrchestratorState::Spinup => {}
            OrchestratorState::Normal => {
                self.ensure_started(ProcessRole::Ingestor).await?;
                self.ensure_started(ProcessRole::Mcp).await?;
                self.ensure_started(ProcessRole::Watchdog).await?;
            }
            OrchestratorState::Update => {
                self.stop_child(ProcessRole::Ingestor).await?;
                self.stop_child(ProcessRole::Mcp).await?;
                self.ensure_started(ProcessRole::Watchdog).await?;
            }
            OrchestratorState::Closing => {
                self.shutdown_all().await?;
            }
        }

        Ok(())
    }

    async fn ensure_started(&mut self, role: ProcessRole) -> Result<()> {
        if self.children.contains_key(&role) {
            return Ok(());
        }

        let current_exe = std::env::current_exe().context("failed resolving current executable")?;
        let mut cmd = Command::new(current_exe);

        cmd.arg(role.as_arg())
            .arg("--config")
            .arg(&self.config_path)
            .env("OPENCODESEARCH_IPC_SOCKET", &self.ipc_socket_path)
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());

        let child = cmd
            .spawn()
            .with_context(|| format!("failed spawning child process for {}", role.as_arg()))?;

        if let Some(pid) = child.id() {
            eprintln!("started {} pid={}", role.as_arg(), pid);
        }

        self.children.insert(role, child);
        Ok(())
    }

    async fn stop_child(&mut self, role: ProcessRole) -> Result<()> {
        if let Some(mut child) = self.children.remove(&role) {
            let _ = child.start_kill();
            let _ = child.wait().await;
            eprintln!("stopped {}", role.as_arg());
        }
        Ok(())
    }

    async fn shutdown_all(&mut self) -> Result<()> {
        let roles = self.children.keys().copied().collect::<Vec<_>>();
        for role in roles {
            self.stop_child(role).await?;
        }

        if self.ipc_socket_path.exists() {
            let _ = std::fs::remove_file(&self.ipc_socket_path);
        }
        Ok(())
    }

    async fn monitor_and_restart(&mut self) -> Result<()> {
        if self.state == OrchestratorState::Closing {
            return Ok(());
        }

        let roles = self.children.keys().copied().collect::<Vec<_>>();
        let mut crashed = Vec::new();

        for role in roles {
            if let Some(child) = self.children.get_mut(&role) {
                if let Some(status) = child.try_wait().context("failed polling child status")? {
                    eprintln!("child {} exited with status {}", role.as_arg(), status);
                    crashed.push(role);
                }
            }
        }

        for role in crashed {
            self.children.remove(&role);

            // In UPDATE state only watchdog should be active.
            if self.state == OrchestratorState::Update && role != ProcessRole::Watchdog {
                continue;
            }

            self.ensure_started(role).await?;
        }

        Ok(())
    }

    async fn handle_ipc_signals(&mut self) -> Result<()> {
        let mut buffer = [0_u8; 128];

        if let Some(socket) = &self.ipc_socket {
            match socket.try_recv(&mut buffer) {
                Ok(size) => {
                    if let Some(signal) = UpdateSignal::parse(&buffer[..size]) {
                        match signal {
                            UpdateSignal::UpdateStart
                                if self.state == OrchestratorState::Normal =>
                            {
                                self.transition_to(OrchestratorState::Update).await?;
                            }
                            UpdateSignal::UpdateEnd if self.state == OrchestratorState::Update => {
                                self.transition_to(OrchestratorState::Normal).await?;
                            }
                            _ => {}
                        }
                    }
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(err) => return Err(err).context("ipc receive failed"),
            }
        }

        Ok(())
    }

    fn check_shutdown_signal(&self) -> bool {
        // Keep portable fallback check via environment for testability.
        if std::env::var("OPENCODESEARCH_FORCE_SHUTDOWN")
            .ok()
            .as_deref()
            == Some("1")
        {
            return true;
        }
        false
    }
}
