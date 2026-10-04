// SPDX-License-Identifier: AGPL-3.0-or-later
//! The validity guards, the aggregation and the live mesh as bench reads it (split from `mesh_bench.rs` at the move to serve).

use super::*;

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
    } else if let Some(wrong) = served
        .iter()
        .find(|m| !names_primary(m, g.primary_model_id))
    {
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
    let itls: Vec<f64> = trials
        .iter()
        .flat_map(|t| t.itl_ms.iter().copied())
        .collect();

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
pub(crate) fn co_resident_roles(body: &serde_json::Value, primary_model_id: &str) -> Vec<String> {
    let mut roles: Vec<String> = body
        .get("inference")
        .and_then(|i| i.get("resident"))
        .and_then(|r| r.as_array())
        .map(|slots| {
            slots
                .iter()
                .filter(|s| s.get("resident").and_then(|r| r.as_bool()) == Some(true))
                // An alias of the measured model, under any role name.
                .filter(|s| s.get("model_id").and_then(|m| m.as_str()) != Some(primary_model_id))
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
