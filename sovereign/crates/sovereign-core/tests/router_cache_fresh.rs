// SPDX-License-Identifier: AGPL-3.0-or-later
//! CI freshness gate for the pre-built router-embed cache.
//!
//! The desktop ships `sovereign/router/router-embed-cache.json` baked into the
//! binary so first launch HITS the cache instead of re-embedding ~310 router
//! exemplars sequentially (minutes on a CPU-only embed slot — the prescriptive
//! worst case). This test is the GUARANTEE that the committed artifact never
//! ships stale: it fails the build whenever the cache doesn't cover the current
//! exemplars, or wasn't built for the prescribed embed model. No inference —
//! pure text-key coverage + a model-identity fingerprint compare — so it runs
//! free in every `cargo test`, and as the `desktop-release.yml` pre-flight.
//!
//! Belt-and-suspenders with `scripts/bump-desktop-version.sh`, which
//! regenerates the cache when stale so a release is fresh by construction; this
//! test is the suspenders — even a hand-cut release can't ship stale.

use sovereign_core::models_manifest::DEFAULT_MANIFEST;
use sovereign_core::router_bootstrap::baked_exemplar_specs;
use sovereign_core::router_embed_cache::{check_cache_fresh, BAKED_ROUTER_EMBED_CACHE};

#[test]
fn committed_router_embed_cache_is_fresh() {
    let specs = baked_exemplar_specs().expect("baked router exemplar TOMLs must parse");
    let fingerprint = DEFAULT_MANIFEST
        .prescribed_embed_fingerprint()
        .expect("models.toml must declare a `default`-profile embed model");

    if let Err(reason) = check_cache_fresh(BAKED_ROUTER_EMBED_CACHE, &specs, &fingerprint) {
        panic!(
            "\nsovereign/router/router-embed-cache.json is STALE:\n  {reason}\n\n\
             Regenerate it:\n  \
             cargo build -p sovereign-cli-llm && sovereign router-cache rebuild\n\n\
             (scripts/bump-desktop-version.sh runs this automatically at release.)\n"
        );
    }
}
