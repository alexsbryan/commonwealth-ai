// SPDX-License-Identifier: AGPL-3.0-or-later
//! sv-surface D5: the local-corpus REGISTRY reads in the desktop's
//! command surface hold no `LocalCorpusManager`. They read the daemon's
//! `/internal/corpus/local/` routes over `client_base_url()`, in both boot
//! modes, because Local means the daemon is in-process over the very Arc
//! `state.rs` handed to `WatchedSubsystem::install`.
//!
//! # The state this makes unrepresentable
//!
//! A second local-corpus manager answering a question the daemon's manager
//! already answers. Eight commands were repointed here —
//! `lc_list`, `lc_remove`, `lc_incomplete_jobs`, `lc_check_git`,
//! `lc_list_snapshots`, `lc_rollback`, `lc_clean`, `lc_search` — plus the
//! config reads inside `lc_ingest` and `lc_enrich_now`. On an attached
//! boot each of those used to answer out of the DESKTOP's manager while
//! the daemon ingested into its own, so a vault registered by one was
//! invisible to the other.
//!
//! # The rule this file encodes, which is NOT "everything the route list
//! # covers" (ARCH §7 — make it structural, not remembered)
//!
//! A command crosses when its answer is a function of the REGISTRY ON
//! DISK — state both managers see. A command STAYS when its answer is a
//! function of ONE manager instance's in-memory state and the producer of
//! that state has not crossed. Crossing a consumer while its producer
//! stays is how a working pane starts answering "not found":
//!
//!   * `manager.get_preview` reads the `cluster_results` cache that
//!     `manager.cluster` filled — and `lc_cluster` is app-local (its only
//!     output is the Tauri emitter). `lc_write_tags` reaches the same
//!     cache through `get_preview`.
//!   * `manager.cancel` flips the flag in the CorpusEngine cancellation
//!     registry of the engine that is running the job — and `lc_ingest`'s
//!     bespoke arm runs it here.
//!   * `ocr_available` reports the `OcrCtx` installed on THIS instance by
//!     `install_ocr_ctx_for_app` — and the OCR ingest arm runs here too.
//!
//! `the_paired_stays_are_still_paired` below is that rule as a test: it
//! fires only while the producer is still local, so the rung that moves
//! `lc_cluster` / the ingest job across finds it already satisfied.
//!
//! # Calibration (ARCH §18.1 — name the failing input)
//!
//! PRODUCTION lines only, and only above `#[cfg(test)]`; comment lines are
//! dropped, for the reason `reading_wire_types_census` records — the doc
//! comment that explains why a manager call is gone has to NAME it to do
//! so, and prose that mentions a needle is documentation, not a second
//! path.
//!
//! Watched to fail: `manager.list().await` planted back in `lc_list`
//! (red at `the_registry_reads_read_the_wire`, naming `.list(`), then
//! removed.

use std::path::Path;

fn production_source(rel: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
    let src =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let prod = match src.find("\n#[cfg(test)]") {
        Some(i) => &src[..i],
        None => &src[..],
    };
    prod.lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

const LC_COMMANDS: &str = "src/local_corpus_commands.rs";

/// The manager calls whose answers are a function of the registry on
/// disk. Every one has a route; none of them may run here again.
///
/// Spelled with the leading dot and the argument the command passed, so
/// the wire call that replaced each one (`.lc_snapshots::<SnapshotMeta>(
/// &corpus_id)`) does not match its own needle — what matches is the
/// manager method coming back.
const CROSSED_MANAGER_CALLS: &[&str] = &[
    "manager.list()",
    ".remove(&corpus_id)",
    ".incomplete_jobs()",
    ".check_git(&corpus_id)",
    ".list_snapshots(&corpus_id)",
    ".rollback(&corpus_id",
    ".clean(&corpus_id)",
    ".search(&corpus_id",
];

#[test]
fn the_registry_reads_read_the_wire() {
    let code = production_source(LC_COMMANDS);
    for call in CROSSED_MANAGER_CALLS {
        assert!(
            !code.contains(call),
            "sv-surface D5: local_corpus_commands.rs calls `{}` on a local \
             LocalCorpusManager again. That answer is a function of the \
             registry on disk, and lc_http serves it over \
             /internal/corpus/local/ from the daemon's manager — the only \
             one an attached boot shares with the ingest that wrote it. \
             Keep the return type, lose the read.",
            call
        );
    }
    // The crossing is the ONE path, not a Local arm beside it.
    assert!(
        !code.contains("is_attach_mode"),
        "sv-surface D5: local_corpus_commands.rs forks on the boot mode. \
         The campaign's directive is one path in both modes; Local means \
         the daemon is in-process, not that there is a second manager."
    );
    // `lc_search`'s 10 moved DOWN to the route (ARCH §10.6). A command
    // that re-derives it is a second decider whose answer silently wins.
    assert!(
        !code.contains("limit.unwrap_or(10)"),
        "sv-surface D5/§10.6: local_corpus_commands.rs re-applies \
         lc_search's default limit. The host applies it; passing Option \
         through is what makes the route the one decider."
    );
}

/// Every command that STAYS local is paired with the producer that pins
/// it. While the producer is local, the consumer must be too — see the
/// module header. Each assertion goes quiet on its own the moment its
/// producer crosses, so this does not stand in the way of the next rung.
#[test]
fn the_paired_stays_are_still_paired() {
    let code = production_source(LC_COMMANDS);

    if code.contains(".cluster(&corpus_id") {
        assert!(
            code.contains(".get_preview("),
            "sv-surface D5: lc_get_preview reads the wire while lc_cluster \
             still runs here. `get_preview` reads the `cluster_results` \
             cache that `cluster` filled ON THIS INSTANCE — repointing the \
             consumer alone makes the Organizer answer `no clustering run \
             on record` on an attached boot. Move lc_cluster across first."
        );
        assert!(
            code.contains(".write_tags("),
            "sv-surface D5: lc_write_tags reads the wire while lc_cluster \
             still runs here. `write_tags` reaches the same \
             `cluster_results` cache through `get_preview`. Move lc_cluster \
             across first."
        );
    }

    if code.contains(".ingest(&corpus_id") {
        assert!(
            code.contains(".cancel(&corpus_id)"),
            "sv-surface D5: lc_cancel crosses while lc_ingest's bespoke arm \
             still ingests here. `cancel` flips a flag in the cancellation \
             registry of the engine RUNNING the job; a cancel sent to the \
             daemon cannot stop a job running in this process. Move the \
             ingest job across first."
        );
        assert!(
            code.contains(".ocr_available()"),
            "sv-surface D5: lc_ocr_available crosses while lc_ingest's OCR \
             arm still runs here. The OcrCtx is per-instance \
             (install_ocr_ctx_for_app installs it on THIS one), so a wire \
             answer of `true` would offer OCR that the local arm then \
             cannot perform. Move the ingest job across first."
        );
    }
}
