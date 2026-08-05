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
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use futures::StreamExt;
use sovereign_core::mesh_measurements as mm;

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
            if !host.contains(':') && !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()) =>
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

// ---------------------------------------------------------------------------
// The guards
// ---------------------------------------------------------------------------

/// Whether the host daemon survived the run.
///
/// Detected from `/status`'s own uptime rather than from `pgrep`: a bare
/// process match hits bash wrappers whose command line merely *contains* the
/// daemon path, and a daemon running on a deleted inode after a rebuild must
/// not count either. Uptime going backwards is unambiguous.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HostLiveness {
    /// Same process throughout.
    Alive,
    /// Uptime went backwards — the daemon died and something restarted it. A
    /// worker's `GGML_ABORT` kills the host process, and this is what that
    /// looks like from outside.
    Restarted,
    /// `/status` stopped answering.
    Gone,
}

/// Everything the validity guards judge.
///
/// Assembled by the shell from real observations, judged here. Every field is
/// data, not a handle — which is what lets all nine guards be exercised in a
/// unit test with no daemon, no peer and no GPU.
pub(crate) struct GuardInput<'a> {
    /// Timed trials, in order.
    pub(crate) trials: &'a [Trial],
    /// The model id occupying the primary slot, per `/status`.
    pub(crate) primary_model_id: &'a str,
    /// Whether `/status` reported the primary slot **resident** immediately
    /// before the timed trials, and again after.
    ///
    /// This is the load-bearing half of the served-slot guard. See
    /// [`evaluate_guards`] for why the SSE `model` field cannot do the job.
    pub(crate) primary_serving_before: bool,
    /// The same reading, after the timed trials.
    pub(crate) primary_serving_after: Option<bool>,
    /// Tokens the canary produced.
    pub(crate) canary_tokens: u32,
    /// Placement read after the canary and before the timed trials.
    pub(crate) placement_before: &'a PlacementSnapshot,
    /// Placement read after the timed trials. `None` when it could not be
    /// re-read at all, which is itself a failure.
    pub(crate) placement_after: Option<&'a PlacementSnapshot>,
    /// `(peer name, online)` for every peer holding a shard, before the run.
    pub(crate) peers_before: &'a [(String, bool)],
    /// The same peers, after.
    pub(crate) peers_after: &'a [(String, bool)],
    /// Whether the daemon survived.
    pub(crate) host_alive_after: HostLiveness,
}

/// Judge a run. Empty means every guard passed.
///
/// Ordered so the most explanatory failure comes first: a dead daemon accounts
/// for every downstream symptom, and an operator reading the list top-down
/// should meet the cause before the consequences.
pub(crate) fn evaluate_guards(g: &GuardInput) -> Vec<String> {
    let mut problems = Vec::new();

    // ── ported guard 6: host alive ──────────────────────────────────────────
    match g.host_alive_after {
        HostLiveness::Alive => {}
        HostLiveness::Restarted => problems.push(
            "the host daemon DIED during the run and was restarted — a worker's GGML_ABORT \
             kills the host process. Everything below describes a broken run, not this \
             configuration."
                .to_string(),
        ),
        HostLiveness::Gone => problems.push(
            "the host daemon stopped answering /status during the run — it died and was not \
             restarted. Everything below describes a broken run, not this configuration."
                .to_string(),
        ),
    }

    // ── ported guard 5: canary first ────────────────────────────────────────
    if g.canary_tokens == 0 {
        problems.push(
            "the canary produced zero tokens — this path is not generating output, so the \
             timed run had nothing to measure."
                .to_string(),
        );
    }

    // ── ported guard 1: which slot served it (the Fast-slot trap) ───────────
    //
    // TWO checks, and the second is the one that works.
    //
    // The SSE `model` field is what the shell script this replaces asserted on,
    // and on this server it is a **verbatim echo of the string the client
    // requested** — every frame of every response says `commonwealth/primary`
    // because that is what was asked for, whatever actually served it. Asserting
    // on it therefore proves only that the request was addressed correctly. It
    // is kept because a client that requests the wrong model IS a real mistake
    // worth catching, but it cannot see a hijack and must never be mistaken for
    // the guard that can. (Measured 2026-07-28: a run against a primary whose
    // compute child was not serving returned ~100 tok/s from a 122B model with
    // this check passing cleanly.)
    //
    // Residency is the signal that attributes. If `/status` says the primary
    // slot was not resident, then whatever produced these tokens was not the
    // model this record names — no matter what the frames claim.
    let served: Vec<&str> = g
        .trials
        .iter()
        .filter_map(|t| t.served_model.as_deref())
        .collect();
    if served.is_empty() {
        problems.push(
            "no `model` field on any SSE frame — the run cannot be attributed to a slot, and \
             an unattributed number cannot be filed against a configuration."
                .to_string(),
        );
    } else if let Some(wrong) = served.iter().find(|m| !names_primary(m, g.primary_model_id)) {
        problems.push(format!(
            "WRONG MODEL REQUESTED: frames name `{wrong}`, but the primary is `{}`.",
            g.primary_model_id
        ));
    }
    if !g.primary_serving_before || g.primary_serving_after == Some(false) {
        problems.push(format!(
            "WRONG SLOT: /status reports the primary (`{}`) was NOT resident during the run, \
             so these tokens came from some other slot — the small always-hot model, or a \
             fallback. A hijacked request returns quickly and successfully and proves nothing; \
             the SSE `model` field cannot see this, because it only echoes what was requested.",
            g.primary_model_id
        ));
    }
    if g.primary_serving_after.is_none() {
        problems.push(
            "could not re-read the primary slot's residency after the run, so there is no \
             evidence the model that answered was still the one this record names."
                .to_string(),
        );
    }

    // ── ported guard 3: placement re-read after ─────────────────────────────
    match g.placement_after {
        None => problems.push(
            "could not re-read the placement after the run, so there is no evidence every \
             timed token crossed the same boundary."
                .to_string(),
        ),
        Some(after) if after != g.placement_before => problems.push(format!(
            "placement changed during the run: {} → {}. Not every timed token was decoded by \
             the configuration this record would be filed under.",
            describe_placement(g.placement_before),
            describe_placement(after)
        )),
        Some(_) => {}
    }

    // ── ported guard 4: peer liveness before and after ──────────────────────
    for (name, online) in g.peers_before {
        if !online {
            problems.push(format!(
                "peer `{name}` holds a shard but was not online when the run started — the \
                 bridge cache can re-mint a known worker with no probe, so discovery keeps \
                 reporting a peer that is already gone."
            ));
        }
    }
    for (name, online) in g.peers_after {
        if !online {
            problems.push(format!(
                "peer `{name}` went offline during the run — the tail of the measurement did \
                 not cross the boundary it claims to."
            ));
        }
    }

    // ── ported guard 2 / new guard 1: enough frames to be a rate ────────────
    if g.trials.is_empty() {
        problems.push("no timed trials completed.".to_string());
    }
    for (i, t) in g.trials.iter().enumerate() {
        if t.content_frames < 2 || t.decode_span_s <= 0.0 {
            problems.push(format!(
                "trial {} produced {} content frame(s) over {:.3}s — a decode rate needs at \
                 least two timestamps to sit between.",
                i + 1,
                t.content_frames,
                t.decode_span_s
            ));
        } else if t.content_frames < MIN_CONTENT_FRAMES {
            problems.push(format!(
                "trial {} produced only {} content frames (floor {MIN_CONTENT_FRAMES}) — too \
                 short to average out scheduler jitter.",
                i + 1,
                t.content_frames
            ));
        }
    }

    // ── new guard 3: the generation actually completed ───────────────────────
    for (i, t) in g.trials.iter().enumerate() {
        match t.finish_reason.as_deref() {
            Some("length") | Some("stop") => {}
            Some(other) => problems.push(format!(
                "trial {} finished with reason `{other}` — only `stop` and `length` are \
                 complete generations; anything else timed a truncated or errored run.",
                i + 1
            )),
            None => problems.push(format!(
                "trial {} never sent a terminal `finish_reason` — the stream ended without \
                 saying it was done, so the last frame may not be the last token.",
                i + 1
            )),
        }
    }

    // ── new guard 2: the machine was in a steady state ──────────────────────
    if let Some(spread) = trial_spread(g.trials) {
        if spread > MAX_TRIAL_SPREAD {
            problems.push(format!(
                "trials disagree by {:.0}% (limit {:.0}%) — this machine was not in a steady \
                 state. Something else was using the GPU, or the model was still warming.",
                spread * 100.0,
                MAX_TRIAL_SPREAD * 100.0
            ));
        }
    }

    problems
}

/// Whether a served model name identifies the primary slot.
///
/// Accepts the alias the request was made under (the server resolves it), the
/// bare `primary`, and any name that contains or is contained by the primary's
/// model id — GGUF stems get suffixed and truncated on the way through the API
/// surface, and a substring match in either direction is what survives that
/// without waving through a *different* model.
fn names_primary(served: &str, primary_model_id: &str) -> bool {
    if served == PRIMARY_ALIAS || served == "primary" {
        return true;
    }
    if primary_model_id.is_empty() {
        return false;
    }
    served.contains(primary_model_id) || primary_model_id.contains(served)
}

/// Relative disagreement between the fastest and slowest trial. `None` with
/// fewer than two trials — one sample cannot disagree with itself, and
/// reporting `0%` there would claim a steadiness that was never tested.
pub(crate) fn trial_spread(trials: &[Trial]) -> Option<f64> {
    if trials.len() < 2 {
        return None;
    }
    let rates: Vec<f64> = trials
        .iter()
        .map(|t| t.decode_tok_s)
        .filter(|r| *r > 0.0)
        .collect();
    if rates.len() < 2 {
        return None;
    }
    let min = rates.iter().copied().fold(f64::INFINITY, f64::min);
    let max = rates.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    (min > 0.0).then(|| (max - min) / min)
}

/// One-line placement description for a guard message.
fn describe_placement(p: &PlacementSnapshot) -> String {
    if p.workers.is_empty() {
        format!("{} ({} local)", p.mode, p.local_blocks)
    } else {
        let w: Vec<String> = p
            .workers
            .iter()
            .map(|w| format!("{}×{}", w.blocks, w.endpoint))
            .collect();
        format!("{} ({} local + {})", p.mode, p.local_blocks, w.join(" + "))
    }
}

// ---------------------------------------------------------------------------
// Aggregation
// ---------------------------------------------------------------------------

/// The numbers a run reports, across its trials.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Aggregate {
    /// Median trial rate — the headline.
    pub(crate) decode_tok_s: f64,
    /// Slowest trial.
    pub(crate) decode_tok_s_min: f64,
    /// Fastest trial.
    pub(crate) decode_tok_s_max: f64,
    /// Median time to first content token.
    pub(crate) ttft_ms: f64,
    /// Median inter-token latency, pooled across trials.
    pub(crate) itl_p50_ms: f64,
    /// 95th percentile of the same, where link jitter shows up.
    pub(crate) itl_p95_ms: f64,
    /// Prefill rate. `Some` only where the server reported real prompt tokens.
    pub(crate) prefill_tok_s: Option<f64>,
    /// Content frames summed across trials.
    pub(crate) content_frames: u32,
    /// Trials contributing.
    pub(crate) trials: u32,
}

/// Reduce trials to the reported numbers. `None` when nothing timed.
///
/// The median, not the mean: a single trial that hit a garbage-collection pause
/// or a background compile should not drag the headline, and with three trials
/// the median is the honest middle. The min and max travel alongside it so the
/// spread is never hidden behind the middle.
pub(crate) fn aggregate(trials: &[Trial]) -> Option<Aggregate> {
    let rates: Vec<f64> = trials
        .iter()
        .map(|t| t.decode_tok_s)
        .filter(|r| *r > 0.0)
        .collect();
    if rates.is_empty() {
        return None;
    }
    let ttfts: Vec<f64> = trials.iter().filter_map(|t| t.ttft_s).collect();
    let itls: Vec<f64> = trials.iter().flat_map(|t| t.itl_ms.iter().copied()).collect();

    // Prefill is a rate only where the SERVER counted the prompt. `None` renders
    // as "n/a", never as an estimate from string length — the exact mistake the
    // deleted `run_baseline_benchmark` made.
    let prefill = {
        let per_trial: Vec<f64> = trials
            .iter()
            .filter_map(|t| match (t.prompt_tokens, t.ttft_s) {
                (Some(p), Some(ttft)) if ttft > 0.0 && p > 0 => Some(p as f64 / ttft),
                _ => None,
            })
            .collect();
        median(&per_trial)
    };

    Some(Aggregate {
        decode_tok_s: median(&rates).unwrap_or(0.0),
        decode_tok_s_min: rates.iter().copied().fold(f64::INFINITY, f64::min),
        decode_tok_s_max: rates.iter().copied().fold(f64::NEG_INFINITY, f64::max),
        ttft_ms: median(&ttfts).map(|s| s * 1000.0).unwrap_or(0.0),
        itl_p50_ms: percentile(&itls, 0.50).unwrap_or(0.0),
        itl_p95_ms: percentile(&itls, 0.95).unwrap_or(0.0),
        prefill_tok_s: prefill,
        content_frames: trials.iter().map(|t| t.content_frames).sum(),
        trials: trials.len() as u32,
    })
}

/// Median of a sample. `None` when empty.
fn median(xs: &[f64]) -> Option<f64> {
    percentile(xs, 0.50)
}

/// Nearest-rank percentile. `None` when empty.
fn percentile(xs: &[f64], q: f64) -> Option<f64> {
    if xs.is_empty() {
        return None;
    }
    let mut v = xs.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let idx = ((v.len() as f64 - 1.0) * q).round() as usize;
    v.get(idx).copied()
}

// ---------------------------------------------------------------------------
// The live mesh, as bench reads it
// ---------------------------------------------------------------------------

/// What `mesh bench` needs from `/v1/mesh/status`.
#[derive(Debug, Clone, Default)]
pub(crate) struct MeshView {
    /// This node's mesh member name — the host's node key in the digest.
    pub(crate) self_name: String,
    /// This node's advertised hardware fingerprint. `None` on a daemon too old
    /// to advertise one, which means no key can be built at all.
    pub(crate) self_hw_fingerprint: Option<u64>,
    /// This node's GPU backend, recorded for display.
    pub(crate) self_backend: Option<String>,
    /// RPC endpoint → the member behind it, via the `rpc_workers` node ids.
    ///
    /// Carries the peer's hardware fingerprint as well as its name: a shard is
    /// keyed on both, and this is the only place the bench learns a *peer's*
    /// hardware (`self_hw_fingerprint` covers only this node).
    pub(crate) endpoint_nodes: HashMap<String, NodeIdentity>,
    /// Member name → online.
    pub(crate) online: HashMap<String, bool>,
    /// Member name → what that machine is, for the record's witness.
    ///
    /// Descriptive only. The digest keys on the *fingerprint*, which is opaque;
    /// this is what lets a reader who did not run the measurement see that the
    /// worker was a 51 GB metal machine rather than the integer
    /// `8092819206175989101`. See [`mm::MachineWitness`].
    pub(crate) machines: HashMap<String, mm::MachineWitness>,
}

impl MeshView {
    /// Parse `/v1/mesh/status`. Tolerant of missing fields: every one of them
    /// has an honest downstream consequence (no fingerprint → no key; no name
    /// → the endpoint host stands in), and none of them is worth failing the
    /// whole command over here.
    pub(crate) fn parse(body: &serde_json::Value) -> Self {
        let mut view = MeshView::default();
        let empty = Vec::new();
        let members = body
            .get("members")
            .and_then(|m| m.as_array())
            .unwrap_or(&empty);
        let mut nodes: HashMap<String, NodeIdentity> = HashMap::new();
        for m in members {
            let name = m
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or("?")
                .to_string();
            let hw = m.get("hw_fingerprint").and_then(|v| v.as_u64());
            if let Some(id) = m.get("node_id").and_then(|n| n.as_str()) {
                nodes.insert(
                    id.to_string(),
                    NodeIdentity {
                        name: name.clone(),
                        hw,
                    },
                );
            }
            view.online.insert(
                name.clone(),
                m.get("status").and_then(|s| s.as_str()) == Some("online"),
            );
            view.machines.insert(
                name.clone(),
                mm::MachineWitness {
                    node_key: name.clone(),
                    vram_gb: m
                        .get("vram_gb")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0)
                        .min(u64::from(u32::MAX)) as u32,
                    backend: m
                        .get("backend")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                },
            );
            if m.get("is_self").and_then(|b| b.as_bool()).unwrap_or(false) {
                view.self_name = name;
                view.self_hw_fingerprint = hw;
                view.self_backend = m
                    .get("backend")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
            }
        }
        for w in body
            .get("rpc_workers")
            .and_then(|w| w.as_array())
            .unwrap_or(&empty)
        {
            let (Some(ep), Some(id)) = (
                w.get("endpoint").and_then(|e| e.as_str()),
                w.get("node_id").and_then(|n| n.as_str()),
            ) else {
                continue;
            };
            if let Some(node) = nodes.get(id) {
                view.endpoint_nodes.insert(ep.to_string(), node.clone());
            }
        }
        view
    }

    /// The witness for a placement, built from the same inputs as its digest.
    ///
    /// `mode` and `total_blocks` must be exactly what
    /// [`mm::placement_digest`] was called with, or the record will carry a
    /// witness that explains some other configuration —
    /// [`mm::PlacementWitness::explains`] is what catches that, and the readers
    /// treat an unfaithful witness as no witness at all.
    ///
    /// Only machines actually named in `shards` are described. A peer that
    /// holds no blocks is not part of this configuration, and describing it
    /// would make the record's explanation depend on who happened to be online
    /// when it was written.
    pub(crate) fn witness(
        &self,
        mode: &str,
        total_blocks: u32,
        shards: &[mm::PlacementShard],
    ) -> mm::PlacementWitness {
        mm::PlacementWitness {
            mode: mode.to_string(),
            total_blocks,
            shards: shards.to_vec(),
            machines: shards
                .iter()
                .filter_map(|s| self.machines.get(&s.node_key).cloned())
                .collect(),
        }
    }
}

/// Parse the primary slot out of `/status`.
///
/// Returns `(model_id, resident, placement)`. `None` when there is no primary
/// slot at all, which is a configuration problem rather than a measurement one.
pub(crate) fn primary_from_status(
    body: &serde_json::Value,
) -> Option<(String, bool, PlacementSnapshot)> {
    let slot = body
        .get("inference")?
        .get("resident")?
        .as_array()?
        .iter()
        .find(|s| s.get("role").and_then(|r| r.as_str()) == Some("primary"))?;

    let model_id = slot
        .get("model_id")
        .and_then(|m| m.as_str())
        .unwrap_or_default()
        .to_string();
    let resident = slot
        .get("resident")
        .and_then(|r| r.as_bool())
        .unwrap_or(false);

    let mut placement = PlacementSnapshot::default();
    if let Some(p) = slot.get("placement") {
        placement.mode = p
            .get("mode")
            .and_then(|m| m.as_str())
            .unwrap_or_default()
            .to_string();
        placement.total_blocks = p.get("total_blocks").and_then(|b| b.as_u64()).unwrap_or(0) as u32;
        placement.local_blocks = p.get("local_blocks").and_then(|b| b.as_u64()).unwrap_or(0) as u32;
        if let Some(ws) = p.get("workers").and_then(|w| w.as_array()) {
            for w in ws {
                placement.workers.push(WorkerSnapshot {
                    endpoint: w
                        .get("endpoint")
                        .and_then(|e| e.as_str())
                        .unwrap_or_default()
                        .to_string(),
                    blocks: w.get("blocks").and_then(|b| b.as_u64()).unwrap_or(0) as u32,
                    holds_output: w
                        .get("holds_output")
                        .and_then(|b| b.as_bool())
                        .unwrap_or(false),
                });
            }
        }
    }
    Some((model_id, resident, placement))
}

/// Is the primary model actually the thing answering right now?
///
/// This is the guard that attributes a run to a slot, and it has to understand
/// **two** hosting modes, because the obvious reading of `/status` is wrong for
/// one of them:
///
/// - **In-process.** `inference.resident[role=primary].resident` is the
///   `ollama ps` analog and says it directly.
/// - **Compute child.** `ComputeRoutedProvider::resident_slots()` forwards the
///   *in-process* engine's view, and the in-process engine never loaded the
///   model — the child did. So a perfectly healthy child-hosted primary reports
///   `resident: false` **forever**. Reading only that field would make a VALID
///   measurement impossible on this configuration, which is a worse failure
///   than the vacuous check it replaced: it would refuse honest runs instead of
///   accepting dishonest ones.
///
/// So a child whose `model_id` matches and whose lifecycle is `serving` counts.
/// `warming` and `starting` deliberately do not — during those the request is
/// answered by something else, which is exactly the case being caught.
pub(crate) fn primary_is_serving(body: &serde_json::Value, primary_model_id: &str) -> bool {
    let Some(inference) = body.get("inference") else {
        return false;
    };
    let in_process = inference
        .get("resident")
        .and_then(|r| r.as_array())
        .is_some_and(|slots| {
            slots.iter().any(|s| {
                s.get("role").and_then(|r| r.as_str()) == Some("primary")
                    && s.get("resident").and_then(|r| r.as_bool()) == Some(true)
            })
        });
    if in_process {
        return true;
    }
    inference
        .get("compute_children")
        .and_then(|c| c.as_array())
        .is_some_and(|kids| {
            kids.iter().any(|k| {
                k.get("model_id").and_then(|m| m.as_str()) == Some(primary_model_id)
                    && k.get("lifecycle").and_then(|l| l.as_str()) == Some("serving")
            })
        })
}

/// The reason the primary's compute children have all given up, if they have.
///
/// The canary waits out a cold load, which on a large model legitimately takes
/// minutes. But "not serving yet" and "will never serve" look identical from
/// the residency field alone, and the daemon already knows the difference: a
/// child that has failed says so, with the reason it exited.
///
/// Without this the bench spends `CANARY_ATTEMPTS × CANARY_RETRY` — ten minutes
/// — printing "This is the cold load, not a failure" at an operator whose
/// `/status` has been saying `lifecycle: "failed", last_exit: "no eligible RPC
/// workers"` the whole time. Observed on RuggedFox 2026-07-29. Telling someone
/// to keep waiting for something that already failed is the opposite of what
/// this command is for.
///
/// `None` — keep waiting — in every case that is not unambiguously terminal:
///
/// - **No children at all.** The primary is in-process; there is nothing here
///   to have failed, and residency is the only signal.
/// - **Any replica not `failed`.** `starting`, `warming` and `restarting` are
///   the cold load itself; `serving` and `degraded` are answering. A pool with
///   one dead replica and one live one is not a dead end.
///
/// Only when every replica backing this model has failed is the wait pointless.
pub(crate) fn primary_children_failed(
    body: &serde_json::Value,
    primary_model_id: &str,
) -> Option<String> {
    let kids: Vec<&serde_json::Value> = body
        .get("inference")?
        .get("compute_children")?
        .as_array()?
        .iter()
        .filter(|k| k.get("model_id").and_then(|m| m.as_str()) == Some(primary_model_id))
        .collect();
    if kids.is_empty() {
        return None;
    }
    if !kids
        .iter()
        .all(|k| k.get("lifecycle").and_then(|l| l.as_str()) == Some("failed"))
    {
        return None;
    }
    // The child's own words. `last_exit` is why it died; `last_transition_reason`
    // is why it moved — prefer the former and fall back, so the operator gets
    // the daemon's account rather than this command's paraphrase of it.
    let reason = kids.iter().find_map(|k| {
        k.get("last_exit")
            .and_then(|r| r.as_str())
            .or_else(|| k.get("last_transition_reason").and_then(|r| r.as_str()))
            .filter(|r| !r.is_empty())
    });
    Some(reason.unwrap_or("no reason reported").to_string())
}

/// Daemon uptime in seconds, for the liveness comparison.
pub(crate) fn uptime_from_status(body: &serde_json::Value) -> Option<u64> {
    body.get("process")?.get("uptime_seconds")?.as_u64()
}

/// Daemon resident-set size in MB, for [`mm::RunConditions`].
pub(crate) fn rss_mb_from_status(body: &serde_json::Value) -> Option<u64> {
    body.get("process")?.get("rss_mb")?.as_u64()
}

/// Roles resident **alongside** the primary, sorted, for [`mm::RunConditions`].
///
/// The measured model is excluded on purpose: it is the thing being measured,
/// not something competing with it. Everything else that reports
/// `resident: true` holds memory and can take GPU time during the trials, which
/// is the whole reason to record this.
///
/// Excluded on **two** grounds, because the role name alone is not enough:
///
/// - `role == "primary"`, the obvious case.
/// - Any role whose `model_id` equals the primary's. When `[models].fast` is
///   absent, `fast_path()` falls back to the primary GGUF and `/status` reports
///   a `fast` slot holding the *same model* — observed live 2026-07-29 with
///   `fast` and `primary` both `Qwen3.6-35B-A3B-MTP-UD-Q6_K`. Filtering by name
///   alone would have recorded the measured model as its own co-resident and
///   made an evicted-slot run look like a co-resident one, quietly inverting the
///   experiment this field exists to support.
///
/// Read from the in-process `inference.resident` array only. A compute child is
/// deliberately not counted: the primary is precisely what runs there in the
/// child-hosted mode (see [`primary_is_serving`]), so counting children would
/// list the measured model as its own co-resident by the other route.
pub(crate) fn co_resident_roles(
    body: &serde_json::Value,
    primary_model_id: &str,
) -> Vec<String> {
    let mut roles: Vec<String> = body
        .get("inference")
        .and_then(|i| i.get("resident"))
        .and_then(|r| r.as_array())
        .map(|slots| {
            slots
                .iter()
                .filter(|s| s.get("resident").and_then(|r| r.as_bool()) == Some(true))
                // An alias of the measured model, under any role name.
                .filter(|s| {
                    s.get("model_id").and_then(|m| m.as_str()) != Some(primary_model_id)
                })
                .filter_map(|s| s.get("role").and_then(|r| r.as_str()))
                .filter(|role| *role != "primary")
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    roles.sort();
    roles.dedup();
    roles
}

/// Judge liveness from two uptime readings.
///
/// `after == None` means `/status` stopped answering. Uptime going backwards
/// means a different process is answering now.
pub(crate) fn liveness(before: Option<u64>, after: Option<u64>) -> HostLiveness {
    match (before, after) {
        (_, None) => HostLiveness::Gone,
        (Some(b), Some(a)) if a < b => HostLiveness::Restarted,
        _ => HostLiveness::Alive,
    }
}

// ---------------------------------------------------------------------------
// The shell
// ---------------------------------------------------------------------------

/// Parsed `mesh bench` arguments.
struct BenchArgs {
    /// The optional assertion: this GGUF must be what is resident.
    assert_model: Option<PathBuf>,
    trials: u32,
    json: bool,
    history: bool,
}

/// `svrn mesh bench [<model.gguf>] [--trials <n>] [--json] [--history]`
pub(crate) async fn cmd_bench(args: &[String]) -> i32 {
    let mut parsed = BenchArgs {
        assert_model: None,
        trials: 3,
        json: false,
        history: false,
    };
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--trials" => {
                i += 1;
                match args.get(i).and_then(|s| s.parse::<u32>().ok()) {
                    Some(n) if (1..=20).contains(&n) => parsed.trials = n,
                    _ => {
                        eprintln!("--trials: a count from 1 to 20 (default 3)");
                        return 2;
                    }
                }
            }
            "--json" => parsed.json = true,
            "--history" => parsed.history = true,
            "--help" | "-h" => {
                sovereign_cli_shared::help::print(&HELP_MESH_BENCH);
                return 0;
            }
            s if parsed.assert_model.is_none() && !s.starts_with('-') => {
                parsed.assert_model = Some(PathBuf::from(s));
            }
            other => {
                eprintln!("Unknown arg: {other}");
                return 2;
            }
        }
        i += 1;
    }
    run_bench(parsed).await
}

/// Exit codes, so a script can branch without parsing prose.
mod exit {
    /// A valid measurement was taken and recorded.
    pub(super) const OK: i32 = 0;
    /// The run completed but tripped a guard. The record is written for
    /// glassbox and will never be served back.
    pub(super) const INVALID: i32 = 1;
    /// The named GGUF is not what is resident, or the daemon and the config
    /// disagree about which model that is.
    pub(super) const ASSERTION: i32 = 3;
    /// No key could be constructed, so nothing could be filed. Nothing ran.
    pub(super) const NO_KEY: i32 = 4;
    /// The daemon is not reachable.
    pub(super) const NO_DAEMON: i32 = 5;
}

async fn run_bench(args: BenchArgs) -> i32 {
    let cfg = match sovereign_core::setup_config::SetupConfig::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("could not read the configuration: {e}");
            return exit::NO_KEY;
        }
    };
    let port = cfg.daemon.client_port;
    let n_ctx = cfg.models.effective_context_size();
    let primary_path = cfg.models.primary.clone();

    // ── the model, from the config the daemon loads from ───────────────────
    let (n_layer, sizes) = match read_model_header(&primary_path) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{e}");
            return exit::NO_KEY;
        }
    };
    let fingerprint = mm::model_fingerprint(&sizes, n_layer);

    // ── the assertion, if one was made ─────────────────────────────────────
    if let Some(asserted) = &args.assert_model {
        let (a_layer, a_sizes) = match read_model_header(asserted) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("{e}");
                return exit::ASSERTION;
            }
        };
        let asserted_fp = mm::model_fingerprint(&a_sizes, a_layer);
        if asserted_fp != fingerprint {
            eprintln!(
                "ASSERTION FAILED — the model you named is not the one this daemon runs.\n\
                 \n  you named   {}\n              {asserted_fp}\n\
                 \n  configured  {}\n              {fingerprint}\n\
                 \n`mesh bench` measures the configuration you are RUNNING; it never loads one.\n\
                 To measure the model you named, point the config at it and restart the daemon:\n\
                 \n    [models]\n    primary = \"{}\"\n",
                asserted.display(),
                primary_path.display(),
                asserted.display()
            );
            return exit::ASSERTION;
        }
    }

    // ── history, if that is all that was asked for ─────────────────────────
    // Answered before any HTTP: "what has this machine measured" is a read of a
    // local file, and making it depend on a running daemon would deny the
    // operator their own recorded history exactly when the daemon is the thing
    // that is broken.
    if args.history {
        return show_history(&sizes, n_layer);
    }

    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("http client: {e}");
            return exit::NO_DAEMON;
        }
    };

    // ── the mesh, for the host's identity and the peers' names ─────────────
    let mesh_body = match get_json(&client, port, "/v1/mesh/status").await {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{e}");
            return exit::NO_DAEMON;
        }
    };
    let mesh = MeshView::parse(&mesh_body);
    let Some(host) = mm::HostIdentity::from_live_mesh(mesh.self_hw_fingerprint) else {
        eprintln!(
            "This daemon advertises no hardware fingerprint, so there is no key under which a\n\
             measurement could be filed — and a record filed under a placeholder would be served\n\
             back on somebody else's machine.\n\n\
             Nothing was measured. Restart the daemon on a build that advertises one:\n\
             \n    systemctl --user restart sovereign.service\n"
        );
        return exit::NO_KEY;
    };

    // ── the slot, before anything is fired ─────────────────────────────────
    let status_body = match get_json(&client, port, "/status").await {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{e}");
            return exit::NO_DAEMON;
        }
    };
    let uptime_before = uptime_from_status(&status_body);
    let Some((primary_model_id, resident_before, _)) = primary_from_status(&status_body) else {
        eprintln!("no `primary` slot in /status — this daemon has no primary model configured.");
        return exit::NO_KEY;
    };
    let config_stem = primary_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    if !primary_model_id.is_empty() && primary_model_id != config_stem {
        eprintln!(
            "The running daemon and the configuration disagree about the primary model.\n\
             \n  /status says  {primary_model_id}\n  config says   {config_stem}\n\
             \nThe header read for the fingerprint came from the config, so a record filed now\n\
             would describe a model that is not the one being measured. Restart the daemon to\n\
             pick up the config, or point this config line at what is actually loaded:\n\
             \n    [models]\n    primary = \"{}\"\n",
            primary_path.display()
        );
        return exit::ASSERTION;
    }

    // ── the canary (which also absorbs a cold load) ────────────────────────
    eprintln!(
        "Measuring the running configuration. This is not instant — a cold {} load alone can\n\
         take minutes, and {} timed trials follow it.\n",
        config_stem, args.trials
    );
    if !resident_before {
        eprintln!("The primary slot is idle-unloaded; the canary will pay for a cold load.");
    }
    eprintln!("[1/{}] canary ({CANARY_MAX_TOKENS} tokens) …", args.trials + 1);
    let canary_start = Instant::now();
    // Retried, bounded, and only while the daemon says it is still coming up.
    //
    // This is NOT retry-until-lucky — the thing being waited out is a lazy slot
    // loading, which is the cold-load path the whole command is built around. A
    // canary that fires into "child not serving (starting)" and gives up leaves
    // the timed trials to be answered by whatever else picks them up, which is
    // precisely the false result the canary exists to prevent. Any other error,
    // and any exhausted deadline, falls through to the guards.
    let mut canary_tokens = 0u32;
    let mut canary_err = None;
    for attempt in 1..=CANARY_ATTEMPTS {
        let (tokens, err) = canary(port).await;
        canary_tokens = tokens;
        canary_err = err;

        // Tokens flowing is NOT the condition to stop on. While the primary is
        // still coming up, something else answers — that is the whole hijack —
        // so a canary that stopped at "I got tokens" would hand the timed
        // trials to the wrong slot and produce a run the guards then have to
        // throw away. Wait for the model we came to measure.
        let status = get_json(&client, port, "/status").await.ok();
        let serving = status
            .as_ref()
            .is_some_and(|b| primary_is_serving(b, &primary_model_id));
        if serving && tokens > 0 {
            break;
        }
        // The daemon already knows this will never come up. Waiting out the
        // remaining attempts would spend ten minutes calling a failure a cold
        // load; the guards downstream still record the run as invalid, they
        // just get to do it now and with the child's own reason attached.
        if let Some(reason) =
            status.as_ref().and_then(|b| primary_children_failed(b, &primary_model_id))
        {
            eprintln!(
                "      the primary's compute child has FAILED — not a cold load: {reason}\n      \
                 Not waiting out the remaining {} attempt(s); nothing can serve this model \
                 until that child starts.",
                CANARY_ATTEMPTS - attempt
            );
            canary_err = Some(format!("primary compute child failed: {reason}"));
            break;
        }
        let coming_up = !serving || canary_err.as_deref().is_some_and(slot_still_starting);
        if !coming_up || attempt == CANARY_ATTEMPTS {
            break;
        }
        eprintln!(
            "      the primary is not serving yet (attempt {attempt}/{CANARY_ATTEMPTS}) — \
             waiting {}s. This is the cold load, not a failure.",
            CANARY_RETRY.as_secs()
        );
        tokio::time::sleep(CANARY_RETRY).await;
    }
    let canary_elapsed = canary_start.elapsed().as_secs_f64();
    // A cold load is attributed to the canary only when the slot was actually
    // cold. Attributing it unconditionally would report a warm run's canary
    // latency as a load time, which is a number nobody can act on.
    let cold_load_s = (!resident_before).then_some(canary_elapsed);
    if let Some(e) = &canary_err {
        eprintln!("      canary error: {e}");
    }
    eprintln!(
        "      canary produced {canary_tokens} token(s) in {canary_elapsed:.1}s{}",
        if cold_load_s.is_some() {
            " (includes the cold load)"
        } else {
            ""
        }
    );

    // ── the placement, read AFTER the load so it is real ───────────────────
    let after_canary = match get_json(&client, port, "/status").await {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{e}");
            return exit::NO_DAEMON;
        }
    };
    let placement_before = primary_from_status(&after_canary)
        .map(|(_, _, p)| p)
        .unwrap_or_default();
    let primary_serving_before = primary_is_serving(&after_canary, &primary_model_id);
    if !primary_serving_before {
        eprintln!(
            "      WARNING: /status still reports the primary NOT resident. Anything that \
             answers now is a different slot; the run will be recorded INVALID."
        );
    }

    let host_identity = NodeIdentity {
        name: mesh.self_name.clone(),
        hw: mesh.self_hw_fingerprint,
    };
    let shards = match shards_from_placement(&placement_before, &host_identity, n_layer, &|ep| {
        mesh.endpoint_nodes.get(ep).cloned()
    }) {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "cannot describe the running placement, so no record can be filed: {e}\n\
                 Nothing was measured."
            );
            return exit::NO_KEY;
        }
    };
    let human = placement_human(&shards, &mesh.self_name, &placement_before.mode);
    eprintln!("      placement: {human}\n");

    let peers_before = peer_liveness(&shards, &mesh);

    // ── the timed trials ───────────────────────────────────────────────────
    // Wall clock across the trials, for `RunConditions::run_span_s`. Not a
    // performance figure — a cross-check: two runs of the same trial count whose
    // spans differ sharply were not taken under the same load.
    let trials_started = std::time::Instant::now();
    let mut trials: Vec<Trial> = Vec::with_capacity(args.trials as usize);
    for n in 0..args.trials {
        eprintln!("[{}/{}] timed trial …", n + 2, args.trials + 1);
        let frames = match timed_trial(port).await {
            Ok(f) => f,
            Err(e) => {
                eprintln!("      trial failed: {e}");
                Vec::new()
            }
        };
        let t = parse_trial(&frames);
        eprintln!(
            "      {:.2} tok/s · {} frames · TTFT {:.2}s · finish={}",
            t.decode_tok_s,
            t.content_frames,
            t.ttft_s.unwrap_or(0.0),
            t.finish_reason.as_deref().unwrap_or("none")
        );
        for line in &t.non_sse_lines {
            eprintln!("      non-SSE line: {}", &line[..line.len().min(160)]);
        }
        trials.push(t);
    }

    // ── the after-state ────────────────────────────────────────────────────
    let after_body = get_json(&client, port, "/status").await.ok();
    let uptime_after = after_body.as_ref().and_then(uptime_from_status);
    let primary_serving_after = after_body
        .as_ref()
        .map(|b| primary_is_serving(b, &primary_model_id));
    let placement_after = after_body
        .as_ref()
        .and_then(|b| primary_from_status(b).map(|(_, _, p)| p));
    let mesh_after = get_json(&client, port, "/v1/mesh/status").await.ok();
    let peers_after = match &mesh_after {
        Some(b) => peer_liveness(&shards, &MeshView::parse(b)),
        // Unreadable mesh state is not evidence of health. Reporting every peer
        // as offline is the honest reading: we cannot say they were up.
        None => shards
            .iter()
            .filter(|s| s.node_key != mesh.self_name)
            .map(|s| (s.node_key.clone(), false))
            .collect(),
    };

    let problems = evaluate_guards(&GuardInput {
        trials: &trials,
        primary_model_id: &primary_model_id,
        primary_serving_before,
        primary_serving_after,
        canary_tokens,
        placement_before: &placement_before,
        placement_after: placement_after.as_ref(),
        peers_before: &peers_before,
        peers_after: &peers_after,
        host_alive_after: liveness(uptime_before, uptime_after),
    });
    let verdict = if problems.is_empty() {
        mm::Verdict::Valid
    } else {
        mm::Verdict::Invalid {
            problems: problems.clone(),
        }
    };

    // ── the record ─────────────────────────────────────────────────────────
    let agg = aggregate(&trials).unwrap_or(Aggregate {
        decode_tok_s: 0.0,
        decode_tok_s_min: 0.0,
        decode_tok_s_max: 0.0,
        ttft_ms: 0.0,
        itl_p50_ms: 0.0,
        itl_p95_ms: 0.0,
        prefill_tok_s: None,
        content_frames: 0,
        trials: trials.len() as u32,
    });
    let nodes = shards.len() as u32;
    // Classified from the placement this run measured, not from what discovery
    // might choose next time — the number belongs to the link it was taken on.
    let link = placement_link(&placement_before);
    // Built from the same three values the digest is, on the next line, so the
    // witness cannot drift from the key it explains.
    let witness = mesh.witness(digest_mode(&shards), n_layer, &shards);
    let key = mm::MeasurementKey::for_plan(
        host,
        fingerprint,
        mm::placement_digest(digest_mode(&shards), n_layer, &shards),
        n_ctx,
        link,
    );
    debug_assert!(
        witness.explains(&key.placement_digest),
        "the witness and the key were built from different inputs"
    );
    // Read from the two `/status` bodies already in hand — the before-poll and
    // the after-poll — so recording the conditions costs no extra round trip and
    // cannot itself perturb what it is measuring.
    let conditions = mm::RunConditions {
        co_resident_roles: co_resident_roles(&status_body, &primary_model_id),
        host_rss_mb_before: rss_mb_from_status(&status_body),
        host_rss_mb_after: after_body.as_ref().and_then(rss_mb_from_status),
        host_uptime_s: uptime_before,
        run_span_s: Some(trials_started.elapsed().as_secs_f64()),
        // Filtered by `carries_weight` — the same decider `shards_from_placement`
        // uses — so the recorded routes are exactly the machines the placement
        // names, and an idle peer cannot add a route that carried nothing.
        rpc_endpoints: placement_before
            .workers
            .iter()
            .filter(|w| carries_weight(w))
            .map(|w| w.endpoint.clone())
            .collect(),
    };
    let record = mm::MeasurementRecord {
        key: key.clone(),
        witness: Some(witness),
        conditions: Some(conditions),
        decode_tok_s: agg.decode_tok_s,
        decode_tok_s_min: agg.decode_tok_s_min,
        decode_tok_s_max: agg.decode_tok_s_max,
        ttft_ms: agg.ttft_ms,
        itl_p50_ms: agg.itl_p50_ms,
        itl_p95_ms: agg.itl_p95_ms,
        prefill_tok_s: agg.prefill_tok_s,
        cold_load_s,
        trials: agg.trials,
        content_frames: agg.content_frames,
        model_name: config_stem.to_string(),
        placement_human: human.clone(),
        nodes,
        hops: nodes.saturating_sub(1),
        measured_at: now_unix(),
        build: env!("CARGO_PKG_VERSION").to_string(),
        backend: mesh.self_backend.clone(),
        // Still `None`, and `None` is the honest value — not zero, which would
        // read as "no latency". Checked 2026-07-30: iroh 1.0 does not expose a
        // per-peer RTT on `remote_info` (the RTT machinery in
        // `socket/biased_rtt_path_selector.rs` is internal), so
        // `MeshIrohAccess::peer_path_on` can classify the path but not time it.
        // The two candidates are quinn connection stats, reachable only where a
        // connection is held (daemon-side, and only if one exists), or an
        // explicit timed round trip to the peer — which would measure the link
        // PLUS the peer's own request handling, and must not be filed under a
        // field named `link_rtt_ms` as though it were link latency alone.
        // Until one is built, `RunConditions` carries what can be observed
        // honestly instead.
        link_rtt_ms: None,
        verdict: verdict.clone(),
    };

    let mut file = mm::load();
    mm::record(&mut file, record.clone());
    let store_note = match mm::save(&file) {
        Ok(()) => match mm::store_path() {
            Some(p) => format!("recorded in {}", p.display()),
            None => "not recorded (SOVEREIGN_MESH_MEASUREMENTS=0)".to_string(),
        },
        Err(e) => format!("NOT recorded — writing the store failed: {e}"),
    };

    // Disk first, mesh second, and in that order for a reason: the local record
    // is the authoritative copy and the gossip buffer is a wire buffer the daemon
    // rebuilds from this file at every boot. So publishing can fail without
    // anything being lost, and `mesh bench` still works with no daemon at all.
    let travel = crate::mesh_travel::publish(&record).await;

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&render_bench_json(&record, &store_note, &travel))
                .unwrap_or_default()
        );
    } else {
        print!("{}", render_bench_human(&record, &store_note, &travel));
    }
    if verdict.is_valid() {
        exit::OK
    } else {
        exit::INVALID
    }
}

/// Read a GGUF's block count and tensor table — the header parse both the
/// fingerprint and `mesh plan` are built on.
fn read_model_header(path: &Path) -> Result<(u32, Vec<(String, Option<u32>, u64)>), String> {
    use sovereign_inference::embedded as inf;
    let n_layer = match inf::gguf_block_count(path) {
        Ok(Some(n)) if n > 0 => n,
        Ok(_) => {
            return Err(format!(
                "could not read a positive block_count from {} (not a GGUF, or missing \
                 <arch>.block_count)",
                path.display()
            ))
        }
        Err(e) => return Err(format!("reading {}: {e}", path.display())),
    };
    let sizes = inf::tensor_sizes(path)
        .map_err(|e| format!("reading tensor table from {}: {e}", path.display()))?;
    Ok((n_layer, sizes))
}

/// Which shard-holding peers were online. The host itself is excluded — its
/// liveness is the `HostLiveness` check, not this one.
pub(crate) fn peer_liveness(shards: &[mm::PlacementShard], mesh: &MeshView) -> Vec<(String, bool)> {
    shards
        .iter()
        .filter(|s| s.node_key != mesh.self_name)
        .map(|s| {
            (
                s.node_key.clone(),
                mesh.online.get(&s.node_key).copied().unwrap_or(false),
            )
        })
        .collect()
}

/// Seconds since the epoch. `0` if the clock is before it, which no comparison
/// in this module treats as meaningful.
fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// GET a JSON endpoint off the local daemon.
async fn get_json(
    client: &reqwest::Client,
    port: u16,
    path: &str,
) -> Result<serde_json::Value, String> {
    let url = format!("http://127.0.0.1:{port}{path}");
    let resp = client.get(&url).send().await.map_err(|e| {
        format!(
            "daemon at {url} not reachable: {e}\n  hint: start it with `svrn daemon start` \
             (or `systemctl --user start sovereign.service`)"
        )
    })?;
    if !resp.status().is_success() {
        return Err(format!("daemon returned HTTP {} from {url}", resp.status()));
    }
    resp.json()
        .await
        .map_err(|e| format!("bad JSON from {url}: {e}"))
}

/// Fire the canary. Returns `(completion_tokens, error)`.
///
/// Non-streaming and tiny on purpose: it proves tokens flow at all before the
/// timing window is spent, and if a bad worker is going to abort the host it
/// happens here, with a clean attribution, rather than inside a timed trial.
async fn canary(port: u16) -> (u32, Option<String>) {
    // Its own client with a long timeout: this request pays for the cold load
    // of a model that may be tens of gigabytes across a mesh, which the 20s
    // status client would abandon halfway through.
    let long = match reqwest::Client::builder()
        .timeout(Duration::from_secs(1800))
        .build()
    {
        Ok(c) => c,
        Err(e) => return (0, Some(format!("http client: {e}"))),
    };
    let body = serde_json::json!({
        "model": PRIMARY_ALIAS,
        "max_tokens": CANARY_MAX_TOKENS,
        "temperature": 0,
        "messages": [{"role": "user", "content": "Say hello."}],
    });
    let resp = match long
        .post(format!("http://127.0.0.1:{port}/v1/chat/completions"))
        .json(&body)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return (0, Some(e.to_string())),
    };
    let v: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => return (0, Some(format!("bad canary JSON: {e}"))),
    };
    let tokens = v
        .get("usage")
        .and_then(|u| u.get("completion_tokens"))
        .and_then(|c| c.as_u64())
        .unwrap_or(0) as u32;
    let err = v
        .get("error")
        .map(|e| e.to_string())
        .or_else(|| (tokens == 0).then(|| "no completion_tokens in the response".to_string()));
    (tokens, err)
}

/// Fire one timed streaming completion and stamp every line as it arrives.
///
/// The timer starts before the request is sent, so time-to-first-token includes
/// prefill and tunnel setup — which is the honest reading of "how long until it
/// starts answering" and is reported separately from the decode rate rather
/// than smeared into it.
async fn timed_trial(port: u16) -> Result<Vec<Frame>, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(600))
        .build()
        .map_err(|e| format!("http client: {e}"))?;
    let body = serde_json::json!({
        "model": PRIMARY_ALIAS,
        "max_tokens": PROBE_MAX_TOKENS,
        "temperature": 0,
        "stream": true,
        "messages": [{"role": "user", "content": PROBE_PROMPT}],
    });

    let t0 = Instant::now();
    let resp = client
        .post(format!("http://127.0.0.1:{port}/v1/chat/completions"))
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        let code = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(format!("HTTP {code}: {}", &text[..text.len().min(300)]));
    }

    let mut frames = Vec::new();
    let mut buf = String::new();
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("stream broke: {e}"))?;
        let t = t0.elapsed().as_secs_f64();
        buf.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(pos) = buf.find('\n') {
            let line: String = buf.drain(..=pos).collect();
            let line = line.trim_end();
            if line.trim().is_empty() {
                continue;
            }
            frames.push(Frame::from_line(t, line));
        }
    }
    let tail = buf.trim().to_string();
    if !tail.is_empty() {
        frames.push(Frame::from_line(t0.elapsed().as_secs_f64(), &tail));
    }
    Ok(frames)
}

/// A placement digest shortened for a table column.
///
/// Disambiguation only — never an identity. Two runs whose abbreviations differ
/// definitely measured different configurations; two whose abbreviations match
/// should be compared on the full digest before anyone calls them one spread.
pub(crate) fn abbreviated_digest(digest: &str) -> String {
    match digest.split_once(':') {
        Some((tag, hash)) => format!("{tag}:{}", &hash[..hash.len().min(8)]),
        None => digest.chars().take(12).collect(),
    }
}

/// Whether any two runs share a placement *description* while sitting under
/// different placement *keys*.
///
/// Pairwise across the whole set, not adjacent rows: the store is ordered by
/// key then by time, so two rows describing the same split under different
/// digests are usually separated by other rows.
pub(crate) fn has_ambiguous_placement(rows: &[&mm::MeasurementRecord]) -> bool {
    rows.iter().enumerate().any(|(i, a)| {
        rows[i + 1..].iter().any(|b| {
            a.placement_human == b.placement_human
                && a.key.placement_digest != b.key.placement_digest
        })
    })
}

/// `--history`: every run filed for this model, invalid ones included.
///
/// Filtered by model fingerprint rather than by the full key on purpose — an
/// operator asking "what has this machine measured" wants the other splits too,
/// which is exactly the comparison that decides whether to move the host role.
fn show_history(sizes: &[(String, Option<u32>, u64)], n_layer: u32) -> i32 {
    let file = mm::load();
    let fp = mm::model_fingerprint(sizes, n_layer);
    let rows: Vec<&mm::MeasurementRecord> = file
        .records()
        .iter()
        .filter(|r| r.key.model_fingerprint == fp)
        .collect();
    if rows.is_empty() {
        println!(
            "No runs recorded for this model on this machine yet.\n\n  \
             Take one:  svrn mesh bench\n"
        );
        return exit::OK;
    }
    println!("Runs recorded for this model ({} of them):\n", rows.len());
    for r in &rows {
        let verdict = match &r.verdict {
            mm::Verdict::Valid => "VALID  ".to_string(),
            mm::Verdict::Invalid { problems } => format!("INVALID ({} problem(s))", problems.len()),
        };
        println!(
            "  {verdict}  {:>7.2} tok/s   {:<28}  {} trial(s)  build {}  {}",
            r.decode_tok_s,
            r.placement_human,
            r.trials,
            r.build,
            abbreviated_digest(&r.key.placement_digest),
        );
        // The conditions, indented under their run. This is the line that makes
        // two rows at different rates comparable — or shows that they are not.
        if let Some(line) = r.conditions.as_ref().and_then(|c| c.describe()) {
            println!("      · {line}");
        } else {
            println!("      · conditions not recorded (run predates 2026-07-30)");
        }
        if let mm::Verdict::Invalid { problems } = &r.verdict {
            for p in problems {
                println!("      ! {p}");
            }
        }
    }
    println!(
        "\n  Only VALID runs on the exact split being planned are served back to `mesh plan`;\n  \
         the others are here so the failure is inspectable."
    );
    // Earned the hard way: two runs 69 minutes apart both rendered "36 local +
    // 12 @BeefyMac" while sitting under DIFFERENT placement digests, and a
    // reader compared them as one configuration and filed a 26% variance that
    // was never real. The human string is a description, not an identity, so the
    // key it belongs to is now on the row beside it.
    if has_ambiguous_placement(&rows) {
        println!(
            "  NOTE: two runs above share a placement description but sit under different\n  \
             keys (the trailing pd2: tag). They measured different configurations and must\n  \
             not be compared as a spread."
        );
    }
    if let Some(p) = mm::store_path() {
        println!("  store: {}", p.display());
    }
    exit::OK
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// The machine-readable run.
pub(crate) fn render_bench_json(
    r: &mm::MeasurementRecord,
    store_note: &str,
    travel: &crate::mesh_travel::Published,
) -> serde_json::Value {
    let (verdict, problems) = match &r.verdict {
        mm::Verdict::Valid => ("valid", Vec::new()),
        mm::Verdict::Invalid { problems } => ("invalid", problems.clone()),
    };
    serde_json::json!({
        "verdict": verdict,
        "problems": problems,
        "store": store_note,
        // Whether this run reached the mesh, kept separate from `store` because
        // they can and do differ: a record is written to disk before it is
        // published, and a consumer that conflates them would report data loss
        // for a daemon that was merely not running.
        "travel": travel.as_json(),
        "key": {
            "probe_version": r.key.probe_version,
            "model_fingerprint": r.key.model_fingerprint,
            "placement_digest": r.key.placement_digest,
            "host_hw_fingerprint": r.key.host_hw_fingerprint,
            "n_ctx": r.key.n_ctx,
            "link": r.key.link.as_str(),
        },
        "model": r.model_name,
        "placement": r.placement_human,
        // The inputs `placement_digest` was computed from, so a consumer can
        // say what changed when a key changes. Null on a record filed before
        // 2026-07-30, when only the hash was kept.
        "witness": r.witness,
        "split": r.witness.as_ref().map(|w| w.describe_split()),
        // What else was true of the box while this ran. Null on a record filed
        // before 2026-07-30 — which means "not recorded", NOT "the box was
        // quiet"; a consumer that reads the absence as quiet has invented a
        // condition nobody observed.
        "conditions": r.conditions,
        "nodes": r.nodes,
        "hops": r.hops,
        "decode_tok_s": r.decode_tok_s,
        "decode_tok_s_min": r.decode_tok_s_min,
        "decode_tok_s_max": r.decode_tok_s_max,
        "ttft_ms": r.ttft_ms,
        "itl_p50_ms": r.itl_p50_ms,
        "itl_p95_ms": r.itl_p95_ms,
        // Null, never a number, when the server did not count the prompt. A
        // consumer must handle the absence rather than divide by a fabrication.
        "prefill_tok_s": r.prefill_tok_s,
        "cold_load_s": r.cold_load_s,
        "link_rtt_ms": r.link_rtt_ms,
        "trials": r.trials,
        "content_frames": r.content_frames,
        "backend": r.backend,
        "build": r.build,
        "measured_at": r.measured_at,
    })
}

/// The human-readable run.
pub(crate) fn render_bench_human(
    r: &mm::MeasurementRecord,
    store_note: &str,
    travel: &crate::mesh_travel::Published,
) -> String {
    use std::fmt::Write;
    let mut o = String::new();
    let _ = writeln!(o);
    let _ = writeln!(o, "Model:          {}", r.model_name);
    let _ = writeln!(
        o,
        "Placement:      {}  ({} node(s), {} hop(s)/token)",
        r.placement_human, r.nodes, r.hops
    );
    let _ = writeln!(o, "Context:        {} tokens", r.key.n_ctx);
    // Only when there is a hop to characterise. A single-node run has no link,
    // and "Link: local" beside "Placement: 48 local" is noise. For anything
    // distributed it is load-bearing: the same split over a tunnel rather than
    // a direct address has read ~2.3x apart on this fleet, so a reader who
    // cannot see which one produced this number cannot use it.
    if r.nodes > 1 {
        let _ = writeln!(
            o,
            "Link:           {}{}",
            r.key.link.as_str(),
            match r.link_rtt_ms {
                Some(ms) => format!("   ({ms:.0} ms to the furthest worker)"),
                None => String::new(),
            }
        );
    }
    if let Some(b) = &r.backend {
        let _ = writeln!(o, "Backend:        {b}");
    }
    // What else was true of the box. Shown for every run, valid or not, because
    // it is the context a reader needs to judge whether two runs are comparable
    // — the question that went unanswerable when one key came back 43% apart.
    if let Some(c) = &r.conditions {
        if let Some(line) = c.describe() {
            let _ = writeln!(o, "Conditions:     {line}");
        }
        if let Some(span) = c.run_span_s {
            let _ = writeln!(o, "Run span:       {span:.0} s across the timed trials");
        }
    }
    let _ = writeln!(o);

    match &r.verdict {
        mm::Verdict::Valid => {
            let _ = writeln!(
                o,
                "Decode:         {:.2} tok/s   (median of {} trial(s); {:.2}–{:.2} across them)",
                r.decode_tok_s, r.trials, r.decode_tok_s_min, r.decode_tok_s_max
            );
            let _ = writeln!(o, "TTFT:           {:.0} ms", r.ttft_ms);
            let _ = writeln!(
                o,
                "Inter-token:    p50 {:.1} ms · p95 {:.1} ms",
                r.itl_p50_ms, r.itl_p95_ms
            );
            match r.prefill_tok_s {
                Some(p) => {
                    let _ = writeln!(o, "Prefill:        {p:.0} tok/s");
                }
                None => {
                    let _ = writeln!(o, "Prefill:        n/a (server omits stream usage)");
                }
            }
            if let Some(c) = r.cold_load_s {
                let _ = writeln!(o, "Cold load:      {c:.0} s   (paid once, by the canary)");
            }
            let _ = writeln!(o);
            let _ = writeln!(
                o,
                "This is a real measurement of the configuration you are running. `svrn mesh plan`\n\
                 on this exact model and split will now report it instead of \"not measured\"."
            );
        }
        mm::Verdict::Invalid { problems } => {
            let _ = writeln!(
                o,
                "INVALID — {} guard(s) tripped. These numbers describe a broken run and will\n\
                 never be served back by `mesh plan`:\n",
                problems.len()
            );
            for p in problems {
                let _ = writeln!(o, "  ! {p}");
            }
            let _ = writeln!(o);
            let _ = writeln!(
                o,
                "For the record: {:.2} tok/s over {} trial(s), {} content frame(s).",
                r.decode_tok_s, r.trials, r.content_frames
            );
            let _ = writeln!(
                o,
                "The run is kept so the failure is inspectable (`svrn mesh bench --history`); a\n\
                 discarded failure teaches nobody anything, and dropping it silently would make\n\
                 this tool retry-until-lucky."
            );
        }
    }
    let _ = writeln!(o);
    let _ = writeln!(o, "  {store_note}");
    // Two lines, not one, because they answer different questions: the store note
    // is "is my measurement safe", this is "can anyone else see it". Only shown
    // for a valid run — an invalid one never travels, and saying so beneath a run
    // that already failed adds a second disappointment for no information.
    if r.verdict.is_valid() {
        let _ = writeln!(o, "  {}", travel.note());
    }
    // The digest beside what it was computed from. Without the second half this
    // line is a hash the operator cannot check, and a key that changes for an
    // unknown reason is a number nobody can attribute later — which is exactly
    // what happened to this fleet's 16:05 run on 2026-07-29.
    match &r.witness {
        Some(w) => {
            let _ = writeln!(
                o,
                "  key: {}  ({})",
                r.key.placement_digest,
                w.describe_split()
            );
        }
        None => {
            let _ = writeln!(o, "  key: {}", r.key.placement_digest);
        }
    }
    o
}

// ---------------------------------------------------------------------------
// Help
// ---------------------------------------------------------------------------

pub(crate) const HELP_MESH_BENCH: sovereign_cli_shared::help::Help =
    sovereign_cli_shared::help::Help {
        command: "svrn mesh bench",
        summary: "Measure how fast the model you are running actually decodes, and record it.",
        sections: &[
            sovereign_cli_shared::help::HelpSection::Usage(
                "svrn mesh bench [<model.gguf>] [--trials <n>] [--json] [--history]",
            ),
            sovereign_cli_shared::help::HelpSection::Flags(&[
                (
                    "<model.gguf>",
                    "An ASSERTION, not a selection: this file must be what the daemon has \
                     resident, or the command exits 3 naming the config line. It never loads it.",
                ),
                (
                    "--trials <n>",
                    "Timed trials to run, 1–20 (default 3). More trials tighten the spread; \
                     they do not change what is measured.",
                ),
                ("--json", "Emit the run as machine-readable JSON."),
                (
                    "--history",
                    "List every run recorded for this model on this machine, invalid ones \
                     included. Measures nothing.",
                ),
            ]),
            sovereign_cli_shared::help::HelpSection::Notes(
                "Measures the configuration you are RUNNING; it never installs one. There is no \
                 slot to select, so there is no slot to get wrong.\n\n\
                 Fires real streaming completions at the real HTTP surface and times the SSE \
                 frames, so the number includes the actual RPC split and network path. Decode \
                 rate is steady state — time to first token is reported separately rather than \
                 smeared into it.\n\n\
                 Nine validity guards run on every measurement: which slot served it, per-frame \
                 timing, placement unchanged across the run, peer liveness before and after, a \
                 canary first, host survival, a 32-frame floor, inter-trial spread within 25%, \
                 and a complete finish reason. A run that trips any of them is recorded but \
                 never served back to `mesh plan` — failures are kept so they can be inspected, \
                 not so they can be retried until one passes.\n\n\
                 Not instant. A cold load of a large model can take minutes before the first \
                 trial starts. Exit 0 valid · 1 guard tripped · 2 bad arguments · 3 assertion \
                 failed · 4 nothing measurable · 5 no daemon.",
            ),
            sovereign_cli_shared::help::HelpSection::Examples(&[
                (
                    "svrn mesh bench",
                    "Measure whatever is loaded right now, three trials",
                ),
                (
                    "svrn mesh bench ~/models/Qwen3.5-122B-Q5_K_XL.gguf",
                    "The same, but fail loudly if that is not what is loaded",
                ),
                (
                    "svrn mesh bench --history",
                    "What has this machine already measured?",
                ),
            ]),
        ],
    };

#[cfg(test)]
mod tests;
