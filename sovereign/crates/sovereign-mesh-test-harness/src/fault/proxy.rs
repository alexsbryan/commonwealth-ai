// SPDX-License-Identifier: AGPL-3.0-or-later
//! [`FaultProxy`] — a per-`(observer, target)` localhost TCP forwarder that
//! enforces the directed [`WireFault`] from the shared policy on each accepted
//! connection. Sits between a dialing node's HTTP client and the target's real
//! internal listener.
//!
//! Clean edges bypass the proxy entirely ([`super::FaultTransport`] dials the
//! target directly), so only faulted edges pay the forwarding cost and the
//! common path stays byte-faithful. Wire faults are read per-connection from
//! the shared policy, so they can change mid-scenario.

use std::net::SocketAddr;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use commonwealth_core::ids::NodeId;

use super::policy::{SharedPolicy, WireFault};

pub struct FaultProxy {
    pub listen_addr: SocketAddr,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
}

impl FaultProxy {
    /// Bind a localhost listener forwarding to `upstream`, consulting `policy`
    /// for the `(observer, target)` [`WireFault`] on each accepted connection.
    pub async fn spawn(
        observer: NodeId,
        target: NodeId,
        upstream: SocketAddr,
        policy: SharedPolicy,
    ) -> std::io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let listen_addr = listener.local_addr()?;
        let (tx, mut rx) = tokio::sync::oneshot::channel::<()>();

        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut rx => break,
                    accepted = listener.accept() => {
                        let Ok((inbound, _)) = accepted else { continue };
                        let fault = policy
                            .read()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .wire_fault(observer, target)
                            .unwrap_or_default();
                        tokio::spawn(handle_conn(inbound, upstream, fault));
                    }
                }
            }
        });

        Ok(Self {
            listen_addr,
            shutdown: Some(tx),
        })
    }
}

impl Drop for FaultProxy {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
    }
}

async fn handle_conn(inbound: TcpStream, upstream: SocketAddr, fault: WireFault) {
    if !fault.connect_delay.is_zero() {
        tokio::time::sleep(fault.connect_delay).await;
    }
    if fault.drop_conn {
        return; // accepted, then dropped — no upstream dial
    }
    let Ok(outbound) = TcpStream::connect(upstream).await else {
        return;
    };
    let (mut client_r, mut client_w) = inbound.into_split();
    let (mut up_r, mut up_w) = outbound.into_split();

    // client -> upstream: straight copy.
    let c2u = tokio::spawn(async move {
        let _ = tokio::io::copy(&mut client_r, &mut up_w).await;
        let _ = up_w.shutdown().await;
    });

    // upstream -> client: apply cut_after_bytes + throttle_bps.
    let cut = fault.cut_after_bytes;
    let throttle = fault.throttle_bps;
    let u2c = tokio::spawn(async move {
        let mut buf = [0u8; 8192];
        let mut sent = 0usize;
        loop {
            let n = match up_r.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            let take = match cut {
                Some(limit) if sent + n >= limit => limit.saturating_sub(sent),
                _ => n,
            };
            if take > 0 && client_w.write_all(&buf[..take]).await.is_err() {
                break;
            }
            sent += take;
            if let Some(bps) = throttle {
                if bps > 0 {
                    tokio::time::sleep(Duration::from_secs_f64(take as f64 / bps as f64)).await;
                }
            }
            if cut.is_some_and(|limit| sent >= limit) {
                break; // truncate the response — drop the connection
            }
        }
        let _ = client_w.shutdown().await;
    });

    let _ = tokio::join!(c2u, u2c);
}
