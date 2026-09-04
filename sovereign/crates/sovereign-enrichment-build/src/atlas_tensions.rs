// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn enrich atlas-tensions` — Phase A Step 4 (Landing 3,
//! deterministic half).
//!
//! Reads the resolved atlas (atoms.json + edges.json) and
//! enumerates tension *candidates*: atom pairs plausibly in
//! tension based on two deterministic signals — claim ↔ claim
//! entity-overlap and claim ↔ state entity-overlap. Writes the
//! candidate list to `atlas/tension_candidates.json`.
//!
//! Landing 4 will add an LLM classification pass that consumes
//! this file and materialises real `Tension` edges on
//! `edges.json`. Until then the candidates are a reviewable
//! "what to look at next" list.
//!
//! Candidate selection is strategy-driven (`Pipeline::tension_strategy`):
//! the literary/philosophy atlas uses the deterministic graph signals
//! (entity-overlap + cross-position; intra-cluster is implemented in
//! `analysis::tensions` but still fed an empty cluster map here pending a
//! stable sketch → atom mapping). Custom-ontology (`custom_atlas`)
//! corpora — governance rule-sets, policy docs — instead use an embedding
//! top-K net: each rule is embedded via the daemon embed slot and paired
//! with its nearest neighbours, because their cross-document,
//! uniformly-worded rules defeat the entity/cluster signals. See
//! `TensionStrategy` for why.

use std::path::PathBuf;

use corpus_engine::enrichment::atlas::{
    analysis::{
        drop_non_comparable_pairs, restrict_claims_to_types,
        tensions::{
            drop_same_named_speaker_pairs, select_candidates, select_embedding_topk,
            CandidateSelectionInput, TensionCandidatesOutput, TensionStrategy,
        },
        BetweenOutcome, ComparabilityReport, CorpusShape,
    },
    read_atlas_atoms, read_atlas_ontology, write_tension_candidates, AtomEnvelope, ATLAS_DIRNAME,
};
use corpus_engine_vocab::ontology::OntologyPolicies;

use super::config::EnrichConfig;
use super::inference_client::DaemonInferenceClient;
use super::paths;

/// What the deterministic candidate pass produced.
#[derive(Debug, Clone)]
pub struct TensionCandidatesReport {
    pub claims: usize,
    pub states: usize,
    pub entities: usize,
    pub strategy: TensionStrategy,
    /// True when the pipeline could not be resolved and the Graph
    /// strategy was substituted for it. "The pipeline asked for graph"
    /// and "we could not tell what the pipeline asked for" produce the
    /// same candidates and are not the same fact (ARCH §18.3) — before
    /// 2026-08-26 an `unwrap_or_default()` made them indistinguishable.
    pub strategy_defaulted: bool,
    pub same_speaker_dropped: usize,
    /// What `tension.between` did before selection — including the INERT
    /// case, which is not the same outcome as "declared nothing" and is not
    /// the same as "dropped nothing".
    pub between: BetweenOutcome,
    /// What the declared `same` criterion removed, and on what coverage.
    /// Default (every field empty) when the corpus declares nothing.
    pub comparability: ComparabilityReport,
    /// The shape the selector was derived from.
    pub shape: CorpusShape,
    pub candidates: usize,
    pub written_to: PathBuf,
}

impl TensionCandidatesReport {
    /// One line naming what this pass produced, for the build
    /// orchestrator's `StepDone` event.
    pub fn summary(&self) -> String {
        let strategy = match self.strategy {
            TensionStrategy::Graph => "graph".to_string(),
            TensionStrategy::EmbeddingTopK { k, floor } => {
                format!("embedding top-{k} (floor {floor})")
            }
        };
        let mut s = format!(
            "{} candidate pair(s) via {strategy} over {} claim(s) + {} state(s) + {} entity atom(s)",
            self.candidates, self.claims, self.states, self.entities
        );
        if self.strategy_defaulted {
            s.push_str(" (strategy defaulted — pipeline did not resolve)");
        }
        if self.same_speaker_dropped > 0 {
            s.push_str(&format!(
                "; {} same-speaker pair(s) dropped",
                self.same_speaker_dropped
            ));
        }
        match self.between {
            BetweenOutcome::Applied { dropped, .. } if dropped > 0 => {
                s.push_str(&format!("; {dropped} claim(s) outside `tension.between`"));
            }
            BetweenOutcome::Inert => s.push_str("; `tension.between` inert (no claim_kind)"),
            _ => {}
        }
        if self.comparability.dropped > 0 {
            s.push_str(&format!(
                "; {} non-comparable pair(s) dropped",
                self.comparability.dropped
            ));
        }
        s
    }
}

/// Select tension candidates and write `tension_candidates.json`.
///
/// Keeps its progress printing: the embedding strategy embeds every
/// claim through the daemon and the per-strategy lines are the
/// operator's view of a slow pass.
pub async fn run(parsed: &ParsedTensions) -> Result<TensionCandidatesReport, String> {
    let cfg = EnrichConfig::require(&parsed.corpus_id)
        .map_err(|e| format!("loading enrichment config: {e}"))?;

    let atlas_dir = atlas_dir_for(&cfg.corpus_id);
    let atoms = read_atlas_atoms(&atlas_dir).map_err(|e| {
        format!(
            "reading {}/atoms.json: {e}. Run `svrn enrich atlas-resolve {} --phase all` first.",
            atlas_dir.display(),
            cfg.corpus_id
        )
    })?;

    // Partition atoms by kind. Claim + State drive entity-overlap
    // candidates; entities feed the cross-position concept-overlap
    // signal (claims attributed to different position concepts whose
    // content mentions the same entity).
    let mut claims = Vec::new();
    let mut states = Vec::new();
    let mut entities = Vec::new();
    for a in atoms.atoms {
        match a {
            AtomEnvelope::Claim(c) => claims.push(c),
            AtomEnvelope::State(s) => states.push(s),
            AtomEnvelope::Entity(e) => entities.push(e),
            _ => {}
        }
    }

    println!(
        "  loaded {} claim atom(s) + {} state atom(s) + {} entity atom(s) from atlas",
        claims.len(),
        states.len(),
        entities.len(),
    );

    // What this corpus DECLARED. Absent for every corpus built before
    // ontology v1 and for every corpus that declares nothing — read as
    // "declares nothing", never as an error (`read_atlas_ontology`).
    let policies: OntologyPolicies = read_atlas_ontology(&atlas_dir)
        .map(|f| f.policies)
        .unwrap_or_default();

    // `tension.between` is an allow-list over claim TYPES and it runs
    // BEFORE selection: a claim that cannot be one half of a tension
    // should not cost an embedding, and should not be able to become
    // somebody's nearest neighbour.
    let between = &policies.derivation.tension.between;
    let between_outcome = restrict_claims_to_types(&mut claims, between);
    match between_outcome {
        BetweenOutcome::NotDeclared => {}
        BetweenOutcome::Inert => println!(
            "  ⚠ tension.between = [{}] is INERT: no claim atom carries a `claim_kind`, so the \
             allow-list has nothing to select on and was NOT applied (applying it would empty \
             the pool). Every claim stays in scope. This is the declared type never reaching \
             the atom — fix it upstream, in extraction and resolution.",
            between.join(", "),
        ),
        BetweenOutcome::Applied { dropped, kept } => println!(
            "  · tension.between = [{}]: {kept} claim(s) in scope ({dropped} outside the \
             declared types)",
            between.join(", "),
        ),
    }

    // Candidate selection is strategy-driven (glassbox: the chosen
    // strategy is logged). The literary/philosophy atlas uses the
    // deterministic graph signals; custom-ontology corpora use an
    // embedding top-K net (see `TensionStrategy`). Resolve the pipeline
    // to read its strategy; fall back to the graph default if the
    // pipeline can't be resolved (preserves legacy behaviour).
    // An unresolvable pipeline still gets the Graph strategy — that is
    // the legacy behaviour and it is correct — but the SUBSTITUTION is
    // now recorded rather than erased by `unwrap_or_default()`.
    //
    // The strategy is DERIVED, not read off a constant: the pipeline is
    // handed the corpus's measured shape and answers with the selector it
    // wants (`Pipeline::derive_tension_strategy`, whose default ignores the
    // shape and returns `tension_strategy()` — so nothing that has not
    // opted in moves). The derivation is printed because a derived choice
    // nobody can see is not glassbox (ARCH §9.1).
    let shape = CorpusShape::of(&claims);
    let (strategy, strategy_defaulted) = match super::pipeline_resolve::resolve_pipeline(&cfg) {
        Some(p) => (p.derive_tension_strategy(&shape), false),
        None => (TensionStrategy::Graph, true),
    };
    if strategy_defaulted {
        tracing::warn!(
            pipeline_id = %cfg.pipeline_id,
            "tension candidates: pipeline did not resolve; substituting the Graph strategy"
        );
    }
    println!("  · corpus shape: {}", shape.describe());

    let mut candidates = match strategy {
        TensionStrategy::Graph => {
            println!("  · candidate strategy: graph (cluster + entity-overlap + co-occurrence)");
            select_candidates(CandidateSelectionInput {
                claims: &claims,
                states: &states,
                // Intra-cluster candidates not wired here — see module
                // comment in `tensions.rs`.
                claim_clusters: &[],
                entities: &entities,
            })
        }
        TensionStrategy::EmbeddingTopK { k, floor } => {
            println!("  · candidate strategy: embedding top-K (k={k}, floor={floor})");
            // Embed each rule's text via the daemon embed slot, then pair
            // by cosine. The graph path needs no model; this path does —
            // but a custom-ontology build already has the daemon up for
            // the classifier that follows.
            let client = DaemonInferenceClient::from_enrich_config(&cfg)
                .map_err(|e| format!("building inference client for embeddings: {e}"))?;
            let (embed, _chat) = client.into_closures();
            let mut embeddings = Vec::with_capacity(claims.len());
            for (i, c) in claims.iter().enumerate() {
                match embed(&c.content).await {
                    Ok(v) => embeddings.push(v),
                    Err(e) => return Err(format!("embedding claim {i} ({}): {e}", c.id.as_str())),
                }
            }
            println!(
                "  · embedded {} claim(s) for similarity selection",
                embeddings.len()
            );
            select_embedding_topk(&claims, &embeddings, k, floor)
        }
    };

    // Strategy-agnostic de-noise: drop pairs where both claims are by the
    // same named speaker (Person/Institution). Critical for the embedding
    // top-K net, which otherwise pairs a speaker's many mutually-consistent
    // claims by topic similarity. (No-op for Concept/topic attributions —
    // see `drop_same_named_speaker_pairs`.)
    let before = candidates.len();
    drop_same_named_speaker_pairs(&mut candidates, &claims, &entities);
    let same_speaker_dropped = before - candidates.len();
    if candidates.len() < before {
        println!(
            "  · same-speaker filter: {} -> {} candidates (dropped {} same-speaker pair(s))",
            before,
            candidates.len(),
            before - candidates.len()
        );
    }

    // The author's own comparability criterion (`tension.same`), applied
    // last so the operator sees what each filter cost separately. A no-op
    // for every corpus that declares nothing.
    let comparability = drop_non_comparable_pairs(&mut candidates, &claims, &entities, &policies);
    if let Some(line) = comparability.summary() {
        println!("  · {line}");
    }
    // A declared criterion no claim carries is doing nothing, silently.
    // Say so: it is the difference between "the filter agreed with the
    // author" and "the extractor never filled the field" (ARCH §18.1).
    let inert = comparability.inert_fields();
    if !inert.is_empty() {
        println!(
            "  ⚠ `same` field(s) [{}] are absent from every claim in scope — \
             they ruled nothing out. Comparability rests on the remaining field(s).",
            inert.join(", "),
        );
        tracing::warn!(
            target: "atlas.tensions",
            inert = ?inert,
            "declared `same` fields carried by no claim"
        );
    }

    if candidates.is_empty() {
        println!(
            "  · no entity-overlap candidates found. Either no claim has an `attributed_to` \
             set, or every attribution is singular (no pair possible)."
        );
    }

    let out = TensionCandidatesOutput::new(candidates);
    let count = out.candidates.len();
    let written_to = write_tension_candidates(&atlas_dir, &out)
        .map_err(|e| format!("writing tension_candidates.json: {e}"))?;

    Ok(TensionCandidatesReport {
        claims: claims.len(),
        states: states.len(),
        entities: entities.len(),
        strategy,
        strategy_defaulted,
        same_speaker_dropped,
        between: between_outcome,
        comparability,
        shape,
        candidates: count,
        written_to,
    })
}

/// Print the closing lines the way `svrn enrich atlas-tensions` always has.
pub fn render(report: &TensionCandidatesReport) {
    println!("  ✓ {} candidate pair(s)", report.candidates);
    println!("  ✓ wrote {}", report.written_to.display());
}

fn atlas_dir_for(corpus_id: &str) -> PathBuf {
    paths::index_root(corpus_id).join(ATLAS_DIRNAME)
}

/// A parsed `atlas-tensions` invocation. Public so the `enrich build`
/// orchestrator constructs one directly instead of round-tripping
/// through argv.
#[derive(Debug, Clone)]
pub struct ParsedTensions {
    pub corpus_id: String,
}

pub fn parse_args(args: &[String]) -> Result<ParsedTensions, String> {
    let mut corpus_id: Option<String> = None;
    for arg in args {
        match arg.as_str() {
            other if other.starts_with("--") => {
                return Err(format!("unknown flag: {other}"));
            }
            other => {
                if corpus_id.is_none() {
                    corpus_id = Some(other.to_string());
                } else {
                    return Err(format!("unexpected positional argument: {other}"));
                }
            }
        }
    }
    let corpus_id = corpus_id.ok_or_else(|| "missing <corpus-id>".to_string())?;
    Ok(ParsedTensions { corpus_id })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_args_accepts_bare_corpus_id() {
        let p = parse_args(&["brothers_karamazov".into()]).unwrap();
        assert_eq!(p.corpus_id, "brothers_karamazov");
    }

    #[test]
    fn parse_args_rejects_missing_corpus_id() {
        let err = parse_args(&[]).unwrap_err();
        assert!(err.contains("corpus-id"));
    }

    #[test]
    fn parse_args_rejects_unknown_flag() {
        let err = parse_args(&["bk".into(), "--nope".into()]).unwrap_err();
        assert!(err.contains("unknown flag"));
    }
}
