// SPDX-License-Identifier: AGPL-3.0-or-later
//! Forwarding a leased unit to a program's execute origin (pb-work-donor).
//!
//! A kind a program runs in its own process — the svrn daemon's `ingest:v1`,
//! through its own corpus engine — is served on that program's loopback port
//! and registered in this node's origin table as `Admit::Local` under
//! `oicp_types::work::exec::exec_slot`. [`Origins::refresh`] finds each one by
//! the listing and asks it to describe itself once; [`OriginExecutor`] is the
//! `JobExecutor` the donor registers for it, so the one drive leases, renews,
//! cancels and reports the unit exactly as it does its own `process:v1`.
//!
//! Nothing here decides anything about the unit: the origin's executor runs
//! its own validate before the lease ([`OriginExecutor::validate_remote`]) and
//! again inside `execute`, and its outcome — the verdict, or the `JobError` —
//! travels back whole.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use commonwealth_core::ids::NodeId;
use commonwealth_media::origins::Admit;
use commonwealth_work::executor::{ExecuteFuture, JobContext, JobError, JobExecutor};
use commonwealth_work::refusal::WorkRefusal;
use kernel_types::Judgement;
use oicp_types::work::exec::{
    exec_slot_kind, ExecCancel, ExecDescription, ExecEvent, ExecRun, EXEC_CANCEL_PATH,
    EXEC_DESCRIBE_PATH, EXEC_RUN_PATH, EXEC_VALIDATE_PATH,
};
use oicp_types::{JobExecutorDescriptor, JobKind, JobUnit};
use tracing::{debug, info, warn};

use super::TRACE_TARGET;
use crate::RailsDaemon;

/// How often a unit in flight is checked for a cancel to forward — the
/// origin's own executor polls its flag at the same cadence.
const CANCEL_POLL: Duration = Duration::from_millis(500);

/// A bound on the describe and validate doors, which answer from memory: an
/// origin that does not answer them in this long is not taking work.
const QUICK_DOOR: Duration = Duration::from_secs(5);

fn client() -> &'static reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(reqwest::Client::new)
}

/// One registered execute origin, as the donor runs units through it.
pub struct OriginExecutor {
    slot: String,
    addr: SocketAddr,
    kind: JobKind,
    description: ExecDescription,
}

impl OriginExecutor {
    /// Ask the origin at `addr` to describe itself. Refused when the answer
    /// names a kind other than its slot's, so a table entry cannot run one
    /// kind's units through another kind's executor.
    pub async fn describe(slot: &str, addr: SocketAddr) -> Result<Self, String> {
        let kind =
            exec_slot_kind(slot).ok_or_else(|| format!("`{slot}` is not an execute slot"))?;
        let description: ExecDescription = client()
            .get(format!("http://{addr}{EXEC_DESCRIBE_PATH}"))
            .timeout(QUICK_DOOR)
            .send()
            .await
            .and_then(|r| r.error_for_status())
            .map_err(|e| format!("the describe door did not answer: {e}"))?
            .json()
            .await
            .map_err(|e| format!("the describe answer is not an ExecDescription: {e}"))?;
        if description.descriptor.kind != kind {
            return Err(format!(
                "the origin registered as `{slot}` describes `{}`",
                description.descriptor.kind
            ));
        }
        Ok(Self {
            slot: slot.to_string(),
            addr,
            kind,
            description,
        })
    }

    pub fn slot(&self) -> &str {
        &self.slot
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn kind(&self) -> &JobKind {
        &self.kind
    }

    /// The roster identity the registrant asked its units' credit to go to.
    pub fn credit_node(&self) -> Option<NodeId> {
        self.description.credit_node
    }

    /// The origin's executor's own pure check, before any lease. `Err` is
    /// could-not-ask, which the donor reports and skips this round rather than
    /// reading as either verdict.
    pub async fn validate_remote(&self, unit: &JobUnit) -> Result<Result<(), WorkRefusal>, String> {
        client()
            .post(format!("http://{}{EXEC_VALIDATE_PATH}", self.addr))
            .timeout(QUICK_DOOR)
            .json(unit)
            .send()
            .await
            .and_then(|r| r.error_for_status())
            .map_err(|e| format!("the validate door did not answer: {e}"))?
            .json()
            .await
            .map_err(|e| format!("the validate answer is not a verdict: {e}"))
    }

    async fn run(
        &self,
        unit: &JobUnit,
        ctx: &JobContext,
    ) -> Result<(Judgement, serde_json::Value), JobError> {
        let unreachable = |why: String| JobError::Spawn {
            program: format!("the execute origin `{}` at {}", self.slot, self.addr),
            reason: why,
        };
        let request = ExecRun {
            unit: unit.clone(),
            workdir: ctx.workdir().to_path_buf(),
        };
        let mut resp = client()
            .post(format!("http://{}{EXEC_RUN_PATH}", self.addr))
            .json(&request)
            .send()
            .await
            .and_then(|r| r.error_for_status())
            .map_err(|e| unreachable(e.to_string()))?;
        debug!(target: TRACE_TARGET, unit = %unit.unit_hash, slot = %self.slot,
               "work donor: forwarded a unit to its execute origin");
        let mut pending: Vec<u8> = Vec::new();
        let mut cancel_sent = false;
        let mut poll = tokio::time::interval(CANCEL_POLL);
        poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                chunk = resp.chunk() => {
                    let chunk = match chunk {
                        Ok(Some(c)) => c,
                        Ok(None) => break,
                        Err(e) => return Err(JobError::NoVerdict {
                            reason: format!("the execute origin `{}` dropped the unit's stream: {e}", self.slot),
                        }),
                    };
                    pending.extend_from_slice(&chunk);
                    while let Some(end) = pending.iter().position(|b| *b == b'\n') {
                        let line: Vec<u8> = pending.drain(..=end).collect();
                        match serde_json::from_slice::<ExecEvent>(&line) {
                            Ok(ExecEvent::Progress { note }) => ctx.progress(&note),
                            Ok(ExecEvent::Done { outcome }) => return outcome,
                            Err(e) => return Err(JobError::NoVerdict {
                                reason: format!("the execute origin `{}` answered a line that is not an event: {e}", self.slot),
                            }),
                        }
                    }
                }
                _ = poll.tick(), if !cancel_sent => {
                    if ctx.cancel_requested() {
                        cancel_sent = true;
                        let sent = client()
                            .post(format!("http://{}{EXEC_CANCEL_PATH}", self.addr))
                            .timeout(QUICK_DOOR)
                            .json(&ExecCancel { unit_hash: unit.unit_hash.clone() })
                            .send()
                            .await;
                        debug!(target: TRACE_TARGET, unit = %unit.unit_hash, slot = %self.slot,
                               sent = sent.is_ok(), "work donor: forwarded the cancel to the execute origin");
                    }
                }
            }
        }
        Err(JobError::NoVerdict {
            reason: format!(
                "the execute origin `{}` closed the unit's stream without an outcome",
                self.slot
            ),
        })
    }
}

impl JobExecutor for OriginExecutor {
    fn descriptor(&self) -> JobExecutorDescriptor {
        self.description.descriptor.clone()
    }

    /// The part of the check that needs no round trip: the kind. The origin's
    /// own check runs through [`OriginExecutor::validate_remote`] before the
    /// lease, and again inside `execute`.
    fn validate(&self, unit: &JobUnit) -> Result<(), WorkRefusal> {
        if unit.kind == self.kind {
            return Ok(());
        }
        Err(if unit.kind.is_skew_of(&self.kind) {
            WorkRefusal::VersionSkew {
                wanted: unit.kind.clone(),
                offered: self.kind.clone(),
            }
        } else {
            WorkRefusal::KindNotOffered {
                kind: unit.kind.clone(),
            }
        })
    }

    fn execute<'a>(&'a self, unit: &'a JobUnit, ctx: &'a JobContext) -> ExecuteFuture<'a> {
        Box::pin(self.run(unit, ctx))
    }
}

/// The execute origins this node's table lists, each described once per
/// (slot, address): a program that restarts on a new port is described again,
/// and one whose claim lapsed drops out.
#[derive(Default)]
pub struct Origins {
    known: HashMap<(String, SocketAddr), Arc<OriginExecutor>>,
}

impl Origins {
    pub async fn refresh(&mut self, daemon: &RailsDaemon) -> Vec<Arc<OriginExecutor>> {
        let listed: Vec<(String, SocketAddr)> = daemon
            .origins
            .listing()
            .into_iter()
            .filter(|o| o.admit == Admit::Local && exec_slot_kind(&o.slot).is_some())
            .map(|o| (o.slot, o.addr))
            .collect();
        for gone in self
            .known
            .keys()
            .filter(|k| !listed.contains(k))
            .cloned()
            .collect::<Vec<_>>()
        {
            info!(target: TRACE_TARGET, slot = %gone.0, addr = %gone.1,
                  "work donor: an execute origin left the table");
            self.known.remove(&gone);
        }
        let mut found = Vec::new();
        for (slot, addr) in listed {
            if let Some(known) = self.known.get(&(slot.clone(), addr)) {
                found.push(Arc::clone(known));
                continue;
            }
            match OriginExecutor::describe(&slot, addr).await {
                Ok(origin) => {
                    info!(target: TRACE_TARGET, slot = %slot, %addr,
                          isolation = ?origin.description.descriptor.isolation,
                          credit_node = ?origin.credit_node(),
                          "work donor: an execute origin registered — its kind can be offered");
                    let origin = Arc::new(origin);
                    self.known.insert((slot, addr), Arc::clone(&origin));
                    found.push(origin);
                }
                Err(why) => warn!(target: TRACE_TARGET, slot = %slot, %addr, why = %why,
                                  "work donor: a listed execute origin did not describe itself; not offered this round"),
            }
        }
        found.sort_by(|a, b| a.slot.cmp(&b.slot));
        found
    }
}

#[cfg(test)]
mod tests;
