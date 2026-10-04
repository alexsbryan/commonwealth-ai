// SPDX-License-Identifier: AGPL-3.0-or-later
//! R-5 red — a personal-corpus chunk must not reach a remote-model
//! payload.
//!
//! Order deep-research-t2a, red R-5: "a personal-corpus chunk can
//! reach a remote payload at HEAD — `enrich_cmd/providers.rs` still
//! carries zero privacy tokens". The registry that red named is gone
//! (enrichment talks to one host, 2026-10-03); the payload it guarded
//! now leaves when that one host is off this machine. These tests drive
//! the shipped client at `http://vendor.invalid`, reached through a
//! recording proxy ([`OffBoxHost`]), so a chunk that leaves is a body the
//! recorder holds.
//!
//! The fix (the egress boundary in sovereign-core::egress, custody-release
//! check + run-scoped consent grant, default-deny) refuses BEFORE any
//! request leaves the machine: the recorder holds nothing and `complete`
//! returns an error that names what was withheld.

use super::inference_client::DaemonInferenceClient;
use super::test_env::{scoped_home, HomeGuard};
use corpus_engine::enrichment::pipeline::ChatPrompt;
use sovereign_enrichment_build::mock_provider::{mock_openai_host, CONTENT_OK};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// A chat host off this machine whose traffic a recorder sees: requests for
/// `http://vendor.invalid` go through the mock as an HTTP proxy, which reqwest
/// reads from the environment when a client is built. Loopback stays direct.
/// Holds the `HOME` lock, which serializes every env-mutating test here.
struct OffBoxHost {
    base_url: &'static str,
    recorded: Arc<Mutex<Vec<String>>>,
    _home: HomeGuard,
}

impl OffBoxHost {
    async fn start() -> Self {
        let home = scoped_home();
        let (proxy, recorded) = mock_openai_host(CONTENT_OK).await;
        std::env::set_var("HTTP_PROXY", &proxy);
        std::env::set_var("NO_PROXY", "localhost,127.0.0.1,::1");
        Self {
            base_url: "http://vendor.invalid",
            recorded,
            _home: home,
        }
    }
}

impl Drop for OffBoxHost {
    fn drop(&mut self) {
        std::env::remove_var("HTTP_PROXY");
        std::env::remove_var("NO_PROXY");
    }
}

/// R-5. The personal-chunk-to-remote-payload red. Fails at HEAD
/// (the payload arrives); green after the boundary refuses before
/// any request leaves the machine.
#[tokio::test]
async fn personal_chunk_must_not_reach_a_remote_payload() {
    let host = OffBoxHost::start().await;
    let recorded = Arc::clone(&host.recorded);

    // The shipped enrich dispatch path, pointed at a chat host off this
    // machine.
    let client = DaemonInferenceClient::new(host.base_url, "remote-model", "embed").unwrap();

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
/// off-box host through `from_enrich_config`, the construction path every
/// enrich verb shares. THE FAILING INPUT: before 2026-10-03 nothing read a
/// grant there (`with_consent` had no caller), so this dispatch was refused
/// with "grant absent — default-deny" and the recorder held nothing.
#[tokio::test]
async fn an_exported_consent_reaches_the_remote_provider_through_from_enrich_config() {
    use sovereign_contracts::types::Custody;
    use sovereign_enrichment_build::config::EnrichConfig;
    use sovereign_enrichment_build::inference_client::export_run_consent;

    // The host's guard holds the HOME lock, which also serializes this test's
    // write to the consent carrier. This guard clears the carrier even on
    // panic, and drops before the host's.
    let host = OffBoxHost::start().await;
    struct ClearCarrier;
    impl Drop for ClearCarrier {
        fn drop(&mut self) {
            std::env::remove_var("SVRNMESH_EGRESS_CONSENT");
        }
    }
    let _clear = ClearCarrier;

    export_run_consent(Custody::Personal);
    let cfg = EnrichConfig {
        schema_version: sovereign_enrichment_build::config::CONFIG_SCHEMA_VERSION,
        corpus_id: "consent-e2e".into(),
        pipeline_id: "philosophy_atlas".into(),
        source_path: "corpus:consent-e2e".into(),
        chapter_regex: String::new(),
        chat_model: "remote-model".into(),
        chat_models: None,
        embed_model: "embed".into(),
        base_url: host.base_url.into(),
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

    let received = host.recorded.lock().unwrap().clone();
    assert_eq!(
        received.len(),
        1,
        "the exported grant must reach the off-box host; got {result:?}"
    );
    assert_eq!(result.expect("released, then answered"), "ok");
}

/// THE FAILING INPUT: before 2026-10-03 a dispatch counted as local when its
/// provider's base equalled the client's own daemon base, so pointing that base
/// itself at a vendor (`--chat-url https://api.deepseek.com` with a bare model
/// id) sent the chunk with no grant. The refusal names the host it withheld
/// the chunk from.
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
