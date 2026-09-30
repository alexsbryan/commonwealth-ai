// SPDX-License-Identifier: AGPL-3.0-or-later
//! The `svrn enrich` sub-verbs that are svrn's: `raptor`, `raptor-index` and
//! `summary-atoms` write through svrn's tools (phase-b-70 (2)). The rest of
//! `enrich` is ingest's and runs in `svrn-ingest` (sovereign-pipeline); the
//! dispatcher routes each sub-verb to its owner (pb-cli-llm-ingest-move).

pub mod raptor;
pub mod raptor_census;
pub mod raptor_index;
pub mod summary_atoms;

pub async fn run_enrich(args: &[String]) -> i32 {
    let Some((cmd, rest)) = args.split_first() else {
        return crate::ingest_verb_elsewhere("enrich", "");
    };
    match cmd.as_str() {
        "raptor" => raptor::cmd_raptor(rest).await,
        "raptor-index" => raptor_index::cmd_raptor_index(rest).await,
        "summary-atoms" => summary_atoms::cmd_summary_atoms(rest).await,
        other => crate::ingest_verb_elsewhere("enrich", other),
    }
}
