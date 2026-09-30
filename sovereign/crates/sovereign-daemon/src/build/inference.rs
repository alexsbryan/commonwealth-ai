//! A terminal's inference provider — extracted from `run_daemon` (§3.3).
//!
//! A terminal holds no weights and forwards to its entry node. Every other
//! node's serving is serve's (pb-serve-distributes): the daemon dials it or a
//! distribution hosts it, and no engine is assembled here.

use std::sync::Arc;

use sovereign_core::setup_config::SetupConfig;

/// A terminal's provider, or `Err(())` when its entry binding is unusable
/// (the caller returns 1; the operator-facing diagnostics are printed here).
///
/// `mesh` is the view a `terminal` resolves its entry node through. Unbound at
/// this point in the boot — it answers "no peers" until `DeferredDaemon::bind`
/// — which is correct: a terminal that boots before gossip converges reports
/// its entry node unreachable and starts serving the moment it appears.
pub fn terminal_provider(
    config: &SetupConfig,
    mesh: Arc<crate::DeferredDaemon>,
) -> Result<Arc<oicp_client::SplitInferenceProvider>, ()> {
    // ── terminal: hold nothing, forward everything ──────────────────────
    //
    // A `terminal`-class node has no `[models]`, so there is no GGUF to load
    // and no engine to hand back. Its provider is the SAME
    // `SplitInferenceProvider` the CLI's chat bootstrap and the attach-mode
    // desktop already use — chat and embeddings each go to the entry node over
    // HTTP, with `embed_batch` routed to the batch path so corpus ingest does
    // not degrade to one round-trip per chunk (`oicp-client/src/lib.rs:1477`).
    //
    // It inherits `resident_slots() == []` from the trait, which is what makes
    // `build_self_manifest` advertise nothing: this node must not claim its
    // entry node's model as its own, or peers would route to a machine that
    // holds no weights (§18.3).
    //
    // The chat id is the mesh ALIAS `primary`, not a GGUF stem. Aliases are the
    // routable names — the entry node resolves one to whichever slot is hot,
    // and a concrete quant would pin this terminal to one machine's filename
    // and break the day that node swaps quants (`docs/ANCHOR_NODE.md`).
    // `node_class()`, not `models.is_none()`: a `[models]` table that exists but
    // names no primary holds nothing, and a node holding nothing with an entry
    // is a terminal however its file is shaped.
    if config.node_class() == sovereign_core::setup_config::NodeClass::Terminal {
        // `node_class()` answered Terminal, which it only does when a binding
        // is present — but read it rather than assert it, so a future change to
        // one cannot make the other lie.
        if let Some(binding) = config.node.binding() {
            // `"unknown"` on absence, never `""`. That literal is the TRAIT's
            // sentinel for "this provider cannot identify its embed model"
            // (`sovereign-contracts::traits::embed_model_id`), and the readers
            // test for it by value — `memory.rs`'s `model_known` gate is the
            // one that decides whether a stored embedding may be matched and
            // persisted under this name. An empty string passes that gate as a
            // real model named empty-string, so rows land under it and a later
            // real embed model mis-ranks against them (§18.3).
            let embed_model_id = config
                .local_embed_model_id()
                .unwrap_or_else(|| "unknown".to_string());
            tracing::info!(
                entry = %binding.describe(),
                embed_model = %embed_model_id,
                "terminal node: holding no weights, forwarding chat + embeddings to the entry node"
            );
            use sovereign_core::setup_config::EntryBinding;
            let provider = match binding {
                // Bound by identity: resolved through the mesh on every turn,
                // so the terminal follows its entry node across addresses and
                // reaches it over whatever path the transport offers —
                // including the iroh one, which on an encrypted mesh is the
                // only ingress there is.
                EntryBinding::Node(hex) => {
                    // The terminal resolves its entry node through the roster
                    // port; the daemon's `DeferredDaemon` is that source until
                    // it is bound.
                    let mesh_port: Arc<dyn sovereign_contracts::venue::VenueSource> =
                        Arc::clone(&mesh) as Arc<_>;
                    let resolver = match crate::build::entry_endpoint::EntryNodeEndpoint::parse(
                        mesh_port, &hex,
                    ) {
                        Ok(r) => Arc::new(r),
                        Err(e) => {
                            eprintln!("error: [node] entry_node is unusable — {e}");
                            return Err(());
                        }
                    };
                    // THIS node's identity, stamped on every request the
                    // terminal sends its entry node. Without it the entry node
                    // admits a terminal's turns as its own local traffic:
                    // unrationable, unaccounted, and invisible on
                    // `/status.inference.peer_requests` — which is why the
                    // two-machine corroboration ("fire an embedding, watch the
                    // tally move") could not pass on 2026-08-31.
                    //
                    // Resolved with `persist::resolve_self_node_id`, THE canonical
                    // answer to "who is this node" — the same function the daemon
                    // itself calls later in this boot (`daemon_cmd/mod.rs`, the
                    // `resolve_self_node_id` beside the note store). Two calls to
                    // one implementation, so they cannot disagree.
                    //
                    // Not `DeferredDaemon::self_node_id()`: that is async and
                    // answers `None` until the join handshake, and this provider is
                    // built before it. Not `load_node_id` either — that reads ONLY
                    // `<data_dir>/node_id`, and an existing mesh commonly carries
                    // the id inside `mesh.json` with the standalone file never
                    // materialised. A terminal that joined a mesh is exactly that
                    // case, so the narrow read left it unstamped on the boot right
                    // after `mesh join` and the tally stayed empty. Not
                    // `load_or_generate_self_node_id` either: the comment at the
                    // daemon's own call site says it "would ignore (2)", the
                    // mesh.json id, "which is exactly the bug we're fixing".
                    //
                    // `resolve_self_node_id` also repairs the case where the file
                    // holds a PEER's id — adopting that would make this terminal
                    // stamp another machine's identity onto its own traffic, which
                    // is worse than being unstamped.
                    let node_id_hex = Some(
                        sovereign_mesh::persist::resolve_self_node_id(&config.data.dir).to_hex(),
                    );
                    Arc::new(oicp_client::SplitInferenceProvider::resolved(
                        resolver,
                        // Always off-box, and structurally so: peers are the
                        // mesh MINUS this node, so an entry node resolved from
                        // the peer set is by construction another machine. The
                        // locus is passed rather than sniffed because the
                        // resolved address is often an iroh bridge on
                        // 127.0.0.1 whose far end is that other machine —
                        // reading the address would report ForwardsOnBox and
                        // honour `local_only` for a turn that leaves the host.
                        sovereign_core::traits::ServingLocus::ForwardsOffBox,
                        "primary".to_string(),
                        embed_model_id,
                        config.effective_context_size(),
                        String::new(),
                        node_id_hex,
                    ))
                }
                // Bound by address: an entry node that is not a mesh member —
                // a daemon on this machine, or one on a trusted LAN. Unchanged
                // from the 2026-08-30 shape, including deriving the locus from
                // the address, which is the whole truth in this case.
                EntryBinding::Address(url) => Arc::new(oicp_client::SplitInferenceProvider::new(
                    &url,
                    "primary".to_string(),
                    embed_model_id,
                    config.effective_context_size(),
                    String::new(),
                )),
            };
            return Ok(provider);
        }
    }
    // Only a terminal reaches here (`boot_serving`); a node that holds
    // weights is served by serve. Refused by name, never assembled.
    tracing::error!(target: "serving_path", node_class = ?config.node_class(), "boot: a terminal provider was asked of a node that is not a terminal");
    eprintln!("error: this node is not a terminal; its serving is serve's, and this daemon assembles no engine");
    Err(())
}
