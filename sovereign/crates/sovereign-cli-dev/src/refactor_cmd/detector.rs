// SPDX-License-Identifier: AGPL-3.0-or-later
//! The detector set — five instruments behind one trait, and the interlock
//! that makes closure honest (`quality/REFACTOR_LEDGER.md`).
//!
//! # Why a trait at all
//!
//! The five instruments were already named together in exactly one place —
//! `schedule.rs`'s module doc — and invoked from five scattered call sites in
//! `run_gate`. This promotes that list to a closed set (ARCH §2: closed sets
//! are enums, not tables) so that the ledger's central rule can be stated in
//! code rather than in prose:
//!
//! > **A holding is open if and only if its detector still fires on it.**
//!
//! There is no `mark_done`, no `close()` that writes a row, no state an agent
//! can set. Progress is a MEASUREMENT taken fresh on every invocation. That is
//! what makes the burn-down unforgeable and what makes a dead worker session a
//! no-op rather than something to reconcile.
//!
//! # Scoping is a POST-FILTER, never a narrowed input
//!
//! This is the design's load-bearing correction, and it was found by measuring
//! rather than by reasoning. `quality/REFACTOR_LEDGER.md` pre-registered
//! "every detector can be re-run against a named file set". Two of the five
//! cannot, and the reason is not cost:
//!
//! - [`converge::census`] computes "defined as a type in more than ONE CRATE".
//! - [`shape_census`] weights every match by IDF **over the population**, and
//!   its `rare_df = 20` gate is an absolute document frequency.
//!
//! Hand either one six files from a single crate and it returns zero — not
//! because the duplication is gone, but because a cross-crate predicate cannot
//! see across crates it was not shown. **A detector that stops firing for the
//! wrong reason is exactly the fake closure this whole mechanism exists to
//! prevent**, arriving through the front door.
//!
//! So [`Detector::fire`] always computes the WHOLE population, and callers
//! narrow the RESULT with [`Site::in_files`]. That is exact — `TypeDef.file`
//! and `ShapeSide.file` both carry the file — and it makes the trait uniform
//! instead of growing five different scoping rules. The cost is the graph load
//! (~7-11s for 320k symbols + 1.6M refs); an order takes a worker half an hour,
//! so a ten-second close is noise.
//!
//! # The negative control
//!
//! A detector can stop firing because the duplication was converged, or
//! because the pattern broke. Those are indistinguishable from the outside and
//! only one of them is good news. So every [`FireReport`] carries the verdict
//! of a **control site** — a place that must STILL match — and a run whose
//! control went silent is [`Verdict::CouldNotJudge`], never a pass (ARCH
//! §18.1: a check with no failing input you can name is not a check).
//!
//! The precedent is in the next file over: `quality/refactors/node-id.toml` is
//! kept deliberately failing, and `refactor_wire.rs` pins that direction in
//! `negative_control_node_id_fails_with_the_production_bytes`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use corpus_engine_scip::converge::{
    census as name_census, cross_crate_reached, type_defs, SourceScope,
};
use corpus_engine_scip::shape::{field_signatures, shape_census, ShapeOptions};
use corpus_engine_scip::{ScipRefRecord, ScipSymbolRecord};
use kernel_types::{Judgement, Reason, Verdict};
use regex::Regex;

use super::census;

/// The closed set. Adding a kind is a deliberate edit here, not a row someone
/// drops into a config file (ARCH §2).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum DetectorId {
    /// `<field>: String` declarations that a newtype could take over.
    FieldAtom,
    /// Cross-crate duplicate SHAPES — the renamed fork a name census cannot see.
    Shape,
    /// The same type NAME defined in more than one crate.
    Name,
    /// Duplicate BEHAVIOUR — exact and near function clones.
    Behaviour,
    /// Duplicate INTENT — the same job with different code, which a name
    /// census, a shape census and a clone search all miss by construction.
    Intent,
    /// Hand-rolled `--flag` match loops where a derived parser would serve.
    ArgLoop,
    /// Provenance carried through an untyped `metadata` map instead of a
    /// typed `Origin` / `Custody` / `Attribution` field.
    ProvenanceChannel,
    /// A struct holding many fields that each carry their own guard, so no
    /// invariant can span them. The composition-root smell the god-object
    /// measure is blind to.
    UnownedCell,
}

impl DetectorId {
    pub fn as_str(self) -> &'static str {
        match self {
            DetectorId::FieldAtom => "field-atom",
            DetectorId::Shape => "shape",
            DetectorId::Name => "name",
            DetectorId::Behaviour => "behaviour",
            DetectorId::Intent => "intent",
            DetectorId::ArgLoop => "arg-loop",
            DetectorId::ProvenanceChannel => "provenance-channel",
            DetectorId::UnownedCell => "unowned-cell",
        }
    }

    /// Every detector, in the order `status` reports them.
    pub const ALL: [DetectorId; 8] = [
        DetectorId::FieldAtom,
        DetectorId::Shape,
        DetectorId::Name,
        DetectorId::Behaviour,
        DetectorId::Intent,
        DetectorId::ArgLoop,
        DetectorId::ProvenanceChannel,
        DetectorId::UnownedCell,
    ];
}

/// One place the codebase must be coerced.
///
/// Deliberately carries NO durable identity of its own: a `Site` is re-derived
/// on every run, and the ledger joins it to a judgement by [`Site::key`].
/// Coordinates rot — a peer's commit moves every line in the file — so `line`
/// is for rendering an order and is never persisted.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Site {
    pub detector: DetectorId,
    /// Repo-relative path.
    pub file: String,
    pub line: i32,
    /// The SCIP qualified name when the detector has one, else the file path.
    /// Stable across reformatting and line moves, which is the whole point.
    pub locus: String,
    /// What the detector matched — a field name, a type name, a clone digest.
    pub token: String,
    /// One line a worker can read without opening anything else.
    pub note: String,
}

impl Site {
    /// The join key against the label store: `detector/locus/token`.
    ///
    /// Not a hash — a human has to be able to `grep` a `.jsonl` for this and
    /// see which site it means.
    ///
    /// # It survives the codebase moving, which is the point
    ///
    /// This program's whole job is to change the tree underneath itself, so a
    /// judgement that could not outlive its site moving would be worthless. No
    /// coordinate appears here: a line move, a reformat, or an unrelated edit
    /// in the same file all leave the key identical (pinned by
    /// `a_key_does_not_move_when_the_site_moves`). A file or symbol RENAME does
    /// break it — that is the one lossy case, and it surfaces as a named
    /// orphan rather than a silent gap.
    ///
    /// # Granularity, and why it errs the safe way
    ///
    /// For the file-keyed detectors several sites in one file share a key, so
    /// the label is per-file rather than per-declaration. Closure is therefore
    /// conservative: convert three of four `corpus_id: String` lines in a file
    /// and the detector still fires, so all four stay open. Measured on the
    /// first live close — converting all four in `corpus_grant.rs` closed all
    /// four, while `corpus_collaborate.rs` untouched stayed open. Partial
    /// progress inside a file is invisible, and that is the correct direction
    /// to be wrong in: it can under-report progress, never fake it.
    pub fn key(&self) -> String {
        format!("{}/{}/{}", self.detector.as_str(), self.locus, self.token)
    }

    /// Narrow a whole-population result to a named file set.
    ///
    /// This is the ONLY supported way to scope a detector — see the module
    /// doc for why narrowing the input instead is a correctness bug.
    pub fn in_files(sites: &[Site], files: &[String]) -> Vec<Site> {
        sites
            .iter()
            .filter(|s| files.iter().any(|f| f == &s.file))
            .cloned()
            .collect()
    }
}

/// A place that must still match, or the run judged nothing.
///
/// Declared as a const beside each detector so a detector without one does not
/// compile. When a control is legitimately converged away its test fails loudly
/// and the seat picks a new one — that failure is the mechanism working, not a
/// breakage.
#[derive(Debug, Clone, Copy)]
pub struct ControlSite {
    pub file: &'static str,
    pub token: &'static str,
    /// Why this site is a fair control — what it would mean if it went quiet.
    pub why: &'static str,
}

/// What one detector run produced, and whether it may be believed.
#[derive(Debug, Clone)]
pub struct FireReport {
    pub detector: DetectorId,
    pub sites: Vec<Site>,
    /// The control verdict. `Passed` means the instrument is demonstrably live
    /// on this tree; anything else means the sites must not be acted on.
    pub control: Judgement,
    /// Printed on every run and stamped into every order. Changing it restarts
    /// the series — the same guard the miss-rate bar carries.
    pub settings_digest: String,
}

impl FireReport {
    /// Build a report and adjudicate its own control in one place, so no
    /// detector can forget to.
    fn new(
        detector: DetectorId,
        sites: Vec<Site>,
        control: ControlSite,
        settings_digest: impl Into<String>,
    ) -> FireReport {
        let subject = format!("{} control", detector.as_str());
        let fired = sites
            .iter()
            .any(|s| s.file == control.file && s.token == control.token);
        let control = if fired {
            Judgement::passed(
                subject,
                Reason::new(format!(
                    "control site {}:{} still matches — instrument is live",
                    control.file, control.token
                ))
                .unwrap_or_else(|| Reason::literal("control site still matches")),
            )
        } else {
            Judgement::could_not_judge(
                subject,
                Reason::new(format!(
                    "control site {}:{} did not match. {} \
                     Either the site was legitimately converged (pick a new control) \
                     or the detector is broken — until that is settled this run \
                     closes nothing",
                    control.file, control.token, control.why
                ))
                .unwrap_or_else(|| Reason::literal("control site did not match")),
            )
        };
        FireReport {
            detector,
            sites,
            control,
            settings_digest: settings_digest.into(),
        }
    }

    /// May the sites in this report be acted on?
    pub fn is_live(&self) -> bool {
        self.control.verdict() == Verdict::Passed
    }
}

/// Everything a detector needs, loaded once and shared across all five.
///
/// The graph halves are the expensive part (~7-11s, ~0.8-1.1 GB resident), so
/// they are read once by the caller rather than per detector.
pub struct DetectorCtx<'a> {
    pub root: &'a Path,
    pub symbols: &'a [ScipSymbolRecord],
    pub refs: &'a [ScipRefRecord],
    pub scope: &'a SourceScope,
    /// Corpus index dir, for the behaviour detector's chunk reads.
    pub index_path: &'a Path,
    pub corpus_id: &'a str,
}

/// What one run of this detector costs, and on what evidence.
///
/// This is not a hint — `close` has an affordability bar (under 30s, so the
/// proof chain stays cheap enough to run after every order) and a detector that
/// cannot meet it must say so rather than silently blowing the budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CostClass {
    /// Runs inside the close budget.
    Cheap,
    /// Does not. Carries the measurement that says so — never an opinion.
    Expensive(&'static str),
}

#[async_trait::async_trait]
pub trait Detector: Send + Sync {
    fn id(&self) -> DetectorId;

    /// The frozen settings, rendered. Printed on every run.
    fn settings_digest(&self) -> String;

    /// The site that must still match.
    fn control(&self) -> ControlSite;

    /// Measured cost. Defaults to cheap; override with evidence.
    fn cost(&self) -> CostClass {
        CostClass::Cheap
    }

    /// The WHOLE population. Never scope this — scope the result with
    /// [`Site::in_files`].
    async fn fire(&self, ctx: &DetectorCtx<'_>) -> Result<FireReport, String>;
}

// ── 1. Field atoms ───────────────────────────────────────────────────────────

/// Seat ruling, inherited from `schedule.rs`: these fields are `String`
/// because they ARE open text. Newtyping them is the spec's attack #3 — every
/// minted type is one more thing a future symbol can duplicate.
const OPEN_TEXT_ATOMS: &[&str] = &[
    "name",
    "content",
    "description",
    "text",
    "label",
    "title",
    "summary",
    "reason",
    "message",
    "question",
];

/// Below this many declarations an atom is tail, not a subject.
const ATOM_DECL_FLOOR: usize = 20;

pub struct FieldAtomDetector;

#[async_trait::async_trait]
impl Detector for FieldAtomDetector {
    fn id(&self) -> DetectorId {
        DetectorId::FieldAtom
    }

    fn settings_digest(&self) -> String {
        format!(
            "floor={ATOM_DECL_FLOOR};open_text={}",
            OPEN_TEXT_ATOMS.len()
        )
    }

    fn control(&self) -> ControlSite {
        ControlSite {
            // Verified present 2026-08-23. `corpus_id` is the factory's first
            // declared subject and had 369 String declarations at 63c72af8.
            file: "sovereign/crates/sovereign-core/src/conv_entity_graph.rs",
            token: "corpus_id",
            why: "corpus_id is the factory's first subject; if no String \
                  declaration of it remains, the atom is converged and this \
                  control must move.",
        }
    }

    async fn fire(&self, ctx: &DetectorCtx<'_>) -> Result<FireReport, String> {
        let files = census::walk_rs_files(ctx.root, census::EXCLUDE_DIRS_DECL);

        // ONE pass over the tree, not one pass per atom. The obvious spelling
        // here is `string_field_census` to get the counts, then
        // `find_decl_sites(field)` for each — but that re-reads all ~1,900
        // files once per atom, so twenty atoms is twenty full walks. The
        // generic decl regex already captures the field name, so a single pass
        // yields the counts AND the sites together.
        let decl = census::string_field_decl_re();
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        let mut found: Vec<(String, String, usize)> = Vec::new();
        for path in &files {
            let Ok(text) = std::fs::read_to_string(path) else {
                continue;
            };
            let rel = rel_path(ctx.root, path);
            for (i, line) in text.lines().enumerate() {
                if let Some(c) = decl.captures(line) {
                    let field = c[1].to_string();
                    *counts.entry(field.clone()).or_insert(0) += 1;
                    found.push((field, rel.clone(), i + 1));
                }
            }
        }

        let sites = found
            .into_iter()
            .filter(|(field, _, _)| {
                counts.get(field).copied().unwrap_or(0) >= ATOM_DECL_FLOOR
                    && !OPEN_TEXT_ATOMS.contains(&field.as_str())
            })
            .map(|(field, file, line)| {
                let n = counts[&field];
                Site {
                    detector: DetectorId::FieldAtom,
                    line: line as i32,
                    locus: file.clone(),
                    file,
                    note: format!("`{field}: String` — {n} declarations workspace-wide"),
                    token: field,
                }
            })
            .collect();

        Ok(FireReport::new(
            self.id(),
            sites,
            self.control(),
            self.settings_digest(),
        ))
    }
}

// ── 2. Duplicate shapes ──────────────────────────────────────────────────────

pub struct ShapeDetector;

#[async_trait::async_trait]
impl Detector for ShapeDetector {
    fn id(&self) -> DetectorId {
        DetectorId::Shape
    }

    fn settings_digest(&self) -> String {
        let o = ShapeOptions::default();
        format!(
            "threshold={};min_shared={};rare_df={};min_fields={}",
            o.threshold, o.min_shared, o.rare_df, o.min_fields
        )
    }

    fn control(&self) -> ControlSite {
        ControlSite {
            // `ClaimCitation` == `DrCitation` is the canonical cross-crate
            // shape pair — the renamed fork a NAME census structurally cannot
            // see, and the example `converge_cmd`'s own help text cites.
            //
            // The first control tried here was `Verdict`, and the interlock
            // rejected it on the first live run: `Verdict` is a FIELDLESS
            // ENUM, and `shape_census` matches field signatures with
            // `min_fields = 2`, so it can never appear in this detector's
            // population. A control the instrument cannot see by construction
            // would have made every run could-not-judge forever. Recorded
            // because the next person to pick a control will reach for a
            // famous name rather than a shaped one.
            file: "sovereign/crates/sovereign-core/src/deep_research/icd.rs",
            token: "ClaimCitation",
            why: "ClaimCitation/DrCitation is the canonical cross-crate shape \
                  fork. If it stops matching, either it was converged (pick a \
                  new control) or the shape detector has gone blind.",
        }
    }

    async fn fire(&self, ctx: &DetectorCtx<'_>) -> Result<FireReport, String> {
        let sigs = field_signatures(ctx.symbols, ctx.refs, ctx.scope);
        // FROZEN. Changing any knob restarts the series (campaign guard).
        let census = shape_census(ctx.symbols, &sigs, ctx.scope, &ShapeOptions::default());
        let mut sites = Vec::new();
        for group in &census.groups {
            let members = group.members.join(", ");
            for pair in std::iter::once(&group.best).chain(group.pairs.iter()) {
                for side in [&pair.a, &pair.b] {
                    let site = Site {
                        detector: DetectorId::Shape,
                        file: side.file.clone(),
                        line: side.line,
                        locus: if side.qualified.is_empty() {
                            side.file.clone()
                        } else {
                            side.qualified.clone()
                        },
                        token: side.name.clone(),
                        note: format!("shape group score {:.2} — {}", group.top_score, members),
                    };
                    if !sites.contains(&site) {
                        sites.push(site);
                    }
                }
            }
        }
        Ok(FireReport::new(
            self.id(),
            sites,
            self.control(),
            self.settings_digest(),
        ))
    }
}

// ── 3. Duplicate names ───────────────────────────────────────────────────────

pub struct NameDetector;

#[async_trait::async_trait]
impl Detector for NameDetector {
    fn id(&self) -> DetectorId {
        DetectorId::Name
    }

    fn settings_digest(&self) -> String {
        "reachable_only=true;kin=false".to_string()
    }

    fn control(&self) -> ControlSite {
        ControlSite {
            file: "kernel-types/src/judgement.rs",
            token: "Verdict",
            why: "Verdict is defined in ten crates and the kernel's is the \
                  declared canonical.",
        }
    }

    async fn fire(&self, ctx: &DetectorCtx<'_>) -> Result<FireReport, String> {
        let defs = type_defs(ctx.symbols, ctx.scope);
        let reached = cross_crate_reached(&defs, ctx.refs, ctx.scope);
        let census = name_census(&defs, &reached, ctx.scope, false);
        let mut sites = Vec::new();
        for row in census.rows.iter().filter(|r| r.is_reachable()) {
            for def in &row.defs {
                sites.push(Site {
                    detector: DetectorId::Name,
                    file: def.file.clone(),
                    line: def.line,
                    locus: if def.qualified.is_empty() {
                        def.file.clone()
                    } else {
                        def.qualified.clone()
                    },
                    token: row.name.clone(),
                    note: format!(
                        "`{}` defined in {} crates ({} reached across a boundary)",
                        row.name,
                        row.crates.len(),
                        row.reached_crates.len()
                    ),
                });
            }
        }
        Ok(FireReport::new(
            self.id(),
            sites,
            self.control(),
            self.settings_digest(),
        ))
    }
}

// ── 4. Duplicate behaviour ───────────────────────────────────────────────────

pub struct BehaviourDetector;

#[async_trait::async_trait]
impl Detector for BehaviourDetector {
    fn id(&self) -> DetectorId {
        DetectorId::Behaviour
    }

    fn settings_digest(&self) -> String {
        format!(
            "min_lines={};near_threshold={}",
            sovereign_tools::code::dry_report::DEFAULT_MIN_LINES,
            sovereign_tools::code::dry_report::DEFAULT_NEAR_THRESHOLD
        )
    }

    /// Measured on this host 2026-08-23, first live run: the exact tier
    /// finished in ~1.4s, then the near tier's O(n²) pass over 24,823 reps ran
    /// **156s on 12 threads** — 3m39s for the whole sweep. That is five times
    /// the close budget, so this detector is opt-in rather than silently
    /// blowing it. Not a guess; the number is why.
    fn cost(&self) -> CostClass {
        CostClass::Expensive(
            "near tier is O(n²) over ~24.8k reps — measured 156s on 12 threads, 2026-08-23",
        )
    }

    fn control(&self) -> ControlSite {
        ControlSite {
            // Chosen by measurement, not by taste: the first live full run
            // (2026-08-31, 185 exact groups / 356 near clusters / ~10,703
            // redundant lines) reported this group at 5 copies x 8 lines.
            file: "sovereign/crates/sovereign-core/src/deep_research/fetch.rs",
            token: "2e0ac3170ee6",
            why: "`alignment_decision` is duplicated five times — four in \
                  deep_research/fetch.rs and once in search.rs — as a \
                  byte-identical 8-line body. It is a plain exact clone with \
                  no near-tier threshold in its way, so a silent control here \
                  means the exact tier stopped seeing identical bodies, not \
                  that a cutoff drifted. Either those five were legitimately \
                  converged (pick a new control) or the detector is broken.",
        }
    }

    async fn fire(&self, ctx: &DetectorCtx<'_>) -> Result<FireReport, String> {
        use sovereign_tools::code::dry_report::{
            build_dry_report, short_hash, DryInputs, DEFAULT_MIN_LINES, DEFAULT_NEAR_THRESHOLD,
        };
        let report = build_dry_report(DryInputs {
            index_path: ctx.index_path,
            corpus_id: ctx.corpus_id,
            min_lines: DEFAULT_MIN_LINES,
            near_threshold: DEFAULT_NEAR_THRESHOLD,
            scope: None,
        })
        .await
        .map_err(|e| format!("dry_report: {e}"))?;

        let mut sites = Vec::new();
        for clone in &report.exact_clones {
            for m in &clone.members {
                sites.push(Site {
                    detector: DetectorId::Behaviour,
                    file: m.file.clone(),
                    line: m.line_start as i32,
                    locus: m.symbol.clone(),
                    token: short_hash(&clone.signature).to_string(),
                    note: format!(
                        "exact clone, {} lines, {} copies",
                        clone.lines,
                        clone.members.len()
                    ),
                });
            }
        }
        for cluster in &report.near_clusters {
            for m in &cluster.members {
                sites.push(Site {
                    detector: DetectorId::Behaviour,
                    file: m.file.clone(),
                    line: m.line_start as i32,
                    locus: m.symbol.clone(),
                    token: format!("near:{:.3}", cluster.min_sim),
                    note: format!("near clone cluster of {}", cluster.members.len()),
                });
            }
        }
        Ok(FireReport::new(
            self.id(),
            sites,
            self.control(),
            self.settings_digest(),
        ))
    }
}

// ── 4b. Duplicate intent ─────────────────────────────────────────────────────

pub struct IntentDetector;

#[async_trait::async_trait]
impl Detector for IntentDetector {
    fn id(&self) -> DetectorId {
        DetectorId::Intent
    }

    fn settings_digest(&self) -> String {
        super::intent::IntentOptions::default().digest()
    }

    /// Unlike [`BehaviourDetector`], this one blocks on discriminative terms
    /// instead of comparing every pair, so it is not the O(n²) that keeps
    /// behaviour out of the close budget. The measurement replaces this note
    /// on the first live run.
    fn cost(&self) -> CostClass {
        CostClass::Cheap
    }

    fn control(&self) -> ControlSite {
        ControlSite {
            file: "",
            token: "",
            why: "Not yet pinned: the intent control is selected from the \
                  first live run and recorded here, the same way the \
                  behaviour control was. Until it is, this detector reports \
                  COULD-NOT-JUDGE by construction rather than pretending to \
                  a proof it has not got.",
        }
    }

    async fn fire(&self, ctx: &DetectorCtx<'_>) -> Result<FireReport, String> {
        let opts = super::intent::IntentOptions::default();
        let symbols = super::intent::load_intent_corpus(ctx.index_path, &opts)?;
        let clusters = super::intent::intent_census(&symbols, &opts);

        let mut sites = Vec::new();
        for c in &clusters {
            let token = c.token();
            let homes = c.members.len();
            let crates: BTreeSet<&str> = c.members.iter().map(|m| m.krate.as_str()).collect();
            for m in &c.members {
                sites.push(Site {
                    detector: DetectorId::Intent,
                    file: m.file.clone(),
                    line: m.line,
                    locus: if m.qualified_name.is_empty() {
                        m.file.clone()
                    } else {
                        m.qualified_name.clone()
                    },
                    token: token.clone(),
                    note: format!(
                        "same job as {} other{} across {} crates ({})",
                        homes - 1,
                        if homes == 2 { "" } else { "s" },
                        crates.len(),
                        c.terms.join(", ")
                    ),
                });
            }
        }
        Ok(FireReport::new(
            self.id(),
            sites,
            self.control(),
            self.settings_digest(),
        ))
    }
}

// ── 5. Hand-rolled arg loops ─────────────────────────────────────────────────

pub struct ArgLoopDetector;

#[async_trait::async_trait]
impl Detector for ArgLoopDetector {
    fn id(&self) -> DetectorId {
        DetectorId::ArgLoop
    }

    fn settings_digest(&self) -> String {
        "min_flag_arms=2".to_string()
    }

    fn control(&self) -> ControlSite {
        ControlSite {
            file: "sovereign/crates/sovereign-cli-dev/src/refactor_cmd/mod.rs",
            token: "flag-surface",
            why: "refactor_cmd's own dispatcher hand-rolls its flag loop. If \
                  this detector cannot see the file it is defined in, it \
                  cannot see anything.",
        }
    }

    async fn fire(&self, ctx: &DetectorCtx<'_>) -> Result<FireReport, String> {
        let files = census::walk_rs_files(ctx.root, census::EXCLUDE_DIRS_DECL);
        let scan = census::arg_loop_scan(&files);
        let sites = scan
            .hand_rolled
            .iter()
            .map(|(path, arms)| {
                let rel = rel_path(ctx.root, path);
                Site {
                    detector: DetectorId::ArgLoop,
                    file: rel.clone(),
                    line: 0,
                    locus: rel,
                    token: "flag-surface".to_string(),
                    note: format!(
                        "{arms} hand-rolled long-flag arms; a derived parser would serve"
                    ),
                }
            })
            .collect();
        Ok(FireReport::new(
            self.id(),
            sites,
            self.control(),
            self.settings_digest(),
        ))
    }
}

// ── 6. The provenance channel ────────────────────────────────────────────────

/// Seat ruling: the metadata keys that answer **where did this content come
/// from**, which is the question `kernel_types::Origin` exists to answer as a
/// typed field.
///
/// Each entry carries the field it migrates onto, because that is what the
/// order's `(expected, found) -> edit` rule needs and it is not derivable from
/// the key alone.
///
/// # What is deliberately NOT here, and why
///
/// The same scan sees 41 other keys on the same maps. They are excluded by
/// ruling, not by oversight, and the boundary is the shape of `Origin` itself
/// (`Source` / `Server` / `Grain` / `Locator`):
///
/// - **Bibliographic** — `title`, `authors`, `year`, `language`, `subjects`,
///   `gutenberg_id`, `locc`, `bookshelves`, `abstract`. These are facts about
///   the DOCUMENT, not about its acquisition. Folding them into `Origin` would
///   widen the noun into a catalogue record, which is the additive move this
///   program exists to refuse.
/// - **Structural** — `ordinal`, `section_id`, `section_path`, `raptor_level`,
///   `raptor_node_id`, `atlas_tier`. Index position, not provenance.
///   `raptor_level` is the closest call: derivedness IS the `Grain` question.
///   It is out because the grain decision does not read this channel today —
///   `grounding/sealed.rs` takes `grains` as a parameter from the caller — so
///   firing here would report a site that is not on the path being migrated.
///
/// Widening this list is a settings change, and the digest below makes it a
/// visible diff that restarts the series (interlock 7).
const PROVENANCE_KEYS: &[(&str, &str)] = &[
    (
        "source",
        "Origin::source — the channel content arrived through",
    ),
    ("source_id", "Origin::source — the channel's own id"),
    ("url", "Origin::locator — where it was fetched from"),
    ("peer_name", "Origin::server — which machine served it"),
    ("peer", "Origin::server — same question, second spelling"),
    (
        "custody",
        "Evidence::custody — the CUSTODY_META_KEY channel",
    ),
    (
        "attributed_to",
        "Attribution — which engine produced the text",
    ),
];

/// Provenance riding an untyped `HashMap<String, String>`.
///
/// This is the instrument phase 4 of the register needs and the one the other
/// five cannot supply. `field-atom` sees `<name>: String` DECLARATIONS;
/// `shape` and `name` see duplicate TYPES. None of them can see a fact travelling
/// as a map entry, because there is no type there to be duplicated — which is
/// exactly the defect: `Origin` is not being re-implemented, it is being
/// bypassed.
///
/// `quality/REFACTOR_LEDGER.md` names this detector `provenance-metadata-writer`
/// in its worked example and never built it. The name here is
/// `provenance-channel`, because it fires on READS as well as writes: a
/// `metadata.get("source")` downstream is the other half of the same channel
/// and migrating only the writers would leave readers looking for a key nobody
/// sets any more.
///
/// # It matches statements, not lines
///
/// The canonical site is multi-line:
///
/// ```ignore
/// metadata.insert(
///     crate::types::CUSTODY_META_KEY.to_string(),
///     crate::types::Custody::Personal.as_str().to_string(),
/// );
/// ```
///
/// A line-anchored scan — including the `rg` one-liner the register's `measure`
/// records — misses every write in that shape. Measured on this tree
/// 2026-08-24: line-anchored found 1 `custody` site, statement-anchored found 3.
pub struct ProvenanceChannelDetector;

impl ProvenanceChannelDetector {
    fn matcher() -> Regex {
        let literals = PROVENANCE_KEYS
            .iter()
            .map(|(k, _)| regex::escape(k))
            .collect::<Vec<_>>()
            .join("|");
        // `(?s)` so `.` spans newlines; the receiver may be a local (`meta`,
        // `metadata`) or a field (`chunk.metadata`) — `\b` holds after the dot.
        Regex::new(&format!(
            r#"(?s)\bmeta(?:data)?\s*\.\s*(?:get|insert|contains_key|remove)\s*\(\s*&?\s*(?:"({literals})"|(?:[A-Za-z_][A-Za-z0-9_]*\s*::\s*)*CUSTODY_META_KEY)"#
        ))
        .expect("provenance-channel matcher is a literal pattern")
    }
}

#[async_trait::async_trait]
impl Detector for ProvenanceChannelDetector {
    fn id(&self) -> DetectorId {
        DetectorId::ProvenanceChannel
    }

    fn settings_digest(&self) -> String {
        format!("keys={};test_scope=excluded", PROVENANCE_KEYS.len())
    }

    fn control(&self) -> ControlSite {
        ControlSite {
            file: "sovereign/crates/sovereign-core/src/runtime/retrieval_pipeline.rs",
            token: "custody",
            why: "The estate stamp writes Custody through CUSTODY_META_KEY at \
                  acquisition — the canonical untyped-provenance site, and the \
                  one `quality/CONCEPTS.toml`'s Custody row cites. It is also \
                  the multi-line shape a line-anchored scan cannot see, so a \
                  silent control here means the matcher regressed to \
                  line-matching rather than that the channel was closed.",
        }
    }

    async fn fire(&self, ctx: &DetectorCtx<'_>) -> Result<FireReport, String> {
        let re = Self::matcher();
        let files = census::walk_rs_files(ctx.root, census::EXCLUDE_DIRS_MENTIONS);
        let mut sites = Vec::new();
        for path in &files {
            let Ok(raw) = std::fs::read_to_string(path) else {
                continue;
            };
            // Comments first: a `#[cfg(test)]` written INSIDE a comment is
            // prose too, and blanking comments settles both cases at once.
            let src = census::strip_test_scope(&census::strip_comments(&raw));
            let rel = rel_path(ctx.root, path);
            for m in re.captures_iter(&src) {
                let whole = m.get(0).expect("group 0 always exists");
                let key = m
                    .get(1)
                    .map(|g| g.as_str().to_string())
                    .unwrap_or_else(|| "custody".to_string());
                let line = src[..whole.start()].lines().count() as i32;
                let onto = PROVENANCE_KEYS
                    .iter()
                    .find(|(k, _)| *k == key)
                    .map(|(_, onto)| *onto)
                    .unwrap_or("Origin");
                sites.push(Site {
                    detector: DetectorId::ProvenanceChannel,
                    file: rel.clone(),
                    line,
                    // One holding per (file, key): a file touching `"source"`
                    // four times is ONE decision to migrate, not four.
                    locus: rel.clone(),
                    token: key.clone(),
                    note: format!("provenance rides metadata[{key:?}] — migrate onto {onto}"),
                });
            }
        }
        // De-duplicate to one holding per (file, key), keeping the first line.
        sites.sort_by(|a, b| (&a.file, &a.token, a.line).cmp(&(&b.file, &b.token, b.line)));
        sites.dedup_by(|a, b| a.file == b.file && a.token == b.token);
        Ok(FireReport::new(
            self.id(),
            sites,
            self.control(),
            self.settings_digest(),
        ))
    }
}

// ── 7. Unowned cells ─────────────────────────────────────────────────────────

/// Types that make a field independently mutable.
///
/// Each carries its own interior mutability, so a field declared with one can
/// be written without holding any guard the struct's OTHER fields are under.
/// Two such fields therefore cannot be read together atomically — which is the
/// same statement as *no invariant may span them*.
///
/// `Arc<T>` is deliberately absent: sharing a handle is not independent
/// mutation. `Arc<Mutex<T>>` is caught by the `Mutex` row, and a bare
/// `Arc<Config>` is exactly the immutable shared state this detector should
/// stay quiet about.
const CELL_KINDS: &[(&str, &str)] = &[
    (r"\bArcSwap", "ArcSwap"),
    (r"\bRwLock\s*<", "RwLock"),
    (r"\bMutex\s*<", "Mutex"),
    (r"\bAtomic[A-Z]\w*", "Atomic"),
    (r"\bSemaphore", "Semaphore"),
    (r"\bOnceCell\s*<|\bOnceLock\s*<", "OnceCell"),
    (r"\bmpsc::Sender", "mpsc-tx"),
];

/// Below this many cells a struct is a counter set, not a composition root.
///
/// # The number is measured, not preferred
///
/// Ranked over this tree, the band immediately under the floor is
/// single-kind counter structs — `HealthTracker` (6 `Atomic`) and
/// `VerifyStats` (6 `Atomic`) — where the fields genuinely are independent
/// tallies and there IS no invariant to span them. At 8 and above every hit is
/// a runtime root or a manager. A floor errs toward under-reporting, which is
/// the safe direction for the same reason [`Site::key`] gives: it can miss a
/// subject, never invent one.
const CELL_FLOOR: usize = 8;

/// Fields declared at the top level of a struct body, each paired with the
/// source text of its own declaration.
///
/// `open` is the byte offset of the `{` that opens the body. Depth is tracked
/// over all three bracket pairs so an attribute like `#[serde(default = "..")]`
/// or a nested type cannot be mistaken for a field, and a field is only
/// recognised at the start of a line — the shape rustfmt guarantees.
fn struct_fields(src: &str, open: usize) -> Vec<(usize, String, String)> {
    let bytes = src.as_bytes();
    let mut depth = 0usize;
    let mut end = src.len();
    // Byte scan, not `char_indices().skip(open)`: `open` is a BYTE offset, so
    // skipping that many CHARS starts mid-body on any file containing a
    // multi-byte character earlier on — which underflowed `depth` on the first
    // `}` and panicked the whole run. Braces are ASCII, so bytes are exact and
    // a UTF-8 continuation byte can never be mistaken for one.
    for (i, b) in bytes.iter().enumerate().skip(open) {
        match b {
            b'{' => depth += 1,
            b'}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    end = i;
                    break;
                }
            }
            _ => {}
        }
    }
    let body_start = open + 1;
    if body_start >= end {
        return Vec::new();
    }

    // Two guards, because either alone lets a continuation line through.
    // `^ {4}` is rustfmt's field indent under this workspace's stock config
    // (`rustfmt.toml` is deliberately defaults-only), so a wrapped type's
    // continuation sits deeper and is skipped. `[^:]` after the colon rejects
    // a path segment: without it the second line of
    //     peer: std::sync::RwLock<
    //         std::collections::HashMap<NodeId, u64>,
    // reads as a field named `std`, which is how this was caught.
    let field_re = Regex::new(r"^ {4}(?:pub(?:\s*\([^)]*\))?\s+)?([a-z_][a-z0-9_]*)\s*:[^:]")
        .expect("static shape");

    // Positions where a field declaration begins: line start, all depths zero.
    let mut starts: Vec<(usize, String)> = Vec::new();
    let (mut curly, mut paren, mut brack) = (0usize, 0usize, 0usize);
    let mut line_start = body_start;
    for i in body_start..end {
        match bytes[i] {
            b'{' => curly += 1,
            b'}' => curly = curly.saturating_sub(1),
            b'(' => paren += 1,
            b')' => paren = paren.saturating_sub(1),
            b'[' => brack += 1,
            b']' => brack = brack.saturating_sub(1),
            b'\n' => {
                line_start = i + 1;
                continue;
            }
            _ => {}
        }
        if i != line_start || curly + paren + brack != 0 {
            continue;
        }
        let line_end = src[i..end].find('\n').map_or(end, |o| i + o);
        if let Some(c) = field_re.captures(&src[i..line_end]) {
            starts.push((i, c[1].to_string()));
        }
    }

    starts
        .iter()
        .enumerate()
        .map(|(n, (pos, name))| {
            let stop = starts.get(n + 1).map_or(end, |(p, _)| *p);
            (*pos, name.clone(), src[*pos..stop].to_string())
        })
        .collect()
}

/// One struct, as this detector sees it.
pub struct CellScan {
    pub name: String,
    pub line: i32,
    pub fields: usize,
    pub kinds: BTreeMap<&'static str, usize>,
}

impl CellScan {
    pub fn cells(&self) -> usize {
        self.kinds.values().sum()
    }

    /// `18 RwLock, 14 Atomic, …` — the evidence, in a fixed order so two runs
    /// of the same tree render the same string.
    pub fn mix(&self) -> String {
        self.kinds
            .iter()
            .map(|(k, n)| format!("{n} {k}"))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

pub struct UnownedCellDetector;

impl UnownedCellDetector {
    /// The cell kind carried by one field declaration, in `CELL_KINDS` order.
    /// A field counts ONCE however many guards its type nests — `RwLock<Vec<
    /// Mutex<T>>>` is one field somebody can write alone, not two.
    fn cell_kind(decl: &str) -> Option<&'static str> {
        CELL_KINDS.iter().find_map(|(pat, label)| {
            Regex::new(pat)
                .ok()
                .filter(|re| re.is_match(decl))
                .map(|_| *label)
        })
    }

    /// Every named-field struct in one file, already stripped of comments and
    /// `#[cfg(test)]` scopes by the caller.
    ///
    /// Split out from [`Detector::fire`] because the floor, the parser and the
    /// kind table are the whole instrument, and an instrument that can only be
    /// exercised by walking a real tree is one nobody pins (§18.1).
    pub fn scan_text(text: &str) -> Vec<CellScan> {
        let decl_re =
            Regex::new(r"(?m)^(?:pub(?:\s*\([^)]*\))?\s+)?struct\s+([A-Z]\w*)\s*(?:<[^>]*>\s*)?\{")
                .expect("static shape");
        let mut out = Vec::new();
        for m in decl_re.captures_iter(text) {
            let whole = m.get(0).expect("group 0 always present");
            let fields = struct_fields(text, whole.end() - 1);
            if fields.is_empty() {
                continue;
            }
            let mut kinds: BTreeMap<&'static str, usize> = BTreeMap::new();
            for (_, _, decl) in &fields {
                if let Some(k) = Self::cell_kind(decl) {
                    *kinds.entry(k).or_insert(0) += 1;
                }
            }
            out.push(CellScan {
                name: m[1].to_string(),
                line: text[..whole.start()].lines().count() as i32 + 1,
                fields: fields.len(),
                kinds,
            });
        }
        out
    }
}

#[async_trait::async_trait]
impl Detector for UnownedCellDetector {
    fn id(&self) -> DetectorId {
        DetectorId::UnownedCell
    }

    fn settings_digest(&self) -> String {
        format!("floor={CELL_FLOOR};kinds={}", CELL_KINDS.len())
    }

    fn control(&self) -> ControlSite {
        ControlSite {
            // Verified present 2026-08-24: 60 fields, 40 of them cells
            // (18 RwLock, 14 Atomic, 4 ArcSwap, 3 Mutex, 1 Semaphore).
            file: "commonwealth/crates/commonwealth-api/src/state.rs",
            token: "AppStateInner",
            why: "AppStateInner is the mesh daemon's composition root and the \
                  worst holding on this tree by a factor of two. If it has \
                  gone quiet either the root was genuinely given an owner \
                  (pick a new control) or the parser stopped seeing struct \
                  fields at all.",
        }
    }

    async fn fire(&self, ctx: &DetectorCtx<'_>) -> Result<FireReport, String> {
        let files = census::walk_rs_files(ctx.root, census::EXCLUDE_DIRS_DECL);
        let mut sites = Vec::new();
        for path in &files {
            let Ok(raw) = std::fs::read_to_string(path) else {
                continue;
            };
            // Comments blanked (a doc comment describing `RwLock` is prose
            // about the pattern, not the pattern — the same false positive
            // `strip_comments` was written for) and `#[cfg(test)]` scopes
            // dropped (a test double's counters are not a runtime root).
            let text = census::strip_test_scope(&census::strip_comments(&raw));
            let rel = rel_path(ctx.root, path);
            for scan in Self::scan_text(&text) {
                if scan.cells() < CELL_FLOOR {
                    continue;
                }
                sites.push(Site {
                    detector: DetectorId::UnownedCell,
                    line: scan.line,
                    file: rel.clone(),
                    locus: rel.clone(),
                    note: format!(
                        "{} of {} fields carry their own guard ({}) — no two can be \
                         read under one guard, so no invariant may span them",
                        scan.cells(),
                        scan.fields,
                        scan.mix()
                    ),
                    token: scan.name,
                });
            }
        }
        sites.sort_by(|a, b| (&a.file, &a.token).cmp(&(&b.file, &b.token)));

        Ok(FireReport::new(
            self.id(),
            sites,
            self.control(),
            self.settings_digest(),
        ))
    }
}

/// Every detector, constructed. The one place the set is enumerated.
pub fn all() -> Vec<Box<dyn Detector>> {
    vec![
        Box::new(FieldAtomDetector),
        Box::new(ShapeDetector),
        Box::new(NameDetector),
        Box::new(BehaviourDetector),
        Box::new(IntentDetector),
        Box::new(ArgLoopDetector),
        Box::new(ProvenanceChannelDetector),
        Box::new(UnownedCellDetector),
    ]
}

fn rel_path(root: &Path, p: &Path) -> String {
    p.strip_prefix(root)
        .unwrap_or(p)
        .to_string_lossy()
        .replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn site(detector: DetectorId, file: &str, token: &str) -> Site {
        Site {
            detector,
            file: file.to_string(),
            line: 1,
            locus: file.to_string(),
            token: token.to_string(),
            note: String::new(),
        }
    }

    #[test]
    fn the_id_set_is_closed_and_every_id_renders() {
        assert_eq!(DetectorId::ALL.len(), 7);
        for id in DetectorId::ALL {
            assert!(!id.as_str().is_empty());
        }
    }

    /// The multi-line shape is the one that matters: the canonical custody
    /// write spans four lines, and the `rg` one-liner in the register's
    /// `measure` field cannot see it. If this ever fails, the detector has
    /// regressed to line-matching and its count is an undercount that reads
    /// like progress.
    #[test]
    fn the_matcher_sees_a_write_that_spans_lines() {
        let re = ProvenanceChannelDetector::matcher();
        let multi = "metadata.insert(\n    crate::types::CUSTODY_META_KEY.to_string(),\n";
        assert!(re.is_match(multi), "multi-line CUSTODY_META_KEY insert");
        assert!(re.is_match("c.metadata.get(\"source\")"), "field receiver");
        assert!(
            re.is_match("meta.insert(\"peer_name\".to_string(), n)"),
            "short receiver"
        );
        assert!(re.is_match("m.metadata.get(&\"url\")"), "borrowed key");
    }

    /// The exclusions are a ruling, so they are pinned. A key that is about the
    /// DOCUMENT or its INDEX POSITION is not provenance, and widening the set by
    /// accident would flood the ledger with holdings no `Origin` field can take.
    #[test]
    fn bibliographic_and_structural_keys_are_not_provenance() {
        let re = ProvenanceChannelDetector::matcher();
        for key in [
            "title",
            "authors",
            "year",
            "ordinal",
            "section_id",
            "raptor_level",
        ] {
            assert!(
                !re.is_match(&format!("metadata.get(\"{key}\")")),
                "{key} must not read as provenance"
            );
        }
    }

    /// Test fixtures are not production sites. `corpus-engine/src/index/
    /// evidence.rs` is the case that makes this load-bearing: its test module
    /// builds `metadata` maps carrying `"custody"` precisely BECAUSE the
    /// production type has already converged off that channel. Counting them
    /// would report the converged case as unconverged — a burn-down that goes
    /// UP when work lands.
    #[test]
    fn an_inline_test_module_is_not_a_production_site() {
        let src = "fn prod() { metadata.get(\"source\"); }\n\
                   #[cfg(test)]\n\
                   mod tests {\n\
                       fn t() { metadata.get(\"custody\"); }\n\
                   }\n\
                   fn after() { metadata.get(\"url\"); }\n";
        let stripped = census::strip_test_scope(src);
        let re = ProvenanceChannelDetector::matcher();
        let keys: Vec<String> = re
            .captures_iter(&stripped)
            .filter_map(|c| c.get(1).map(|g| g.as_str().to_string()))
            .collect();
        assert_eq!(
            keys,
            vec!["source", "url"],
            "the test module's key must be gone"
        );
        assert_eq!(
            stripped.lines().count(),
            src.lines().count(),
            "line numbering must survive stripping, or every reported line is wrong"
        );
    }

    #[test]
    fn a_key_names_the_detector_the_locus_and_the_token() {
        let s = site(DetectorId::FieldAtom, "a/b.rs", "corpus_id");
        assert_eq!(s.key(), "field-atom/a/b.rs/corpus_id");
    }

    /// The resilience property the whole store depends on: the codebase moves
    /// under us constantly — every refactor this program lands shifts lines —
    /// so a judgement must survive its site moving. Coordinates are rendered,
    /// never keyed.
    #[test]
    fn a_key_does_not_move_when_the_site_moves() {
        let mut before = site(DetectorId::Name, "a/b.rs", "Verdict");
        before.line = 42;
        let mut after = before.clone();
        after.line = 907; // 865 lines inserted above it by an unrelated commit
        after.note = "recomputed note".into();
        assert_eq!(before.key(), after.key());
    }

    /// The positive half of the interlock: a run whose control matched is live.
    #[test]
    fn a_control_that_fires_makes_the_run_live() {
        let control = ControlSite {
            file: "a/b.rs",
            token: "corpus_id",
            why: "test",
        };
        let sites = vec![site(DetectorId::FieldAtom, "a/b.rs", "corpus_id")];
        let r = FireReport::new(DetectorId::FieldAtom, sites, control, "t");
        assert_eq!(r.control.verdict(), Verdict::Passed);
        assert!(r.is_live());
    }

    /// The negative half, and the one that matters. A detector that went quiet
    /// must not be able to report a clean sweep — silence is COULD-NOT-JUDGE,
    /// never a pass (ARCH §18.1).
    #[test]
    fn a_silent_control_refuses_the_whole_run_even_with_sites_present() {
        let control = ControlSite {
            file: "a/b.rs",
            token: "corpus_id",
            why: "test",
        };
        // Sites exist — just not the control's. A naive implementation would
        // call this a successful run with 1 finding.
        let sites = vec![site(DetectorId::FieldAtom, "other/c.rs", "tenant_id")];
        let r = FireReport::new(DetectorId::FieldAtom, sites, control, "t");
        assert_eq!(r.control.verdict(), Verdict::CouldNotJudge);
        assert!(!r.is_live());
        assert!(r.control.reason().as_str().contains("closes nothing"));
    }

    /// An empty sweep is the shape a broken instrument takes, so it must be
    /// refused for the same reason.
    #[test]
    fn an_empty_sweep_is_could_not_judge_not_a_clean_bill() {
        let control = ControlSite {
            file: "a/b.rs",
            token: "corpus_id",
            why: "test",
        };
        let r = FireReport::new(DetectorId::Name, Vec::new(), control, "t");
        assert_eq!(r.control.verdict(), Verdict::CouldNotJudge);
    }

    #[test]
    fn scoping_filters_results_and_never_narrows_input() {
        let sites = vec![
            site(DetectorId::Name, "a/b.rs", "Verdict"),
            site(DetectorId::Name, "c/d.rs", "Verdict"),
        ];
        let scoped = Site::in_files(&sites, &["a/b.rs".to_string()]);
        assert_eq!(scoped.len(), 1);
        assert_eq!(scoped[0].file, "a/b.rs");
    }

    /// The frozen-settings guard is only a guard if the digest actually moves
    /// when a knob does.
    #[test]
    fn the_shape_digest_names_every_knob_that_moves_the_number() {
        let d = ShapeDetector.settings_digest();
        for knob in ["threshold", "min_shared", "rare_df", "min_fields"] {
            assert!(d.contains(knob), "digest {d:?} omits {knob}");
        }
    }

    /// The behaviour control was pinned 2026-08-31 from the first live full
    /// run. Its predecessor asserted the OPPOSITE — that no control existed —
    /// so that the absence could not quietly persist as a pass. This is the
    /// replacement that test asked for.
    #[test]
    fn the_behaviour_control_is_pinned_and_can_therefore_pass() {
        let c = BehaviourDetector.control();
        assert!(!c.file.is_empty(), "the behaviour control lost its site");
        assert!(!c.token.is_empty(), "the behaviour control lost its token");

        // Absent its site the verdict must still be COULD-NOT-JUDGE, never a
        // silent pass — the property the old test protected.
        let empty = FireReport::new(DetectorId::Behaviour, Vec::new(), c, "t");
        assert_eq!(empty.control.verdict(), Verdict::CouldNotJudge);

        // Present, it goes live.
        let hit = FireReport::new(
            DetectorId::Behaviour,
            vec![Site {
                detector: DetectorId::Behaviour,
                file: c.file.to_string(),
                line: 677,
                locus: "alignment_decision".into(),
                token: c.token.to_string(),
                note: String::new(),
            }],
            c,
            "t",
        );
        assert_eq!(hit.control.verdict(), Verdict::Passed);
    }

    /// The token a behaviour site carries must be the spelling the report
    /// renders, or a control pinned from the report can never match.
    #[test]
    fn the_behaviour_token_is_the_rendered_short_hash() {
        let full = "2e0ac3170ee6aabbccddeeff00112233";
        assert_eq!(
            sovereign_tools::code::dry_report::short_hash(full),
            BehaviourDetector.control().token,
            "control token must be the 12-char rendered form"
        );
    }

    // ── unowned cells ────────────────────────────────────────────────────────

    fn scan_one(src: &str) -> CellScan {
        let mut v = UnownedCellDetector::scan_text(src);
        assert_eq!(v.len(), 1, "expected exactly one struct in fixture");
        v.remove(0)
    }

    #[test]
    fn a_field_is_a_cell_when_its_type_carries_its_own_guard() {
        let s = scan_one(
            "pub struct Root {\n\
             \x20   a: RwLock<u64>,\n\
             \x20   b: std::sync::atomic::AtomicBool,\n\
             \x20   c: ArcSwap<Config>,\n\
             \x20   d: Mutex<Vec<u8>>,\n\
             \x20   e: tokio::sync::Semaphore,\n\
             }\n",
        );
        assert_eq!(s.fields, 5);
        assert_eq!(s.cells(), 5);
        assert_eq!(
            s.mix(),
            "1 ArcSwap, 1 Atomic, 1 Mutex, 1 RwLock, 1 Semaphore"
        );
    }

    /// The distinction the whole detector rests on: a shared handle is not
    /// independent mutation. `Arc<Config>` is exactly the immutable shared
    /// state a clean root SHOULD be full of.
    #[test]
    fn a_shared_handle_is_not_a_cell() {
        let s = scan_one(
            "pub struct Root {\n\
             \x20   cfg: Arc<Config>,\n\
             \x20   engine: Arc<dyn Engine>,\n\
             \x20   started: std::time::Instant,\n\
             \x20   name: String,\n\
             }\n",
        );
        assert_eq!(s.fields, 4);
        assert_eq!(s.cells(), 0);
    }

    /// Nested guards are still one field somebody can write alone. Counting
    /// them twice would inflate every root that holds a collection of locks.
    #[test]
    fn nested_guards_count_the_field_once() {
        let s = scan_one("struct R {\n    a: RwLock<Vec<Mutex<u8>>>,\n}\n");
        assert_eq!(s.cells(), 1);
    }

    /// Attributes carry parens and quoted text; a naive line scan reads
    /// `default = "x"` as a field named `default`.
    #[test]
    fn an_attribute_is_not_a_field() {
        let s = scan_one(
            "struct R {\n\
             \x20   #[serde(default, rename = \"aa\")]\n\
             \x20   a: RwLock<u64>,\n\
             \x20   #[serde(skip)]\n\
             \x20   b: Mutex<u64>,\n\
             }\n",
        );
        assert_eq!(s.fields, 2);
        assert_eq!(s.cells(), 2);
    }

    /// A field whose type spans lines must not have its continuation lines
    /// read as further fields.
    #[test]
    fn a_multiline_field_type_is_one_field() {
        let s = scan_one(
            "struct R {\n\
             \x20   peer: std::sync::RwLock<\n\
             \x20       std::collections::HashMap<NodeId, u64>,\n\
             \x20   >,\n\
             \x20   n: AtomicUsize,\n\
             }\n",
        );
        assert_eq!(s.fields, 2);
        assert_eq!(s.cells(), 2);
    }

    /// Prose about the pattern is not the pattern — the same false positive
    /// `census::strip_comments` exists to kill, restated as a bar here because
    /// `fire` is the only place the two are composed.
    #[test]
    fn a_doc_comment_naming_a_guard_is_not_a_cell() {
        let raw = "struct R {\n\
                   \x20   /// Protected by an RwLock elsewhere. Not a Mutex here.\n\
                   \x20   plain: u64,\n\
                   }\n";
        let s = scan_one(&census::strip_comments(raw));
        assert_eq!(s.fields, 1);
        assert_eq!(s.cells(), 0, "the comment's `RwLock` was counted");
    }

    /// Found by running the detector over the real tree, which panicked on the
    /// first file carrying a non-ASCII character: `open` is a byte offset and
    /// the body scan was skipping that many chars, so it started mid-body and
    /// underflowed the brace depth. Unicode in doc comments and note text is
    /// ordinary here, so this is the common case, not an exotic one.
    #[test]
    fn a_multibyte_character_before_the_struct_does_not_desync_the_scan() {
        let src = "/// Grounding — a decision, not a guess. £ ✓\n\
                   struct R {\n\
                   \x20   a: RwLock<u64>,\n\
                   \x20   b: Mutex<u64>,\n\
                   }\n";
        let s = scan_one(src);
        assert_eq!(s.fields, 2);
        assert_eq!(s.cells(), 2);
    }

    #[test]
    fn a_tuple_struct_and_an_empty_struct_are_not_subjects() {
        assert!(UnownedCellDetector::scan_text("pub struct Id(String);\n").is_empty());
        assert!(UnownedCellDetector::scan_text("pub struct Marker {}\n").is_empty());
    }

    /// The floor is the instrument's only knob, so it gets a failing input on
    /// each side of it rather than a single happy-path assertion (§18.1).
    #[test]
    fn the_floor_admits_at_the_boundary_and_refuses_below_it() {
        let field = |i: usize| format!("    f{i}: RwLock<u64>,\n");
        let build =
            |n: usize| format!("struct R {{\n{}}}\n", (0..n).map(field).collect::<String>());
        let at = scan_one(&build(CELL_FLOOR));
        assert_eq!(at.cells(), CELL_FLOOR);
        assert!(at.cells() >= CELL_FLOOR, "boundary must fire");

        let below = scan_one(&build(CELL_FLOOR - 1));
        assert!(below.cells() < CELL_FLOOR, "one under the floor must not");
    }

    #[test]
    fn the_unowned_cell_digest_names_every_knob_that_moves_the_number() {
        let d = UnownedCellDetector.settings_digest();
        for knob in ["floor", "kinds"] {
            assert!(d.contains(knob), "digest {d:?} omits {knob}");
        }
    }

    /// Registration is what makes a detector reachable from `status`, `order`
    /// and the ledger. A detector that exists but is not in both lists is
    /// invisible in exactly the way this program is meant to prevent.
    #[test]
    fn every_detector_id_is_constructed_by_all() {
        let built: Vec<DetectorId> = all().iter().map(|d| d.id()).collect();
        for id in DetectorId::ALL {
            assert!(built.contains(&id), "{} is not in all()", id.as_str());
        }
        assert_eq!(built.len(), DetectorId::ALL.len());
    }
}
