// SPDX-License-Identifier: AGPL-3.0-or-later
//! Rendering a run, and the verb's help (split from `mesh_bench.rs` at the move to serve).

use super::*;

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

pub(crate) const HELP_MESH_BENCH: sovereign_cli_base::help::Help = sovereign_cli_base::help::Help {
    command: "svrn mesh bench",
    summary: "Measure how fast the model you are running actually decodes, and record it.",
    sections: &[
        sovereign_cli_base::help::HelpSection::Usage(
            "svrn mesh bench [<model.gguf>] [--trials <n>] [--json] [--history]",
        ),
        sovereign_cli_base::help::HelpSection::Flags(&[
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
        sovereign_cli_base::help::HelpSection::Notes(
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
        sovereign_cli_base::help::HelpSection::Examples(&[
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
