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
}

impl LabelStore {
    /// Read every detector's label file under `root`.
    ///
    /// A missing file is not an error — an unlabelled detector is the normal
    /// starting state, not a fault.
    pub fn load(root: &Path) -> LabelStore {
        let mut store = LabelStore::default();
        for id in DetectorId::ALL {
            let path = Self::path_for(root, id);
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            for (i, line) in text.lines().enumerate() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                match serde_json::from_str::<Label>(line) {
                    // Last line wins: a correction is an append, never an edit.
                    Ok(label) => {
                        store.by_key.insert(label.key.clone(), label);
                    }
                    Err(e) => store
                        .malformed
                        .push(format!("{}:{}: {e}", path.display(), i + 1)),
                }
            }
        }
        store
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
        use std::io::Write as _;
        let path = Self::path_for(root, id);
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
    #[serde(default)]
    pub disposition: Option<String>,
    #[serde(default)]
    pub in_program: bool,
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
