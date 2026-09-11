// SPDX-License-Identifier: AGPL-3.0-or-later
//! sv-surface D5 + D8: the local-corpus REGISTRY reads in the desktop's
//! command surface hold no `LocalCorpusManager`. They read the daemon's
//! `/internal/corpus/local/` routes over `client_base_url()`, in both boot
//! modes, because Local means the daemon is in-process over the very Arc
//! `state.rs` handed to `WatchedSubsystem::install`.
//!
//! # The state this makes unrepresentable
//!
//! A second local-corpus manager answering a question the daemon's manager
//! already answers. Eight commands were repointed on D5 —
//! `lc_list`, `lc_remove`, `lc_incomplete_jobs`, `lc_check_git`,
//! `lc_list_snapshots`, `lc_rollback`, `lc_clean`, `lc_search` — plus the
//! config reads inside `lc_ingest` and `lc_enrich_now`. On an attached
//! boot each of those used to answer out of the DESKTOP's manager while
//! the daemon ingested into its own, so a vault registered by one was
//! invisible to the other.
//!
//! D8 added three more, and `commands/corpus.rs`'s Library listing with
//! them: the INGEST JOB itself (`.ingest(`), `lc_cancel` (`.cancel(`) and
//! `lc_ocr_available` (`.ocr_available(`). The job crossed because the
//! progress route it needed now exists — `GET
//! /internal/corpus/local/{c}/ingest/progress`, over a terminal receipt
//! carrying `IngestStats` — and the other two crossed WITH it, because
//! they were only ever pinned here by it (below).
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
//!   * `manager.cancel` flipped the flag in the CorpusEngine cancellation
//!     registry of the engine RUNNING the job, and `ocr_available`
//!     reported the `OcrCtx` installed on THIS instance — both pinned by
//!     `lc_ingest`'s bespoke arm. D8 crossed that arm, so both moved with
//!     it and their conditional assertions are gone rather than left
//!     standing over a producer that is no longer here. A conditional that
//!     can never fire is not a gate (ARCH §18.1).
//!
//! `the_paired_stays_are_still_paired` below is that rule as a test: it
//! fires only while the producer is still local, so the rung that moves
//! `lc_cluster` across finds it already satisfied.
//!
//! # D9c: the rule read in the OTHER direction (2026-09-10)
//!
//! `the_paired_stays_are_still_paired` guards a consumer crossing ahead
//! of its producer. The defect the real-mode harness found on run 5 is
//! the same pairing broken the other way round — a PRODUCER left behind
//! while its consumers crossed — and nothing here fired, because the
//! producer was not one of the calls this file names.
//!
//! `lc_pre_scan` registered the corpus on the DESKTOP's manager. D8
//! crossed the ingest job (502304f63), which also removed the ingest
//! pairing conditional that had been standing over it. On an attached
//! boot the daemon's manager never learned the corpus, and the ingest
//! answered `404 … corpus 'folder-corpus-2918e9ebc0b5' is not
//! registered locally`. In Local mode the two managers are one instance,
//! which is why every unit test stayed green.
//!
//! `the_registration_producer_crossed_with_its_consumers` is that half
//! as a POSITIVE test: while any registration-dependent consumer reads
//! the wire, no `.register(` may run on a local manager, and
//! `.lc_register::<` must be here.
//!
//! The one manager read that legitimately remains in `lc_pre_scan` is
//! `snapshot_root()` — this process's storage LAYOUT
//! (`{data_dir}/vault-snapshots`), not corpus state. It lands in the
//! config as `write_back.snapshot_dir`, so the daemon honours the path
//! it was handed. Both boots resolve the same `data_dir` today; a boot
//! where they diverge would put an attached vault's snapshots under the
//! app's root rather than the daemon's, and the fix then is a
//! daemon-side default stamped by the register route, not a second
//! guess here.
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
//! removed. D8 re-watched with `.ingest(&corpus_id` planted back into
//! `lc_ingest` — see `quality/twin-plants.toml`, family
//! `local-corpus-ingest`.

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

/// The Library listing joins the registry into every notebook row. It is
/// the same read under a different roof, so it obeys the same rule.
const CORPUS_COMMANDS: &str = "src/commands/corpus.rs";

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
    // D8: the ingest job and the two surfaces it pinned.
    ".ingest(&corpus_id",
    ".cancel(&corpus_id)",
    ".ocr_available()",
];

/// The routes that REFUSE an unregistered corpus — every one answers
/// `corpus '{id}' is not registered locally` when the daemon's manager
/// does not hold it. Spelled as the wire call the command makes, so the
/// list reads as "these consumers are across".
const REGISTRATION_DEPENDENT_WIRE_CALLS: &[&str] = &[
    ".lc_ingest::<",
    ".lc_ingest_progress::<",
    ".lc_cancel::<",
    ".lc_get::<",
    ".lc_search::<",
    ".lc_snapshots::<",
    ".lc_check_git::<",
    ".lc_clean::<",
    ".lc_rollback::<",
    ".lc_remove(",
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

/// The Library listing does not assemble itself, and an absent source is
/// an ERROR there — not an empty map.
///
/// The swallow this pins was measured: `notebook_list` degraded a missing
/// manager to `HashMap::new()`, which is byte-identical to "no local
/// corpora are registered". Every vault in the list then lost its
/// source-kind, display name and scope and rendered as a catalog row,
/// with nothing anywhere saying why (ARCH §18.3).
///
/// # D8 pinned the registry READ; D9b moved the whole FOLD (2026-09-10)
///
/// The assertion here was `commands/corpus.rs` contains
/// `.lc_list::<LocalCorpusConfig>()` — the registry crossing D8 landed.
/// 2a9a9e91e took the five-source fold itself down to
/// `GET /internal/corpus/notebooks` (serving only its inputs would have
/// kept the copy here and added five round trips), and D9b consumed it.
/// So `corpus.rs` reads no registry at all now, and the positive half
/// moves with it: the shelf crossing is what must exist. That is a
/// STRENGTHENING, not a relaxation — the file that used to be able to
/// swallow the registry no longer touches it, and the swallow it could
/// still perform (`Ok(Vec::new())` on a missing engine) is gone in the
/// same rung: `notebook_list` returns the daemon's words on failure.
#[test]
fn the_library_listing_reads_the_registry_over_the_wire() {
    let code = production_source(CORPUS_COMMANDS);
    assert!(
        code.contains(".corpus_notebooks::<NotebookSummary>()"),
        "sv-surface D8/D9b: commands/corpus.rs no longer reads the Library \
         shelf over the wire. The five-source fold — installed indexes, the \
         local-corpus registry, the file atlas, conv-tiered enrichment, the \
         governance oplog — is the daemon's, and on an attached boot its \
         manager is the one the ingest wrote to."
    );
    assert!(
        !code.contains(".lc_list::<LocalCorpusConfig>()"),
        "sv-surface D9b: commands/corpus.rs reads the local-corpus registry \
         directly again. The registry is ONE of the shelf's five sources \
         and the daemon joins all five; re-reading one of them here is a \
         second assembler for a row that already has an owner (§10.6)."
    );
    assert!(
        !code.contains("state.local_corpus"),
        "sv-surface D8: commands/corpus.rs holds a LocalCorpusManager \
         again. That answer is a function of the registry on disk."
    );
    assert!(
        !code.contains("None => HashMap::new(),"),
        "sv-surface D8/§18.3: the local-corpus registry read in \
         commands/corpus.rs defaults an absence to an empty map again. An \
         empty registry and an unreachable one render identically — every \
         vault silently loses its source-kind, name and scope. Report the \
         failure; do not substitute a shape that reads like an answer."
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

    // The ingest pair (lc_cancel, lc_ocr_available) is GONE from this test,
    // not silently satisfied: D8 crossed the producer, so both consumers
    // crossed with it and are pinned by `CROSSED_MANAGER_CALLS` above. A
    // conditional whose guard can no longer be true proves nothing.
}

/// The registration PRODUCER crosses with the consumers that need it.
///
/// The rule of `the_paired_stays_are_still_paired`, read the other way
/// round. That test catches a consumer crossing ahead of its producer;
/// this one catches a producer left behind while its consumers cross —
/// which is what actually happened, and what no test here could see.
///
/// Every needle in `REGISTRATION_DEPENDENT_WIRE_CALLS` is a route that
/// answers `corpus '{id}' is not registered locally` when the DAEMON's
/// manager does not hold the corpus. `.register(` is the only call that
/// makes it hold one. While the first list is non-empty, the second call
/// may not run against a local manager.
///
/// # Calibration (ARCH §18.1 — name the failing input)
///
/// Watched to fail 2026-09-10, both halves, in this tree:
///
/// ```text
/// manager.register(config.clone()) planted back into lc_pre_scan
///     → `.register(` assertion, red
/// .lc_register::<LocalCorpusConfig, LocalCorpusConfig>( removed
///     → `.lc_register::<` assertion, red
/// ```
///
/// The FIRST list is asserted non-empty for the reason §18.2 gives: a
/// guard whose premise is silently false proves nothing, and if every
/// consumer ever moved back off the wire this test would pass by
/// vacuum.
#[test]
fn the_registration_producer_crossed_with_its_consumers() {
    let code = production_source(LC_COMMANDS);

    let consumers: Vec<&&str> = REGISTRATION_DEPENDENT_WIRE_CALLS
        .iter()
        .filter(|needle| code.contains(**needle))
        .collect();
    assert!(
        !consumers.is_empty(),
        "calibration: not one registration-dependent route is read over the \
         wire in local_corpus_commands.rs, so this test would pass without \
         checking anything. Either the needles in \
         REGISTRATION_DEPENDENT_WIRE_CALLS have been respelled, or the \
         consumers moved back off the wire — both want a look."
    );

    assert!(
        !code.contains(".register("),
        "sv-surface D9c: local_corpus_commands.rs registers a corpus on a \
         LOCAL LocalCorpusManager while {consumers:?} read the daemon's. \
         The registry on disk is what those routes answer from, and on an \
         attached boot the two managers are different instances — the \
         corpus goes into this process's and the daemon 404s the ingest \
         (`corpus '...' is not registered locally`, real-mode journeys run \
         5). Register over POST /internal/corpus/local, and take the \
         corpus_id from the response: the manager keeps an existing id \
         when the path is already registered under one."
    );

    assert!(
        code.contains(".lc_register::<"),
        "sv-surface D9c: nothing in local_corpus_commands.rs registers a \
         corpus over the wire, yet {consumers:?} require one to be \
         registered on the daemon's manager. A consumer with no producer \
         is the 404 this pairing exists to prevent."
    );
}
