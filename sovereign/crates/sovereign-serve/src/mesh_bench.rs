// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn mesh bench` — measure how fast the configuration you are **running**
//! actually decodes, and file the number under the key `svrn mesh plan` looks
//! up.
//!
//! # The one rule
//!
//! **This command measures what is loaded. It never loads what it wants to
//! measure.** There is no slot argument, no model argument that selects
//! anything, and no `--distributed` flag. It reads the daemon's own report of
//! which model occupies the primary slot and how that model is placed, fires
//! real completions at the real HTTP surface, and times the frames coming back.
//!
//! That is not a convenience — it is the mechanism that satisfies
//! `SCHEDULER_QUALITY.md` §4.5's "probe the model being scored". A benchmark
//! that installs its own configuration measures the benchmark. The optional
//! `<model.gguf>` argument is therefore an **assertion**, not a selection: it is
//! fingerprinted from its header and compared against the resident primary, and
//! a mismatch is exit 3 naming the config line to fix.
//!
//! # Why the guards are the interesting part
//!
//! Getting a tokens-per-second number is easy. Getting one that is *about the
//! thing you think it is about* is the whole problem, and every guard below was
//! earned by a specific observed false result — see the header of
//! `scripts/measure-distributed-decode.sh`, which this command replaces.
//!
//! The worst of them is the Fast-slot trap. This repo runs a small always-hot
//! `fast` model beside the big `primary` one. A request that gets hijacked to
//! the fast slot returns quickly and successfully and proves *nothing* — the
//! small model is 100% local, so a "distributed decode" measurement taken that
//! way is a local decode of a different model.
//!
//! The shell script guarded this by asserting the SSE `model` field names the
//! primary. **That check cannot work**, and this command's first live run
//! proved it: the field is a verbatim echo of the string the client requested,
//! so it says `commonwealth/primary` no matter what answered. With the 122B's
//! compute child still starting, requests came back at ~100 tok/s — impossible
//! for that model — with the check passing cleanly. See [`primary_is_serving`]
//! for what replaced it, and for the second trap hiding behind the first.
//!
//! A run that trips any guard is still **written to the store**. Discarding a
//! failure teaches nobody anything, and silently dropping it turns the tool into
//! retry-until-lucky. It is simply recorded [`Verdict::Invalid`] and
//! `mesh_measurements::lookup` never returns it.
//!
//! # The seam
//!
//! Everything below the `cmd_bench` shell is pure: [`parse_trial`],
//! [`evaluate_guards`], [`aggregate`] and [`shards_from_placement`] take
//! observations and return verdicts, with no HTTP, no clock, and no filesystem.
//! That is what makes nine guards testable without a GPU, a peer, or a
//! 100-gigabyte model. Keep the seam: if a guard needs a new fact, add it to
//! [`GuardInput`] and have the shell go get it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::mesh_measurements as mm;
use futures::StreamExt;

// ---------------------------------------------------------------------------
// The probe
//
// These three constants ARE the probe protocol. `PROBE_VERSION` in
// `mesh_measurements` exists to make numbers taken under different values
// incomparable rather than silently mixed, so:
//
//   CHANGING ANYTHING IN THIS BLOCK REQUIRES BUMPING mm::PROBE_VERSION.
//
// This is also why there is no `--max-tokens` flag. A knob whose adjustment
// invalidates comparison against every prior record, while looking like a
// harmless tuning option, is a trap. `--trials` is safe by contrast: it changes
// how many samples are drawn, not what is being sampled.
// ---------------------------------------------------------------------------

/// The prompt every timed trial sends. Deterministic and long enough to stream
/// well past the 32-frame floor, with no dependence on the model's knowledge.
const PROBE_PROMPT: &str = "Count from 1 to 60, one number per line.";

/// Token budget for a timed trial.
const PROBE_MAX_TOKENS: u32 = 192;

/// Token budget for the canary. Small on purpose: its job is to prove tokens
/// flow at all (and to absorb a cold load) before the timed window is spent.
const CANARY_MAX_TOKENS: u32 = 8;

/// Minimum content frames a trial must produce to be called a decode rate.
/// Below this the measurement is dominated by scheduler jitter.
const MIN_CONTENT_FRAMES: u32 = 32;

/// Maximum permitted disagreement between the fastest and slowest trial, as a
/// fraction of the fastest. Above this the machine was not in a steady state.
const MAX_TRIAL_SPREAD: f64 = 0.25;

/// The alias every request uses. Resolves to the primary slot server-side.
const PRIMARY_ALIAS: &str = "commonwealth/primary";

/// How many times the canary will re-fire while the slot is still loading.
/// With [`CANARY_RETRY`] this is a ~10 minute ceiling — long enough for a cold
/// load of a model measured in tens of gigabytes, short enough to give up.
const CANARY_ATTEMPTS: u32 = 20;

/// Wait between canary attempts.
const CANARY_RETRY: Duration = Duration::from_secs(30);

/// Whether an error from the canary says "the slot is coming up", as opposed to
/// "the slot is broken". Matched on prose because that is what the wire carries;
/// a false negative here just means the canary gives up early and the guards
/// report an honest failure, which is the safe direction.
fn slot_still_starting(err: &str) -> bool {
    let e = err.to_ascii_lowercase();
    e.contains("not serving")
        || e.contains("starting")
        || e.contains("slot unavailable")
        || e.contains("loading")
}

// ---------------------------------------------------------------------------
// Observations — what one streamed completion produced
// ---------------------------------------------------------------------------

/// One line of the SSE response, stamped at the moment it arrived.
///
/// Non-`data:` lines are kept rather than dropped: when the server returns an
/// error body instead of a stream, that body is the only evidence of what went
/// wrong, and a reader that skips it reports "0 frames" for a request that was
/// actually rejected with a reason.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Frame {
    /// Seconds since the request was sent — so time-to-first-token (prefill
    /// plus tunnel setup) separates from the steady-state inter-token rate.
    /// Wall-clock over total tokens smears the two together, which is how a
    /// slow link can be made to look like a slow model.
    pub(crate) t_s: f64,
    /// Body after `data:`, when this was an SSE data line.
    pub(crate) data: Option<String>,
    /// The raw line, when it was not.
    pub(crate) raw: Option<String>,
}

impl Frame {
    /// Classify a single received line.
    pub(crate) fn from_line(t_s: f64, line: &str) -> Self {
        if let Some(rest) = line.strip_prefix("data:") {
            Self {
                t_s,
                data: Some(rest.trim().to_string()),
                raw: None,
            }
        } else {
            Self {
                t_s,
                data: None,
                raw: Some(line.to_string()),
            }
        }
    }
}

/// What one timed streaming completion produced, after parsing.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Trial {
    /// The `model` field the server put on its frames. `None` when no frame
    /// carried one — which is itself a guard failure, because an unattributed
    /// run cannot be filed against a configuration.
    pub(crate) served_model: Option<String>,
    /// Frames that carried actual content. The timing basis.
    pub(crate) content_frames: u32,
    /// Seconds to the first content frame.
    pub(crate) ttft_s: Option<f64>,
    /// Seconds between the first and last content frame.
    pub(crate) decode_span_s: f64,
    /// `(content_frames - 1) / decode_span_s` — steady state, TTFT excluded.
    /// Zero when there is no span to divide by; the frame-count guard is what
    /// catches that, not this number.
    pub(crate) decode_tok_s: f64,
    /// Every inter-frame gap in milliseconds. Pooled across trials for the
    /// latency percentiles, where link jitter shows up as a p95 far above p50.
    pub(crate) itl_ms: Vec<f64>,
    /// The terminal `finish_reason`, when the server sent one.
    pub(crate) finish_reason: Option<String>,
    /// `usage.prompt_tokens` from the terminal frame. The **only** admissible
    /// source of a prefill rate — never `text.len() / 4`.
    pub(crate) prompt_tokens: Option<u32>,
    /// Lines that were not SSE data frames, for glassbox triage.
    pub(crate) non_sse_lines: Vec<String>,
    /// First 200 characters of the generated text, so a reader can see that a
    /// plausible-looking rate came from plausible-looking output.
    pub(crate) text_head: String,
}

/// Turn stamped lines into one trial's numbers. Pure.
pub(crate) fn parse_trial(frames: &[Frame]) -> Trial {
    let mut out = Trial::default();
    let mut text = String::new();
    let mut stamps: Vec<f64> = Vec::new();
    let no_choices: Vec<serde_json::Value> = Vec::new();

    for f in frames {
        let Some(data) = &f.data else {
            if let Some(raw) = &f.raw {
                out.non_sse_lines.push(raw.clone());
            }
            continue;
        };
        if data == "[DONE]" {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(data) else {
            out.non_sse_lines.push(data.clone());
            continue;
        };
        if let Some(m) = v.get("model").and_then(|m| m.as_str()) {
            out.served_model = Some(m.to_string());
        }
        if let Some(p) = v
            .get("usage")
            .and_then(|u| u.get("prompt_tokens"))
            .and_then(|p| p.as_u64())
        {
            out.prompt_tokens = Some(p as u32);
        }
        let mut got_content = false;
        for ch in v
            .get("choices")
            .and_then(|c| c.as_array())
            .unwrap_or(&no_choices)
        {
            if let Some(piece) = ch
                .get("delta")
                .and_then(|d| d.get("content"))
                .and_then(|c| c.as_str())
            {
                if !piece.is_empty() {
                    text.push_str(piece);
                    got_content = true;
                }
            }
            if let Some(r) = ch.get("finish_reason").and_then(|r| r.as_str()) {
                out.finish_reason = Some(r.to_string());
            }
        }
        if got_content {
            stamps.push(f.t_s);
        }
    }

    out.content_frames = stamps.len() as u32;
    out.ttft_s = stamps.first().copied();
    if stamps.len() > 1 {
        out.decode_span_s = stamps[stamps.len() - 1] - stamps[0];
        if out.decode_span_s > 0.0 {
            out.decode_tok_s = (stamps.len() - 1) as f64 / out.decode_span_s;
        }
        out.itl_ms = stamps.windows(2).map(|w| (w[1] - w[0]) * 1000.0).collect();
    }
    out.text_head = text.chars().take(200).collect();
    out
}

// ---------------------------------------------------------------------------
// Placement
// ---------------------------------------------------------------------------

/// The daemon's report of where the primary's weights are, as `/status` states
/// it. Compared before and after the timed run: a mid-run revert to local turns
/// the tail of the measurement into local decode, which is exactly the false
/// result that looks most like a success.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct PlacementSnapshot {
    /// The daemon's own word: `local` | `distributed` | `child-distributed` |
    /// `stream-split` | `forming`.
    pub(crate) mode: String,
    /// Blocks the plan apportions. `0` for a plain local load, which computes
    /// no block plan.
    pub(crate) total_blocks: u32,
    /// Blocks on this node's own GPU.
    pub(crate) local_blocks: u32,
    /// Remote workers holding a share.
    pub(crate) workers: Vec<WorkerSnapshot>,
}

/// One remote worker's share, as `/status` states it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorkerSnapshot {
    /// Raw-TCP rpc-server endpoint, `host:port`.
    pub(crate) endpoint: String,
    /// Blocks pinned onto this worker.
    pub(crate) blocks: u32,
    /// Whether it holds the output head.
    pub(crate) holds_output: bool,
}

/// Turn a live placement into the shard list the measurement key hashes.
///
/// Two properties make this worth its own function:
///
/// **It must agree with `mesh plan`.** The plan side builds its shards from a
/// `DeviceRow` list; this side builds them from what the daemon reports. If the
/// two disagree about node naming or block ranges, every record filed here is
/// unfindable — a store that grows and never answers. The agreement is:
/// contiguous ascending block ranges in the daemon's device order (remote
/// workers first, host last, which is the order `plan_shards_weighted` is
/// called with), a mesh member *name* as the node key, and only the devices
/// that actually hold something.
///
/// **A local load reports no block plan.** `total_blocks` is `0` for a plain
/// local load, so the range comes from the GGUF's own layer count instead —
/// which is what `mesh plan` hashes for the same configuration.
///
/// `resolve` maps an RPC endpoint to the mesh member behind it — name *and*
/// hardware fingerprint, because a shard is identified by both.
///
/// **Every machine carrying weight must be identifiable, or nothing is filed.**
/// A worker whose endpoint resolves to no mesh member, or to a member on a
/// daemon too old to advertise a fingerprint, produces an `Err`. This replaced
/// an endpoint-host fallback that looked forgiving and was not: `mesh plan`
/// builds its shards by walking mesh *members*, so it can never reconstruct a
/// key naming a non-member endpoint. Every record filed through that fallback
/// was write-only — stored, counted, and impossible to look up. Refusing says
/// so at the moment it happens instead.
pub(crate) fn shards_from_placement(
    placement: &PlacementSnapshot,
    host: &NodeIdentity,
    n_layer: u32,
    resolve: &dyn Fn(&str) -> Option<NodeIdentity>,
) -> Result<Vec<mm::PlacementShard>, String> {
    if n_layer == 0 {
        return Err("the model reports zero transformer blocks".to_string());
    }
    if placement.workers.is_empty() {
        return Ok(vec![host.shard(Some((0, n_layer - 1)), true)?]);
    }

    let worker_total: u32 = placement.workers.iter().map(|w| w.blocks).sum();
    let total = if placement.total_blocks > 0 {
        placement.total_blocks
    } else {
        n_layer
    };
    if worker_total + placement.local_blocks != total {
        return Err(format!(
            "placement does not add up: {} worker block(s) + {} local != {total} total",
            worker_total, placement.local_blocks
        ));
    }
    if total != n_layer {
        return Err(format!(
            "placement apportions {total} blocks but the GGUF has {n_layer} — \
             the resident model is not the one whose header was read"
        ));
    }

    let mut shards = Vec::with_capacity(placement.workers.len() + 1);
    let mut next = 0u32;
    for w in &placement.workers {
        let blocks = if w.blocks == 0 {
            None
        } else {
            let range = (next, next + w.blocks - 1);
            next += w.blocks;
            Some(range)
        };
        // A worker holding nothing is not part of the placement. Dropping it
        // keeps the digest describing the machines that carry the model, which
        // is what makes it stable across an idle peer joining or leaving.
        if !carries_weight(w) {
            continue;
        }
        let peer = resolve(&w.endpoint).ok_or_else(|| {
            format!(
                "the worker at {} is carrying part of the model but is not a known mesh \
                 member, so the machine cannot be named in the key",
                endpoint_host(&w.endpoint)
            )
        })?;
        shards.push(peer.shard(blocks, w.holds_output)?);
    }
    let host_holds_output = !placement.workers.iter().any(|w| w.holds_output);
    if placement.local_blocks > 0 || host_holds_output {
        shards.push(host.shard(
            (placement.local_blocks > 0).then_some((next, total - 1)),
            host_holds_output,
        )?);
    }
    Ok(shards)
}

/// A machine that can appear in a placement: what it is called, and what it is.
///
/// Both halves are required to key a measurement. The name alone was the key
/// until 2026-07-29, which meant a peer could replace its GPU and keep every
/// number it had ever filed. See [`mm::PlacementShard::hw`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NodeIdentity {
    pub(crate) name: String,
    pub(crate) hw: Option<u64>,
}

impl NodeIdentity {
    /// This machine's share of a placement, or an error naming it if it never
    /// said what hardware it is.
    ///
    /// The refusal lives here, at the one place a shard is built, so the host
    /// and every worker are held to the same standard by construction rather
    /// than by two call sites remembering to agree.
    fn shard(
        &self,
        blocks: Option<(u32, u32)>,
        holds_output: bool,
    ) -> Result<mm::PlacementShard, String> {
        let hw = self.hw.ok_or_else(|| {
            format!(
                "{} is carrying part of the model but advertises no hardware fingerprint \
                 (a daemon too old to report one), so a measurement filed against it could \
                 not say which machine produced it",
                self.name
            )
        })?;
        Ok(mm::PlacementShard {
            node_key: self.name.clone(),
            hw: Some(hw),
            blocks,
            holds_output,
        })
    }
}

/// Whether a worker is actually part of the placement.
///
/// A worker apportioned no blocks and holding no output head carries none of
/// the model: it changes nothing about how the model decodes, and including it
/// would make the digest depend on which idle peers happened to be online.
///
/// Shared by [`shards_from_placement`] and [`placement_link`] so the digest and
/// the link class always describe the *same set of machines*. Duplicating the
/// rule would let an idle tunnelled peer classify a run as `Tunnel` while
/// contributing nothing to the digest — a key that changes for a machine that
/// is not carrying anything.
fn carries_weight(w: &WorkerSnapshot) -> bool {
    w.blocks > 0 || w.holds_output
}

/// The [`mm::LinkClass`] of a live placement, from the endpoints ggml dialled.
///
/// Reads the same `/status` placement the shards come from, so the link is the
/// one this run actually used rather than the one discovery might pick next
/// time. A local load (no workers carrying weight) is `Local`.
pub(crate) fn placement_link(placement: &PlacementSnapshot) -> mm::LinkClass {
    let links: Vec<mm::LinkClass> = placement
        .workers
        .iter()
        .filter(|w| carries_weight(w))
        .map(|w| mm::link_class_of_endpoint(&w.endpoint))
        .collect();
    mm::LinkClass::summarize(&links)
}

/// `host:port` → `host`. Ports churn across restarts; a digest that included
/// one would miss on every lookup after a worker bounce.
///
/// The colon is not enough to find the port. `fd7a:115c::1` is a bare IPv6
/// address whose final segment is all digits, so a naive `rsplit_once(':')`
/// truncates the address and calls the result a host. Only two forms are
/// unambiguous, and both are left alone otherwise:
///
/// - `[<ipv6>]:port` — the bracketed form, which is what mesh addresses use.
/// - `<host>:port` with exactly one colon — IPv4 or a name.
fn endpoint_host(endpoint: &str) -> String {
    if endpoint.starts_with('[') {
        if let Some(close) = endpoint.rfind(']') {
            return endpoint[..=close].to_string();
        }
        return endpoint.to_string();
    }
    match endpoint.rsplit_once(':') {
        Some((host, port))
            if !host.contains(':')
                && !port.is_empty()
                && port.chars().all(|c| c.is_ascii_digit()) =>
        {
            host.to_string()
        }
        _ => endpoint.to_string(),
    }
}

/// The digest's `mode`, derived from topology rather than from the daemon's
/// mode string.
///
/// The daemon distinguishes `local`, `distributed`, `child-distributed`,
/// `stream-split` and `forming`; `mesh plan` — which has no daemon to ask —
/// only ever produces `local` or `distributed`. Deriving from shard count keeps
/// the two vocabularies in agreement, which is the property that makes a record
/// findable. The daemon's own word is preserved verbatim in the record's
/// `placement_human`, so nothing is hidden from a reader.
pub(crate) fn digest_mode(shards: &[mm::PlacementShard]) -> &'static str {
    if shards.len() <= 1 {
        "local"
    } else {
        "distributed"
    }
}

/// Render a placement the way an operator says it out loud.
pub(crate) fn placement_human(
    shards: &[mm::PlacementShard],
    host_name: &str,
    daemon_mode: &str,
) -> String {
    let blocks_of = |s: &mm::PlacementShard| s.blocks.map_or(0, |(a, b)| b - a + 1);
    let mut parts: Vec<String> = Vec::new();
    if let Some(host) = shards.iter().find(|s| s.node_key == host_name) {
        parts.push(format!("{} local", blocks_of(host)));
    }
    for s in shards.iter().filter(|s| s.node_key != host_name) {
        parts.push(format!("{} @{}", blocks_of(s), s.node_key));
    }
    let body = if parts.is_empty() {
        "unplaced".to_string()
    } else {
        parts.join(" + ")
    };
    // Surface the daemon's own word when it is not the plain one the digest
    // uses, so `child-distributed` never silently reads as `distributed`.
    if daemon_mode != digest_mode(shards) && !daemon_mode.is_empty() {
        format!("{body} ({daemon_mode})")
    } else {
        body
    }
}

mod guards;
mod render;
mod shell;

pub(crate) use guards::*;
pub(crate) use render::*;
pub use shell::cmd_bench;
#[cfg(test)]
use shell::*;

#[cfg(test)]
mod tests;
