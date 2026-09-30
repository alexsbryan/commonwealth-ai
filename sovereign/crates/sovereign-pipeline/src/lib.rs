// SPDX-License-Identifier: AGPL-3.0-or-later
//! Generic ingestion-pipeline driver.
//!
//! See `recipe.rs` for the per-corpus surface and `driver.rs` for the
//! claim/dispatch/ack loop. The two are coupled only through the
//! `Worklist` primitive in `worklist.rs`, so swapping out the driver
//! (or running a non-driver consumer like the dashboard) is trivial.

pub mod adaptive;
pub mod classifier;
pub mod driver;
pub mod recipe;
pub mod status;
pub mod worklist;

pub use driver::{run_recipe, DriverConfig, RunSummary, Shutdown};
pub use recipe::{Recipe, RecipeError};
pub use status::{report, StatusReport};
pub use worklist::{State, Stats, Worklist, WorklistError, WorklistRow};

// ingest's CLI verbs (`svrn enrich|corpus|atlas|meta-atlas|recipe|pipeline|
// alignment`, and `bench atlas`), moved from sovereign-cli-llm by
// pb-cli-llm-ingest-move with their module names kept at the crate root, so
// every `crate::<module>` path inside them resolves unchanged. `svrn-ingest`
// answers them through [`run_cli_verb`].
mod alignment_cmd;
mod atlas_cmd;
mod bench_atlas;
mod corpus_cmd;
use sovereign_cli_base::corpus_resolve;
mod corpus_scrub_cmd;
mod corpus_snapshot_cmd;
mod daemon_inference;
mod enrich_cmd;
mod meta_atlas_cmd;
mod pipeline_cmd;
mod recipe_cmd;

pub use corpus_cmd::run_corpus;
pub use enrich_cmd::run_enrich;

/// The verbs [`run_cli_verb`] answers, by their `svrn` spelling.
pub const CLI_VERBS: &[&str] = &[
    "enrich",
    "corpus",
    "atlas",
    "meta-atlas",
    "recipe",
    "pipeline",
    "alignment",
    "bench",
];

/// Run one of ingest's CLI verbs with the arguments after it; `None` when
/// `verb` is not in [`CLI_VERBS`].
pub async fn run_cli_verb(verb: &str, rest: &[String]) -> Option<i32> {
    Some(match verb {
        "enrich" => enrich_cmd::run_enrich(rest).await,
        "corpus" => corpus_cmd::run_corpus(rest).await,
        "atlas" => atlas_cmd::run_atlas(rest).await,
        "meta-atlas" => meta_atlas_cmd::run_meta_atlas(rest).await,
        "recipe" => recipe_cmd::run_recipe(rest).await,
        "pipeline" => pipeline_cmd::run_pipeline(rest).await,
        "alignment" => alignment_cmd::run_alignment(rest).await,
        // ingest's white-box lane under bench's spelling (b024722fa); the
        // rest of `bench` is sovereign-cli-bench's.
        "bench" => match rest.first().map(String::as_str) {
            Some("atlas") => bench_atlas::cmd_atlas(&rest[1..]).await,
            sub => {
                let sub = sub.unwrap_or("");
                tracing::debug!(sub, "bench verb reached svrn-ingest");
                eprintln!(
                    "svrn-ingest: `bench {sub}` is bench's; it runs in sovereign-cli-bench. \
                     Run it as `svrn bench {sub}`."
                );
                2
            }
        },
        _ => return None,
    })
}

/// A sub-verb of an ingest verb that svrn answers (it opens svrn's store,
/// calls svrn's routes or uses svrn's tools, phase-b-70 (2)): a named pointer,
/// exit 2, never "unknown".
pub(crate) fn svrn_verb_elsewhere(verb: &str, sub: &str) -> i32 {
    tracing::debug!(verb, sub, "svrn sub-verb reached svrn-ingest");
    eprintln!(
        "svrn-ingest: `{verb} {sub}` is svrn's; it runs in sovereign-cli-llm. \
         Run it as `svrn {verb} {sub}`."
    );
    2
}
