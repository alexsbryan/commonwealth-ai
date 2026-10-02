// SPDX-License-Identifier: AGPL-3.0-or-later
//! The `svrn corpus` sub-verbs that are svrn's: `ingest` and `share` run svrn's
//! workflow client, `pull` is svrn's member act, and `catalog`,
//! `extract-entities` and the watched-folder verbs open svrn's store or call
//! its routes (phase-b-70 (2)). The rest of `corpus` is ingest's and runs in
//! `svrn-ingest` (sovereign-pipeline); the dispatcher routes each sub-verb to
//! its owner (pb-cli-llm-ingest-move).

mod ingest;
mod pull;

pub async fn run_corpus(args: &[String]) -> i32 {
    let Some((first, rest)) = args.split_first() else {
        return crate::ingest_verb_elsewhere("corpus", "");
    };
    match first.as_str() {
        "ingest" => ingest::cmd_corpus_ingest(rest).await,
        "share" => ingest::cmd_corpus_share(rest).await,
        "pull" => pull::cmd_corpus_pull(rest).await,
        "catalog" => crate::corpus_catalog_cmd::run_catalog(rest).await,
        "extract-entities" => crate::corpus_extract_entities_cmd::run_extract_entities(rest).await,
        // Proxied through the daemon's `/internal/corpus/watch/*` routes.
        "watch" => crate::corpus_watch_cmd::run_register(rest).await,
        "watch-list" => crate::corpus_watch_cmd::run_list(rest).await,
        "watch-status" => crate::corpus_watch_cmd::run_status(rest).await,
        "watch-pause" => crate::corpus_watch_cmd::run_pause(rest).await,
        "watch-resume" => crate::corpus_watch_cmd::run_resume(rest).await,
        "watch-confirm-deletion" => crate::corpus_watch_cmd::run_confirm_deletion(rest).await,
        "watch-sync-now" => crate::corpus_watch_cmd::run_sync_now(rest).await,
        "watch-add-root" => crate::corpus_watch_cmd::run_add_root(rest).await,
        "watch-remove-root" => crate::corpus_watch_cmd::run_remove_root(rest).await,
        "watch-remove" => crate::corpus_watch_cmd::run_remove(rest).await,
        other => crate::ingest_verb_elsewhere("corpus", other),
    }
}
