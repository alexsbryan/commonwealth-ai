// SPDX-License-Identifier: AGPL-3.0-or-later
//! One integration-test binary for this crate.
//!
//! Each former `tests/<name>.rs` is now `tests/main/<name>.rs`, declared
//! below, so cargo links ONE executable instead of one per file. Every
//! test still runs; its name gains the module path as a prefix, so a
//! filter that named a file now names a module:
//!
//!     cargo test -p <crate> --test main <module>::
//!
//! `#[path]` is load-bearing: `tests/main.rs` is a CRATE ROOT, so a bare
//! `mod foo;` resolves to `tests/foo.rs` — which cargo would then also
//! link as its own test binary, which is the thing this file exists to
//! stop. The attribute keeps the sources in `tests/main/`, a directory
//! cargo does not scan for targets.
//!
//! Files still sitting directly in `tests/` are there on purpose — they
//! need process isolation, or a `.config/nextest.toml` override keys on
//! their binary name. Do not fold those in.

#[path = "main/atlas_surface_e2e.rs"]
mod atlas_surface_e2e;
#[path = "main/canonical_pull_e2e.rs"]
mod canonical_pull_e2e;
#[path = "main/capabilities_published.rs"]
mod capabilities_published;
#[path = "main/chat_completion_e2e.rs"]
mod chat_completion_e2e;
#[path = "main/client_exposure.rs"]
mod client_exposure;
#[path = "main/common/mod.rs"]
mod common;
#[path = "main/conv_surface_e2e.rs"]
mod conv_surface_e2e;
#[path = "main/corpus_sharing_over_iroh_e2e.rs"]
mod corpus_sharing_over_iroh_e2e;
#[path = "main/corpus_watch_http_e2e.rs"]
mod corpus_watch_http_e2e;
#[path = "main/d6_surface_e2e.rs"]
mod d6_surface_e2e;
#[path = "main/d8_surface_e2e.rs"]
mod d8_surface_e2e;

#[path = "main/d9_turn_extras_e2e.rs"]
mod d9_turn_extras_e2e;
#[path = "main/d9a_corpus_catalog_e2e.rs"]
mod d9a_corpus_catalog_e2e;
#[path = "main/d9a_documents_e2e.rs"]
mod d9a_documents_e2e;
#[path = "main/daemon_variant_census.rs"]
mod daemon_variant_census;
#[path = "main/daemon_wiring.rs"]
mod daemon_wiring;
#[path = "main/dst_scenarios.rs"]
mod dst_scenarios;
#[path = "main/embeddings_e2e.rs"]
mod embeddings_e2e;
#[path = "main/emitter_origin_concurrency.rs"]
mod emitter_origin_concurrency;
#[path = "main/enrich_surface_e2e.rs"]
mod enrich_surface_e2e;
#[path = "main/finish_reason_streaming.rs"]
mod finish_reason_streaming;
#[path = "main/fold_ingest_abandoned_unit_e2e.rs"]
mod fold_ingest_abandoned_unit_e2e;
#[path = "main/fold_ingest_coverage_refusal_e2e.rs"]
mod fold_ingest_coverage_refusal_e2e;
#[path = "main/fold_ingest_cross_node_merge_e2e.rs"]
mod fold_ingest_cross_node_merge_e2e;
#[path = "main/gossip_auth.rs"]
mod gossip_auth;
#[path = "main/gossip_integration.rs"]
mod gossip_integration;
#[path = "main/guest_lender_routing.rs"]
mod guest_lender_routing;
#[path = "main/guest_over_iroh_e2e.rs"]
mod guest_over_iroh_e2e;
#[path = "main/injection_order.rs"]
mod injection_order;
#[path = "main/iroh_dialer_admission_e2e.rs"]
mod iroh_dialer_admission_e2e;
#[path = "main/iroh_transport_e2e.rs"]
mod iroh_transport_e2e;
#[path = "main/join_handshake.rs"]
mod join_handshake;
#[path = "main/join_key_persistence.rs"]
mod join_key_persistence;
#[path = "main/join_parks_not_leaves.rs"]
mod join_parks_not_leaves;
#[path = "main/knowledge_client_unavailability.rs"]
mod knowledge_client_unavailability;
#[path = "main/knowledge_fanout_e2e.rs"]
mod knowledge_fanout_e2e;
#[path = "main/knowledge_served_e2e.rs"]
mod knowledge_served_e2e;
#[path = "main/landscape_digest_http_e2e.rs"]
mod landscape_digest_http_e2e;
#[path = "main/lc_surface_e2e.rs"]
mod lc_surface_e2e;
#[path = "main/load_awareness_e2e.rs"]
mod load_awareness_e2e;
#[path = "main/local_only_boot.rs"]
mod local_only_boot;
#[path = "main/local_only_corpus_locality.rs"]
mod local_only_corpus_locality;
#[path = "main/local_pod_smoke.rs"]
mod local_pod_smoke;
#[path = "main/loopback_parity.rs"]
mod loopback_parity;
#[path = "main/manifest_fanout_concurrency.rs"]
mod manifest_fanout_concurrency;
#[path = "main/mesh_sim_scoreboard.rs"]
mod mesh_sim_scoreboard;
#[path = "main/mesh_switch.rs"]
mod mesh_switch;
#[path = "main/meshapp_parcels_e2e.rs"]
mod meshapp_parcels_e2e;
#[path = "main/meshapp_surface_e2e.rs"]
mod meshapp_surface_e2e;
#[path = "main/models_http_e2e.rs"]
mod models_http_e2e;
#[path = "main/node_id_persistence.rs"]
mod node_id_persistence;
#[path = "main/openai_finish_reason.rs"]
mod openai_finish_reason;
#[path = "main/pattern_observation_e2e.rs"]
mod pattern_observation_e2e;
#[path = "main/peer_preference_manifest.rs"]
mod peer_preference_manifest;
#[path = "main/peer_tally_status_e2e.rs"]
mod peer_tally_status_e2e;
#[path = "main/plaintext_join_over_iroh_e2e.rs"]
mod plaintext_join_over_iroh_e2e;
#[path = "main/port_config.rs"]
mod port_config;
#[path = "main/reading_http_e2e.rs"]
mod reading_http_e2e;
#[path = "main/recipe_surface_e2e.rs"]
mod recipe_surface_e2e;
#[path = "main/replication_sender_census.rs"]
mod replication_sender_census;
#[path = "main/responses_adapter_e2e.rs"]
mod responses_adapter_e2e;
#[path = "main/rotate_pre_split_guard.rs"]
mod rotate_pre_split_guard;
#[path = "main/scheduler_decision_records.rs"]
mod scheduler_decision_records;
#[path = "main/scheduler_replay_agreement.rs"]
mod scheduler_replay_agreement;
#[path = "main/spec_gate_e2e.rs"]
mod spec_gate_e2e;
#[path = "main/storage_snapshot_e2e.rs"]
mod storage_snapshot_e2e;
#[path = "main/throughput_ledger_emission.rs"]
mod throughput_ledger_emission;
#[path = "main/try_resume_first_gossip.rs"]
mod try_resume_first_gossip;
#[path = "main/turn_surface.rs"]
mod turn_surface;
#[path = "main/wire_view_drift.rs"]
mod wire_view_drift;
#[path = "main/worker_e2e.rs"]
mod worker_e2e;

// ─── The wiring of this file is itself a gate ────────────────────────────────
//
// Every test above is a `#[path = "main/<name>.rs"] mod <name>;` PAIR, and
// three times during the sv-surface campaign an alphabetical insert landed a
// new `mod` line BETWEEN an existing attribute and its `mod` — which silently
// re-pointed one module at another's file and stranded the other with no path
// at all. Each time a whole test file stopped being compiled into this binary
// while the commit that added it reported green: `meshapp_parcels_e2e` (slice
// 3) never ran once, and `enrich_surface_e2e` (slice 4) never ran either.
//
// A comment asking the next person to be careful is what failed. This is the
// invariant as code (ARCH principle 10): a mis-wired pair, or a file in
// `tests/main/` that nothing declares, fails here.

/// `mod common;` is the one module resolved conventionally
/// (`tests/common/mod.rs`) rather than by an explicit path.
const UNPATHED_MODULES: &[&str] = &["common"];

#[test]
fn every_test_module_is_wired_to_its_own_file() {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = std::fs::read_to_string(manifest.join("tests/main.rs"))
        .expect("this test binary's own root module is readable");
    let lines: Vec<&str> = source.lines().collect();

    let mut declared: Vec<String> = Vec::new();
    let mut problems: Vec<String> = Vec::new();

    for (i, line) in lines.iter().enumerate() {
        let Some(name) = line
            .strip_prefix("mod ")
            .and_then(|rest| rest.strip_suffix(';'))
            .filter(|n| {
                !n.is_empty()
                    && n.chars()
                        .all(|c| c.is_ascii_lowercase() || c == '_' || c.is_ascii_digit())
            })
        else {
            continue;
        };
        if UNPATHED_MODULES.contains(&name) {
            continue;
        }
        // Walk up past any `#[cfg(...)]` gates to the `#[path]` attribute.
        let mut j = i;
        let attr = loop {
            if j == 0 {
                break None;
            }
            j -= 1;
            let prev = lines[j].trim();
            if prev.starts_with("#[cfg") {
                continue;
            }
            break Some(prev);
        };
        match attr.and_then(|a| {
            a.strip_prefix("#[path = \"main/")
                .and_then(|r| r.strip_suffix(".rs\"]"))
        }) {
            None => problems.push(format!(
                "line {}: `mod {name};` has no `#[path = \"main/{name}.rs\"]` above it",
                i + 1
            )),
            Some(target) if target != name => problems.push(format!(
                "line {}: `mod {name};` is wired to `main/{target}.rs` — an insert landed \
                 between an attribute and its own `mod` line",
                i + 1
            )),
            Some(_) => declared.push(name.to_string()),
        }
    }

    let dir = manifest.join("tests/main");
    // An entry this gate cannot read is a broken fixture, not a finding:
    // swallowing it would let a file hide from the completeness check
    // below, which is the whole point of the check (ARCH principle 6).
    let mut on_disk: Vec<String> = std::fs::read_dir(&dir)
        .expect("tests/main is readable")
        .map(|e| {
            e.expect("every entry under tests/main is readable")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .filter_map(|name| name.strip_suffix(".rs").map(|s| s.to_string()))
        .collect();
    on_disk.sort();

    for file in &on_disk {
        if !declared.contains(file) {
            problems.push(format!(
                "`tests/main/{file}.rs` exists but no `mod {file};` declares it — it is not \
                 compiled into this binary and its tests have never run"
            ));
        }
    }
    for name in &declared {
        if !on_disk.contains(name) {
            problems.push(format!(
                "`mod {name};` names `tests/main/{name}.rs`, which does not exist"
            ));
        }
    }

    assert!(
        problems.is_empty(),
        "tests/main.rs module wiring ({} declared, {} files on disk):\n  {}",
        declared.len(),
        on_disk.len(),
        problems.join("\n  ")
    );
}
