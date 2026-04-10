use std::collections::VecDeque;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use tokio::net::TcpStream;
use tracing::{debug, warn};

use commonwealth_core::capabilities::ProcessKind;

/// Health status of a managed process.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case", tag = "status")]
pub enum HealthStatus {
    Healthy,
    Degraded { reason: String },
    Unresponsive,
    Dead,
    Unknown,
}

/// Result of a single health check.
#[derive(Debug, Clone)]
pub struct HealthCheckResult {
    pub status: HealthStatus,
    pub latency: Option<Duration>,
    pub checked_at: Instant,
}

/// Configuration for health checking.
#[derive(Debug, Clone)]
pub struct HealthCheckConfig {
    /// Interval between health checks. Default: 5 seconds.
    pub check_interval: Duration,
    /// Timeout for individual health check probes. Default: 3 seconds.
    pub check_timeout: Duration,
    /// Number of consecutive failures before marking as unresponsive.
    pub failure_threshold: u32,
    /// Latency above this is considered degraded. Default: 5 seconds.
    pub degraded_latency: Duration,
    /// Number of recent latency samples to track.
    pub latency_window: usize,
}

impl Default for HealthCheckConfig {
    fn default() -> Self {
        Self {
            check_interval: Duration::from_secs(5),
            check_timeout: Duration::from_secs(3),
            failure_threshold: 3,
            degraded_latency: Duration::from_secs(5),
            latency_window: 20,
        }
    }
}

/// Tracks health state for a single process.
pub struct HealthTracker {
    pub address: SocketAddr,
    pub kind: ProcessKind,
    pub config: HealthCheckConfig,
    pub current_status: HealthStatus,
    pub consecutive_failures: u32,
    pub latency_history: VecDeque<Duration>,
}

impl HealthTracker {
    pub fn new(address: SocketAddr, kind: ProcessKind, config: HealthCheckConfig) -> Self {
        Self {
            address,
            kind,
            config,
            current_status: HealthStatus::Unknown,
            consecutive_failures: 0,
            latency_history: VecDeque::new(),
        }
    }

    /// Run a single health check and update state.
    pub async fn check(&mut self) -> HealthCheckResult {
        let start = Instant::now();

        let check_result = match self.kind {
            ProcessKind::LlamaServer => self.check_http().await,
            ProcessKind::RpcServer => self.check_tcp().await,
        };

        let latency = start.elapsed();

        let status = match check_result {
            Ok(()) => {
                self.consecutive_failures = 0;
                self.record_latency(latency);

                if latency > self.config.degraded_latency {
                    HealthStatus::Degraded {
                        reason: format!("high latency: {:.0}ms", latency.as_millis()),
                    }
                } else {
                    HealthStatus::Healthy
                }
            }
            Err(reason) => {
                self.consecutive_failures += 1;

                if self.consecutive_failures >= self.config.failure_threshold {
                    warn!(
                        address = %self.address,
                        failures = self.consecutive_failures,
                        "process unresponsive"
                    );
                    HealthStatus::Unresponsive
                } else {
                    debug!(
                        address = %self.address,
                        failures = self.consecutive_failures,
                        reason = reason,
                        "health check failed"
                    );
                    HealthStatus::Degraded { reason }
                }
            }
        };

        let previous = self.current_status.clone();
        self.current_status = status.clone();

        if previous != status {
            debug!(
                address = %self.address,
                previous = ?previous,
                current = ?status,
                "health status changed"
            );
        }

        HealthCheckResult {
            status,
            latency: Some(latency),
            checked_at: start,
        }
    }

    /// HTTP health check for llama-server: GET /health.
    async fn check_http(&self) -> Result<(), String> {
        let url = format!("http://{}/health", self.address);

        let result = tokio::time::timeout(self.config.check_timeout, async {
            // Simple TCP connect + HTTP request without pulling in reqwest.
            let stream = TcpStream::connect(self.address)
                .await
                .map_err(|e| format!("connect failed: {e}"))?;

            // Send minimal HTTP GET.
            let request = format!(
                "GET /health HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
                self.address
            );

            stream
                .writable()
                .await
                .map_err(|e| format!("not writable: {e}"))?;

            stream
                .try_write(request.as_bytes())
                .map_err(|e| format!("write failed: {e}"))?;

            // Read response — just check we get something back.
            stream
                .readable()
                .await
                .map_err(|e| format!("not readable: {e}"))?;

            let mut buf = [0u8; 256];
            match stream.try_read(&mut buf) {
                Ok(n) if n > 0 => {
                    let response = String::from_utf8_lossy(&buf[..n]);
                    if response.contains("200") || response.contains("OK") {
                        Ok(())
                    } else {
                        Err(format!("unhealthy response from {url}"))
                    }
                }
                Ok(_) => Err("empty response".into()),
                Err(e) => Err(format!("read failed: {e}")),
            }
        })
        .await;

        match result {
            Ok(inner) => inner,
            Err(_) => Err("health check timed out".into()),
        }
    }

    /// TCP connect check for rpc-server.
    async fn check_tcp(&self) -> Result<(), String> {
        let result =
            tokio::time::timeout(self.config.check_timeout, TcpStream::connect(self.address)).await;

        match result {
            Ok(Ok(_stream)) => Ok(()),
            Ok(Err(e)) => Err(format!("TCP connect failed: {e}")),
            Err(_) => Err("TCP connect timed out".into()),
        }
    }

    fn record_latency(&mut self, latency: Duration) {
        self.latency_history.push_back(latency);
        if self.latency_history.len() > self.config.latency_window {
            self.latency_history.pop_front();
        }
    }

    /// Average latency over the recent window.
    pub fn average_latency(&self) -> Option<Duration> {
        if self.latency_history.is_empty() {
            return None;
        }
        let total: Duration = self.latency_history.iter().sum();
        Some(total / self.latency_history.len() as u32)
    }

    /// P95 latency over the recent window.
    pub fn p95_latency(&self) -> Option<Duration> {
        if self.latency_history.is_empty() {
            return None;
        }
        let mut sorted: Vec<Duration> = self.latency_history.iter().copied().collect();
        sorted.sort();
        let idx = (sorted.len() as f64 * 0.95).ceil() as usize - 1;
        Some(sorted[idx.min(sorted.len() - 1)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn health_status_serde_roundtrip() {
        let statuses = vec![
            HealthStatus::Healthy,
            HealthStatus::Degraded {
                reason: "slow".into(),
            },
            HealthStatus::Unresponsive,
            HealthStatus::Dead,
            HealthStatus::Unknown,
        ];
        for status in statuses {
            let json = serde_json::to_string(&status).unwrap();
            let back: HealthStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(status, back);
        }
    }

    #[test]
    fn health_tracker_initial_state() {
        let tracker = HealthTracker::new(
            "127.0.0.1:8080".parse().unwrap(),
            ProcessKind::LlamaServer,
            HealthCheckConfig::default(),
        );
        assert_eq!(tracker.current_status, HealthStatus::Unknown);
        assert_eq!(tracker.consecutive_failures, 0);
        assert!(tracker.average_latency().is_none());
    }

    #[test]
    fn latency_tracking() {
        let mut tracker = HealthTracker::new(
            "127.0.0.1:8080".parse().unwrap(),
            ProcessKind::LlamaServer,
            HealthCheckConfig {
                latency_window: 3,
                ..Default::default()
            },
        );

        tracker.record_latency(Duration::from_millis(10));
        tracker.record_latency(Duration::from_millis(20));
        tracker.record_latency(Duration::from_millis(30));

        let avg = tracker.average_latency().unwrap();
        assert!(avg >= Duration::from_millis(19) && avg <= Duration::from_millis(21));

        // Window is 3, adding a 4th should evict the first.
        tracker.record_latency(Duration::from_millis(40));
        assert_eq!(tracker.latency_history.len(), 3);

        let avg = tracker.average_latency().unwrap();
        // (20 + 30 + 40) / 3 = 30
        assert!(avg >= Duration::from_millis(29) && avg <= Duration::from_millis(31));
    }

    #[test]
    fn p95_latency() {
        let mut tracker = HealthTracker::new(
            "127.0.0.1:8080".parse().unwrap(),
            ProcessKind::RpcServer,
            HealthCheckConfig {
                latency_window: 20,
                ..Default::default()
            },
        );

        for i in 1..=20 {
            tracker.record_latency(Duration::from_millis(i));
        }

        let p95 = tracker.p95_latency().unwrap();
        // P95 of 1..=20 should be 19.
        assert_eq!(p95, Duration::from_millis(19));
    }

    #[tokio::test]
    async fn tcp_health_check_to_nonexistent_port() {
        let mut tracker = HealthTracker::new(
            // Port 1 is unlikely to be listening.
            "127.0.0.1:1".parse().unwrap(),
            ProcessKind::RpcServer,
            HealthCheckConfig {
                check_timeout: Duration::from_millis(200),
                failure_threshold: 2,
                ..Default::default()
            },
        );

        // First failure — should be Degraded.
        let result = tracker.check().await;
        assert!(matches!(result.status, HealthStatus::Degraded { .. }));

        // Second failure — should be Unresponsive.
        let result = tracker.check().await;
        assert_eq!(result.status, HealthStatus::Unresponsive);
    }

    #[tokio::test]
    async fn tcp_health_check_to_listening_port() {
        // Start a TCP listener.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        // Accept connections in background.
        tokio::spawn(async move {
            loop {
                let _ = listener.accept().await;
            }
        });

        let mut tracker =
            HealthTracker::new(addr, ProcessKind::RpcServer, HealthCheckConfig::default());

        let result = tracker.check().await;
        assert_eq!(result.status, HealthStatus::Healthy);
        assert!(result.latency.unwrap() < Duration::from_secs(1));
    }
}
