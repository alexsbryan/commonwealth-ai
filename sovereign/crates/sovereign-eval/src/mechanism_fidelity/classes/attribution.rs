// SPDX-License-Identifier: AGPL-3.0-or-later
//! Attribution-support: a **corpus-grounded** reasoning class. Where
//! wealth-tax is synthetic (the structural prior IS the oracle), here the
//! oracle is *exact and known by construction* — we mine real `Claim`
//! atoms that carry their own supporting evidence, so "is this claim
//! supported by this passage?" has a ground-truth answer we control.
//!
//! The metamorphic structure reuses the same four variants + two render
//! modes as the reference class, so the orchestrator, scorer, and verdict
//! read it without any special-casing:
//!
//!   * **base** — claim + its genuine supporting excerpt → supported
//!     (`structural_p = 1.0`).
//!   * **dir_p1 (DIR, −1)** — the excerpt is *negated*; it now contradicts
//!     the claim → unsupported (`0.0`). A faithful reader collapses; a
//!     model keying on topical overlap ("the passage mentions the same
//!     thing") stays put. The magnitude/direction bands apply.
//!   * **dir_p2 (DIR, 0)** — an irrelevant distractor sentence is appended
//!     to the genuine excerpt; support is unchanged (`1.0`). Catches a
//!     model that lowers confidence merely because the passage got longer
//!     or noisier. Flat band.
//!   * **inv_i1 (INV, 0)** — the excerpt is cosmetically reframed (same
//!     content, different framing); support must not move (`1.0`).
//!
//! **Negative control (blindfold).** The stripped render *withholds the
//! passage* entirely. Because dir_p1 only changes the (now-hidden) passage
//! while the claim is held fixed, the stripped base and stripped dir_p1
//! prompts are byte-identical — the control literally cannot see what
//! changed, so any directional movement it shows is a leak. The
//! `control_cannot_cheat` test additionally guarantees the claim text
//! never contains the supporting excerpt verbatim, so a blindfolded model
//! can't self-verify from the claim alone.

use std::path::Path;

use rand::rngs::StdRng;
use rand::seq::SliceRandom;
use rand::SeedableRng;

use crate::flywheel::mining::mine_claims;
use crate::mechanism_fidelity::class::{prob_of, ReasoningClass, RenderedProbe};
use crate::mechanism_fidelity::perturb::PerturbKind;

#[derive(Debug, Default)]
pub struct AttributionSupport;

impl AttributionSupport {
    /// Negate the supporting excerpt so it contradicts the claim. A
    /// deterministic, model-free transformation: the framing flips the
    /// passage's polarity without an LLM in the pure-logic layer.
    fn negate(s: &str) -> String {
        format!("It is categorically false that: {s}")
    }

    /// Cosmetic reframe — same content, different presentation. The
    /// invariance probe: a support decision that flips on this was never
    /// reading the passage.
    fn reframe(s: &str) -> String {
        format!("As the source itself puts it — {s}")
    }

    /// Append an irrelevant (true, on-topic-neutral) distractor. Support
    /// is unchanged; a model that conflates passage length/noise with
    /// weaker support fails the flat band.
    fn with_distractor(s: &str) -> String {
        format!("{s} (This document was retained in the archive for record-keeping.)")
    }

    /// Render an attribution prompt. `passage = None` is the blindfold
    /// control — the supporting passage is withheld, so the model must
    /// answer from the claim alone. The letter legend is anchored here so
    /// the class owns its own choice set.
    fn render(claim: &str, passage: Option<&str>) -> String {
        let body = match passage {
            Some(p) => format!("Passage:\n{p}\n\nClaim:\n{claim}\n\nDoes the passage SUPPORT the claim?"),
            None => format!(
                "Claim:\n{claim}\n\n(The supporting passage has been withheld.)\n\nDoes the (withheld) passage SUPPORT the claim?"
            ),
        };
        format!("{body}\n\nAnswer with exactly one letter — A = supported, B = unsupported.")
    }
}

impl ReasoningClass for AttributionSupport {
    fn id(&self) -> &'static str {
        "attribution_support"
    }

    fn system_prompt(&self) -> &'static str {
        "You are a careful fact-checker. Decide whether the passage supports the claim, and answer with a single letter."
    }

    fn candidates(&self) -> Vec<String> {
        vec!["A".into(), "B".into()]
    }

    fn target_prob(&self, dist: &[(String, f64)]) -> f64 {
        // A = supported, B = unsupported.
        prob_of(dist, "A")
    }

    fn build_probes(&self, n: usize, seed: u64, corpus: Option<&Path>) -> Vec<RenderedProbe> {
        // Attribution is corpus-grounded; without a corpus there is nothing
        // to mine (the orchestrator turns an empty matrix into a clear error).
        let Some(corpus) = corpus else {
            return Vec::new();
        };
        // Strict: attribution needs a substantial standalone excerpt for its
        // negate/reframe transforms, so it does NOT fall back to passage_preview.
        let mut claims = mine_claims(corpus, false);
        if claims.is_empty() {
            return Vec::new();
        }
        // Deterministic order, then a seeded shuffle, so a (seed, n) battery
        // is reproducible bit-for-bit but varies with the seed.
        claims.sort_by(|a, b| a.id.cmp(&b.id));
        let mut rng = StdRng::seed_from_u64(seed);
        claims.shuffle(&mut rng);
        claims.truncate(n.min(claims.len()));

        let mut out = Vec::new();
        for c in &claims {
            // The four full-render passages and their exact oracle probs.
            let support = c.excerpt.clone();
            let negated = Self::negate(&support);
            let distracted = Self::with_distractor(&support);
            let reframed = Self::reframe(&support);

            let full = [
                ("base", PerturbKind::Ref, 0i8, support.as_str(), 1.0),
                ("dir_p1", PerturbKind::Dir, -1, negated.as_str(), 0.0),
                ("dir_p2", PerturbKind::Dir, 0, distracted.as_str(), 1.0),
                ("inv_i1", PerturbKind::Inv, 0, reframed.as_str(), 1.0),
            ];
            for (variant, kind, sign, passage, sp) in full {
                out.push(RenderedProbe {
                    case_id: c.id.clone(),
                    variant: variant.to_string(),
                    render: "full".to_string(),
                    paraphrase: false,
                    kind,
                    expected_sign: sign,
                    prompt: Self::render(&c.content, Some(passage)),
                    structural_p: sp,
                });
            }

            // Blindfold control — the passage is withheld, so base / dir_p1
            // / dir_p2 stripped prompts are byte-identical (the control is
            // provably blind). structural_p still carries the would-be
            // oracle so the control's d_struct matches the full render's;
            // a faithful-but-blind model's d_agent is ~0, which is the leak
            // detector.
            let control = [
                ("base", PerturbKind::Ref, 0i8, 1.0),
                ("dir_p1", PerturbKind::Dir, -1, 0.0),
                ("dir_p2", PerturbKind::Dir, 0, 1.0),
            ];
            for (variant, kind, sign, sp) in control {
                out.push(RenderedProbe {
                    case_id: c.id.clone(),
                    variant: variant.to_string(),
                    render: "stripped".to_string(),
                    paraphrase: false,
                    kind,
                    expected_sign: sign,
                    prompt: Self::render(&c.content, None),
                    structural_p: sp,
                });
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Write a tiny atoms.json fixture and return its corpus root. Each call
    /// gets a UNIQUE dir: `File::create` truncates, so sharing one path races a
    /// concurrent reader to an empty parse (an empty claim set → `probes[0]`
    /// panics). Unique dirs remove the shared mutable state entirely.
    fn fixture_corpus() -> std::path::PathBuf {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("mf_attribution_unit_fixture_{n}"));
        let atlas = root.join("atlas");
        std::fs::create_dir_all(&atlas).unwrap();
        let atoms = serde_json::json!({
            "schema_version": 1,
            "atoms": [
                {"atom_type": "Claim", "data": {
                    "id": "claim-aaaa",
                    "content": "The ingest pipeline keys downstream behavior on the recipe's chunker, not the corpus id.",
                    "evidence": [{"chunk_id": "sec_1", "passage_preview": "pipeline is source-agnostic"}],
                    "quotable_excerpt": "downstream keys on the threaded_turns chunker and conversational domain"
                }},
                {"atom_type": "Claim", "data": {
                    "id": "claim-bbbb",
                    "content": "Forced-choice elicitation reads the masked next-token distribution in one pass.",
                    "evidence": [{"chunk_id": "sec_2", "passage_preview": "one forward pass"}],
                    "quotable_excerpt": "the daemon reads the candidate leading-token logits and softmaxes them"
                }},
                {"atom_type": "Claim", "data": {
                    "id": "claim-cheat",
                    "content": "the sky is blue today",
                    "evidence": [{"chunk_id": "sec_3", "passage_preview": "x"}],
                    "quotable_excerpt": "the sky is blue today"
                }},
                {"atom_type": "Section", "data": {"id": "sec-zzzz", "title": "ignored non-claim"}}
            ]
        });
        let mut f = std::fs::File::create(atlas.join("atoms.json")).unwrap();
        f.write_all(serde_json::to_string_pretty(&atoms).unwrap().as_bytes())
            .unwrap();
        root
    }

    #[test]
    fn mines_claims_and_excludes_cheatable_and_nonclaims() {
        let corpus = fixture_corpus();
        let claims = mine_claims(&corpus, false);
        let ids: Vec<&str> = claims.iter().map(|c| c.id.as_str()).collect();
        // The two genuine claims survive; the cheatable one (excerpt ==
        // content) and the non-Claim atom are excluded.
        assert!(ids.contains(&"claim-aaaa"));
        assert!(ids.contains(&"claim-bbbb"));
        assert!(!ids.contains(&"claim-cheat"), "self-verifiable claim must be excluded");
        assert_eq!(claims.len(), 2);
    }

    #[test]
    fn probe_shape_and_oracle() {
        let corpus = fixture_corpus();
        let cls = AttributionSupport;
        let probes = cls.build_probes(10, 0, Some(corpus.as_path()));
        // 2 mined claims × (full×4 + control×3) = 14 probes.
        assert_eq!(probes.len(), 14);

        // Exact oracle: base supported (1.0), dir_p1 collapsed (0.0).
        let base = probes
            .iter()
            .find(|p| p.is_base() && p.render == "full")
            .unwrap();
        assert_eq!(base.structural_p, 1.0);
        let p1 = probes
            .iter()
            .find(|p| p.variant == "dir_p1" && p.render == "full")
            .unwrap();
        assert_eq!(p1.structural_p, 0.0);
        assert_eq!(p1.expected_sign, -1);

        // Letter legend anchored; full render shows the passage.
        assert!(base.prompt.contains("A = supported"));
        assert!(base.prompt.contains("Passage:"));
    }

    #[test]
    fn control_cannot_cheat() {
        let corpus = fixture_corpus();
        let cls = AttributionSupport;
        let probes = cls.build_probes(10, 0, Some(corpus.as_path()));

        // Within a case, stripped base and stripped dir_p1 prompts are
        // byte-identical — the blindfold control cannot see the negation.
        let case = &probes[0].case_id;
        let sbase = probes
            .iter()
            .find(|p| &p.case_id == case && p.is_base() && p.is_control())
            .unwrap();
        let sp1 = probes
            .iter()
            .find(|p| &p.case_id == case && p.variant == "dir_p1" && p.is_control())
            .unwrap();
        assert_eq!(
            sbase.prompt, sp1.prompt,
            "control must be blind to the dir_p1 negation"
        );
        // The control prompt withholds the passage entirely.
        assert!(sbase.prompt.contains("withheld"));
        assert!(!sbase.prompt.contains("Passage:"));
    }

    #[test]
    fn no_corpus_yields_no_probes() {
        let cls = AttributionSupport;
        assert!(cls.build_probes(10, 0, None).is_empty());
    }
}
