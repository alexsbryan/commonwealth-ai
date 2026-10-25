// SPDX-License-Identifier: AGPL-3.0-or-later
use std::path::{Path, PathBuf};

use sovereign_contracts::engine_config::{EmbedInputs, GrammarSupport};
use sovereign_contracts::setup_config::{DaemonSection, ModelsSection};

use super::*;

const PRIMARY: &str = "/m/Qwen3.6-35B-A3B-MTP-UD-Q6_K.gguf";
const FAST: &str = "/m/Qwen3.5-4B-UD-MTP-Q6_K_XL.gguf";
const EMBED: &str = "/m/Qwen3-Embedding-0.6B-Q8_0.gguf";

fn models() -> ModelsSection {
    ModelsSection {
        primary: PRIMARY.into(),
        fast: Some(FAST.into()),
        embed: EMBED.into(),
        context_size: Some(65536),
        fast_context_size: Some(32768),
        ..Default::default()
    }
}

/// Every role pinned, so no section sleeps.
fn idle() -> DaemonSection {
    DaemonSection {
        primary_idle_secs: 0,
        fast_idle_secs: 0,
        embed_idle_secs: 0,
        ..Default::default()
    }
}

/// The MTP checkpoints declare one draft layer; the embedder declares none.
fn layers(path: &Path) -> std::io::Result<Option<u32>> {
    Ok(path.to_string_lossy().contains("MTP").then_some(1))
}

#[test]
fn the_preset_serves_each_gguf_by_its_stem_with_mtp_only_where_declared() {
    // A chat model whose GGUF declares no draft layers gets no drafting.
    let mut m = models();
    m.code = Some("/m/Qwen3-Coder-30B-A3B-Q6_K.gguf".into());
    let preset = RouterPreset::from_models(&m, None, &idle(), layers).unwrap();
    let ini = preset.render();
    assert!(
        ini.starts_with("version = 1\n\n[*]\nn-gpu-layers = 99\n"),
        "{ini}"
    );
    assert!(ini.contains(
        "\n[Qwen3.5-4B-UD-MTP-Q6_K_XL]\nmodel = /m/Qwen3.5-4B-UD-MTP-Q6_K_XL.gguf\nctx-size = 32768\n\
         kv-unified = true\nparallel = 8\nspec-type = draft-mtp\nspec-draft-n-max = 3\n"
    ), "{ini}");
    assert!(ini.contains(
        "\n[Qwen3.6-35B-A3B-MTP-UD-Q6_K]\nmodel = /m/Qwen3.6-35B-A3B-MTP-UD-Q6_K.gguf\nctx-size = 65536\n\
         kv-unified = true\nspec-type = draft-mtp\n"
    ), "{ini}");
    assert!(ini.contains(
        "\n[Qwen3-Embedding-0.6B-Q8_0]\nmodel = /m/Qwen3-Embedding-0.6B-Q8_0.gguf\nctx-size = 8192\n\
         embedding = true\nbatch-size = 8192\nubatch-size = 8192\n"
    ), "{ini}");
    assert!(ini.contains(
        "\n[Qwen3-Coder-30B-A3B-Q6_K]\nmodel = /m/Qwen3-Coder-30B-A3B-Q6_K.gguf\nctx-size = 65536\n\
         kv-unified = true\n\n"
    ), "{ini}");
    assert_eq!(ini.matches("spec-type").count(), 2, "{ini}");
    assert_eq!(preset.warm_models().len(), 4);
}

#[test]
fn the_remote_half_follows_from_the_preset() {
    let preset = RouterPreset::from_models(&models(), None, &idle(), layers).unwrap();
    let s = preset.engine_section("http://127.0.0.1:4242/v1");
    assert_eq!(s.endpoint.as_deref(), Some("http://127.0.0.1:4242/v1"));
    assert_eq!(s.model_id.as_deref(), Some("Qwen3.6-35B-A3B-MTP-UD-Q6_K"));
    assert_eq!(
        s.fast_model_id.as_deref(),
        Some("Qwen3.5-4B-UD-MTP-Q6_K_XL")
    );
    assert_eq!(
        s.embed_model_id.as_deref(),
        Some("Qwen3-Embedding-0.6B-Q8_0")
    );
    assert_eq!(
        s.context_size, 32768,
        "the budget the fast model can also hold"
    );
    assert_eq!(s.embed_inputs, Some(EmbedInputs::Client));
    assert_eq!(s.grammar, Some(GrammarSupport::Llguidance));
    // The embedded engine's 300s decode deadline, on every chat request.
    assert_eq!(
        s.extra_params,
        Some(serde_json::json!({ "t_max_predict_ms": 300_000 }))
    );
}

#[test]
fn one_gguf_in_two_roles_is_one_entry() {
    let mut m = models();
    m.fast = None;
    m.code = Some(PRIMARY.into());
    let preset = RouterPreset::from_models(
        &m,
        Some(PathBuf::from("/m/Qwen3-Reranker-0.6B.gguf")),
        &idle(),
        layers,
    )
    .unwrap();
    let ini = preset.render();
    assert_eq!(
        ini.matches("[Qwen3.6-35B-A3B-MTP-UD-Q6_K]").count(),
        1,
        "{ini}"
    );
    // The rerank section carries the slots a document-per-task route needs;
    // the embedder does not (one task per request there).
    assert!(
        ini.contains(
            "[Qwen3-Reranker-0.6B]\nmodel = /m/Qwen3-Reranker-0.6B.gguf\nctx-size = 8192\n\
         reranking = true\nbatch-size = 8192\nubatch-size = 8192\nparallel = 4\n"
        ),
        "{ini}"
    );
    assert!(
        !ini.contains("[Qwen3-Embedding-0.6B-Q8_0]\nmodel = /m/Qwen3-Embedding-0.6B-Q8_0.gguf\nctx-size = 8192\nembedding = true\nbatch-size = 8192\nubatch-size = 8192\nparallel"),
        "{ini}"
    );
    assert_eq!(preset.engine_section("http://x/v1").fast_model_id, None);
}

#[test]
fn each_section_sleeps_on_its_roles_idle_window_and_a_pinned_role_never() {
    let mut m = models();
    m.code = Some("/m/Qwen3-Coder-30B-A3B-Q6_K.gguf".into());
    let daemon = DaemonSection {
        primary_idle_secs: 1800,
        fast_idle_secs: 900,
        embed_idle_secs: 0,
        ..Default::default()
    };
    let preset = RouterPreset::from_models(
        &m,
        Some(PathBuf::from("/m/Qwen3-Reranker-0.6B.gguf")),
        &daemon,
        layers,
    )
    .unwrap();
    let ini = preset.render();
    let section = |name: &str| {
        let start = ini.find(&format!("[{name}]")).expect(name);
        let rest = &ini[start..];
        rest[..rest[1..].find("\n[").map_or(rest.len(), |e| e + 1)].to_string()
    };
    assert!(
        section("Qwen3.6-35B-A3B-MTP-UD-Q6_K").contains("sleep-idle-seconds = 1800\n"),
        "{ini}"
    );
    assert!(
        section("Qwen3.5-4B-UD-MTP-Q6_K_XL").contains("sleep-idle-seconds = 900\n"),
        "{ini}"
    );
    // Code shares the primary's lazy slot in the embedded engine.
    assert!(
        section("Qwen3-Coder-30B-A3B-Q6_K").contains("sleep-idle-seconds = 1800\n"),
        "{ini}"
    );
    assert!(
        !section("Qwen3-Embedding-0.6B-Q8_0").contains("sleep-idle"),
        "{ini}"
    );
    assert!(
        !section("Qwen3-Reranker-0.6B").contains("sleep-idle"),
        "{ini}"
    );
}

#[test]
fn fast_and_primary_on_one_gguf_are_one_section_serving_both_roles() {
    let mut m = models();
    m.fast = Some(PRIMARY.into());
    let daemon = DaemonSection {
        primary_idle_secs: 300,
        fast_idle_secs: 900,
        ..Default::default()
    };
    let preset = RouterPreset::from_models(&m, None, &daemon, layers).unwrap();
    let ini = preset.render();
    assert_eq!(
        ini.matches("[Qwen3.6-35B-A3B-MTP-UD-Q6_K]").count(),
        1,
        "{ini}"
    );
    // One copy of the weights, freed only once both roles let it go, with
    // the fast lane's slots.
    assert!(
        ini.contains("sleep-idle-seconds = 900\nkv-unified = true\nparallel = 8\n"),
        "{ini}"
    );
    let s = preset.engine_section("http://x/v1");
    assert_eq!(s.model_id, s.fast_model_id);
    let pinned = DaemonSection {
        fast_idle_secs: 0,
        ..daemon
    };
    let ini = RouterPreset::from_models(&m, None, &pinned, layers)
        .unwrap()
        .render();
    assert!(
        ini.contains("ctx-size = 65536\nkv-unified = true\nparallel = 8\n"),
        "a pinned role pins the shared section: {ini}"
    );
}

#[test]
fn every_role_names_the_model_that_fills_it_embed_and_rerank_included() {
    let mut m = models();
    m.code = Some(PRIMARY.into());
    let preset = RouterPreset::from_models(
        &m,
        Some(PathBuf::from("/m/Qwen3-Reranker-0.6B.gguf")),
        &idle(),
        layers,
    )
    .unwrap();
    assert_eq!(
        preset.roles(),
        vec![
            ("primary", "Qwen3.6-35B-A3B-MTP-UD-Q6_K"),
            ("fast", "Qwen3.5-4B-UD-MTP-Q6_K_XL"),
            ("code", "Qwen3.6-35B-A3B-MTP-UD-Q6_K"),
            ("embed", "Qwen3-Embedding-0.6B-Q8_0"),
            (
                sovereign_inference::served_kind::RERANK.role,
                "Qwen3-Reranker-0.6B"
            ),
        ]
    );
    assert_eq!(
        preset.path_of("Qwen3-Embedding-0.6B-Q8_0"),
        Some(Path::new(EMBED))
    );
}

#[test]
fn what_the_router_does_not_serve_yet_is_refused_by_name() {
    let mut m = models();
    m.extra.insert("coder".into(), "/m/coder.gguf".into());
    let err = RouterPreset::from_models(&m, None, &idle(), layers).unwrap_err();
    assert!(err.contains("[models.extra] (coder)"), "{err}");

    let unreadable =
        |_: &Path| -> std::io::Result<Option<u32>> { Err(std::io::Error::other("bad magic")) };
    let err = RouterPreset::from_models(&models(), None, &idle(), unreadable).unwrap_err();
    assert!(err.contains("MTP draft layers: bad magic"), "{err}");
}

#[test]
fn the_digest_names_the_rendered_preset() {
    let a = RouterPreset::from_models(&models(), None, &idle(), layers).unwrap();
    let b = RouterPreset::from_models(&models(), None, &idle(), layers).unwrap();
    assert_eq!(a.digest(), b.digest());
    let mut m = models();
    m.context_size = Some(16384);
    let c = RouterPreset::from_models(&m, None, &idle(), layers).unwrap();
    assert_ne!(a.digest(), c.digest());
}

/// es-f2: a distributed preset writes its peers into every section as
/// `rpc`, and writes it BEFORE `-ts` would be written — llama.cpp keeps a
/// preset's options in a map keyed by each option's first spelling, so
/// `--rpc` (which registers the devices) applies before `--tensor-split`
/// (`arg.cpp:2598` states the same ordering for `-mmdev`).
///
/// Only the CHAT sections take it. The embedder and reranker are small and
/// belong on this card — a peer holding a chat shard has no memory for them
/// (the e2e's first run OOM'd the peer loading the embedder beside the 9B's
/// share). Two failing inputs: a preset with no peers must render exactly
/// what it rendered before this row (no `rpc` line anywhere), and one WITH
/// peers must differ in its digest, or a reload naming other peers would be
/// accepted in silence.
#[test]
fn a_distributed_preset_names_its_peers_on_every_chat_section_before_any_split() {
    use crate::llama_router::placement::RouterPlacement;
    let local = RouterPreset::from_models(&models(), None, &idle(), layers).unwrap();
    assert!(
        !local.render().contains("rpc ="),
        "a single-card preset wrote an rpc key:\n{}",
        local.render()
    );
    let placement = RouterPlacement::from_endpoints(["192.168.1.20:50052", "10.1.2.3:50052"])
        .expect("two peers");
    let distributed = local.clone().with_placement(placement.clone());
    assert_eq!(distributed.placement().peers().len(), 2);
    let ini = distributed.render();
    let rpc = "rpc = 192.168.1.20:50052,10.1.2.3:50052";
    // The chat sections: the 35B primary and the 4B fast model.
    for section in ["Qwen3.6-35B-A3B-MTP-UD-Q6_K", "Qwen3.5-4B-UD-MTP-Q6_K_XL"] {
        let start = ini
            .find(&format!("[{section}]"))
            .unwrap_or_else(|| panic!("{section} is not a section of\n{ini}"));
        assert!(
            ini[start..].starts_with(&format!("[{section}]\nmodel = ")),
            "{ini}"
        );
        // index 0 is the header, 1 the model, 2 the ctx, 3 the peers.
        let line = ini[start..].lines().nth(3).unwrap_or_default();
        assert_eq!(line, rpc, "{section}: {ini}");
    }
    // The embedder is NOT distributed.
    let embed_at = ini
        .find("[Qwen3-Embedding-0.6B-Q8_0]")
        .expect("the embedder");
    assert!(
        !ini[embed_at..]
            .lines()
            .take(5)
            .any(|l| l.starts_with("rpc =")),
        "the embedder was distributed:\n{ini}"
    );
    assert_eq!(
        ini.matches("rpc =").count(),
        2,
        "one rpc line per chat section, none for the encoders:\n{ini}"
    );
    // A reload naming another split is a different router.
    let other = local
        .clone()
        .with_placement(RouterPlacement::from_endpoints(["192.168.1.21:50052"]).expect("one peer"));
    assert_ne!(distributed.digest(), other.digest());
    assert_ne!(local.digest(), distributed.digest());
}

#[test]
fn the_router_models_listing_reads_status_value_and_a_failed_load() {
    // The shape `get_router_models` writes (llama.cpp server-models.cpp)..
    let body = serde_json::json!({"data": [
        {"id": "Qwen3.6-35B-A3B-MTP-UD-Q6_K", "status": {"value": "loaded"}},
        {"id": "Qwen3-Embedding-0.6B-Q8_0", "status": {"value": "loading"}},
        {"id": "Qwen3-Reranker-0.6B", "status": {"value": "unloaded", "failed": true, "exit_code": 1}},
    ]});
    assert_eq!(
        live::parse_models(&body),
        vec![
            (
                "Qwen3.6-35B-A3B-MTP-UD-Q6_K".to_string(),
                "loaded".to_string()
            ),
            (
                "Qwen3-Embedding-0.6B-Q8_0".to_string(),
                "loading".to_string()
            ),
            (
                "Qwen3-Reranker-0.6B".to_string(),
                "failed (exit 1)".to_string()
            ),
        ]
    );
}

#[test]
fn the_models_warmed_are_the_longest_run_that_fits_primary_first() {
    let mut m = models();
    m.code = Some(PRIMARY.into());
    let preset = RouterPreset::from_models(
        &m,
        Some(PathBuf::from("/m/Qwen3-Reranker-0.6B.gguf")),
        &idle(),
        layers,
    )
    .unwrap();
    let all = |_: &[sovereign_inference::capacity::SlotPlan]| true;
    // The code role names the primary's GGUF, so it is not counted twice.
    assert_eq!(
        preset.models_that_fit(all),
        vec![
            "Qwen3.6-35B-A3B-MTP-UD-Q6_K",
            "Qwen3.5-4B-UD-MTP-Q6_K_XL",
            "Qwen3-Embedding-0.6B-Q8_0",
            "Qwen3-Reranker-0.6B",
        ]
    );
    let two = |s: &[sovereign_inference::capacity::SlotPlan]| s.len() <= 2;
    assert_eq!(
        preset.models_that_fit(two),
        vec!["Qwen3.6-35B-A3B-MTP-UD-Q6_K", "Qwen3.5-4B-UD-MTP-Q6_K_XL"]
    );
    // Each model is sized at its own context, one KV pool.
    let sized_at_their_ctx = |s: &[sovereign_inference::capacity::SlotPlan]| {
        let ctx: Vec<(u32, u32)> = s.iter().map(|p| (p.n_ctx, p.n_seq_max)).collect();
        [(65536, 1), (32768, 1)].starts_with(&ctx)
    };
    assert_eq!(preset.models_that_fit(sized_at_their_ctx).len(), 2);
    let none = |_: &[sovereign_inference::capacity::SlotPlan]| false;
    assert_eq!(
        preset.models_that_fit(none),
        vec!["Qwen3.6-35B-A3B-MTP-UD-Q6_K"],
        "a primary too big alone is still tried"
    );
}

/// `[models.edit]` (refused before es-r3-fim) is a chat section at its own
/// context, pinned as the embedded engine pins its edit slot; on the fast
/// GGUF it is the fast section, with the fast model's window.
#[test]
fn the_editing_model_is_a_section_of_its_own_or_the_one_it_shares() {
    use sovereign_contracts::setup_config::EditSection;
    let edit = |path: &str| EditSection {
        path: path.into(),
        context_size: None,
        max_tokens: None,
        temperature: None,
        max_prefix_chars: None,
        max_suffix_chars: None,
        next_edit_format: None,
    };
    let windows = DaemonSection {
        primary_idle_secs: 600,
        fast_idle_secs: 300,
        embed_idle_secs: 0,
        ..Default::default()
    };
    let mut m = models();
    m.edit = Some(edit("/m/Mellum2-12B-A2.5B-Instruct-Q6_K.gguf"));
    let preset = RouterPreset::from_models(&m, None, &windows, layers).unwrap();
    assert_eq!(preset.edit(), Some("Mellum2-12B-A2.5B-Instruct-Q6_K"));
    let ini = preset.render();
    assert!(
        ini.contains(
            "\n[Mellum2-12B-A2.5B-Instruct-Q6_K]\nmodel = /m/Mellum2-12B-A2.5B-Instruct-Q6_K.gguf\n\
             ctx-size = 4096\nkv-unified = true\n\n"
        ),
        "{ini}"
    );
    let all = |_: &[sovereign_inference::capacity::SlotPlan]| true;
    assert!(preset
        .models_that_fit(all)
        .contains(&"Mellum2-12B-A2.5B-Instruct-Q6_K"));

    m.edit = Some(edit(FAST));
    let preset = RouterPreset::from_models(&m, None, &windows, layers).unwrap();
    assert_eq!(preset.edit(), Some("Qwen3.5-4B-UD-MTP-Q6_K_XL"));
    assert_eq!(
        preset.warm_models().len(),
        3,
        "no second copy of the fast GGUF"
    );
    let ini = preset.render();
    assert!(
        ini.contains(
            "\n[Qwen3.5-4B-UD-MTP-Q6_K_XL]\nmodel = /m/Qwen3.5-4B-UD-MTP-Q6_K_XL.gguf\n\
             ctx-size = 32768\nsleep-idle-seconds = 300\n"
        ),
        "{ini}"
    );
}

/// A hosted node's router (ENGINE_SWAP_PLAN A6): `[engine] embed_path` and
/// no `[models]` at all, so the router serves the embedder as its one
/// section and no chat model.
#[test]
fn an_embed_only_preset_serves_the_embedder_and_no_chat_model() {
    let daemon = DaemonSection {
        embed_idle_secs: 120,
        ..Default::default()
    };
    let preset = RouterPreset::embed_only(Path::new(EMBED), &daemon).unwrap();
    assert_eq!(preset.primary(), None);
    assert_eq!(
        preset.roles(),
        vec![("embed", "Qwen3-Embedding-0.6B-Q8_0")],
        "the embedder is the only role a hosted router fills"
    );
    assert_eq!(preset.warm_models(), vec!["Qwen3-Embedding-0.6B-Q8_0"]);
    let ini = preset.render();
    assert!(
        ini.contains(
            "\n[Qwen3-Embedding-0.6B-Q8_0]\nmodel = /m/Qwen3-Embedding-0.6B-Q8_0.gguf\n\
             ctx-size = 8192\nsleep-idle-seconds = 120\nembedding = true\n\
             batch-size = 8192\nubatch-size = 8192\n"
        ),
        "{ini}"
    );
    // No chat section at all, so the router never tries to load one.
    assert_eq!(ini.matches("model =").count(), 1, "{ini}");
    assert!(!ini.contains("kv-unified"), "{ini}");

    let section = preset.engine_section("http://127.0.0.1:4242/v1");
    assert_eq!(section.model_id, None, "no chat model id to send");
    assert_eq!(section.fast_model_id, None);
    assert_eq!(
        section.embed_model_id.as_deref(),
        Some("Qwen3-Embedding-0.6B-Q8_0")
    );
    assert_eq!(section.context_size, 8192);
    assert_eq!(section.embed_inputs, Some(EmbedInputs::Client));
}

/// A preset one card no longer holds is refused by name, not warmed in part
/// (ENGINE_SWAP_PLAN A7): the repair loop reloads a dead section with no
/// eviction to make room for it, so a model that loads on demand at boot is
/// a model whose death this node cannot repair.
///
/// Failing input: a bar that admits the run `models_that_fit` warms but not
/// every section — the old behaviour, which returned the shortened run and
/// left the rest to eviction.
#[test]
fn a_preset_one_card_cannot_hold_is_refused_rather_than_warmed_in_part() {
    let mut m = models();
    m.code = Some("/m/Qwen3-Coder-30B-A3B-Q6_K.gguf".into());
    let preset = RouterPreset::from_models(
        &m,
        Some(PathBuf::from("/m/Qwen3-Reranker-0.6B.gguf")),
        &idle(),
        layers,
    )
    .unwrap();
    // Five sections: primary, fast, embed, code, rerank.
    let sections = preset.sections_in_preflight_order();
    assert_eq!(sections.len(), 5, "{sections:?}");
    let two_fit = |s: &[SlotPlan]| s.len() <= 2;
    assert!(
        preset.models_that_fit(two_fit).len() < sections.len(),
        "the failing input must admit less than the whole preset here"
    );
    let err = preset.vram_if_every_section_loaded(two_fit).unwrap_err();
    assert!(
        err.contains("5 models at once") && err.contains("no longer holds"),
        "{err}"
    );
    assert!(preset.vram_if_every_section_loaded(|_| true).is_ok());
}

/// THE FAILING INPUT (2026-10-24, the Mac's hosted daemon after the host
/// flip merge): the pair's remote half was `engine_section`, which points
/// CHAT at the router too, so a `primary` turn reached llama-server as
/// `hosted-default` and came back "model 'hosted-default' not found". The
/// chat half is the vendor's; only embeddings point at the router.
#[test]
fn a_hosted_pair_chats_at_the_vendor_and_embeds_at_the_router() {
    let preset = RouterPreset::embed_only(Path::new(EMBED), &DaemonSection::default()).unwrap();
    let vendor = EngineSection {
        kind: EngineKind::Remote,
        endpoint: Some("https://api.vendor.example/v1".into()),
        api_key: Some("key".into()),
        model_id: Some("vendor-chat".into()),
        embed_path: Some(EMBED.into()),
        context_size: 0,
        ..Default::default()
    };
    let s = preset.hosted_pair_section("http://127.0.0.1:4242/v1", &vendor);
    assert_eq!(s.kind, EngineKind::Remote);
    assert_eq!(s.endpoint.as_deref(), Some("https://api.vendor.example/v1"));
    assert_eq!(s.api_key.as_deref(), Some("key"));
    assert_eq!(s.model_id.as_deref(), Some("vendor-chat"));
    assert_eq!(
        s.embed_endpoint.as_deref(),
        Some("http://127.0.0.1:4242/v1")
    );
    assert_eq!(
        s.embed_model_id.as_deref(),
        Some("Qwen3-Embedding-0.6B-Q8_0")
    );
    assert_eq!(s.embed_inputs, Some(EmbedInputs::Client));
    assert_eq!(
        s.embed_path, None,
        "the router loads the GGUF, not this process"
    );
}
