//! App process lifecycle management.

use thiserror::Error;

use crate::manifest::MeshAppManifest;

#[derive(Debug, Error)]
pub enum LifecycleError {
    #[error("spawn failed: {0}")]
    Spawn(String),
    #[error("already running")]
    AlreadyRunning,
    #[error("not running")]
    NotRunning,
}

/// Status of a managed app process.
#[derive(Debug, Clone, PartialEq)]
pub enum AppStatus {
    Stopped,
    Starting,
    Running { port: u16 },
    Failed(String),
}

/// A managed app process on this node.
pub struct AppProcess {
    pub app_id: String,
    pub manifest: MeshAppManifest,
    pub status: AppStatus,
    child: Option<tokio::process::Child>,
}

impl AppProcess {
    pub fn new(manifest: MeshAppManifest) -> Self {
        let app_id = manifest.app_id.clone();
        Self {
            app_id,
            manifest,
            status: AppStatus::Stopped,
            child: None,
        }
    }

    /// Start the app process. Binds to an OS-assigned port and returns it.
    pub async fn start(&mut self) -> Result<u16, LifecycleError> {
        if matches!(self.status, AppStatus::Running { .. } | AppStatus::Starting) {
            return Err(LifecycleError::AlreadyRunning);
        }

        self.status = AppStatus::Starting;

        // Assign a random available port by binding briefly.
        let port = {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .map_err(|e| LifecycleError::Spawn(format!("port bind failed: {e}")))?;
            listener.local_addr().map(|a| a.port()).unwrap_or(0)
        };

        let child = tokio::process::Command::new(&self.manifest.entrypoint)
            .env("APP_PORT", port.to_string())
            .env("APP_ID", &self.manifest.app_id)
            .spawn()
            .map_err(|e| LifecycleError::Spawn(format!("spawn failed: {e}")))?;

        self.child = Some(child);
        self.status = AppStatus::Running { port };
        Ok(port)
    }

    /// Stop the running process.
    pub async fn stop(&mut self) -> Result<(), LifecycleError> {
        match &mut self.child {
            None => {
                self.status = AppStatus::Stopped;
                Ok(())
            }
            Some(child) => {
                let _ = child.kill().await;
                self.child = None;
                self.status = AppStatus::Stopped;
                Ok(())
            }
        }
    }

    /// Perform a simple health check by trying to connect to the app's port.
    pub async fn health_check(&self) -> bool {
        let port = match &self.status {
            AppStatus::Running { port } => *port,
            _ => return false,
        };
        tokio::net::TcpStream::connect(format!("127.0.0.1:{port}"))
            .await
            .is_ok()
    }
}
