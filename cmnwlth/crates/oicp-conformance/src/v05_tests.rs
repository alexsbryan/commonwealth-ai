// SPDX-License-Identifier: AGPL-3.0-or-later
//! Every v0.5 check, green against the lawful fake and red against the fake
//! that breaks exactly its law (oicp-v0.5.md §6).

use oicp_types::ProviderManifest;

use crate::args::Args;
use crate::auth_checks::{check_auth_local_peer, check_auth_named_client};
use crate::checks::Host;
use crate::evidence_checks::{
    check_evidence_align, check_evidence_text, check_knowledge_document, typed_hit_failures,
    v05_feature_failures,
};
use crate::fake_host::{named_token, revoked_token, spawn, Breaks};
use crate::fixture::Library;
use crate::ingest_checks::{check_ingest_recipe, check_ingest_recipe_test, FixtureState};
use crate::report::{Check, CheckStatus};

struct Bench {
    host: Host,
    manifest: ProviderManifest,
    args: Args,
    lib: Library,
    fixture: FixtureState,
}

async fn bench(breaks: Breaks) -> Bench {
    let fake = spawn(breaks).await;
    let host = Host::new(&fake.url, None);
    let manifest: ProviderManifest = reqwest::get(format!("{}/oicp/v1/capabilities", fake.url))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let lib = Library::load().unwrap();
    Bench {
        host,
        manifest,
        args: Args {
            host: fake.url,
            named_token: Some(named_token()),
            revoked_token: Some(revoked_token()),
            ..Args::default()
        },
        fixture: FixtureState::Installed(lib.corpus_id.clone()),
        lib,
    }
}

#[track_caller]
fn green(c: &Check) {
    assert_eq!(c.status, CheckStatus::Pass, "{}: {}", c.id, c.detail);
}

#[track_caller]
fn red(c: &Check, says: &str) {
    assert_eq!(
        c.status,
        CheckStatus::Fail,
        "{} should fail: {}",
        c.id,
        c.detail
    );
    assert!(
        c.detail.contains(says),
        "{}: want {says:?} in {:?}",
        c.id,
        c.detail
    );
}

#[tokio::test]
async fn ingest_recipe_is_red_against_a_200_on_a_bad_recipe() {
    let b = bench(Breaks::default()).await;
    let (c, fixture) = check_ingest_recipe(&b.host, &b.manifest, &b.args, &b.lib).await;
    green(&c);
    assert_eq!(fixture, FixtureState::Installed(b.lib.corpus_id.clone()));

    let b = bench(Breaks {
        ok_on_bad_recipe: true,
        ..Breaks::default()
    })
    .await;
    let (c, fixture) = check_ingest_recipe(&b.host, &b.manifest, &b.args, &b.lib).await;
    red(&c, "must be 400");
    assert!(matches!(fixture, FixtureState::Unavailable(_)));
}

#[tokio::test]
async fn ingest_recipe_serves_the_fixture_from_a_dir_on_a_local_host() {
    let b = bench(Breaks::default()).await;
    let dir = std::env::temp_dir().join(format!("oicp-fixture-{}", std::process::id()));
    let args = Args {
        fixture_dir: Some(dir.display().to_string()),
        ..b.args.clone()
    };
    let (c, _) = check_ingest_recipe(&b.host, &b.manifest, &args, &b.lib).await;
    green(&c);
    let written = std::fs::read_to_string(dir.join("okafor2019.txt")).unwrap();
    assert_eq!(written, b.lib.doc("okafor2019").unwrap().text);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn ingest_recipe_test_is_red_against_a_report_missing_a_stage() {
    let b = bench(Breaks::default()).await;
    green(&check_ingest_recipe_test(&b.host, &b.manifest, &b.args, &b.lib).await);
    let b = bench(Breaks {
        drop_extract_stage: true,
        ..Breaks::default()
    })
    .await;
    red(
        &check_ingest_recipe_test(&b.host, &b.manifest, &b.args, &b.lib).await,
        "stage `extract` missing",
    );
}

#[tokio::test]
async fn knowledge_document_is_red_against_emptied_metadata() {
    let b = bench(Breaks::default()).await;
    green(&check_knowledge_document(&b.host, &b.manifest, &b.lib, &b.fixture).await);
    let b = bench(Breaks {
        empty_metadata: true,
        ..Breaks::default()
    })
    .await;
    red(
        &check_knowledge_document(&b.host, &b.manifest, &b.lib, &b.fixture).await,
        "came back as None",
    );
}

#[tokio::test]
async fn evidence_text_is_red_against_an_exact_one_off() {
    let b = bench(Breaks::default()).await;
    green(&check_evidence_text(&b.host, &b.manifest, &b.lib, &b.fixture).await);
    let b = bench(Breaks {
        exact_off_by_one: true,
        ..Breaks::default()
    })
    .await;
    red(
        &check_evidence_text(&b.host, &b.manifest, &b.lib, &b.fixture).await,
        "the text there says",
    );
}

#[tokio::test]
async fn evidence_align_is_red_against_a_missing_texts_digest() {
    let b = bench(Breaks::default()).await;
    green(&check_evidence_align(&b.host, &b.manifest, &b.lib, &b.fixture).await);
    let b = bench(Breaks {
        omit_texts_digest: true,
        ..Breaks::default()
    })
    .await;
    red(
        &check_evidence_align(&b.host, &b.manifest, &b.lib, &b.fixture).await,
        "no texts_digest",
    );
}

#[tokio::test]
async fn the_evidence_checks_say_why_they_could_not_judge() {
    let b = bench(Breaks::default()).await;
    let gone = FixtureState::Unavailable("ingest:recipe not advertised".into());
    for c in [
        check_knowledge_document(&b.host, &b.manifest, &b.lib, &gone).await,
        check_evidence_text(&b.host, &b.manifest, &b.lib, &gone).await,
        check_evidence_align(&b.host, &b.manifest, &b.lib, &gone).await,
    ] {
        assert_eq!(c.status, CheckStatus::Skip, "{}", c.id);
        assert!(c.detail.starts_with("could not judge"), "{}", c.detail);
    }
}

#[tokio::test]
async fn auth_named_client_is_red_against_the_loopback_first_resolver() {
    let b = bench(Breaks::default()).await;
    green(&check_auth_named_client(&b.host, &b.manifest, &b.args).await);
    let b = bench(Breaks {
        loopback_first: true,
        ..Breaks::default()
    })
    .await;
    red(
        &check_auth_named_client(&b.host, &b.manifest, &b.args).await,
        "an unissued credential must be 401",
    );
}

#[tokio::test]
async fn auth_local_peer_is_red_against_an_address_only_guard() {
    let b = bench(Breaks::default()).await;
    let c = check_auth_local_peer(&b.host, Some(&b.manifest)).await;
    green(&c);
    assert!(
        c.detail.contains("refused by name where trusted"),
        "{}",
        c.detail
    );
    let b = bench(Breaks {
        address_only_locality: true,
        ..Breaks::default()
    })
    .await;
    red(
        &check_auth_local_peer(&b.host, Some(&b.manifest)).await,
        "a foreign Origin from this client was admitted",
    );
}

#[tokio::test]
async fn auth_local_peer_is_red_against_a_permissive_cors_layer() {
    let b = bench(Breaks {
        permissive_cors: true,
        ..Breaks::default()
    })
    .await;
    red(
        &check_auth_local_peer(&b.host, Some(&b.manifest)).await,
        "still grants https://evil.example",
    );
}

#[test]
fn typed_hits_need_their_document_when_knowledge_document_is_advertised() {
    let bare = serde_json::json!({"content": "x", "corpus_id": "c", "score": 0.5});
    let failures = typed_hit_failures(&[bare]);
    assert_eq!(failures, ["hit #0 carries no document"]);
}

#[test]
fn v05_features_must_co_occur_with_their_sections() {
    let mut m = ProviderManifest::new(vec![]);
    m.features = vec![
        oicp_types::features::EVIDENCE_TEXT.into(),
        oicp_types::features::INGEST_RECIPE.into(),
    ];
    let f = v05_feature_failures(&m);
    assert_eq!(f.len(), 2, "{f:?}");
    assert!(f[0].contains("knowledge.evidence"));
    assert!(f[1].contains("ingest:v1"));
}
