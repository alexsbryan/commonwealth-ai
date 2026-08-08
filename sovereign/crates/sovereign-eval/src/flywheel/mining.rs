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
    mine_claims_bounded(corpus, preview_fallback, usize::MAX)
}

/// [`mine_claims`], stopping after `limit` usable claims.
///
/// **Why this streams instead of `read_to_string` + `from_str`:** the atlas of
/// a large corpus is not small. `~/.sovereign/indexes/wikipedia/atlas/atoms.json`
/// is 800 MB on this box (measured 2026-08-07); parsing it into a
/// `serde_json::Value` costs several GB of resident memory, and the daemon it
/// would sit beside has a recorded 64 GB SIGTERM incident (note `b57b0cd5`).
/// So the array is walked one atom at a time: only the current atom's bytes and
/// the kept claims are ever resident, whatever the file's size.
///
/// The scan is a byte-level bracket matcher rather than a second JSON parser —
/// it finds the `"atoms": [` array, then hands each depth-1 object to
/// `serde_json` individually. String state and escapes are tracked so a `{` or
/// `]` inside a passage preview cannot desynchronize it.
pub fn mine_claims_bounded(corpus: &Path, preview_fallback: bool, limit: usize) -> Vec<MinedClaim> {
    use std::io::Read;

    let path = corpus.join("atlas").join("atoms.json");
    let Ok(file) = std::fs::File::open(&path) else {
        return Vec::new();
    };
    let mut reader = std::io::BufReader::with_capacity(1 << 20, file);

    let mut out = Vec::new();
    if limit == 0 {
        return out;
    }

    // Phase 1 — position the cursor just inside the `"atoms"` array. Matching
    // the key AND the `:` `[` that must follow it keeps a stray "atoms" inside
    // some passage's prose from steering the scan.
    const KEY: &[u8] = b"\"atoms\"";
    let mut matched = 0usize;
    let mut state = Phase1::Key;
    let mut byte = [0u8; 1];
    loop {
        match reader.read(&mut byte) {
            Ok(0) | Err(_) => return out, // no atoms array → no claims
            Ok(_) => {}
        }
        let b = byte[0];
        match state {
            Phase1::Key => {
                if b == KEY[matched] {
                    matched += 1;
                    if matched == KEY.len() {
                        state = Phase1::Colon;
                    }
                } else {
                    // Restart, allowing the mismatched byte to open a new match.
                    matched = usize::from(b == KEY[0]);
                }
            }
            Phase1::Colon => {
                if b.is_ascii_whitespace() {
                } else if b == b':' {
                    state = Phase1::Open;
                } else {
                    state = Phase1::Key;
                    matched = usize::from(b == KEY[0]);
                }
            }
            Phase1::Open => {
                if b.is_ascii_whitespace() {
                } else if b == b'[' {
                    break;
                } else {
                    state = Phase1::Key;
                    matched = usize::from(b == KEY[0]);
                }
            }
        }
    }

    // Phase 2 — one depth-1 object at a time.
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    let mut buf: Vec<u8> = Vec::with_capacity(4096);
    loop {
        match reader.read(&mut byte) {
            Ok(0) | Err(_) => return out, // truncated file: keep what parsed
            Ok(_) => {}
        }
        let b = byte[0];
        if depth > 0 {
            buf.push(b);
        }
        if in_string {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'{' => {
                if depth == 0 {
                    buf.clear();
                    buf.push(b);
                }
                depth += 1;
            }
            b'}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    if let Ok(atom) = serde_json::from_slice::<serde_json::Value>(&buf) {
                        if let Some(c) = claim_from_atom(&atom, preview_fallback) {
                            out.push(c);
                            if out.len() >= limit {
                                return out;
                            }
                        }
                    }
                    buf.clear();
                }
            }
            b']' if depth == 0 => return out, // end of the atoms array
            _ => {}
        }
    }
}

enum Phase1 {
    Key,
    Colon,
    Open,
}

/// The per-atom extraction rule — the single place that decides what a usable
/// mined claim is. Both the whole-file and bounded walks go through it, so
/// there is one definition of "Claim atom with genuine, non-cheatable
/// evidence".
fn claim_from_atom(a: &serde_json::Value, preview_fallback: bool) -> Option<MinedClaim> {
    if a.get("atom_type").and_then(|v| v.as_str()) != Some("Claim") {
        return None;
    }
    let d = a.get("data")?;
    let has_evidence = d
        .get("evidence")
        .and_then(|v| v.as_array())
        .map(|e| !e.is_empty())
        .unwrap_or(false);
    if !has_evidence {
        return None;
    }
    let id = d
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
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
        return None;
    }
    if cheatable(&content, &excerpt) {
        return None;
    }
    Some(MinedClaim {
        id,
        content,
        excerpt,
    })
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
        f.write_all(serde_json::to_string_pretty(&atoms).unwrap().as_bytes())
            .unwrap();
        root
    }

    #[test]
    fn mines_genuine_claims_excludes_cheatable_and_nonclaims() {
        let corpus = fixture_corpus();
        let claims = mine_claims(&corpus, false);
        let ids: Vec<&str> = claims.iter().map(|c| c.id.as_str()).collect();
        assert!(ids.contains(&"claim-aaaa"));
        assert!(
            !ids.contains(&"claim-cheat"),
            "self-verifiable claim must be excluded"
        );
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
        assert!(
            mine_claims(&root, false).is_empty(),
            "strict skips a claim with no quotable_excerpt"
        );
        // Fallback: the passage_preview becomes the excerpt.
        let mined = mine_claims(&root, true);
        assert_eq!(mined.len(), 1);
        assert!(mined[0].excerpt.contains("fast 9B"));
    }

    /// The bracket matcher must survive JSON punctuation inside string values —
    /// a `{`, `]` or escaped quote in a passage preview is ordinary in a
    /// literary corpus, and a desynchronized scan would silently drop every
    /// claim after it.
    #[test]
    fn streaming_scan_is_not_fooled_by_braces_inside_strings() {
        let root = std::env::temp_dir().join("flywheel_mining_bracket_fixture");
        let atlas = root.join("atlas");
        std::fs::create_dir_all(&atlas).unwrap();
        let atoms = serde_json::json!({
            "schema_version": "2.3",
            "atoms": [
                {"atom_type": "Entity", "data": {"id": "e1", "note": "a } and a ] and an \"atoms\": [ decoy"}},
                {"atom_type": "Claim", "data": {
                    "id": "claim-braces",
                    "content": "The scanner tracks string state across escaped quotes and brackets.",
                    "evidence": [{"chunk_id": "c1", "passage_preview": "he said \"} ] {\" and walked on, unbothered"}]
                }},
                {"atom_type": "Claim", "data": {
                    "id": "claim-after",
                    "content": "A claim appearing after the punctuation-heavy one is still mined.",
                    "evidence": [{"chunk_id": "c2", "passage_preview": "the second evidence passage, plainly worded"}]
                }}
            ]
        });
        std::fs::File::create(atlas.join("atoms.json"))
            .unwrap()
            .write_all(serde_json::to_string_pretty(&atoms).unwrap().as_bytes())
            .unwrap();
        let ids: Vec<String> = mine_claims(&root, true).into_iter().map(|c| c.id).collect();
        assert_eq!(ids, vec!["claim-braces", "claim-after"]);
    }

    /// The bound is what makes an 800 MB atlas minable at all — it must stop
    /// early, in order, not truncate a full parse after the fact.
    #[test]
    fn bounded_mining_stops_at_the_limit_in_document_order() {
        let root = std::env::temp_dir().join("flywheel_mining_bounded_fixture");
        let atlas = root.join("atlas");
        std::fs::create_dir_all(&atlas).unwrap();
        let atoms = serde_json::json!({
            "atoms": (1..=5).map(|i| serde_json::json!({
                "atom_type": "Claim",
                "data": {
                    "id": format!("claim-{i:02}"),
                    "content": format!("Claim number {i} states something substantive about the corpus."),
                    "evidence": [{"chunk_id": format!("c{i}"), "passage_preview": format!("evidence passage number {i}, worded independently")}]
                }
            })).collect::<Vec<_>>()
        });
        std::fs::File::create(atlas.join("atoms.json"))
            .unwrap()
            .write_all(serde_json::to_string(&atoms).unwrap().as_bytes())
            .unwrap();
        assert_eq!(mine_claims(&root, true).len(), 5);
        let two = mine_claims_bounded(&root, true, 2);
        assert_eq!(
            two.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
            vec!["claim-01", "claim-02"]
        );
        assert!(mine_claims_bounded(&root, true, 0).is_empty());
    }
}
