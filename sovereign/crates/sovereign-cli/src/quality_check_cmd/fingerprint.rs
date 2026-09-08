// SPDX-License-Identifier: AGPL-3.0-or-later
//! The stack fingerprint: what a number from this run may be compared against.
//!
//! A latency in milliseconds means nothing without the model that produced it
//! and the bank that asked for it. `LaneBaseline::diff` already refuses to
//! compare across model stems (INCOMPARABLE); this is the same rule for the
//! whole run, computed ONCE and printed FIRST so a reader never has to wonder
//! which stack a table describes.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use kernel_types::quality::Instrument;

/// What a number from this run may be compared against.
///
/// A latency in milliseconds means nothing without the model that produced
/// it and the bank that asked for it. `LaneBaseline::diff` already refuses
/// to compare across model stems (INCOMPARABLE); this is the same rule for
/// the whole run, computed ONCE and printed FIRST so a reader never has to
/// wonder which stack a table describes.
#[derive(Debug, Clone)]
pub(super) struct Fingerprint {
    pub(super) hex: String,
    pub(super) primary: String,
    pub(super) fast: String,
    pub(super) embed: String,
    pub(super) smoke_subsets: Vec<String>,
    pub(super) banks: BTreeMap<String, String>,
}

impl Fingerprint {
    pub(super) fn render(&self) -> String {
        let mut s = format!("stack fingerprint: {}\n", self.hex);
        s.push_str(&format!("  primary  {}\n", self.primary));
        s.push_str(&format!("  fast     {}\n", self.fast));
        s.push_str(&format!("  embed    {}\n", self.embed));
        s.push_str(&format!(
            "  smoke    {}\n",
            if self.smoke_subsets.is_empty() {
                "none declared".to_string()
            } else {
                self.smoke_subsets.join(", ")
            }
        ));
        for (lane, hash) in &self.banks {
            s.push_str(&format!("  bank     {lane}={hash}\n"));
        }
        s
    }
}

/// Short content hash. `sha2` is already a dependency of this crate; the
/// digest is truncated to 12 hex chars because it names a directory a human
/// reads, not a security boundary.
pub(super) fn short_hash(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    hex::encode(h.finalize())[..12].to_string()
}

/// The two slot aliases every lane's numbers hang from. `/v1/models` lists
/// each alias with `owned_by: "alias→<stem>"`; the stem is what a baseline
/// is keyed on, because two hosts running "primary" are not running the
/// same model.
async fn resolve_slot_stems(base: &str) -> (String, String) {
    let unknown = || "unresolved".to_string();
    let Ok(resp) = reqwest::Client::new()
        .get(format!("{base}/v1/models"))
        .timeout(Duration::from_secs(10))
        .send()
        .await
    else {
        return (unknown(), unknown());
    };
    let Ok(body) = resp.json::<serde_json::Value>().await else {
        return (unknown(), unknown());
    };
    // `unresolved` is a NAMED absence, and it is load-bearing: it goes into
    // the fingerprint, so a run against an unreachable daemon computes a
    // DIFFERENT fingerprint than a resolved one and cannot silently be
    // compared against a real baseline. It also prints, on the first line.
    let stem_of = |alias: &str| -> String {
        body.get("data")
            .and_then(|d| d.as_array())
            .and_then(|rows| {
                rows.iter()
                    .find(|r| r.get("id").and_then(|v| v.as_str()) == Some(alias))
            })
            .and_then(|r| r.get("owned_by").and_then(|v| v.as_str()))
            // `alias→<stem>`; a non-alias row owns itself.
            .map(|o| o.split('→').next_back().unwrap_or(o).to_string())
            .unwrap_or_else(unknown)
    };
    (stem_of("primary"), stem_of("fast"))
}

/// Subset ids declared in `sovereign/bench/smoke.toml`, sorted.
///
/// In the fingerprint because a baseline captured against a 6-probe subset
/// is not comparable to one captured against 12 — the ci-bench README
/// records the same trap for its cap-specific baselines, where a moved cap
/// false-fired every lane.
///
/// ABSENT and MALFORMED are two answers, not one. A `smoke.toml` that fails
/// to parse would otherwise read as "none declared" — the same fingerprint a
/// host with no subsets at all computes — and every lane would then compare
/// against a baseline captured under subsets it is no longer running
/// (ARCH §18.3). Absent is `Ok(vec![])`; malformed is an `Err` that refuses
/// the run.
pub(super) fn smoke_subset_ids(repo: &Path) -> Result<Vec<String>, String> {
    let path = repo.join("sovereign/bench/smoke.toml");
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let doc: toml::Value = toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut ids: Vec<String> = doc
        .get("subset")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|s| s.get("subset_id").and_then(|v| v.as_str()))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    // Sorted and DEDUPED: one subset spans several banks, so `retrieval-prod-v1`
    // appears once per bank row. The fingerprint wants the SET of subsets this
    // stack runs, and a bank count leaking into it would move the fingerprint
    // every time a subset gained a bank without changing which items run.
    ids.sort();
    ids.dedup();
    Ok(ids)
}

/// A digest of what `smoke.toml`'s rows actually SELECT — every
/// (subset_id, bank, mode/ids) triple, canonicalised and sorted.
///
/// The subset IDs alone are not enough, and the gap is the exact trap the
/// ci-bench README records for its cap-specific baselines. Cut
/// `chaos-monkey-v1` from six probes to four and every id in
/// [`smoke_subset_ids`] is unchanged, every bank hash is unchanged (the bank
/// file was not touched), so the fingerprint is unchanged — and last week's
/// six-probe baseline is then compared against a four-probe run as if they
/// were the same measurement. Renaming the subset on every edit would work
/// and is a thing to remember; this is the same rule made structural
/// (ARCH §10).
///
/// Comments and key ORDER are deliberately not in the digest: only what is
/// selected. A reworded comment must not orphan a baseline.
pub(super) fn smoke_selection_digest(repo: &Path) -> Result<String, String> {
    let path = repo.join("sovereign/bench/smoke.toml");
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok("absent".to_string()),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let doc: toml::Value = toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    // A malformed row REFUSES the run rather than digesting to a
    // placeholder. Two different unreadable files must not hash the same,
    // and "the fingerprint could not read this row" is not a stack anything
    // should be compared against (ARCH §18.3) — the same rule
    // `smoke_subset_ids` applies to a file that will not parse at all.
    let mut rows: Vec<String> = Vec::new();
    // A file with NO `subset` key declares no subsets, which is a real state
    // and the one `smoke_subset_ids` answers `Ok(vec![])` for. A `subset`
    // key that is not an array is a MALFORMED file, and reading it as "none
    // declared" is the substitution this function exists to avoid.
    let empty: Vec<toml::Value> = Vec::new();
    let subsets = match doc.get("subset") {
        None => &empty,
        Some(v) => v
            .as_array()
            .ok_or_else(|| format!("{}: `subset` must be an array of tables", path.display()))?,
    };
    for r in subsets {
        let id = r
            .get("subset_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| format!("{}: a [[subset]] row has no subset_id", path.display()))?;
        let bank = r
            .get("bank")
            .and_then(|v| v.as_str())
            .ok_or_else(|| format!("{}: subset `{id}` has a row with no bank", path.display()))?;
        let what = match (r.get("mode").and_then(|v| v.as_str()), r.get("ids")) {
            (Some(m), None) => format!("mode={m}"),
            (None, Some(ids)) => {
                let arr = ids.as_array().ok_or_else(|| {
                    format!("{}: `ids` for {bank} must be an array", path.display())
                })?;
                let mut v: Vec<&str> = arr.iter().filter_map(|x| x.as_str()).collect();
                if v.len() != arr.len() {
                    return Err(format!(
                        "{}: `ids` for {bank} holds a non-string",
                        path.display()
                    ));
                }
                v.sort_unstable();
                format!("ids={}", v.join(","))
            }
            _ => {
                return Err(format!(
                    "{}: the row for {bank} in subset `{id}` must declare EITHER \
                     mode = \"full\" OR an `ids` list, not both and not neither",
                    path.display()
                ))
            }
        };
        rows.push(format!("{id}|{bank}|{what}"));
    }
    rows.sort();
    Ok(short_hash(rows.join("\n").as_bytes()))
}

pub(super) async fn compute_fingerprint(
    repo: &Path,
    lanes: &[&Instrument],
    base: &str,
) -> Result<Fingerprint, String> {
    let (primary, fast) = resolve_slot_stems(base).await;
    let embed = sovereign_cli_shared::models::configured_embed_model_name();
    let smoke_subsets = smoke_subset_ids(repo)?;
    let mut banks = BTreeMap::new();
    for l in lanes {
        if let Some(bank) = l.bank.as_deref() {
            let hash = std::fs::read(repo.join(bank))
                .map(|b| short_hash(&b))
                .unwrap_or_else(|_| "absent".to_string());
            banks.insert(l.id.clone(), hash);
        }
    }
    let mut canonical = format!("primary={primary}\nfast={fast}\nembed={embed}\n");
    canonical.push_str(&format!("smoke={}\n", smoke_subsets.join(",")));
    canonical.push_str(&format!("smoke-sel={}\n", smoke_selection_digest(repo)?));
    for (lane, hash) in &banks {
        canonical.push_str(&format!("bank:{lane}={hash}\n"));
    }
    tracing::debug!(canonical = %canonical, "quality check: fingerprint inputs");
    Ok(Fingerprint {
        hex: short_hash(canonical.as_bytes()),
        primary,
        fast,
        embed,
        smoke_subsets,
        banks,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ABSENT and MALFORMED are two answers. Collapsing them makes a
    /// mis-edited `smoke.toml` compute the same fingerprint as a host with
    /// no subsets — and every lane then compares against a baseline captured
    /// under subsets it is no longer running.
    #[test]
    fn a_malformed_smoke_file_refuses_the_run_and_an_absent_one_does_not() {
        let tmp = tempfile::tempdir().unwrap();
        let bench = tmp.path().join("sovereign/bench");
        std::fs::create_dir_all(&bench).unwrap();
        // Absent: legitimately no subsets.
        assert_eq!(smoke_subset_ids(tmp.path()), Ok(Vec::new()));
        // Malformed: refused, naming the file.
        std::fs::write(bench.join("smoke.toml"), "[[subset\nnope").unwrap();
        let err = smoke_subset_ids(tmp.path()).unwrap_err();
        assert!(err.contains("smoke.toml"), "{err}");
        // Well-formed: sorted ids.
        std::fs::write(
            bench.join("smoke.toml"),
            "[[subset]]\nsubset_id = \"z1\"\n[[subset]]\nsubset_id = \"a1\"\n\
             [[subset]]\nsubset_id = \"z1\"\n",
        )
        .unwrap();
        // Sorted, and the repeat of `z1` (one subset, two banks) counts once:
        // the fingerprint wants which subsets ran, not how many banks each
        // spans.
        assert_eq!(
            smoke_subset_ids(tmp.path()),
            Ok(vec!["a1".into(), "z1".into()])
        );
    }

    /// The subset IDs cannot see a cut. Cutting `chaos-monkey-v1` from six
    /// probes to four leaves every id and every bank hash unchanged, so
    /// without this digest the fingerprint would match a baseline captured
    /// on the six.
    #[test]
    fn changing_which_ids_a_subset_selects_moves_the_fingerprint() {
        let write = |dir: &Path, body: &str| {
            let bench = dir.join("sovereign/bench");
            std::fs::create_dir_all(&bench).unwrap();
            std::fs::create_dir_all(dir.join("quality")).unwrap();
            std::fs::write(bench.join("smoke.toml"), body).unwrap();
        };
        let six = tempfile::tempdir().unwrap();
        let four = tempfile::tempdir().unwrap();
        let reordered = tempfile::tempdir().unwrap();
        let commented = tempfile::tempdir().unwrap();
        write(
            six.path(),
            "[[subset]]\nsubset_id = \"c\"\nbank = \"b/c/d.toml\"\nids = [\"a\",\"b\",\"e\"]\n",
        );
        write(
            four.path(),
            "[[subset]]\nsubset_id = \"c\"\nbank = \"b/c/d.toml\"\nids = [\"a\",\"b\"]\n",
        );
        write(
            reordered.path(),
            "[[subset]]\nsubset_id = \"c\"\nbank = \"b/c/d.toml\"\nids = [\"e\",\"b\",\"a\"]\n",
        );
        write(
            commented.path(),
            "# a reworded comment\n[[subset]]\nsubset_id = \"c\"\nbank = \"b/c/d.toml\"\nids = [\"a\",\"b\",\"e\"]\n",
        );
        // Same subset ids on both sides — the digest is the only thing that
        // separates them.
        assert_eq!(smoke_subset_ids(six.path()), smoke_subset_ids(four.path()));
        let d6 = smoke_selection_digest(six.path()).unwrap();
        assert_ne!(d6, smoke_selection_digest(four.path()).unwrap());
        // Order and comments are NOT the selection: they must not orphan a
        // baseline.
        assert_eq!(d6, smoke_selection_digest(reordered.path()).unwrap());
        assert_eq!(d6, smoke_selection_digest(commented.path()).unwrap());
        // Absent is a named answer, not a hash of nothing.
        let none = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(none.path().join("sovereign/bench")).unwrap();
        assert_eq!(
            smoke_selection_digest(none.path()),
            Ok("absent".to_string())
        );
        // A malformed row REFUSES rather than digesting to a placeholder:
        // two unreadable files must not hash the same.
        let bad = tempfile::tempdir().unwrap();
        write(
            bad.path(),
            "[[subset]]\nsubset_id = \"c\"\nbank = \"b/c/d.toml\"\n",
        );
        let err = smoke_selection_digest(bad.path()).unwrap_err();
        assert!(err.contains("must declare EITHER"), "{err}");
        write(
            bad.path(),
            "[[subset]]\nbank = \"b/c/d.toml\"\nmode = \"full\"\n",
        );
        assert!(smoke_selection_digest(bad.path())
            .unwrap_err()
            .contains("no subset_id"));
    }
}
