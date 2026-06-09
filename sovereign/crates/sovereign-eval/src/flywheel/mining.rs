// SPDX-License-Identifier: AGPL-3.0-or-later
//! Shared corpus claim-miner: extract `Claim` atoms with genuine supporting
//! evidence from a corpus's `atlas/atoms.json`.
//!
//! Lifted out of `mechanism_fidelity::classes::attribution` so the attribution
//! reasoning class and the flywheel's I1 corpus generator mine claims through
//! one implementation. Robust to mixed atom shapes (parses `data` lazily) and
//! returns an empty vec on any I/O / shape problem so callers report "no
//! probes" rather than panicking.

use std::path::Path;

/// One mined claim with a genuine supporting excerpt.
#[derive(Debug, Clone)]
pub struct MinedClaim {
    pub id: String,
    pub content: String,
    pub excerpt: String,
}

/// True when the claim text already contains the excerpt (or vice versa) — such
/// a claim would let a blindfolded attribution control self-verify, so it is
/// excluded from the battery. The flywheel inherits the same exclusion so its
/// Present probes mine the identical claim set.
pub fn cheatable(content: &str, excerpt: &str) -> bool {
    let c = content.to_lowercase();
    let e = excerpt.to_lowercase();
    // Substring either way, or a long shared run (the excerpt's first ~40 chars
    // appearing in the claim).
    if c.contains(&e) || e.contains(&c) {
        return true;
    }
    let head: String = e.chars().take(40).collect();
    head.len() >= 24 && c.contains(&head)
}

/// Load mined `Claim` atoms with genuine evidence from a corpus's
/// `atlas/atoms.json`.
///
/// `preview_fallback`: most corpora don't populate `quotable_excerpt` — the
/// supporting text lives in the first evidence entry's `passage_preview`. The
/// flywheel's I1 generator opts IN (broad corpus coverage); the attribution
/// reasoning class opts OUT, because its metamorphic negate/reframe/distractor
/// transforms need a substantial standalone excerpt, not a short source
/// fragment (a live scan found only ~2 of 13 enriched corpora carry a real
/// `quotable_excerpt`).
pub fn mine_claims(corpus: &Path, preview_fallback: bool) -> Vec<MinedClaim> {
    let path = corpus.join("atlas").join("atoms.json");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    let Ok(root) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Vec::new();
    };
    let Some(atoms) = root.get("atoms").and_then(|v| v.as_array()) else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for a in atoms {
        if a.get("atom_type").and_then(|v| v.as_str()) != Some("Claim") {
            continue;
        }
        let Some(d) = a.get("data") else { continue };
        let has_evidence = d
            .get("evidence")
            .and_then(|v| v.as_array())
            .map(|e| !e.is_empty())
            .unwrap_or(false);
        if !has_evidence {
            continue;
        }
        let id = d.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let content = d
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        let mut excerpt = d
            .get("quotable_excerpt")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        // Fall back to the first evidence entry's passage_preview when there's
        // no quotable_excerpt and the caller opted in. `.get` on a non-object
        // value returns None, so a malformed evidence entry is skipped safely.
        if excerpt.len() < 12 && preview_fallback {
            excerpt = d
                .get("evidence")
                .and_then(|v| v.as_array())
                .and_then(|a| a.first())
                .and_then(|e| e.get("passage_preview"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
        }
        // A usable probe needs an id, a substantive claim, and a substantive
        // excerpt that the claim doesn't already contain.
        if id.is_empty() || content.len() < 12 || excerpt.len() < 12 {
            continue;
        }
        if cheatable(&content, &excerpt) {
            continue;
        }
        out.push(MinedClaim { id, content, excerpt });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn fixture_corpus() -> std::path::PathBuf {
        let root = std::env::temp_dir().join("flywheel_mining_unit_fixture");
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
                    "id": "claim-cheat",
                    "content": "the sky is blue today",
                    "evidence": [{"chunk_id": "sec_3", "passage_preview": "x"}],
                    "quotable_excerpt": "the sky is blue today"
                }},
                {"atom_type": "Section", "data": {"id": "sec-zzzz", "title": "ignored non-claim"}}
            ]
        });
        let mut f = std::fs::File::create(atlas.join("atoms.json")).unwrap();
        f.write_all(serde_json::to_string_pretty(&atoms).unwrap().as_bytes()).unwrap();
        root
    }

    #[test]
    fn mines_genuine_claims_excludes_cheatable_and_nonclaims() {
        let corpus = fixture_corpus();
        let claims = mine_claims(&corpus, false);
        let ids: Vec<&str> = claims.iter().map(|c| c.id.as_str()).collect();
        assert!(ids.contains(&"claim-aaaa"));
        assert!(!ids.contains(&"claim-cheat"), "self-verifiable claim must be excluded");
        assert_eq!(claims.len(), 1);
    }

    #[test]
    fn missing_corpus_yields_empty() {
        assert!(mine_claims(std::path::Path::new("/no/such/corpus"), false).is_empty());
    }

    #[test]
    fn preview_fallback_mines_claims_without_quotable_excerpt() {
        // A Claim with NO quotable_excerpt but a substantive evidence
        // passage_preview — the common shape across enriched corpora.
        let root = std::env::temp_dir().join("flywheel_mining_preview_fixture");
        let atlas = root.join("atlas");
        std::fs::create_dir_all(&atlas).unwrap();
        let atoms = serde_json::json!({
            "atoms": [{"atom_type": "Claim", "data": {
                "id": "claim-prev",
                "content": "The daemon pins the fast slot and the embed model at startup.",
                "evidence": [{"chunk_id": "c1", "passage_preview": "the daemon eagerly loads the fast 9B and 0.6B embed at boot"}]
                // no quotable_excerpt
            }}]
        });
        std::fs::File::create(atlas.join("atoms.json"))
            .unwrap()
            .write_all(serde_json::to_string(&atoms).unwrap().as_bytes())
            .unwrap();
        // Strict: nothing to mine (no quotable_excerpt).
        assert!(mine_claims(&root, false).is_empty(), "strict skips a claim with no quotable_excerpt");
        // Fallback: the passage_preview becomes the excerpt.
        let mined = mine_claims(&root, true);
        assert_eq!(mined.len(), 1);
        assert!(mined[0].excerpt.contains("fast 9B"));
    }
}
