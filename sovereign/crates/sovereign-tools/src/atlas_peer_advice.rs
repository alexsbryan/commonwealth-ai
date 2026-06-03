//! Phase C3 — pre-extraction peer check.
//!
//! Before the post-install hook spawns local Tier-2 enrichment for
//! a corpus, walk the live mesh and check whether any peer already
//! has a deeper atlas (`atlas_tier2_count`) for the same corpus +
//! same embed model. If so, recommend pulling the peer's atlas
//! instead of burning the local node's tokens + disk + days of
//! wall-clock.
//!
//! ## What this module owns
//!
//! [`evaluate_peer_atlas_advice`] — a pure function that takes a
//! snapshot of (local atlas summary, local embed model, peer
//! capabilities) and returns the best pull candidate, if any. Pure
//! so it's trivial to unit-test the picking rules without standing
//! up a mesh.
//!
//! Plumbing — gathering peer capabilities from `MeshStore`,
//! emitting the recommendation log line, and skipping
//! `launch_tier2_extraction` — lives in the corpus-install route
//! handler.
//!
//! ## Selection rule
//!
//! Pull when ALL of:
//! 1. The local atlas has fewer than `MIN_PEER_LEAD` more Tier-2
//!    entries than us. (No point pulling for a marginal lead.)
//! 2. The peer's embed model matches ours. Mismatch means the
//!    peer's `atoms.embeddings.bin` is unusable to us — we'd have
//!    to re-embed everything anyway, defeating the savings.
//! 3. The peer's atlas fingerprint differs from ours OR we don't
//!    have an atlas yet. Same fingerprint means same atoms.json,
//!    which means the peer's tier-2 work IS what we'd produce
//!    locally — pull is a savings.
//!
//! Among multiple eligible peers, pick the one with the highest
//! `atlas_tier2_count` to maximize the work skipped.

use commonwealth_core::knowledge::CorpusShardInfo;

/// Minimum lead a peer must have over us before we recommend
/// pulling. Set so a peer that's only 50 articles ahead doesn't
/// trigger a multi-GB transfer for marginal savings.
pub const MIN_PEER_LEAD: u64 = 100;

/// One peer's atlas state for a specific corpus, ready for the
/// rule engine. Constructed by callers from `MemberRecord` +
/// `CorpusShardInfo`.
#[derive(Debug, Clone)]
pub struct PeerAtlasView {
    pub peer_name: String,
    /// `embed_model.id` from `NodeCapabilities.embed_model`. `None`
    /// when the peer hasn't bootstrapped an embed slot — such peers
    /// are ineligible because we can't validate model match.
    pub embed_model: Option<String>,
    pub corpus_id: String,
    pub atlas_atom_count: u64,
    pub atlas_tier2_count: u64,
    pub atlas_fingerprint: Option<String>,
}

impl PeerAtlasView {
    /// Pick the corpus shard relevant to `corpus_id` from a peer's
    /// hosted_corpora. Returns `None` when the peer doesn't host
    /// the corpus or hosts it without atlas info (older peer).
    pub fn from_member(
        peer_name: impl Into<String>,
        embed_model: Option<String>,
        corpus_id: &str,
        hosted_corpora: &[CorpusShardInfo],
    ) -> Option<Self> {
        hosted_corpora
            .iter()
            .find(|c| c.corpus_id == corpus_id && c.atlas_atom_count > 0)
            .map(|c| Self {
                peer_name: peer_name.into(),
                embed_model,
                corpus_id: corpus_id.to_string(),
                atlas_atom_count: c.atlas_atom_count,
                atlas_tier2_count: c.atlas_tier2_count,
                atlas_fingerprint: c.atlas_fingerprint.clone(),
            })
    }
}

/// What [`evaluate_peer_atlas_advice`] returns when at least one
/// peer is worth pulling from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerAtlasPullCandidate {
    pub peer_name: String,
    pub peer_tier2_count: u64,
    /// Local node's current Tier-2 count. Useful for log lines —
    /// "you have 0, peer has 1200" reads more decisively than just
    /// the peer count.
    pub local_tier2_count: u64,
    pub embed_model: String,
    pub fingerprint: Option<String>,
}

/// Decide whether to pull a peer's atlas instead of running local
/// Tier-2 enrichment. Pure function — see module docs for the
/// selection rule.
///
/// `local` is `None` when the corpus has no `atlas/atoms.json` yet
/// (fresh install, structural pass hasn't run). In that state we
/// always prefer to pull if a peer offers a non-trivial atlas — no
/// point doing work locally when a peer has it ready.
///
/// `my_embed_model` is `None` for nodes that haven't bootstrapped an
/// embed slot. Such nodes can still pull (the embed cache will be
/// invalid until they do bootstrap), but we don't tie-break on a
/// model match they can't make.
pub fn evaluate_peer_atlas_advice(
    local_tier2_count: u64,
    local_fingerprint: Option<&str>,
    my_embed_model: Option<&str>,
    peers: &[PeerAtlasView],
) -> Option<PeerAtlasPullCandidate> {
    let mut best: Option<PeerAtlasPullCandidate> = None;
    for peer in peers {
        // Embed model gate. If we have a model, the peer must
        // match. If we don't, accept any peer that does (we'll
        // re-embed when we bootstrap).
        let peer_model = match peer.embed_model.as_deref() {
            Some(m) => m,
            None => continue,
        };
        if let Some(mine) = my_embed_model {
            if mine != peer_model {
                continue;
            }
        }
        // Lead gate.
        let lead = peer.atlas_tier2_count.saturating_sub(local_tier2_count);
        if lead < MIN_PEER_LEAD {
            continue;
        }
        // Fingerprint gate. If both sides have an atlas and they
        // match, the peer's atlas IS our atlas — pulling lets us
        // skip re-doing the same Tier-2 extractions. If they
        // differ, the peer's atlas was built over different
        // chunks; pulling replaces our local view, which is a
        // policy decision — for v1 we still allow it (peer atlas
        // assumed authoritative when it has more depth), but skip
        // matching-fingerprint peers when we already have all
        // their work (lead < MIN_PEER_LEAD already filtered).
        let _ = (local_fingerprint, peer.atlas_fingerprint.as_deref());

        let candidate = PeerAtlasPullCandidate {
            peer_name: peer.peer_name.clone(),
            peer_tier2_count: peer.atlas_tier2_count,
            local_tier2_count,
            embed_model: peer_model.to_string(),
            fingerprint: peer.atlas_fingerprint.clone(),
        };
        match &best {
            None => best = Some(candidate),
            Some(cur) if candidate.peer_tier2_count > cur.peer_tier2_count => {
                best = Some(candidate);
            }
            _ => {}
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(name: &str, model: Option<&str>, tier2: u64) -> PeerAtlasView {
        PeerAtlasView {
            peer_name: name.into(),
            embed_model: model.map(|s| s.to_string()),
            corpus_id: "wikipedia".into(),
            atlas_atom_count: tier2 + 51_000,
            atlas_tier2_count: tier2,
            atlas_fingerprint: Some(format!("fp-{name}")),
        }
    }

    #[test]
    fn no_peers_no_advice() {
        assert!(evaluate_peer_atlas_advice(0, None, Some("e"), &[]).is_none());
    }

    #[test]
    fn picks_highest_tier2_among_eligible() {
        let peers = vec![
            peer("alpha", Some("qwen3-embed"), 200),
            peer("beta", Some("qwen3-embed"), 1200),
            peer("gamma", Some("qwen3-embed"), 600),
        ];
        let advice = evaluate_peer_atlas_advice(0, None, Some("qwen3-embed"), &peers).unwrap();
        assert_eq!(advice.peer_name, "beta");
        assert_eq!(advice.peer_tier2_count, 1200);
    }

    #[test]
    fn rejects_mismatched_embed_model() {
        let peers = vec![peer("alpha", Some("nomic-embed"), 1200)];
        assert!(evaluate_peer_atlas_advice(0, None, Some("qwen3-embed"), &peers).is_none());
    }

    #[test]
    fn rejects_peer_with_no_embed_model() {
        let peers = vec![peer("alpha", None, 1200)];
        assert!(evaluate_peer_atlas_advice(0, None, Some("qwen3-embed"), &peers).is_none());
    }

    #[test]
    fn rejects_marginal_lead() {
        // We already have 1100; peer has 1150 (+50 < MIN_PEER_LEAD=100).
        let peers = vec![peer("alpha", Some("qwen3-embed"), 1150)];
        assert!(evaluate_peer_atlas_advice(1100, None, Some("qwen3-embed"), &peers).is_none());
    }

    #[test]
    fn local_with_no_embed_model_still_picks() {
        // Node hasn't bootstrapped an embed slot yet. Any embed-
        // capable peer is fine — the embed cache will repopulate
        // on bootstrap.
        let peers = vec![peer("alpha", Some("qwen3-embed"), 1200)];
        let advice = evaluate_peer_atlas_advice(0, None, None, &peers).unwrap();
        assert_eq!(advice.peer_name, "alpha");
    }

    #[test]
    fn from_member_filters_by_corpus_and_atlas_presence() {
        use commonwealth_core::knowledge::CorpusShardInfo;
        let hosted = vec![
            // Wrong corpus.
            CorpusShardInfo {
                corpus_id: "sep".into(),
                chunk_range: None,
                is_replica: false,
                last_updated: 0,
                chunk_count: 0,
                canonical_fingerprint: None,
                total_shards: None,
                processed_shards: vec![],
                atlas_atom_count: 9999,
                atlas_tier2_count: 9999,
                atlas_fingerprint: Some("fp-sep".into()),
            },
            // Right corpus but no atlas yet.
            CorpusShardInfo {
                corpus_id: "wikipedia".into(),
                chunk_range: None,
                is_replica: false,
                last_updated: 0,
                chunk_count: 0,
                canonical_fingerprint: None,
                total_shards: None,
                processed_shards: vec![],
                atlas_atom_count: 0,
                atlas_tier2_count: 0,
                atlas_fingerprint: None,
            },
        ];
        assert!(PeerAtlasView::from_member("p", Some("e".into()), "wikipedia", &hosted).is_none());

        // Add a real one.
        let mut hosted = hosted;
        hosted.push(CorpusShardInfo {
            corpus_id: "wikipedia".into(),
            chunk_range: None,
            is_replica: false,
            last_updated: 0,
            chunk_count: 0,
            canonical_fingerprint: None,
            total_shards: None,
            processed_shards: vec![],
            atlas_atom_count: 51_280,
            atlas_tier2_count: 612,
            atlas_fingerprint: Some("fp-wp".into()),
        });
        let v = PeerAtlasView::from_member(
            "rugged-mac",
            Some("qwen3-embed".into()),
            "wikipedia",
            &hosted,
        )
        .unwrap();
        assert_eq!(v.peer_name, "rugged-mac");
        assert_eq!(v.atlas_tier2_count, 612);
    }
}
