// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

/// The literal string `svrn mesh bench` sends (`mesh_bench::PRIMARY_ALIAS`),
/// and the one `build_self_manifest` advertises to mesh peers.
const BENCH_AND_MESH_ALIAS: &str = "commonwealth/primary";

#[test]
fn the_child_claims_the_advertised_primary_alias() {
    let ids = distributed_primary_model_ids("Qwen3.5-122B-A10B-UD-Q5_K_XL-00001-of-00003");

    assert!(
        ids.iter().any(|m| m == BENCH_AND_MESH_ALIAS),
        "a node whose primary is child-distributed advertises `{BENCH_AND_MESH_ALIAS}` \
         to peers and resolves it locally; if the child does not CLAIM it, the request \
         falls through to the in-process engine and the fast slot answers with a \
         different model and no error. Got: {ids:?}"
    );
    // The bare form too — OpenAI clients and opencode configs use it.
    assert!(ids.iter().any(|m| m == "primary"), "got: {ids:?}");
    // And the concrete id, which is how a peer addresses this exact GGUF.
    assert!(
        ids.iter()
            .any(|m| m == "Qwen3.5-122B-A10B-UD-Q5_K_XL-00001-of-00003"),
        "got: {ids:?}"
    );
}

/// Whatever `SLOT_ALIAS_POLICY` says is resolvable for `primary` must be
/// claimable. Adding a synonym there without this passing means that synonym
/// silently reaches the wrong model.
#[test]
fn every_resolvable_primary_alias_is_claimable() {
    let ids = distributed_primary_model_ids("some-model");
    for alias in sovereign_contracts::venue::resolution_alias_keys("primary") {
        assert!(
            ids.contains(&alias),
            "`{alias}` resolves to the primary slot but would not route to the \
             distributed child"
        );
    }
}

#[test]
fn a_stem_that_collides_with_an_alias_is_not_duplicated() {
    let ids = distributed_primary_model_ids("primary");
    let count = ids.iter().filter(|m| *m == "primary").count();
    assert_eq!(count, 1, "got: {ids:?}");
}
