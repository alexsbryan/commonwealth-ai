// SPDX-License-Identifier: AGPL-3.0-or-later
//! The atlas walk's evidence path, off a persisted turn's message metadata.
//!
//! `eval run --synth` drives a real turn through `collect_turn` and then reads
//! everything it scores out of the PERSISTED assistant message — never out of a
//! return value (`runner.rs`, "Pull the persisted assistant row to recover the
//! metadata block"). So the walk echo the pipeline now carries
//! (`sovereign_core::runtime::AtlasWalkEcho`) reaches a study only if it rides
//! into that metadata object and is read back out of it. The write is one key
//! in `runtime/streaming.rs`, beside `meta_atlas_hits`; this module is the read.
//!
//! It is a module rather than four lines in `runner.rs` because that file sits
//! within tens of lines of its arch-gate ceiling, and because the read has a
//! case worth a test: see [`atlas_walk_from_metadata`].

use sovereign_core::runtime::{AtlasWalkEcho, ATLAS_WALK_META_KEY};

/// Read the turn's walk echo out of its persisted message metadata.
///
/// `None` means the walk did not run: the key is absent (a turn that took a
/// route which never builds a `KnowledgeQueryPlan`, or a message written before
/// the key existed) or it is `null` (the plan carried `None`). An echo that IS
/// present survives whole — including one with empty `nodes`, which is the
/// different fact "the walk ran and reached nothing" and is never collapsed
/// into the absent case.
///
/// A key that is present and does NOT parse is neither of those. Returning a
/// quiet `None` for it would report "no walk on this row" for a row where the
/// walk ran — an absence defaulted from a failure, which ARCH §6 forbids. That
/// case still yields `None`, because `EvalResult.atlas_walk` has nowhere else
/// to put it, but it says so on stderr first, tagged with the question id, so
/// the run's own log shows the loss instead of swallowing it.
pub fn atlas_walk_from_metadata(
    metadata: Option<&serde_json::Value>,
    question_id: &str,
) -> Option<AtlasWalkEcho> {
    let raw = metadata?.get(ATLAS_WALK_META_KEY)?;
    if raw.is_null() {
        return None;
    }
    match serde_json::from_value::<AtlasWalkEcho>(raw.clone()) {
        Ok(echo) => Some(echo),
        Err(e) => {
            eprintln!(
                "  [{question_id}] {ATLAS_WALK_META_KEY} metadata present but unreadable ({e}); \
                 this row reports NO walk and the walk may have run"
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use sovereign_core::runtime::AtlasWalkNodeEcho;

    /// The two atom ids this test's fixture minted. Every assertion below
    /// compares against THESE, not against "non-empty" — an atom id the echo
    /// path invented joins to nothing, and a study made of joins would read
    /// the invention as reach.
    const SEED_ATOM: &str = "wikish:atom:beta-0f2a";
    const HOP_ATOM: &str = "wikish:atom:gamma-91c4";

    fn fixture_echo() -> AtlasWalkEcho {
        AtlasWalkEcho {
            kind: "entity_lookup".to_string(),
            nodes: vec![
                AtlasWalkNodeEcho {
                    atlas: "wikish".to_string(),
                    atom_id: SEED_ATOM.to_string(),
                    name: "Beta".to_string(),
                    kind: "entity".to_string(),
                    subtype: "article".to_string(),
                    hop: 0,
                    via: None,
                    from: None,
                    score: 0.75,
                },
                AtlasWalkNodeEcho {
                    atlas: "wikish".to_string(),
                    atom_id: HOP_ATOM.to_string(),
                    name: "Gamma".to_string(),
                    kind: "claim".to_string(),
                    subtype: "definition".to_string(),
                    hop: 1,
                    via: Some("mentions".to_string()),
                    from: Some(SEED_ATOM.to_string()),
                    score: 0.31,
                },
            ],
            seeds: 1,
            edges_followed: 1,
            nodes_reached: 2,
            requests: 2,
            summaries_appended: 0,
            added: 1,
            considered: 2,
        }
    }

    /// The whole kill-row question in one assertion: does the walk survive the
    /// hop the `--synth` lane actually makes — pipeline value, into the message
    /// metadata object, persisted, read back out — with the atom ids intact?
    ///
    /// The metadata value is built the way `runtime/streaming.rs` builds it:
    /// the echo serialised into a `json!` object beside `retrieved_chunks` and
    /// `meta_atlas_hits`, not a hand-written JSON literal. A hand-written
    /// literal would test this test's idea of the schema; this tests the type's.
    #[test]
    fn synth_metadata_round_trips_atlas_walk() {
        let echo = fixture_echo();
        let metadata = json!({
            "streamed": true,
            "retrieved_chunks": [ { "title": "Beta", "snippet": "…" } ],
            "meta_atlas_hits": [],
            ATLAS_WALK_META_KEY: echo,
        });

        let back = atlas_walk_from_metadata(Some(&metadata), "q1").expect(
            "the walk echo must survive the metadata hop — it is the only route the \
                     --synth lane has, and without it every reach number is unsourced",
        );

        let ids: Vec<&str> = back.nodes.iter().map(|n| n.atom_id.as_str()).collect();
        assert_eq!(
            ids,
            vec![SEED_ATOM, HOP_ATOM],
            "the atom ids are the join key a reach study is made of; they must come back \
             exactly as the fixture minted them, in path order"
        );

        let hop = &back.nodes[1];
        assert_eq!(
            hop.via.as_deref(),
            Some("mentions"),
            "the edge followed to reach the hop"
        );
        assert_eq!(
            hop.from.as_deref(),
            Some(SEED_ATOM),
            "the node the hop was reached FROM — without it the path is a bag, not a walk"
        );
        assert_eq!(hop.hop, 1);
        assert_eq!(hop.kind, "claim");
        assert_eq!(hop.subtype, "definition");

        assert_eq!(back.kind, "entity_lookup");
        assert_eq!(
            (
                back.seeds,
                back.edges_followed,
                back.nodes_reached,
                back.requests
            ),
            (1, 1, 2, 2),
            "the walk's own ledger"
        );
        assert_eq!(
            (back.summaries_appended, back.added, back.considered),
            (0, 1, 2),
            "what the fetch did with the requests: considered > added is the fetch dropping"
        );
    }

    /// The three not-a-walk cases, which are not one case. A row whose key is
    /// missing and a row whose walk reached nothing both have no atoms to
    /// report, and only one of them measured anything.
    #[test]
    fn absent_and_empty_walks_stay_distinct() {
        assert!(
            atlas_walk_from_metadata(None, "q1").is_none(),
            "no metadata at all: the turn did not reach the handler that writes the key"
        );
        assert!(
            atlas_walk_from_metadata(Some(&json!({ "retrieved_chunks": [] })), "q1").is_none(),
            "key absent: the route never built a plan"
        );
        assert!(
            atlas_walk_from_metadata(Some(&json!({ ATLAS_WALK_META_KEY: null })), "q1").is_none(),
            "key null: the plan carried None, the walk did not run"
        );

        let ran_and_reached_nothing = AtlasWalkEcho {
            nodes: vec![],
            seeds: 0,
            nodes_reached: 0,
            ..fixture_echo()
        };
        let back = atlas_walk_from_metadata(
            Some(&json!({ ATLAS_WALK_META_KEY: ran_and_reached_nothing })),
            "q1",
        )
        .expect("a walk that ran and reached nothing is Some with empty nodes, never None");
        assert!(back.nodes.is_empty());
        assert_eq!(
            back.requests, 2,
            "it ran: the counters are still the walk's"
        );
    }

    /// Present and unreadable is a LOSS, not an absence — the case the reader
    /// prints before it gives up.
    #[test]
    fn unreadable_walk_metadata_yields_no_walk() {
        let corrupt = json!({ ATLAS_WALK_META_KEY: { "kind": "entity_lookup" } });
        assert!(atlas_walk_from_metadata(Some(&corrupt), "q1").is_none());
    }
}
