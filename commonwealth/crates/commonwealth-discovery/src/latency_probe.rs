// SPDX-License-Identifier: AGPL-3.0-or-later
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tokio::net::UdpSocket;
use tokio::sync::RwLock;
use tracing::debug;

use commonwealth_core::ids::NodeId;
use commonwealth_core::latency::{LatencyMatrix, LatencyRecord};
use commonwealth_core::Result;

/// Configuration for the latency prober.
#[derive(Debug, Clone)]
pub struct LatencyProbeConfig {
    /// Interval between probe rounds. Default: 30 seconds.
    pub probe_interval: Duration,
    /// Timeout for waiting for a probe response. Default: 5 seconds.
    pub probe_timeout: Duration,
    /// EWMA smoothing factor (0.0-1.0). Higher = more weight on recent measurements.
    pub ewma_alpha: f32,
}

impl Default for LatencyProbeConfig {
    fn default() -> Self {
        Self {
            probe_interval: Duration::from_secs(30),
            probe_timeout: Duration::from_secs(5),
            ewma_alpha: 0.3,
        }
    }
}

/// Magic bytes to identify Commonwealth latency probes.
const PROBE_MAGIC: &[u8; 4] = b"CWLP";
const PROBE_REQUEST: u8 = 0x01;
const PROBE_RESPONSE: u8 = 0x02;

/// A peer that we probe for latency.
#[derive(Debug, Clone)]
pub struct ProbePeer {
    pub node_id: NodeId,
    pub address: SocketAddr,
}

/// The latency prober sends UDP packets to peers and measures round-trip time.
pub struct LatencyProber {
    self_id: NodeId,
    config: LatencyProbeConfig,
    matrix: Arc<RwLock<LatencyMatrix>>,
}

impl LatencyProber {
    pub fn new(self_id: NodeId, config: LatencyProbeConfig) -> Self {
        Self {
            self_id,
            config,
            matrix: Arc::new(RwLock::new(LatencyMatrix::new())),
        }
    }

    /// Get a reference to the shared latency matrix.
    pub fn matrix_handle(&self) -> Arc<RwLock<LatencyMatrix>> {
        Arc::clone(&self.matrix)
    }

    /// Get a snapshot of the current latency matrix.
    pub async fn matrix(&self) -> LatencyMatrix {
        self.matrix.read().await.clone()
    }

    /// Probe a single peer and record the result.
    pub async fn probe_peer(&self, socket: &UdpSocket, peer: &ProbePeer) -> Result<LatencyRecord> {
        let send_time = Instant::now();
        let timestamp_bytes = now_secs().to_be_bytes();

        // Build probe request: magic(4) + type(1) + self_id(16) + timestamp(8).
        let mut packet = Vec::with_capacity(29);
        packet.extend_from_slice(PROBE_MAGIC);
        packet.push(PROBE_REQUEST);
        packet.extend_from_slice(self.self_id.as_bytes());
        packet.extend_from_slice(&timestamp_bytes);

        socket
            .send_to(&packet, peer.address)
            .await
            .map_err(|e| commonwealth_core::Error::Discovery(format!("probe send failed: {e}")))?;

        // Wait for response.
        let mut buf = [0u8; 64];
        let result =
            tokio::time::timeout(self.config.probe_timeout, socket.recv_from(&mut buf)).await;

        match result {
            Ok(Ok((len, _from))) => {
                let rtt = send_time.elapsed();
                let rtt_ms = rtt.as_secs_f32() * 1000.0;

                if len >= 5 && &buf[0..4] == PROBE_MAGIC && buf[4] == PROBE_RESPONSE {
                    let record = self.update_record(peer.node_id, rtt_ms).await;
                    debug!(
                        peer = %peer.node_id,
                        rtt_ms = format!("{:.1}", rtt_ms),
                        "latency probe completed"
                    );
                    Ok(record)
                } else {
                    Err(commonwealth_core::Error::Discovery(
                        "invalid probe response".into(),
                    ))
                }
            }
            Ok(Err(e)) => Err(commonwealth_core::Error::Discovery(format!(
                "probe recv failed: {e}"
            ))),
            Err(_) => Err(commonwealth_core::Error::Discovery(
                "probe timed out".into(),
            )),
        }
    }

    /// Update the latency record for a peer using EWMA.
    async fn update_record(&self, peer_id: NodeId, rtt_ms: f32) -> LatencyRecord {
        let mut matrix = self.matrix.write().await;
        let alpha = self.config.ewma_alpha;

        let new_record = if let Some(existing) = matrix.get(self.self_id, peer_id) {
            let smoothed_rtt = alpha * rtt_ms + (1.0 - alpha) * existing.rtt_ms;
            let jitter =
                alpha * (rtt_ms - existing.rtt_ms).abs() + (1.0 - alpha) * existing.jitter_ms;
            LatencyRecord {
                rtt_ms: smoothed_rtt,
                jitter_ms: jitter,
                bandwidth_estimate_mbps: existing.bandwidth_estimate_mbps,
                last_measured: now_secs(),
            }
        } else {
            LatencyRecord {
                rtt_ms,
                jitter_ms: 0.0,
                bandwidth_estimate_mbps: 0.0, // Estimated separately.
                last_measured: now_secs(),
            }
        };

        matrix.record(self.self_id, peer_id, new_record);
        new_record
    }

    /// Handle an incoming probe request — send back a response.
    pub async fn handle_probe_request(
        socket: &UdpSocket,
        request_data: &[u8],
        from: SocketAddr,
    ) -> Result<()> {
        if request_data.len() < 5
            || &request_data[0..4] != PROBE_MAGIC
            || request_data[4] != PROBE_REQUEST
        {
            return Ok(()); // Not a valid probe, ignore.
        }

        // Build response: magic(4) + type(1) + echo rest of request.
        let mut response = Vec::with_capacity(request_data.len());
        response.extend_from_slice(PROBE_MAGIC);
        response.push(PROBE_RESPONSE);
        if request_data.len() > 5 {
            response.extend_from_slice(&request_data[5..]);
        }

        socket.send_to(&response, from).await.map_err(|e| {
            commonwealth_core::Error::Discovery(format!("probe response send failed: {e}"))
        })?;

        Ok(())
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_secs()
}

/// Compute EWMA for a new sample given previous value.
pub fn ewma(previous: f32, sample: f32, alpha: f32) -> f32 {
    alpha * sample + (1.0 - alpha) * previous
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ewma_smoothing() {
        let prev = 10.0;
        let sample = 20.0;
        let alpha = 0.3;
        let result = ewma(prev, sample, alpha);
        // 0.3 * 20 + 0.7 * 10 = 6 + 7 = 13
        assert!((result - 13.0).abs() < 0.001);
    }

    #[test]
    fn ewma_alpha_zero_ignores_new_sample() {
        assert!((ewma(10.0, 100.0, 0.0) - 10.0).abs() < 0.001);
    }

    #[test]
    fn ewma_alpha_one_uses_only_new_sample() {
        assert!((ewma(10.0, 100.0, 1.0) - 100.0).abs() < 0.001);
    }

    #[tokio::test]
    async fn latency_prober_loopback_probe() {
        // Bind two sockets and probe between them.
        let socket_a = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let socket_b = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr_b = socket_b.local_addr().unwrap();

        let prober = LatencyProber::new(NodeId::from_u128(1), LatencyProbeConfig::default());
        let peer = ProbePeer {
            node_id: NodeId::from_u128(2),
            address: addr_b,
        };

        // Spawn responder.
        let responder = tokio::spawn(async move {
            let mut buf = [0u8; 64];
            let (len, from) = socket_b.recv_from(&mut buf).await.unwrap();
            LatencyProber::handle_probe_request(&socket_b, &buf[..len], from)
                .await
                .unwrap();
        });

        // Send probe.
        let record = prober.probe_peer(&socket_a, &peer).await.unwrap();

        responder.await.unwrap();

        // Loopback RTT should be very low.
        assert!(
            record.rtt_ms < 100.0,
            "loopback RTT too high: {}",
            record.rtt_ms
        );
        assert!(record.rtt_ms >= 0.0);

        // Check matrix was updated.
        let matrix = prober.matrix().await;
        assert!(matrix
            .get(NodeId::from_u128(1), NodeId::from_u128(2))
            .is_some());
    }

    #[tokio::test]
    async fn latency_prober_ewma_smoothing() {
        let prober = LatencyProber::new(
            NodeId::from_u128(1),
            LatencyProbeConfig {
                ewma_alpha: 0.5,
                ..Default::default()
            },
        );

        // First measurement.
        let rec1 = prober.update_record(NodeId::from_u128(2), 10.0).await;
        assert!((rec1.rtt_ms - 10.0).abs() < 0.001);

        // Second measurement — should be smoothed.
        let rec2 = prober.update_record(NodeId::from_u128(2), 20.0).await;
        // EWMA: 0.5 * 20 + 0.5 * 10 = 15.
        assert!(
            (rec2.rtt_ms - 15.0).abs() < 0.001,
            "expected ~15, got {}",
            rec2.rtt_ms
        );
    }

    #[tokio::test]
    async fn probe_timeout_returns_error() {
        let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let prober = LatencyProber::new(
            NodeId::from_u128(1),
            LatencyProbeConfig {
                probe_timeout: Duration::from_millis(100),
                ..Default::default()
            },
        );

        // Probe a non-listening address — should timeout.
        let peer = ProbePeer {
            node_id: NodeId::from_u128(99),
            address: "127.0.0.1:1".parse().unwrap(),
        };
        let result = prober.probe_peer(&socket, &peer).await;
        assert!(result.is_err());
    }
}
