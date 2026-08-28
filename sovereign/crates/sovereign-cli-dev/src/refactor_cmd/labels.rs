// SPDX-License-Identifier: AGPL-3.0-or-later
//! The durable half of the ledger — judgements, in git, append-only.
//!
//! # What lives here and what does not
//!
//! A label records **what a site MEANS** — the expensive, human-or-model half
//! that no detector can re-derive. It does not record where the site is, and it
//! does not record whether the work is done:
//!
//! - **Locations are not stored.** Spans rot the moment a peer commits, and a
//!   stale span sends a worker to edit the wrong line. Symbols are stable;
//!   coordinates are not. Locations come from the graph, fresh, every run.
//! - **Progress is not stored.** There is no `done` field, because closure is
//!   an absence: a holding is open iff its detector still fires on it. Nothing
//!   an agent can write moves the number.
//!
//! What is left is small, cheap to review in a PR, and impossible to corrupt in
//! a way `git diff` will not show.
//!
//! # Why jsonl and not a database
//!
//! Measured, not assumed: the whole population is ~3,000-5,000 holdings, about
//! 1.2MB. Loading and joining that in memory is microseconds against detector
//! sweeps that cost seconds. A schema would buy nothing and would add
//! migrations, corruption, and a standing "is the ledger stale" question. The
//! file being human-readable is the point — a label is a judgement, and
//! judgements belong in review.
//!
//! Append-only with last-line-wins means a correction is a new line rather than
//! an edit, so the history of an adjudication survives in `git log` without the
//! store having to model it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::detector::{DetectorId, Site};

/// Where label files live, relative to the repo root.
pub const LABELS_DIR: &str = "quality/refactors/labels";

/// What the program does with this site.
///
/// The set is the register's, unchanged (`quality/CONCEPTS.toml`) — a
/// disposition here has to mean the same thing it means there, or the ledger
/// and the concept gate would be two deciders for one question (ARCH §10.6).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum Disposition {
    /// One canonical; this site migrates onto it.
    Converge,
    /// Same name, different concept — rename apart.
    Distinct,
    /// Per-crate convention (error aliases, clap Args). Not duplication.
    Idiom,
    /// Deliberate mirror of a foreign schema.
    ExternalMirror,
    /// Same noun at two altitudes on purpose.
    Layered,
    /// Adjudicated and deliberately left alone.
    Leave,
    /// Adjudication failed. **Mandatory member of the set**: without it a
    /// labeller under pressure guesses, and a confident guess is the
    /// well-formed wrong answer this codebase keeps producing (ARCH §18.3).
    Unsure,
}

impl Disposition {
    pub fn as_str(self) -> &'static str {
        match self {
            Disposition::Converge => "converge",
            Disposition::Distinct => "distinct",
            Disposition::Idiom => "idiom",
            Disposition::ExternalMirror => "external-mirror",
            Disposition::Layered => "layered",
            Disposition::Leave => "leave",
            Disposition::Unsure => "UNSURE",
        }
    }

    /// Parse a disposition as a HUMAN typed it.
    ///
    /// The wire form is kebab-case (`serde(rename_all)`) but [`as_str`] renders
    /// `Unsure` as shouty `UNSURE`, because a refusal must be visible in a
    /// report skimmed at speed. Those two spellings diverging is fine; what was
    /// NOT fine is that the CLI parsed only the wire form while printing the
    /// display form in its error text — so it instructed the operator to type
    /// `UNSURE` and then rejected it. The one disposition ARCH §18.3 makes
    /// mandatory, so a labeller can decline instead of guessing, was the one
    /// spelling that errored while every confident guess succeeded.
    ///
    /// Both spellings are accepted, the same way `kernel_types::Verdict::
    /// parse_wire` takes `could-not-judge` and `could_not_judge`. Returns
    /// `None` rather than defaulting: an unrecognised disposition is an
    /// absence, and absence is reported.
    pub fn parse_cli(s: &str) -> Option<Disposition> {
        let t = s.trim();
        Disposition::ALL
            .into_iter()
            .find(|d| d.as_str().eq_ignore_ascii_case(t) || d.wire().eq_ignore_ascii_case(t))
    }

    /// Every disposition, so the parser and the error text cannot drift apart.
    pub const ALL: [Disposition; 7] = [
        Disposition::Converge,
        Disposition::Distinct,
        Disposition::Idiom,
        Disposition::ExternalMirror,
        Disposition::Layered,
        Disposition::Leave,
        Disposition::Unsure,
    ];

    /// The serialized spelling — what is actually on disk in the jsonl.
    pub fn wire(self) -> &'static str {
        match self {
            Disposition::Converge => "converge",
            Disposition::Distinct => "distinct",
            Disposition::Idiom => "idiom",
            Disposition::ExternalMirror => "external-mirror",
            Disposition::Layered => "layered",
            Disposition::Leave => "leave",
            Disposition::Unsure => "unsure",
        }
    }

    /// Does this disposition put the site in the burn-down?
    ///
    /// Only `converge` is work. Everything else is an adjudicated decision that
    /// the site stays as it is — which is a RESULT, not a backlog item.
    pub fn is_work(self) -> bool {
        matches!(self, Disposition::Converge)
    }
}

/// One judgement about one site.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Label {
    /// `detector/locus/token` — matches [`Site::key`].
    pub key: String,
    /// Where this site is headed. Empty for `distinct`/`idiom`/`leave`.
    #[serde(default)]
    pub dest: String,
    pub disp: Disposition,
    /// Why. Non-empty by convention; a label with no reason cannot be reviewed.
    pub why: String,
    /// `seat`, or a model attribution string.
    pub by: String,
    /// ISO date.
    pub at: String,
}

/// The loaded judgements, plus what the load could not account for.
#[derive(Debug, Default)]
pub struct LabelStore {
    by_key: BTreeMap<String, Label>,
    /// Lines that would not parse, with the file and line number. Reported,
    /// never swallowed.
    pub malformed: Vec<String>,
    /// One key judged differently by two shards. Parallel workers are given
    /// disjoint sites, so this is a partitioning bug — surfaced, never
    /// resolved by filename luck (ARCH §18.3).
    pub collisions: Vec<String>,
}

impl LabelStore {
    /// Read every detector's label file under `root`, including shards.
    ///
    /// A missing file is not an error — an unlabelled detector is the normal
    /// starting state, not a fault.
    ///
    /// # Shards, and why the order is fixed
    ///
    /// Parallel labellers cannot all append to one file: separate processes
    /// interleave writes and a torn line is a lost judgement. Each worker
    /// therefore writes `labels/<detector>.<shard>.jsonl`, and this reads the
    /// base file followed by every shard **in sorted filename order**.
    ///
    /// That order is arbitrary with respect to wall-clock, which would be a
    /// problem if two shards judged the same key — so they are not silently
    /// resolved. Workers are handed disjoint site sets, and any key appearing
    /// in two shards with DIFFERENT dispositions is recorded in
    /// [`collisions`](Self::collisions) rather than quietly last-writer-wins.
    /// Sorted order is used rather than mtime because mtime is a property of
    /// the host, not of the work, and a burn-down that differs between
    /// machines is not a measurement (ARCH §7.5).
    pub fn load(root: &Path) -> LabelStore {
        let mut store = LabelStore::default();
        for id in DetectorId::ALL {
            // Provenance per key, so a cross-shard disagreement is visible.
            let mut seen: std::collections::HashMap<String, (String, Disposition)> =
                std::collections::HashMap::new();
            for path in Self::files_for(root, id) {
                let Ok(text) = std::fs::read_to_string(&path) else {
                    continue;
                };
                let origin = path
                    .file_name()
                    .map(|f| f.to_string_lossy().to_string())
                    .unwrap_or_default();
                for (i, line) in text.lines().enumerate() {
                    let line = line.trim();
                    if line.is_empty() || line.starts_with('#') {
                        continue;
                    }
                    match serde_json::from_str::<Label>(line) {
                        // Last line wins: a correction is an append, never an edit.
                        Ok(label) => {
                            if let Some((prev_origin, prev_disp)) = seen.get(&label.key) {
                                if *prev_origin != origin && *prev_disp != label.disp {
                                    store.collisions.push(format!(
                                        "{}: {} says {}, {} says {}",
                                        label.key,
                                        prev_origin,
                                        prev_disp.as_str(),
                                        origin,
                                        label.disp.as_str()
                                    ));
                                }
                            }
                            seen.insert(label.key.clone(), (origin.clone(), label.disp));
                            store.by_key.insert(label.key.clone(), label);
                        }
                        Err(e) => {
                            store
                                .malformed
                                .push(format!("{}:{}: {e}", path.display(), i + 1))
                        }
                    }
                }
            }
        }
        store
    }

    /// The base file, then every shard, in sorted filename order.
    pub fn files_for(root: &Path, id: DetectorId) -> Vec<PathBuf> {
        let mut out = vec![Self::path_for(root, id)];
        let dir = root.join(LABELS_DIR);
        let prefix = format!("{}.", id.as_str());
        let Ok(rd) = std::fs::read_dir(&dir) else {
            return out;
        };
        let mut shards: Vec<PathBuf> = rd
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                let Some(n) = p.file_name().and_then(|n| n.to_str()) else {
                    return false;
                };
                // `<id>.<shard>.jsonl`, never the base `<id>.jsonl`.
                n.starts_with(&prefix) && n.ends_with(".jsonl") && n != format!("{prefix}jsonl")
            })
            .collect();
        shards.sort();
        out.extend(shards);
        out
    }

    /// `labels/<detector>.<shard>.jsonl` — one parallel worker's own file.
    pub fn shard_path_for(root: &Path, id: DetectorId, shard: &str) -> PathBuf {
        root.join(LABELS_DIR)
            .join(format!("{}.{}.jsonl", id.as_str(), shard))
    }

    pub fn path_for(root: &Path, id: DetectorId) -> PathBuf {
        root.join(LABELS_DIR).join(format!("{}.jsonl", id.as_str()))
    }

    pub fn get(&self, key: &str) -> Option<&Label> {
        self.by_key.get(key)
    }

    pub fn len(&self) -> usize {
        self.by_key.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_key.is_empty()
    }

    /// Labels that no longer join to any live site.
    ///
    /// A site edited but not converged can shift its key, orphaning its
    /// judgement. That is harmless — the site reappears as unlabelled — but it
    /// is SILENT, so it is counted and surfaced rather than swallowed
    /// (ARCH §18.3: absence is reported, never defaulted).
    pub fn orphans(&self, live: &[Site]) -> Vec<&Label> {
        let keys: std::collections::HashSet<String> = live.iter().map(Site::key).collect();
        self.by_key
            .values()
            .filter(|l| !keys.contains(&l.key))
            .collect()
    }

    /// Append one judgement, creating the file if needed.
    pub fn append(root: &Path, id: DetectorId, label: &Label) -> Result<(), String> {
        Self::append_to(root, id, label, None)
    }

    /// Append into a named shard (`None` = the base file).
    pub fn append_to(
        root: &Path,
        id: DetectorId,
        label: &Label,
        shard: Option<&str>,
    ) -> Result<(), String> {
        use std::io::Write as _;
        let path = match shard {
            Some(sh) => Self::shard_path_for(root, id, sh),
            None => Self::path_for(root, id),
        };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        let line = serde_json::to_string(label).map_err(|e| e.to_string())?;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        writeln!(f, "{line}").map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(())
    }
}

/// One register row, as `quality/CONCEPTS.toml` declares it.
///
/// The register already answers, for 31 nouns, exactly what a label needs:
/// which concept, what to do with its twins, and which path owns it. Asking a
/// model to re-derive that would be minting a second decider for a question
/// already settled in a reviewed file (ARCH §10.6), so the deterministic pass
/// reads it instead.
#[derive(Debug, serde::Deserialize)]
pub struct ConceptRow {
    pub name: String,
    pub canonical: String,
    /// `minted` — the canonical path exists today. `planned` — it does not.
    ///
    /// REQUIRED, and deliberately not `#[serde(default)]`: a row that forgets
    /// it must fail to parse rather than default to one of the two answers.
    /// Defaulting to `minted` would claim a home nobody built; defaulting to
    /// `planned` would excuse a home that went missing. Absence is reported,
    /// never defaulted (ARCH §18.3).
    pub home: Home,
    #[serde(default)]
    pub disposition: Option<String>,
    #[serde(default)]
    pub in_program: bool,
}

/// Whether the register's `canonical` names something that exists.
///
/// Separate from `status` on purpose — see the `home` field's note in
/// `quality/CONCEPTS.toml`. `status` measures migration progress; this
/// measures whether there is anywhere to migrate to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Home {
    /// A worker can `use` this path today.
    Minted,
    /// Nothing is there yet.
    Planned,
}

#[derive(Debug, serde::Deserialize)]
struct ConceptFile {
    #[serde(default)]
    concept: Vec<ConceptRow>,
}

pub fn load_register(root: &Path) -> Result<Vec<ConceptRow>, String> {
    let path = root.join("quality/CONCEPTS.toml");
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let parsed: ConceptFile =
        toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(parsed.concept)
}

/// Map a register disposition string onto the ledger's set.
///
/// A disposition the register spells in a way this set does not carry is
/// `None` — REFUSED, not coerced to a default. A row that reads
/// "converge the gate family; distinct the rest" is a composite judgement a
/// human wrote, and flattening it to `converge` would silently label sites
/// that the register deliberately did not settle.
pub fn disposition_from_register(raw: &str) -> Option<Disposition> {
    match raw.trim() {
        "converge" => Some(Disposition::Converge),
        "distinct" => Some(Disposition::Distinct),
        "idiom" => Some(Disposition::Idiom),
        "external-mirror" => Some(Disposition::ExternalMirror),
        "layered" => Some(Disposition::Layered),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    /// The refusal disposition must be typeable in the spelling the tool
    /// PRINTS. Before this, `as_str()` rendered `UNSURE` and the parser took
    /// only `unsure`, so the CLI told the operator to type a value it then
    /// rejected — and it was the one disposition §18.3 makes mandatory so a
    /// labeller can decline instead of guessing.
    #[test]
    fn the_refusal_disposition_parses_in_the_spelling_the_tool_prints() {
        assert_eq!(
            Disposition::parse_cli(Disposition::Unsure.as_str()),
            Some(Disposition::Unsure),
            "the display spelling must round-trip"
        );
        assert_eq!(Disposition::parse_cli("unsure"), Some(Disposition::Unsure));
        assert_eq!(Disposition::parse_cli("UNSURE"), Some(Disposition::Unsure));
    }

    /// Every disposition round-trips from both spellings, and an unknown one
    /// is None rather than a default.
    #[test]
    fn every_disposition_round_trips_and_an_unknown_one_refuses() {
        for d in Disposition::ALL {
            assert_eq!(Disposition::parse_cli(d.as_str()), Some(d), "{d:?} display");
            assert_eq!(Disposition::parse_cli(d.wire()), Some(d), "{d:?} wire");
        }
        assert_eq!(Disposition::parse_cli("probably"), None);
        assert_eq!(Disposition::parse_cli(""), None);
    }

    /// The wire spelling is what serde actually writes. If these drift, labels
    /// written by one version stop loading in the next.
    #[test]
    fn wire_matches_what_serde_serializes() {
        for d in Disposition::ALL {
            let json = serde_json::to_string(&d).unwrap();
            assert_eq!(json, format!("\"{}\"", d.wire()), "{d:?}");
        }
    }

    fn lbl(key: &str, disp: Disposition) -> Label {
        Label {
            key: key.into(),
            dest: String::new(),
            disp,
            why: "because".into(),
            by: "test".into(),
            at: "2026-08-24".into(),
        }
    }

    /// N parallel labellers each own a shard; the store reads all of them.
    #[test]
    fn shards_from_parallel_workers_all_load() {
        let d = tempfile::tempdir().unwrap();
        LabelStore::append_to(
            d.path(),
            DetectorId::Name,
            &lbl("name/a/A", Disposition::Converge),
            Some("w1"),
        )
        .unwrap();
        LabelStore::append_to(
            d.path(),
            DetectorId::Name,
            &lbl("name/b/B", Disposition::Distinct),
            Some("w2"),
        )
        .unwrap();
        LabelStore::append_to(
            d.path(),
            DetectorId::Name,
            &lbl("name/c/C", Disposition::Leave),
            None,
        )
        .unwrap();
        let st = LabelStore::load(d.path());
        assert_eq!(st.len(), 3, "base file plus every shard");
        assert!(st.collisions.is_empty());
        assert_eq!(st.get("name/a/A").unwrap().disp, Disposition::Converge);
    }

    /// Two shards judging one key differently is a PARTITIONING bug. It must be
    /// reported, not resolved by whichever filename sorts last — otherwise a
    /// fan-out silently drops half a disagreement and the burn-down looks clean.
    #[test]
    fn two_shards_disagreeing_on_one_key_is_reported_not_silently_resolved() {
        let d = tempfile::tempdir().unwrap();
        LabelStore::append_to(
            d.path(),
            DetectorId::Name,
            &lbl("name/x/X", Disposition::Converge),
            Some("w1"),
        )
        .unwrap();
        LabelStore::append_to(
            d.path(),
            DetectorId::Name,
            &lbl("name/x/X", Disposition::Distinct),
            Some("w2"),
        )
        .unwrap();
        let st = LabelStore::load(d.path());
        assert_eq!(st.collisions.len(), 1, "got {:?}", st.collisions);
        assert!(st.collisions[0].contains("name/x/X"), "{:?}", st.collisions);
        assert!(st.collisions[0].contains("converge") && st.collisions[0].contains("distinct"));
    }

    /// NEGATIVE CONTROL for the test above: the same key appended twice with
    /// the SAME disposition is a correction, not a collision. Without this, a
    /// collision detector that fired on every duplicate key would pass the test
    /// above while being useless.
    #[test]
    fn the_same_verdict_from_two_shards_is_not_a_collision() {
        let d = tempfile::tempdir().unwrap();
        LabelStore::append_to(
            d.path(),
            DetectorId::Name,
            &lbl("name/x/X", Disposition::Converge),
            Some("w1"),
        )
        .unwrap();
        LabelStore::append_to(
            d.path(),
            DetectorId::Name,
            &lbl("name/x/X", Disposition::Converge),
            Some("w2"),
        )
        .unwrap();
        let st = LabelStore::load(d.path());
        assert!(st.collisions.is_empty(), "got {:?}", st.collisions);
        assert_eq!(st.len(), 1);
    }

    /// Shard order is sorted-by-filename, and re-reading gives the same answer.
    /// mtime would make the burn-down differ between hosts (ARCH §7.5).
    #[test]
    fn shard_read_order_is_deterministic() {
        let d = tempfile::tempdir().unwrap();
        for w in ["w9", "w1", "w5"] {
            LabelStore::append_to(
                d.path(),
                DetectorId::Name,
                &lbl(&format!("name/{w}/K"), Disposition::Leave),
                Some(w),
            )
            .unwrap();
        }
        let a = LabelStore::files_for(d.path(), DetectorId::Name);
        let b = LabelStore::files_for(d.path(), DetectorId::Name);
        assert_eq!(a, b);
        let names: Vec<String> = a
            .iter()
            .skip(1)
            .map(|p| p.file_name().unwrap().to_string_lossy().into())
            .collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted, "shards must be read in sorted order");
    }

    use super::*;

    fn label(key: &str, disp: Disposition) -> Label {
        Label {
            key: key.to_string(),
            dest: "kernel_types::Verdict".to_string(),
            disp,
            why: "test".to_string(),
            by: "seat".to_string(),
            at: "2026-08-23".to_string(),
        }
    }

    fn site(key_file: &str, token: &str) -> Site {
        Site {
            detector: DetectorId::Name,
            file: key_file.to_string(),
            line: 1,
            locus: key_file.to_string(),
            token: token.to_string(),
            note: String::new(),
        }
    }

    #[test]
    fn a_later_line_supersedes_an_earlier_one_for_the_same_key() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        LabelStore::append(
            root,
            DetectorId::Name,
            &label("name/a/V", Disposition::Unsure),
        )
        .unwrap();
        LabelStore::append(
            root,
            DetectorId::Name,
            &label("name/a/V", Disposition::Converge),
        )
        .unwrap();
        let store = LabelStore::load(root);
        assert_eq!(store.len(), 1, "the correction replaced, not duplicated");
        assert_eq!(store.get("name/a/V").unwrap().disp, Disposition::Converge);
    }

    #[test]
    fn a_missing_label_file_is_a_starting_state_not_a_fault() {
        let dir = tempfile::tempdir().unwrap();
        let store = LabelStore::load(dir.path());
        assert!(store.is_empty());
        assert!(store.malformed.is_empty());
    }

    /// The negative control for the loader: a corrupt line must be NAMED, not
    /// skipped into silence.
    #[test]
    fn a_malformed_line_is_reported_and_does_not_abort_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let path = LabelStore::path_for(root, DetectorId::Name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let good = serde_json::to_string(&label("name/a/V", Disposition::Converge)).unwrap();
        std::fs::write(&path, format!("{{ not json\n{good}\n")).unwrap();

        let store = LabelStore::load(root);
        assert_eq!(store.len(), 1, "the good line still loaded");
        assert_eq!(store.malformed.len(), 1, "the bad line was reported");
        assert!(store.malformed[0].contains(":1:"));
    }

    #[test]
    fn only_converge_counts_as_work() {
        assert!(Disposition::Converge.is_work());
        for d in [
            Disposition::Distinct,
            Disposition::Idiom,
            Disposition::ExternalMirror,
            Disposition::Layered,
            Disposition::Leave,
            Disposition::Unsure,
        ] {
            assert!(
                !d.is_work(),
                "{d:?} is an adjudicated result, not a backlog item"
            );
        }
    }

    #[test]
    fn a_label_whose_site_vanished_is_counted_as_an_orphan_not_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        LabelStore::append(
            root,
            DetectorId::Name,
            &label("name/gone.rs/Ghost", Disposition::Converge),
        )
        .unwrap();
        let store = LabelStore::load(root);
        let live = vec![site("here.rs", "Verdict")];
        let orphans = store.orphans(&live);
        assert_eq!(orphans.len(), 1);
        assert_eq!(orphans[0].key, "name/gone.rs/Ghost");
    }

    #[test]
    fn a_label_that_still_joins_is_not_an_orphan() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let s = site("here.rs", "Verdict");
        LabelStore::append(
            root,
            DetectorId::Name,
            &label(&s.key(), Disposition::Converge),
        )
        .unwrap();
        let store = LabelStore::load(root);
        assert!(store.orphans(&[s]).is_empty());
    }

    /// The register carries composite dispositions a human wrote in prose
    /// ("converge the gate family; distinct the rest"). Flattening one to
    /// `converge` would label sites the register deliberately left unsettled,
    /// so an unrecognised spelling REFUSES rather than defaulting.
    #[test]
    fn a_composite_register_disposition_is_refused_not_flattened() {
        assert_eq!(
            disposition_from_register("converge"),
            Some(Disposition::Converge)
        );
        assert_eq!(
            disposition_from_register("converge the gate family; distinct the rest"),
            None
        );
        assert_eq!(disposition_from_register(""), None);
    }

    #[test]
    fn the_wire_form_is_stable_and_reviewable() {
        let json = serde_json::to_string(&label("name/a/V", Disposition::ExternalMirror)).unwrap();
        // kebab-case on the wire so a human reading the file sees the same
        // spelling the register uses.
        assert!(json.contains(r#""disp":"external-mirror""#), "{json}");
    }
}
