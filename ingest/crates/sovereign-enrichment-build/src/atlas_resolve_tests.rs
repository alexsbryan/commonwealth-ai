// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use sovereign_contracts::traits::Tool;

#[test]
fn parse_args_defaults_to_the_whole_layer() {
    let p = parse_args(&["brothers_karamazov".into()]).unwrap();
    assert_eq!(p.corpus_id, "brothers_karamazov");
    assert_eq!(p.phase, ResolvePhase::All);
}

#[test]
fn parse_args_accepts_explicit_phase_3a() {
    let p = parse_args(&["bk".into(), "--phase".into(), "3a".into()]).unwrap();
    assert_eq!(p.phase, ResolvePhase::P3a);
}

#[test]
fn parse_args_accepts_all_and_refuses_the_removed_3b_by_name() {
    let p = parse_args(&["bk".into(), "--phase".into(), "all".into()]).unwrap();
    assert_eq!(p.phase, ResolvePhase::All);
    let err = parse_args(&["bk".into(), "--phase".into(), "3b".into()]).unwrap_err();
    assert!(err.contains("`3b` was removed"), "got: {err}");
}

#[test]
fn parse_args_rejects_unknown_phase() {
    let err = parse_args(&["bk".into(), "--phase".into(), "42".into()]).unwrap_err();
    assert!(err.contains("unknown phase"), "got: {err}");
}

#[test]
fn parse_args_requires_corpus_id() {
    let err = parse_args(&[]).unwrap_err();
    assert!(err.contains("corpus-id"), "got: {err}");
}

/// The `atlas_resolve` workflow leaf validates its params before any IO: a
/// missing `corpus`, a bogus `phase`, and an unknown corpus all fail loudly.
/// (The happy path needs the daemon + a resolved Phase-1 cache — exercised by
/// the integration run, not a unit test.)
#[tokio::test]
async fn atlas_resolve_leaf_validates_params() {
    let ctx = ToolContext {
        conversation_id: Default::default(),
        task_id: None,
        working_directory: None,
        in_reasoning_loop: false,
        agent_session_token: None,
        turn_index: 0,
        ..Default::default()
    };
    assert!(AtlasResolveTool
        .declared()
        .execute(&serde_json::json!({}), &ctx)
        .await
        .is_err());
    assert!(AtlasResolveTool
        .declared()
        .execute(
            &serde_json::json!({ "corpus": "x", "phase": "bogus" }),
            &ctx
        )
        .await
        .is_err());
    assert!(AtlasResolveTool
        .declared()
        .execute(
            &serde_json::json!({ "corpus": "definitely-not-a-real-corpus-zzz" }),
            &ctx
        )
        .await
        .is_err());
}
