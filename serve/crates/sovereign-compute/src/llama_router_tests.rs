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
    assert!(ini.contains("[Qwen3-Reranker-0.6B]\nmodel = /m/Qwen3-Reranker-0.6B.gguf\nctx-size = 8192\nreranking = true\n"), "{ini}");
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
        !ini.contains("sleep-idle"),
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

#[test]
fn the_router_models_listing_reads_status_value_and_a_failed_load() {
    // The shape `get_router_models` writes (llama.cpp server-models.cpp).
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
