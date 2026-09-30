// SPDX-License-Identifier: AGPL-3.0-or-later
//! The `svrn atlas` sub-verbs that are svrn's: `budget`, `status`,
//! `list-corpora`, `list-atoms`, `show-atom` and `typed-extension` read svrn's
//! atlas views and store (phase-b-70 (2)). The rest of `atlas` is ingest's and
//! runs in `svrn-ingest` (sovereign-pipeline); the dispatcher routes each
//! sub-verb to its owner (pb-cli-llm-ingest-move).

pub mod budget;
pub mod inspect;
pub mod status;
pub mod typed_extension;

pub async fn run_atlas(args: &[String]) -> i32 {
    let Some((first, rest)) = args.split_first() else {
        return crate::ingest_verb_elsewhere("atlas", "");
    };
    match first.as_str() {
        "budget" => budget::run(rest).await,
        "status" => status::run(rest).await,
        "list-corpora" => inspect::run_list_corpora(rest).await,
        "list-atoms" => inspect::run_list_atoms(rest).await,
        "show-atom" => inspect::run_show_atom(rest).await,
        "typed-extension" => typed_extension::run(rest).await,
        other => crate::ingest_verb_elsewhere("atlas", other),
    }
}
