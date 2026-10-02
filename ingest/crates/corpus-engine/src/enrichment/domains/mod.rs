// SPDX-License-Identifier: AGPL-3.0-or-later
pub mod business_email;
pub mod conversational;
pub mod institutional;
pub mod personal;
pub mod philosophy;

#[cfg(test)]
mod tests {
    use crate::enrichment::domain::{Domain, SkeletonStorage};
    use crate::enrichment::domains::business_email::BusinessEmailDomain;
    use crate::enrichment::domains::conversational::ConversationalDomain;
    use crate::enrichment::domains::institutional::InstitutionalDomain;
    use crate::enrichment::domains::personal::PersonalDomain;
    use crate::enrichment::domains::philosophy::PhilosophyDomain;

    /// covers: EN-9
    ///
    /// Corpus-shape awareness, asserted across domains rather than within one.
    /// Each domain module already tests its OWN `clustering_config`, and every
    /// one of those tests checks only the three shared constants
    /// (`min_cluster_size`, `epsilon`, `label_sample_size`) — so a copy-paste
    /// that gave every domain the same caps passes all of them.
    ///
    /// The caps are the shape-aware part. `clustering.rs` projects only when
    /// `reduced_dims > 0` and samples only when `max_cluster_points > 0`, so
    /// handing a personal corpus philosophy's numbers means a random
    /// projection to 128 dims over a handful of memories — throwing away
    /// resolution on data that never needed the O(n^2 d) relief — and giving
    /// philosophy personal's numbers means clustering a 200k-chunk corpus
    /// unsampled at full width.
    #[test]
    fn a_large_corpus_domain_and_a_small_one_do_not_share_clustering_caps() {
        let big = PhilosophyDomain.clustering_config();
        let small = PersonalDomain.clustering_config();

        // The large-corpus shape: sample, and project before HDBSCAN.
        assert_eq!(big.max_cluster_points, 30_000);
        assert_eq!(big.reduced_dims, 128);
        // The small-corpus shape: neither. 0 is the documented "no limit" /
        // "no reduction" sentinel in ClusteringConfig.
        assert_eq!(small.max_cluster_points, 0);
        assert_eq!(small.reduced_dims, 0);

        // The copy-paste catch, stated as the thing that must not be true.
        assert_ne!(
            (big.max_cluster_points, big.reduced_dims),
            (small.max_cluster_points, small.reduced_dims),
            "one clustering config applied to every corpus shape is the failure this guards"
        );

        // The other small-corpus domains carry the small shape too — so the
        // difference above is about corpus shape, not about philosophy being
        // the odd one out for some unrelated reason.
        for (name, cfg) in [
            ("conversational", ConversationalDomain.clustering_config()),
            ("institutional", InstitutionalDomain.clustering_config()),
        ] {
            assert_eq!(cfg.max_cluster_points, 0, "{name} must not cap");
            assert_eq!(cfg.reduced_dims, 0, "{name} must not project");
        }

        // And the delegating domain INHERITS its inner domain's shape rather
        // than minting a third set of numbers nobody tuned.
        let delegated = BusinessEmailDomain::new().clustering_config();
        let inner = ConversationalDomain.clustering_config();
        assert_eq!(delegated.max_cluster_points, inner.max_cluster_points);
        assert_eq!(delegated.reduced_dims, inner.reduced_dims);
        assert_eq!(delegated.min_cluster_size, inner.min_cluster_size);
    }

    /// The one table, pinned: where each built-in domain publishes its field
    /// model. A domain that moves to `AtlasAtoms` without its digest reader
    /// moving takes that reader's views dark, and a domain left on
    /// `JsonAndLance` after its reader moves goes dark the other way — neither
    /// failure raises anything at runtime, both are silent, so the pairing is
    /// asserted here rather than remembered (ARCH §7).
    ///
    /// | Domain | Storage | Who reads the digest |
    /// |---|---|---|
    /// | `philosophy` | `AtlasAtoms` | `turn_prepass::splice_ambient_field_digests`, from the atlas |
    /// | `personal` / `conversational` / `institutional` | `JsonAndLance` | `sovereign-tools::knowledge_view::manager`, from `field_skeleton.json` |
    /// | `business_email` | delegates to `conversational` | as above |
    #[test]
    fn each_domain_publishes_where_its_digest_reader_looks() {
        assert!(
            matches!(
                PhilosophyDomain.skeleton_storage(),
                SkeletonStorage::AtlasAtoms
            ),
            "philosophy's digest is rendered from the atlas (ei-7b)"
        );
        for (name, storage) in [
            ("personal", PersonalDomain.skeleton_storage()),
            ("conversational", ConversationalDomain.skeleton_storage()),
            ("institutional", InstitutionalDomain.skeleton_storage()),
        ] {
            assert!(
                matches!(storage, SkeletonStorage::JsonAndLance),
                "{name} is a KnowledgeView domain — its reader has not been ported, \
                 so moving it to AtlasAtoms takes that view dark"
            );
        }
        // The delegating domain inherits rather than minting a third answer.
        assert!(matches!(
            BusinessEmailDomain::new().skeleton_storage(),
            SkeletonStorage::JsonAndLance
        ));
    }
}
