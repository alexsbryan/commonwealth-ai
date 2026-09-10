// SPDX-License-Identifier: AGPL-3.0-or-later
//! sv-surface D3 + D4: the atlas-browse surface and the MeshApp atom
//! readers hold NO local reader. Both read the daemon's routes, in both
//! boot modes, because Local means the daemon is in-process over this
//! process's own `corpus_engine`.
//!
//! # The state this makes unrepresentable
//!
//! A second atlas reader in the desktop. Two of them were deleted here:
//!
//!   * `atlas_commands.rs` constructed a `FileAtlasReader` over
//!     `engine.index_dir()` in each of six browse commands, and carried a
//!     private `section_id → chunk_id` cache — a full 2.8 GB chunks.lance
//!     scan on Wikipedia — so the same map was built once per SURFACE
//!     rather than once per host.
//!   * `commands/meshapp.rs::load_atoms` opened `atlas/atoms.json` with
//!     `corpus_engine::enrichment::atlas::read_atlas_atoms`, which means
//!     it answered NOTHING on an attached boot: the atlas belongs to the
//!     daemon's index dir, not this process's.
//!
//!   * the same file's THIRTEEN explorer ops resolved the corpus's index
//!     directory from this process's `CorpusEngine::installed_indexes()`
//!     and called `sovereign_meshapp::*` on it — thirteen more reads that
//!     answer nothing on an attached boot, each re-applying its own page
//!     default and clamp beside the route's.
//!
//! All three are now one call each onto `sovereign_mesh::{atlas_http,
//! reading_http, meshapp_http}` over `client_base_url()`. Bring any back —
//! a `FileAtlasReader` in a browse command, a `read_atlas_atoms` or a
//! `sovereign_meshapp::load_graph` in the MeshApp bridge — and this goes
//! red naming the rule.
//!
//! # Calibration (ARCH §18.1 — name the failing input)
//!
//! PRODUCTION lines only, and only above `#[cfg(test)]`. Comment lines
//! are excluded for the reason `reading_wire_types_census` records: the
//! doc comment that explains why the local reader is gone has to NAME the
//! reader to do so, and prose that mentions a needle is documentation,
//! not a second path. `atlas_commands.rs`'s own test module is excluded
//! for the analogous reason — its two fixture tests drive
//! `FileAtlasReader` directly ON PURPOSE, to validate the on-disk atlas
//! the ROUTE will read; that is a fixture check, not a desktop read path.

use std::path::Path;

fn production_source(rel: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
    let src =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    // Cut the test module off, then drop comment lines.
    let prod = match src.find("\n#[cfg(test)]") {
        Some(i) => &src[..i],
        None => &src[..],
    };
    prod.lines()
        .filter(|l| {
            let t = l.trim_start();
            !t.starts_with("//")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn the_atlas_browse_surface_holds_no_reader() {
    let code = production_source("src/atlas_commands.rs");
    assert!(
        !code.contains("FileAtlasReader"),
        "sv-surface D4: atlas_commands.rs constructs a FileAtlasReader again. \
         The six browse commands are one call each onto sovereign_mesh::\
         atlas_http's routes over client_base_url() — the daemon holds the \
         reader, and holds it once. A local one is a second decider for the \
         same six answers, and it is the one that cannot see an attached \
         daemon's index_dir."
    );
    assert!(
        !code.contains("index_dir"),
        "sv-surface D4: atlas_commands.rs reads the corpus engine's index_dir \
         again. The browse commands do not know where the atlas lives; the \
         route does."
    );
    assert!(
        !code.contains("section_chunk_index") && !code.contains("SectionMapState"),
        "sv-surface D4: the section→chunk cache is back in atlas_commands.rs. \
         Building it is a full chunks.lance scan (2.8 GB / ~90s on Wikipedia); \
         atlas_http::atom_detail owns that cache now, so it is built ONCE per \
         host rather than once per surface."
    );
}

#[test]
fn the_meshapp_atom_readers_read_the_wire() {
    let code = production_source("src/commands/meshapp.rs");
    assert!(
        !code.contains("read_atlas_atoms"),
        "sv-surface D3: commands/meshapp.rs opens atlas/atoms.json directly \
         again. The three atom readers (read_corpus, search_parcels, \
         parcel_analytics) go through wire_atoms -> \
         TurnClient::corpus_atoms_all over GET /internal/corpus/{{c}}/atoms — \
         the same bytes in both boot modes. A direct file read answers \
         nothing at all on an attached boot."
    );
    // The wire read is the ONE path — not a Local arm beside it.
    assert!(
        !code.contains("is_attach_mode"),
        "sv-surface D3: commands/meshapp.rs forks on the boot mode. The \
         campaign's directive is one path in both modes; Local means the \
         daemon is in-process, not that there is a second reader."
    );
    // The gate CANNOT cross the wire (campaign X2): the webview label is
    // host-assigned to a window in THIS process. Every gated command must
    // still carry it — a wire repoint that dropped it would be a silent
    // authorization hole, so this counts it rather than trusting review.
    let authorize_calls = code.matches("authorize(&installs, webview.label()").count();
    assert!(
        authorize_calls >= 17,
        "sv-surface X2: only {authorize_calls} MeshApp commands still gate on \
         authorize(&installs, webview.label(), ...). The permission gate cannot \
         cross the wire — the webview label is host-assigned to a window in \
         THIS process and is the only unspoofable caller identity the bridge \
         has. A repoint that drops it is an authorization hole, not a cleanup."
    );
}

/// The thirteen explorer ops (sv-surface D3's delete half). Each one is
/// now `TurnClient::meshapp_*` over `GET /internal/meshapp/{corpus}/...`,
/// which runs THIS projection on the daemon's index dir.
///
/// The names are checked as CALLS (`sovereign_meshapp::load_graph(`), not
/// as bare identifiers: the commands still name `sovereign_meshapp`'s DTOs
/// as their return types, and must — a repoint that changed the type the
/// frontend receives would be feature loss dressed as cleanup. What may
/// not come back is the projection running HERE.
const LOCAL_PROJECTIONS: &[&str] = &[
    "sovereign_meshapp::load_graph(",
    "sovereign_meshapp::graph_nodes(",
    "sovereign_meshapp::node_detail(",
    "sovereign_meshapp::findings(",
    "sovereign_meshapp::search_entities(",
    "sovereign_meshapp::load_claims(",
    "sovereign_meshapp::load_questions(",
    "sovereign_meshapp::reconciliation(",
    "sovereign_meshapp::subgraph(",
    "sovereign_meshapp::corpus_stats(",
    "sovereign_meshapp::timeline(",
    "sovereign_meshapp::read_chunk(",
    "sovereign_meshapp::document_feed(",
    "sovereign_meshapp::wrapped::wrapped_artifact(",
];

#[test]
fn the_meshapp_explorer_ops_read_the_wire() {
    let code = production_source("src/commands/meshapp.rs");
    for call in LOCAL_PROJECTIONS {
        assert!(
            !code.contains(call),
            "sv-surface D3: commands/meshapp.rs runs the projection `{call}` \
             in-process again. The thirteen explorer ops are one \
             TurnClient::meshapp_* call each onto GET /internal/meshapp/\
             {{corpus}}/... — the daemon runs the very same sovereign-meshapp \
             function over ITS index dir, which is the only one an attached \
             boot can see. Keep the DTO, lose the read."
        );
    }
    assert!(
        !code.contains("resolve_index_path") && !code.contains("installed_indexes"),
        "sv-surface D3: commands/meshapp.rs resolves a corpus's on-disk index \
         directory again. The bridge does not know where an index lives — the \
         route does, and it resolves it against the DAEMON's corpus engine. A \
         local resolve is how thirteen commands came to answer `corpus is not \
         installed` on a boot where it was installed all along."
    );
    // One decider for the page defaults (ARCH §10.6): the route applies
    // them and says what it applied. A command that re-derives one is a
    // second decider whose answer silently wins over the host's.
    //
    // Scoped to the EXPLORER commands — everything from `meshapp_graph`
    // down. The three parcel folds above it (read_corpus, search_parcels,
    // parcel_analytics) page their own fold over `corpus_atoms_all`, which
    // serves every atom; their caps are this file's to decide and stay.
    let explorer = &code[code
        .find("pub async fn meshapp_graph(")
        .expect("meshapp_graph is the first explorer command; the section marker moved")..];
    for clamp in [
        ".unwrap_or(50).min(500)",
        ".unwrap_or(25).min(100)",
        ".unwrap_or(100).min(500)",
        ".unwrap_or(30).min(80)",
        ".unwrap_or(14).clamp(1, 90)",
    ] {
        assert!(
            !explorer.contains(clamp),
            "sv-surface D3: commands/meshapp.rs re-applies the page clamp \
             `{clamp}`. The defaults and maxima live in meshapp_http's \
             GRAPH_/ENTITY_/ATOM_/SUBGRAPH_/FEED_ constants; passing Option \
             through is what makes the host the one decider."
        );
    }
}
