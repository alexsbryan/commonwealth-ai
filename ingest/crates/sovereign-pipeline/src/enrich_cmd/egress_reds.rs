// SPDX-License-Identifier: AGPL-3.0-or-later
//! R-5 red — a personal-corpus chunk must not reach a remote-model
//! payload.
//!
//! Order deep-research-t2a, red R-5: "a personal-corpus chunk can
//! reach a remote payload at HEAD — `enrich_cmd/providers.rs` still
//! carries zero privacy tokens". This test drives the shipped
//! `enrich --provider` dispatch (`DaemonInferenceClient` +
//! `ProviderRegistry`) with an operator-configured REMOTE
//! OpenAI-compatible provider pointed at a local mock dispatcher
//! that records request bodies, and asks it to complete a prompt
//! containing a personal-corpus chunk.
//!
//! At HEAD nothing refuses: the payload arrives at the mock, the
//! recording is non-empty, and this test FAILS (the red).
//!
//! After the fix (the egress boundary in sovereign-core::egress,
//! custody-release check + run-scoped consent grant, default-deny):
//! the same call is refused BEFORE any request leaves the machine —
//! the mock records nothing and `complete` returns an error that
//! names what was withheld. The red then goes green with zero
//! changes to this test's assertions.

use super::inference_client::DaemonInferenceClient;
use super::test_env::scoped_home;
use corpus_engine::enrichment::pipeline::ChatPrompt;
use sovereign_enrichment_build::mock_provider::{mock_openai_host, CONTENT_OK};
use std::time::Duration;

/// R-5. The personal-chunk-to-remote-payload red. Fails at HEAD
/// (the payload arrives); green after the boundary refuses before
/// any request leaves the machine.
#[tokio::test]
async fn personal_chunk_must_not_reach_a_remote_payload() {
    let _home = scoped_home();
    let (base_url, recorded) = mock_openai_host(CONTENT_OK).await;

    // Operator config: one REMOTE OpenAI-compatible provider pointed
    // at the dispatcher.
    let home = std::env::var("HOME").expect("scoped_home set HOME");
    let cfg_dir = format!("{home}/.config/sovereign");
    std::fs::create_dir_all(&cfg_dir).unwrap();
    std::fs::write(
        format!("{cfg_dir}/providers.toml"),
        format!("[providers.mockprov]\ntype = \"openai-compatible\"\nbase_url = \"{base_url}\"\n"),
    )
    .unwrap();

    // The shipped enrich --provider dispatch path, pointed at the
    // remote provider for the chat model.
    let client = DaemonInferenceClient::new(&base_url, "mockprov:remote-model", "embed").unwrap();

    // A personal-corpus chunk — the estate's own extraction content:
    // the material the boundary must refuse to send to a remote
    // provider.
    let chunk = "PRIVATE-CORPUS-MARKER: the 2024 board minutes of the private foundation, including the unlisted grant deliberations and the compensation figures, which must never leave this machine.";
    let prompt = ChatPrompt::new("Extract the named entities.", chunk);

    let result = tokio::time::timeout(Duration::from_secs(30), client.complete(&prompt)).await;

    let received = recorded.lock().unwrap().clone();
    assert!(
        received.is_empty(),
        "R-5 red (must fail at HEAD): a personal-corpus chunk reached the remote payload — {} request(s) recorded, first body: {}",
        received.len(),
        received
            .first()
            .map(|b| b.chars().take(120).collect::<String>())
            .unwrap_or_default(),
    );

    let err = result
        .expect("complete must not hang — the boundary refuses before any request")
        .expect_err("the boundary must refuse a personal-custody chunk to a remote provider");
    let msg = err.to_string();
    assert!(
        msg.contains("personal")
            || msg.contains("custody")
            || msg.contains("consent")
            || msg.contains("grant"),
        "the refusal must be typed and name what was withheld (personal custody / consent grant): {msg}"
    );
}

/// The grant surface, end to end: a run that exported its consent reaches the
/// remote provider through `from_enrich_config`, the construction path every
/// enrich verb shares. THE FAILING INPUT: before 2026-10-03 nothing read a
/// grant there (`with_consent` had no caller), so this dispatch was refused
/// with "grant absent — default-deny" and the mock recorded nothing.
#[tokio::test]
async fn an_exported_consent_reaches_the_remote_provider_through_from_enrich_config() {
    use sovereign_contracts::types::Custody;
    use sovereign_enrichment_build::config::EnrichConfig;
    use sovereign_enrichment_build::inference_client::export_run_consent;

    // HOME is scoped under the process-wide lock, which also serializes this
    // test's write to the consent carrier against every other env-mutating
    // test in the binary. The guard clears the carrier even on panic.
    let _home = scoped_home();
    struct ClearCarrier;
    impl Drop for ClearCarrier {
        fn drop(&mut self) {
            std::env::remove_var("SVRNMESH_EGRESS_CONSENT");
        }
    }
    let _clear = ClearCarrier;

    let (base_url, recorded) = mock_openai_host(CONTENT_OK).await;
    let home = std::env::var("HOME").expect("scoped_home set HOME");
    let cfg_dir = format!("{home}/.config/sovereign");
    std::fs::create_dir_all(&cfg_dir).unwrap();
    std::fs::write(
        format!("{cfg_dir}/providers.toml"),
        format!("[providers.mockprov]\ntype = \"openai-compatible\"\nbase_url = \"{base_url}\"\n"),
    )
    .unwrap();

    export_run_consent(Custody::Personal);
    let cfg = EnrichConfig {
        schema_version: sovereign_enrichment_build::config::CONFIG_SCHEMA_VERSION,
        corpus_id: "consent-e2e".into(),
        pipeline_id: "philosophy_atlas".into(),
        source_path: "corpus:consent-e2e".into(),
        chapter_regex: String::new(),
        chat_model: "mockprov:remote-model".into(),
        chat_models: None,
        embed_model: "embed".into(),
        // The local daemon's base, deliberately NOT the mock: the egress gate
        // treats a provider whose base equals this one as local.
        base_url: "http://127.0.0.1:9".into(),
        embed_base_url: None,
        min_section_body_words: 0,
        toc_markers: None,
        max_output_tokens: 256,
        phase1b_max_output_tokens: None,
        phase_overrides: None,
        ontology: None,
        created_at: String::new(),
    };
    let client = DaemonInferenceClient::from_enrich_config(&cfg).unwrap();
    let prompt = ChatPrompt::new("Extract the named entities.", "A public paragraph.");
    let result = tokio::time::timeout(Duration::from_secs(30), client.complete(&prompt))
        .await
        .expect("complete must not hang");

    let received = recorded.lock().unwrap().clone();
    assert_eq!(
        received.len(),
        1,
        "the exported grant must reach the remote provider; got {result:?}"
    );
    assert_eq!(result.expect("released, then answered"), "ok");
}

/// THE FAILING INPUT: before 2026-10-03 a dispatch counted as local when its
/// provider's base equalled the client's own daemon base, so pointing that base
/// itself at a vendor (`--chat-url https://api.deepseek.com` with a bare model
/// id) sent the chunk with no grant. The refusal now comes before any request
/// is built, so the unroutable host is never contacted.
#[tokio::test]
async fn a_daemon_base_on_another_host_is_a_remote_payload() {
    let _home = scoped_home();
    let client =
        DaemonInferenceClient::new("https://vendor.invalid", "bare-model", "embed").unwrap();
    let prompt = ChatPrompt::new("Extract the named entities.", "PRIVATE-CORPUS-MARKER");

    let err = tokio::time::timeout(Duration::from_secs(30), client.complete(&prompt))
        .await
        .expect("the boundary refuses before any request, so nothing can hang")
        .expect_err("a chunk bound for another host needs a grant");
    let msg = err.to_string();
    assert!(
        msg.contains("consent") && msg.contains("vendor.invalid"),
        "the refusal must name the grant and the host it withheld the chunk from: {msg}"
    );
}

/// The other side of the same decider: the client's own daemon on loopback is
/// the one endpoint that needs no grant.
#[tokio::test]
async fn the_on_box_daemon_needs_no_grant() {
    let _home = scoped_home();
    let (base_url, recorded) = mock_openai_host(CONTENT_OK).await;
    let client = DaemonInferenceClient::new(&base_url, "bare-model", "embed").unwrap();
    let prompt = ChatPrompt::new("Extract the named entities.", "PRIVATE-CORPUS-MARKER");

    let out = tokio::time::timeout(Duration::from_secs(30), client.complete(&prompt))
        .await
        .expect("complete must not hang")
        .expect("the on-box daemon is never gated");
    assert_eq!(out, "ok");
    assert_eq!(recorded.lock().unwrap().len(), 1);
}
