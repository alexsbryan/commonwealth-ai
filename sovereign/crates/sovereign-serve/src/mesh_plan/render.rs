// SPDX-License-Identifier: AGPL-3.0-or-later
//! Rendering the plan's report as JSON and for a person (split from
//! `mesh_plan.rs` at the move to serve).

use super::*;

/// The machine-readable plan.
///
/// Top-level fields describe the [`Possible`](CapacityBasis::Possible) basis, as
/// they always have. `safe_now` carries the live-free basis when one exists, so a
/// script can gate on "will this load right now" without reparsing prose.
pub(crate) fn render_json(r: &PlanReport) -> serde_json::Value {
    fn devices_of(rows: &[DeviceRow]) -> Vec<serde_json::Value> {
        rows.iter()
            .map(|d| {
                serde_json::json!({
                    "device": d.dev,
                    "role": if d.is_host { "host" } else { "worker" },
                    "vram_gb": gb(d.vram()),
                    "blocks": d.blocks.map(|(a, b)| [a, b]),
                    "block_count": d.blocks.map(|(a, b)| b - a + 1).unwrap_or(0),
                    "holds_output": d.holds_output,
                    "weight_gb": gb(d.weight()),
                    "need_gb": gb(d.need()),
                    "fits": d.fits(),
                })
            })
            .collect()
    }
    let devices_json = devices_of(&r.rows);
    let safe_now_json = match &r.safe_now {
        Some(sn) => serde_json::json!({
            "basis": sn.basis.label(),
            "pooled_gb": gb(sn.pooled),
            "aggregate_gate_pass": sn.gate_pass,
            "per_device_overflow_devices":
                sn.overflows().iter().map(|d| d.dev).collect::<Vec<_>>(),
            "fits": sn.fits(),
            "nodes_used": sn.nodes.active_nodes,
            "hops": sn.nodes.hops_now,
            "devices": devices_of(&sn.rows),
            // True only when no pin overrides it — a pin is what the loader
            // obeys, and `speed` is keyed on whichever basis is executed.
            "is_executed_cut": r.pinned.is_none(),
        }),
        None => serde_json::Value::Null,
    };
    serde_json::json!({
        "model": r.model_name,
        "blocks": r.n_layer,
        "weights_gb": gb(r.total_weight),
        "output_head_gb": gb(r.output_bytes),
        "token_embd_host_ram_gb": gb(r.embd_bytes),
        "block_mass_gb": {
            "min": gb(r.block_mass.min),
            "max": gb(r.block_mass.max),
            "mean": gb(r.block_mass.mean),
            "spread": r.block_mass.spread,
            "uniform": r.block_mass.uniform
        },
        "headroom": r.headroom,
        "headroom_source": if r.headroom_from_flag { "flag" } else { "config" },
        "pooled_gb": gb(r.pooled),
        "aggregate_gate_need_gb": gb(r.gate_need),
        "aggregate_gate_pass": r.gate_pass,
        "per_device_overflow_devices": r.overflows().iter().map(|d| d.dev).collect::<Vec<_>>(),
        "moe": match &r.moe {
            Some(m) => serde_json::json!({
                "routed_expert_gb": gb(m.routed_expert_bytes),
                "routed_expert_pct": 100.0 * m.routed_expert_bytes as f64 / r.total_weight as f64,
                "hot_gb": gb(m.hot_bytes),
                "hot_pct": 100.0 * m.hot_bytes as f64 / r.total_weight as f64,
            }),
            None => serde_json::Value::Null,
        },
        "nodes_used": r.nodes.active_nodes,
        "hops": r.nodes.hops_now,
        "min_nodes": r.nodes.min_nodes,
        "min_hops": r.nodes.hops_min,
        "devices": devices_json,
        "capacity_basis": CapacityBasis::Possible.label(),
        "safe_now": safe_now_json,
        "block_split_pin": r.block_split_pin,
        "pinned": match &r.pinned {
            Some(p) => serde_json::json!({
                "basis": p.basis.label(),
                "aggregate_gate_pass": p.gate_pass,
                "per_device_overflow_devices":
                    p.overflows().iter().map(|d| d.dev).collect::<Vec<_>>(),
                "fits": p.fits(),
                "nodes_used": p.nodes.active_nodes,
                "hops": p.nodes.hops_now,
                "devices": devices_of(&p.rows),
                // A pin outranks both capacity bases: `speed` is keyed on this.
                "is_executed_cut": true,
            }),
            None => serde_json::Value::Null,
        },
        "speed": render_speed_json(r),
    })
}

/// The `speed` object, always present.
///
/// Every numeric field is `null` when there is no measurement — never `0.0`.
/// A consumer will divide by a number; `null` is an absence it has to handle,
/// while zero is a lie it will happily propagate.
fn render_speed_json(r: &PlanReport) -> serde_json::Value {
    let key = match &r.speed_key {
        Some(k) => serde_json::json!({
            "probe_version": k.probe_version,
            "model_fingerprint": k.model_fingerprint,
            "placement_digest": k.placement_digest,
            "host_hw_fingerprint": k.host_hw_fingerprint,
            "n_ctx": k.n_ctx,
            "link": k.link.as_str(),
        }),
        None => serde_json::Value::Null,
    };

    let mut o = serde_json::json!({
        "status": match &r.speed {
            SpeedSection::Measured { .. } => "measured",
            SpeedSection::NotMeasured { .. } => "not_measured",
            SpeedSection::NotMeasurable(_) => "not_measurable",
        },
        "reason": match &r.speed {
            SpeedSection::Measured { .. } => serde_json::Value::Null,
            SpeedSection::NotMeasured { .. } => "no-record".into(),
            SpeedSection::NotMeasurable(NotMeasurable::HypotheticalDevices) =>
                serde_json::Value::from("hypothetical-devices"),
            SpeedSection::NotMeasurable(NotMeasurable::HostUnidentified) =>
                serde_json::Value::from("host-unidentified"),
            SpeedSection::NotMeasurable(NotMeasurable::PeerUnidentified { name }) =>
                serde_json::Value::from(format!("peer-unidentified:{name}")),
        },
        "decode_tok_s": serde_json::Value::Null,
        "decode_tok_s_min": serde_json::Value::Null,
        "decode_tok_s_max": serde_json::Value::Null,
        "ttft_ms": serde_json::Value::Null,
        "itl_p50_ms": serde_json::Value::Null,
        "itl_p95_ms": serde_json::Value::Null,
        "prefill_tok_s": serde_json::Value::Null,
        "n_ctx": r.speed_key.as_ref().map(|k| k.n_ctx),
        "backend": serde_json::Value::Null,
        "runs": serde_json::Value::Null,
        "measured_at": serde_json::Value::Null,
        "measured_build": serde_json::Value::Null,
        "stale": serde_json::Value::Null,
        "near_misses": match &r.speed {
            SpeedSection::NotMeasured { near } => near
                .iter()
                .map(|n| serde_json::json!({
                    "placement_human": n.placement_human,
                    "decode_tok_s": n.decode_tok_s,
                    "measured_at": n.measured_at,
                    "differs_by": n.differs_by,
                    // Null means this machine measured it. A name means a peer
                    // did, and a consumer must not present the two alike — one
                    // is a fact about hardware the reader controls, the other a
                    // report about hardware they have never seen.
                    "taken_by": n.taken_by,
                    // True only for a peer: an exact local hit is a hit, and
                    // `lookup` serves it above rather than as a near miss.
                    "exact": n.is_exact(),
                    // What else was running when this was taken. Null means the
                    // record predates conditions — NOT that the box was quiet.
                    "conditions": n.conditions,
                    // One entry per `differs_by` facet, in the same order.
                    // `measured`/`yours` are null where that side kept no
                    // witness to describe — a real difference we decline to
                    // characterise, not an absent one.
                    "differences": n.detail.iter().map(|d| serde_json::json!({
                        "facet": d.facet,
                        "measured": d.theirs,
                        "yours": d.ours,
                    })).collect::<Vec<_>>(),
                }))
                .collect::<Vec<_>>()
                .into(),
            _ => serde_json::Value::Array(Vec::new()),
        },
        "measure_command": "svrn mesh bench",
        "key": key,
    });

    if let SpeedSection::Measured { summary: s } = &r.speed {
        let m = o.as_object_mut().expect("json! built an object");
        m.insert("decode_tok_s".into(), s.decode_tok_s.into());
        m.insert("decode_tok_s_min".into(), s.decode_tok_s_min.into());
        m.insert("decode_tok_s_max".into(), s.decode_tok_s_max.into());
        m.insert("ttft_ms".into(), s.ttft_ms.into());
        m.insert("itl_p50_ms".into(), s.itl_p50_ms.into());
        m.insert("itl_p95_ms".into(), s.itl_p95_ms.into());
        m.insert("prefill_tok_s".into(), s.prefill_tok_s.into());
        m.insert("backend".into(), s.backend.clone().into());
        m.insert("runs".into(), s.runs.into());
        m.insert("measured_at".into(), s.measured_at.into());
        m.insert("measured_build".into(), s.measured_build.clone().into());
        m.insert("stale".into(), s.stale.into());
    }
    o
}

/// The operator-facing plan.
pub(crate) fn render_human(r: &PlanReport) -> String {
    use std::fmt::Write as _;
    let mut o = String::new();
    let headroom = r.headroom;

    let _ = writeln!(o, "svrn mesh plan — dry run (no load, no GPU)\n");
    let _ = writeln!(o, "Model:  {}", r.model_name);
    let _ = writeln!(
        o,
        "        {} blocks · {:.1} GB weights  (output head {:.1} GB · token_embd {:.1} GB on host RAM)",
        r.n_layer,
        gb(r.total_weight),
        gb(r.output_bytes),
        gb(r.embd_bytes)
    );

    let m = &r.block_mass;
    if m.uniform {
        let _ = writeln!(
            o,
            "Blocks: {:.2}–{:.2} GB (mean {:.2}) · {:.2}× spread → UNIFORM mass",
            gb(m.min),
            gb(m.max),
            gb(m.mean),
            m.spread
        );
        let _ = writeln!(o, "        VRAM-proportional block count ≈ byte-proportional, so heterogeneous VRAM is safe.");
    } else {
        let _ = writeln!(
            o,
            "Blocks: {:.2}–{:.2} GB (mean {:.2}) · {:.2}× spread → NON-UNIFORM mass  (!)",
            gb(m.min),
            gb(m.max),
            gb(m.mean),
            m.spread
        );
        let _ = writeln!(o, "        Split apportions by byte MASS (not count), so heterogeneous VRAM stays balanced — but a single block heavier than a small node's whole share still can't be split contiguously. Watch per-device fit.");
    }

    if let Some(moe) = &r.moe {
        let _ = writeln!(
            o,
            "MoE:    {:.1} GB routed experts ({:.0}% — COLD, only top-k read per token) · {:.1} GB hot skeleton ({:.0}% — every token)",
            gb(moe.routed_expert_bytes),
            100.0 * moe.routed_expert_bytes as f64 / r.total_weight as f64,
            gb(moe.hot_bytes),
            100.0 * moe.hot_bytes as f64 / r.total_weight as f64,
        );
        let _ = writeln!(o, "        Whole blocks (experts included) stay on one node, so decode keeps its {}-hop path — a layer's experts are never scattered across nodes.", r.nodes.hops_now);
    }

    let hr_note = if r.headroom_from_flag {
        "--headroom override — WHAT-IF; the load executes with the [shared_model] headroom"
    } else {
        "matches the load's configured headroom"
    };
    let _ = writeln!(o, "Headroom: {headroom:.2}× ({hr_note}) — weight × {headroom:.2} must fit each device (covers KV + buffers)\n");

    let _ = writeln!(
        o,
        "  dev  role    VRAM       blocks     n   weight     need       fit"
    );
    for d in &r.rows {
        let (blocks_s, n_s) = match d.blocks {
            Some((a, b)) => (format!("{a}-{b}"), format!("{}", b - a + 1)),
            None => ("—".to_string(), "0".to_string()),
        };
        let fit = if d.fits() {
            format!("ok  +{:.1} GB", gb(d.vram() - d.need()))
        } else {
            format!("OVERFLOW -{:.1} GB", gb(d.need() - d.vram()))
        };
        let star = if d.is_host { "*" } else { " " };
        let role = if d.is_host { "host" } else { "worker" };
        let _ = writeln!(
            o,
            "{star} {:>3}  {:<6} {:>6.1} GB  {:<8}  {:>2}  {:>6.1} GB  {:>6.1} GB  {fit}",
            d.dev,
            role,
            gb(d.vram()),
            blocks_s,
            n_s,
            gb(d.weight()),
            gb(d.need())
        );
        if d.is_host && r.embd_bytes > 0 {
            let _ = writeln!(
                o,
                "       (+ token_embd {:.1} GB in host system RAM, not VRAM)",
                gb(r.embd_bytes)
            );
        }
    }

    let _ = writeln!(o);
    let _ = writeln!(
        o,
        "Aggregate gate: pooled {:.1} GB {} model×{headroom:.2} ({:.1} GB) → {}",
        gb(r.pooled),
        if r.gate_pass { ">=" } else { "<" },
        gb(r.gate_need),
        if r.gate_pass {
            "PASS".to_string()
        } else {
            "FAIL — cluster too small; the host reports \"forming\" and does not load".to_string()
        }
    );

    let overflows = r.overflows();
    if overflows.is_empty() {
        let _ = writeln!(o, "Per-device:     all devices fit ok");
    } else {
        let ids: Vec<String> = overflows.iter().map(|d| d.dev.to_string()).collect();
        let _ = writeln!(
            o,
            "Per-device:     {} device(s) OVERFLOW [{}] -> the LIVE load refuses this cut; its own per-device gate reports WorkerOverflow.",
            overflows.len(),
            ids.join(", ")
        );
        let _ = writeln!(o, "\nOptions:");
        let _ = writeln!(o, "   • move the host role to your largest node (--host <idx>) — the host also holds the output head");
        let _ = writeln!(o, "   • lower --headroom for a tighter pack (less KV room), or give the overflowing node more free VRAM");
        if !r.block_mass.uniform {
            let _ = writeln!(o, "   • this model is skewed enough that one block's mass exceeds a small node's share — the split is already mass-aware, so the fix is more VRAM on that node or a different --host, not a smarter split");
        }
    }

    // Nodes & hops advisor — single-stream pipeline decode costs (nodes-1) hops
    // per token, so fewer nodes = fewer hops = lower hop LATENCY. That is a
    // tradeoff, NOT a win button: on a memory-bandwidth-bound host (e.g. a
    // unified-memory APU) offloading layers frees host weight-read bandwidth and
    // can raise THROUGHPUT despite the extra hop — the measured 122B ran ~20%
    // faster distributed (36/12) than solo. So report the hop cost; don't claim
    // fewer nodes is always faster.
    let n = &r.nodes;
    let _ = writeln!(
        o,
        "Nodes:          {} holding blocks → {} network hop{} per token",
        n.active_nodes,
        n.hops_now,
        if n.hops_now == 1 { "" } else { "s" }
    );
    if n.min_nodes < n.active_nodes {
        let _ = writeln!(
            o,
            "                mass alone fits {} node{} ({} hop{}) — {} fewer node(s) would cut {} per-token hop(s) of latency. Net tok/s depends on the host: if it's memory-bandwidth-bound, keeping layers offloaded can still win. Measure both.",
            n.min_nodes,
            if n.min_nodes == 1 { "" } else { "s" },
            n.hops_min,
            if n.hops_min == 1 { "" } else { "s" },
            n.active_nodes - n.min_nodes,
            n.hops_now - n.hops_min,
        );
    }

    render_safe_now_human(&mut o, r);
    render_pin_human(&mut o, r);
    render_speed_human(&mut o, r);
    o
}

/// The `PINNED SPLIT:` block — louder than the other two bases, because when it
/// applies, everything derived from capacity above it is not what will load.
///
/// This exists because the silence was actively misleading. `SOVEREIGN_RPC_BLOCK_SPLIT`
/// has been pinned to `12,36` on this host in a systemd drop-in since 2026-07-27
/// (to match a worker's pre-built rpc-cache), while `mesh plan` went on deriving
/// 14/34 from VRAM and presenting it as the plan. Both numbers were computed
/// correctly; nothing reconciled them, so the plan and the load simply disagreed,
/// and the measurement filed under the real cut was unreachable from the plan's key.
fn render_pin_human(o: &mut String, r: &PlanReport) {
    use std::fmt::Write as _;

    let Some(raw) = &r.block_split_pin else {
        return;
    };

    let Some(p) = &r.pinned else {
        let _ = writeln!(
            o,
            "\nPINNED SPLIT:   SOVEREIGN_RPC_BLOCK_SPLIT={raw} is set but does NOT apply to this\n                model — it needs one count per device summing to {} blocks. The loader\n                REJECTS it too and falls back to the VRAM-derived split above, so the\n                pin is having no effect. Fix or remove it.",
            r.n_layer
        );
        return;
    };

    let counts: Vec<String> = p
        .rows
        .iter()
        .map(|d| {
            format!(
                "dev{} {}",
                d.dev,
                d.blocks.map(|(a, b)| b - a + 1).unwrap_or(0)
            )
        })
        .collect();
    let _ = writeln!(
        o,
        "\nPINNED SPLIT:   SOVEREIGN_RPC_BLOCK_SPLIT={raw} — the loader OBEYS this and ignores VRAM."
    );
    let _ = writeln!(
        o,
        "                Actual cut: {}  →  {}",
        counts.join(" · "),
        verdict(p.gate_pass, &p.overflows())
    );
    if p.rows.iter().map(|d| d.blocks).collect::<Vec<_>>()
        != r.rows.iter().map(|d| d.blocks).collect::<Vec<_>>()
    {
        let _ = writeln!(
            o,
            "                This is NOT the VRAM-derived cut shown in the table above. The table\n                answers 'what would capacity choose'; the pin is what will load."
        );
    }
}

/// The `Safe now:` block — the second capacity basis, and the gap between them.
///
/// Everything above this point answers "could this mesh hold the model", from
/// device totals. This answers "would a load started right now succeed", from the
/// live free memory the loader's own gate reads. Both are true statements about
/// different questions, and the operator is told which is which rather than being
/// handed one number that silently means whichever was easier to obtain.
///
/// The gap line is the load-bearing part. A device short on free memory is not a
/// device that is too small, and the two demand opposite responses: buy hardware
/// versus wait for the resident model to unload. On 2026-07-29 that ambiguity
/// turned a few seconds of teardown transience into an hours-long no-primary
/// outage, because a single collapsed `capacity_mb=20000` could not distinguish a
/// 20 GB device from a 51 GB device with 31 GB briefly held.
fn render_safe_now_human(o: &mut String, r: &PlanReport) {
    use std::fmt::Write as _;

    let Some(sn) = &r.safe_now else {
        let _ = writeln!(
            o,
            "\nSafe now:       UNKNOWN — no live free-memory reading for every device.\n                Everything above is what is POSSIBLE (device totals). Pass --from-mesh\n                against a daemon that reports device_memory to also see what would\n                load right now."
        );
        return;
    };

    let _ = writeln!(
        o,
        "\nTwo capacities, because they answer different questions:"
    );
    let _ = writeln!(
        o,
        "  possible (device total) → can this mesh EVER run this model: pooled {:.1} GB → {}",
        gb(r.pooled),
        verdict(r.gate_pass, &r.overflows())
    );
    let _ = writeln!(
        o,
        "  safe now (live free)    → would a load RIGHT NOW succeed:     pooled {:.1} GB → {}",
        gb(sn.pooled),
        verdict(sn.gate_pass, &sn.overflows())
    );

    // Per-device gap, worst first. Only devices actually holding something can
    // overflow, so a device with no blocks is not interesting here.
    let mut gaps: Vec<(usize, u64, u64)> = r
        .rows
        .iter()
        .filter_map(|p| {
            let s = sn.rows.iter().find(|s| s.dev == p.dev)?;
            (p.vram() > s.vram()).then_some((p.dev, p.vram(), s.vram()))
        })
        .collect();
    gaps.sort_by_key(|&(_, total, free)| std::cmp::Reverse(total.saturating_sub(free)));
    if gaps.is_empty() {
        let _ = writeln!(
            o,
            "                No gap — every device is as free as it is large; nothing else is resident."
        );
    } else {
        for (dev, total, free) in &gaps {
            let _ = writeln!(
                o,
                "  dev {dev}: {:.1} GB total, {:.1} GB free → {:.1} GB held by other work right now",
                gb(*total),
                gb(*free),
                gb(total.saturating_sub(*free))
            );
        }
    }

    // The two bases cut the model differently — say so, because the split is what
    // a measurement is keyed on and what each worker caches.
    let cut = |a: &[DeviceRow]| -> String {
        let mut parts: Vec<String> = a
            .iter()
            .filter_map(|d| d.blocks.map(|(x, y)| (d.dev, y - x + 1)))
            .map(|(dev, n)| format!("{n}@dev{dev}"))
            .collect();
        parts.sort();
        parts.join(" + ")
    };
    let (pc, sc) = (cut(&r.rows), cut(&sn.rows));
    if pc != sc {
        let _ = writeln!(
            o,
            "                Different cut: possible would place {pc}, but a load now places {sc}.\n                The load executes the SAFE NOW cut — that is the one Speed refers to."
        );
    }

    if r.gate_pass && r.overflows().is_empty() && !sn.fits() {
        let _ = writeln!(
            o,
            "                → This model FITS this hardware but will NOT load right now. Free the\n                  memory (retire the resident model) rather than buying VRAM. Note that the\n                  live gate does not retry: it parks, so a refusal here outlives the transient."
        );
    }
}

/// `PASS` / `FAIL` for one basis, naming which gate failed.
fn verdict(gate_pass: bool, overflows: &[&DeviceRow]) -> String {
    match (gate_pass, overflows.is_empty()) {
        (true, true) => "PASS".to_string(),
        (false, _) => "FAIL (aggregate: cluster too small)".to_string(),
        (true, false) => {
            let ids: Vec<String> = overflows.iter().map(|d| d.dev.to_string()).collect();
            format!("FAIL (per-device overflow: dev {})", ids.join(", "))
        }
    }
}

/// The `Speed:` block.
///
/// The whole point of this section is that it is allowed to say nothing. A
/// number appears here only when a run produced it for this exact
/// configuration; otherwise the block names the command that would produce
/// one. It carries no estimate, no interpolation from a neighbouring split,
/// and — deliberately — no guess at how long measuring would take, since that
/// would itself be a fabricated number about a model we have never loaded.
fn render_speed_human(o: &mut String, r: &PlanReport) {
    use std::fmt::Write as _;

    match &r.speed {
        SpeedSection::Measured { summary: s } => {
            let _ = writeln!(
                o,
                "Speed:          {:.1} tok/s decode · TTFT {:.2} s — MEASURED on this exact split",
                s.decode_tok_s,
                s.ttft_ms / 1000.0
            );
            let _ = writeln!(
                o,
                "                {} · ctx {}{}",
                s.placement_human,
                s.n_ctx,
                match &s.backend {
                    Some(b) => format!(" · {b}"),
                    None => String::new(),
                }
            );
            // The headline above is the MEDIAN run (an operator call — see
            // MeasurementSummary); the observed range is run medians, so an
            // outlier run widens the band without setting the headline.
            if s.runs == 1 {
                let _ = writeln!(o, "                1 run · build {}", s.measured_build);
            } else {
                let _ = writeln!(
                    o,
                    "                median of {} runs · observed {:.1}–{:.1} tok/s · build {}",
                    s.runs, s.decode_tok_s_min, s.decode_tok_s_max, s.measured_build
                );
            }
            if s.stale {
                let _ = writeln!(
                    o,
                    "                (!) recorded on a different build than this binary. Re-run `svrn mesh bench`"
                );
                let _ = writeln!(o, "                    if the inference engine changed.");
            }
        }
        SpeedSection::NotMeasured { near } => {
            // Not "for this split" — the split may well be measured and the
            // link be what differs. Saying "split" there sent a reader looking
            // for a difference in the block apportionment that isn't present.
            let _ = writeln!(o, "Speed:          not measured for this configuration.");
            // An unclassifiable link is the one reason a reader cannot work out
            // for themselves from the rest of the output, so it is named.
            if r.speed_key
                .as_ref()
                .is_some_and(|k| k.link == crate::mesh_measurements::LinkClass::Unknown)
            {
                let _ = writeln!(
                    o,
                    "                No rpc-server is discovered for every machine in this plan,"
                );
                let _ = writeln!(
                    o,
                    "                so how the tensor stream would travel is unknown — and that"
                );
                let _ = writeln!(
                    o,
                    "                choice alone has moved decode by ~2.3x on this fleet."
                );
            }
            // A peer who measured *this* configuration outranks any near miss,
            // however recent — it is the only thing here that describes what was
            // actually asked about. Presentation order, chosen here rather than
            // in the sort, because the store's ranking is general-purpose and
            // this priority is a judgement about what a reader needs first.
            let (exact, differing): (Vec<_>, Vec<_>) = near.iter().partition(|n| n.is_exact());
            for n in exact.iter().take(2) {
                let who = n.taken_by.as_deref().unwrap_or("this machine");
                let _ = writeln!(
                    o,
                    "                {who} measured this configuration: {:.1} tok/s.",
                    n.decode_tok_s
                );
                let _ = writeln!(
                    o,
                    "                Same model, split, hardware fingerprint, link and context — but"
                );
                let _ = writeln!(
                    o,
                    "                their machine, so it is a report, not your measurement."
                );
                // An exact-key hit is the strongest thing on this surface, which
                // is exactly why the load it was taken under has to travel with
                // it. Same configuration on a busy box is not the same claim.
                if let Some(c) = &n.conditions {
                    let _ = writeln!(o, "                On their box at the time: {c}.");
                }
            }
            for n in differing.iter().take(2) {
                match n.taken_by.as_deref() {
                    Some(who) => {
                        let _ = writeln!(
                            o,
                            "                Measured by {who}: {} → {:.1} tok/s.",
                            n.placement_human, n.decode_tok_s
                        );
                    }
                    None => {
                        let _ = writeln!(
                            o,
                            "                Measured here: {} → {:.1} tok/s.",
                            n.placement_human, n.decode_tok_s
                        );
                    }
                }
                let _ = writeln!(
                    o,
                    "                That is a different configuration ({}), so its number does not apply here.",
                    n.differs_by.join(", ")
                );
                if let Some(c) = &n.conditions {
                    let _ = writeln!(o, "                Taken with: {c}.");
                }
                // Name each difference concretely where both sides could be
                // described. A facet that cannot be is still listed above: the
                // difference is real, and what is missing is an honest account
                // of it — which is not a licence to invent one.
                for d in n.detail.iter() {
                    if let (Some(theirs), Some(ours)) = (&d.theirs, &d.ours) {
                        let _ = writeln!(
                            o,
                            "                  {:<14} measured: {theirs}   yours: {ours}",
                            d.facet
                        );
                    }
                }
            }
            if near.is_empty() {
                let _ = writeln!(
                    o,
                    "                Sovereign does not quote throughput it has not measured."
                );
            }
            let _ = writeln!(o, "                Measure it:  svrn mesh bench");
        }
        SpeedSection::NotMeasurable(NotMeasurable::HypotheticalDevices) => {
            let _ = writeln!(
                o,
                "Speed:          not measurable — --devices describes hardware that isn't here."
            );
            let _ = writeln!(
                o,
                "                Run this with --from-mesh on the mesh itself, then `svrn mesh bench`."
            );
        }
        SpeedSection::NotMeasurable(NotMeasurable::HostUnidentified) => {
            let _ = writeln!(
                o,
                "Speed:          not measurable — this host advertises no hardware fingerprint,"
            );
            let _ = writeln!(
                o,
                "                so there is no key a measurement could be filed under. Upgrading"
            );
            let _ = writeln!(o, "                the daemon on this node fixes it.");
        }
        SpeedSection::NotMeasurable(NotMeasurable::PeerUnidentified { name }) => {
            let _ = writeln!(
                o,
                "Speed:          not measurable — {name} is holding part of this model but"
            );
            let _ = writeln!(
                o,
                "                advertises no hardware fingerprint, so a number measured on"
            );
            let _ = writeln!(
                o,
                "                this split could not say which machine produced it. Upgrading"
            );
            let _ = writeln!(o, "                the daemon on {name} fixes it.");
        }
    }
}
