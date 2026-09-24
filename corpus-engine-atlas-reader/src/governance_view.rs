// SPDX-License-Identifier: AGPL-3.0-or-later
//! Governance read-model — the join that turns the atlas graph + the
//! governance oplog into a human-facing view of *current law*.
//!
//! This is the single read surface the `govern tensions`/`govern ask`
//! CLI verbs, the desktop Tensions panel, and the detector bench all
//! consume. It answers three questions for a governance corpus:
//!   - which rules are currently in force (and which were superseded /
//!     retracted, for history)?
//!   - which surfaced tensions are still open (ranked for a meeting
//!     agenda), and which were adjudicated (resolved / accepted)?
//!   - what doesn't line up (glass-box data-integrity findings)?
//!
//! Division of labour (see [`super::governance`]): the atlas graph
//! supplies rule *content* (Claim atoms) and the *surfaced* tensions
//! (`EdgeType::Tension` edges); the oplog supplies *decisions*
//! ([`super::governance::derive_active`]). This module joins them.
//!
//! The view core ([`build_view`]) is pure and depends only on minimal
//! projections ([`RuleAtom`], [`RuleTension`]) — not on the full atlas
//! atom schema — so the read-model logic stays testable without the
//! atom graph. The [`GovernanceView::from_atlas_dir`] adapter is the
//! only part coupled to `atoms.json` / `edges.json`, and it reuses the
//! canonical [`AtomsFile`] / [`EdgesFile`] shapes.
//!
//! Glass-box principle (ARCH): the view never silently drops a broken
//! reference. A rule the log governs but no atom backs, a tension whose
//! endpoint is missing, an adjudication of a tension no edge surfaces,
//! an adjudication authored unattended — each is reported as a
//! [`GovernanceIssue`] the operator can see and fix.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::governance::{ActiveSet, GovernanceOpKind, PairKey, RuleStatus, TensionStatus};
use super::governance_change::{derive_active_with_policy, read_rule_facts, RuleFacts};
use crate::atoms::{AtomEnvelope, AtomId, ChunkRef, Claim};
use crate::edges::{Edge, EdgeId, EdgeType, EdgesFile};
use crate::oplog::{Op, OpId, Oplog};
use crate::raw::read_atlas_ontology;
use corpus_index::error::{Error, Result};
use understanding_vocab::ontology::ChangePolicy;

// ── Inputs: minimal projections of the atlas graph ───────────

/// A governed rule's content, projected from a Claim atom. The read
/// model depends on this shape, not on `Claim`, so view logic is
/// decoupled from atom-graph internals.
#[derive(Debug, Clone, PartialEq)]
pub struct RuleAtom {
    pub id: AtomId,
    /// The normative statement (Claim `content`).
    pub text: String,
    /// Deontic force. For a corpus that DECLARED a directive claim type this
    /// is the declared normal form — `require` | `forbid` | `permit` |
    /// `request` — read off the reserved `deontic` attribute, which the
    /// ontology parser already validated against the type's declared modes.
    /// For an undeclared corpus it is the Claim's `claim_kind` verbatim,
    /// exactly as before ontology v1.
    pub deontic: Option<String>,
    /// The scope-entity the rule attaches to: the claim's `subject` (what it
    /// is ABOUT) when the corpus declared one, else `attributed_to` (whose
    /// voice it is). Two rules sharing a scope-entity are what atlas Phase-6
    /// pairs for tension, so this is the load-bearing modeling field — and on
    /// a declared corpus the subject is the correct pairing key, since two
    /// scholars dating the SAME coin are the tension, not two claims by the
    /// same scholar. `subject` is `None` on every claim of an undeclared
    /// corpus, so that path still reads `attributed_to`.
    pub scope: Option<AtomId>,
    /// First evidence chunk — the rule's source citation.
    pub citation: Option<ChunkRef>,
}

/// A surfaced tension, projected from an `EdgeType::Tension` edge.
#[derive(Debug, Clone, PartialEq)]
pub struct RuleTension {
    pub id: EdgeId,
    pub rule_a: AtomId,
    pub rule_b: AtomId,
    /// The sub-question the tension turns on (edge `sub_question`).
    pub why: Option<String>,
    /// Detector confidence in `[0,1]` — ranks the meeting agenda.
    pub confidence: f32,
}

// ── Outputs: the view ────────────────────────────────────────

/// A rule with its derived status, ready to render.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuleView {
    pub id: AtomId,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deontic: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<AtomId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub citation: Option<ChunkRef>,
    pub status: RuleStatus,
    /// Newer rules that retired this one on the DECLARED clock (axis 4).
    /// Separate from `status` because that is what the ACTS decided and
    /// this is what the clock infers — see [`super::governance_change`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded_by_clock: Option<Vec<AtomId>>,
}

/// Whether a surfaced tension is open or how it was adjudicated. Like
/// [`TensionStatus`] but with the `Open` arm the fold can't know on its
/// own (it needs the edge universe this view supplies).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "disposition", rename_all = "snake_case")]
pub enum TensionDisposition {
    Open,
    Resolved {
        by: OpId,
    },
    Accepted {
        by: OpId,
    },
    /// The steward judged this a detector false-positive (not a real
    /// contradiction). Distinct from [`Accepted`](Self::Accepted), which
    /// is a *real* conflict the community tolerates.
    Dismissed {
        by: OpId,
    },
    /// Not an open question because one of its rules is no longer in force
    /// (superseded or retracted). View-only — the fold can't know this
    /// without the edge's endpoints, which this join supplies. Keeps a
    /// resolved conflict closed after an atlas rebuild renumbers its edge,
    /// and keeps a fresh rule's conflict with already-dead law off the
    /// agenda.
    Moot {
        dead_endpoint: AtomId,
    },
}

/// A surfaced tension with both rule texts attached, ready to render as
/// a meeting-agenda row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TensionView {
    pub id: EdgeId,
    pub rule_a: AtomId,
    pub text_a: String,
    pub rule_b: AtomId,
    pub text_b: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
    pub confidence: f32,
    pub disposition: TensionDisposition,
}

/// Glass-box data-integrity finding — surfaced, never silently dropped.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "issue", rename_all = "snake_case")]
pub enum GovernanceIssue {
    /// The oplog governs a rule id with no Claim atom in `atoms.json`
    /// (a human-drafted superseding rule not yet written back, or atom
    /// drift after re-extraction).
    RuleHasNoAtom { rule: AtomId },
    /// A Tension edge references a rule atom that doesn't exist.
    TensionEndpointMissing { tension: EdgeId, endpoint: AtomId },
    /// A live adjudication maps to no current tension and can't be
    /// re-matched — genuine drift the steward should re-adjudicate.
    /// Fires only for (a) a legacy edge-id-only decision whose edge no
    /// longer surfaces, or (b) an endpoint-carrying decision one of whose
    /// rule atoms has vanished (the rule text was edited). A decision
    /// whose endpoints still exist but whose edge the detector simply
    /// didn't re-surface this rebuild is *normal weekly variance*, held in
    /// the pair map for re-match — deliberately NOT an issue.
    AdjudicatedTensionNotSurfaced { tension: EdgeId },
    /// INV-2: an adjudication was authored by a non-human actor.
    UnattendedAct { op: OpId },
}

/// The joined read-model: every governed rule with status, every
/// surfaced tension with disposition (open first, ranked by
/// confidence), and any integrity issues.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GovernanceView {
    pub rules: Vec<RuleView>,
    pub tensions: Vec<TensionView>,
    pub issues: Vec<GovernanceIssue>,
}

impl GovernanceView {
    /// Rules currently in force.
    pub fn active_rules(&self) -> impl Iterator<Item = &RuleView> {
        self.rules
            .iter()
            .filter(|r| matches!(r.status, RuleStatus::Active))
    }

    /// Tensions still awaiting adjudication — the meeting agenda.
    pub fn open_tensions(&self) -> impl Iterator<Item = &TensionView> {
        self.tensions
            .iter()
            .filter(|t| matches!(t.disposition, TensionDisposition::Open))
    }

    /// Section ids (an atom's evidence `chunk_id` is a *section* id like
    /// `"sec_00001"`, not a chunk row id) that carry a superseded or
    /// retracted rule — the *dead-law sections* retrieval must drop so an
    /// answer is never grounded in a rule no longer in force (FR-9 RL-3,
    /// the no-dead-law red line).
    ///
    /// An amended section is treated as dead-law *wholesale*: chunk-level
    /// retrieval can't surgically excise one rule's sentence from a chunk
    /// it shares with co-located rules, so the conservative-for-RL-3 choice
    /// is to drop the amended section and rely on the *superseding* decision
    /// — which lives in its own (kept) section. Co-located un-amended
    /// provisions in the dropped section are lost with it; the precise fix
    /// is sub-chunk (atom-span) filtering. Pair with [`chunk_to_section_map`]
    /// to turn these section ids into the chunk row ids retrieval carries.
    pub fn dead_law_sections(&self) -> HashSet<String> {
        self.rules
            .iter()
            .filter(|r| {
                matches!(
                    r.status,
                    RuleStatus::Superseded { .. } | RuleStatus::Retracted { .. }
                ) || r.superseded_by_clock.is_some()
            })
            .filter_map(|r| r.citation.as_ref().map(|c| c.chunk_id.clone()))
            .collect()
    }

    /// Read `atoms.json` + `edges.json` + the governance oplog from a
    /// corpus atlas dir and build the view. Missing files read as empty,
    /// so a corpus mid-setup degrades gracefully rather than erroring.
    pub fn from_atlas_dir(atlas_dir: impl AsRef<Path>) -> Result<Self> {
        let dir = atlas_dir.as_ref();
        let rules = read_rule_atoms(dir)?;
        let tensions = read_tensions(dir)?;
        let ops = Oplog::<GovernanceOpKind>::new(dir).read_all()?;
        // Axis 4: when the corpus declared which claim types supersede,
        // the fold gains the clock pass. `atlas/ontology.json` is absent
        // for every corpus that declared nothing and for every atlas built
        // before ontology v1, and absence reads as "declares nothing".
        let policies = read_atlas_ontology(dir)
            .map(|f| f.policies)
            .unwrap_or_default();
        if policies.change.supersedes.is_empty() {
            return Ok(build_view(&rules, &tensions, &ops));
        }
        let facts = read_rule_facts(dir, &policies)?;
        Ok(build_view_with(
            &rules,
            &tensions,
            &ops,
            &facts,
            &policies.change,
        ))
    }
}

/// Why a corpus's chunk → section map came back empty.
///
/// This distinction is the whole point. Until 2026-08-05 an empty map meant
/// either "this corpus has no section structure" or "this corpus HAS section
/// structure and its join was never written", and no caller could tell which.
/// The second state went unnoticed across 1779 of 1788 local corpora for as
/// long as the field existed, because every reader treated the empty map as
/// the benign case. An absence that cannot be distinguished from a legitimate
/// zero is not reported — it is defaulted (ARCH_PRINCIPLES §18.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinStatus {
    /// The join is present. (Possibly for only some sections — see
    /// [`SectionJoinStatus::sections_with_chunks`].)
    Present,
    /// No `chapters.json`, or it could not be read/parsed. The corpus has no
    /// declared section structure; a reader should behave as before.
    NoSectionStructure,
    /// `chapters.json` declares sections and NOT ONE carries a chunk id.
    /// A fault, not a shape: the corpus knows its own headings and cannot
    /// connect any of them to retrievable text. Repair with
    /// `svrn enrich backfill-sections <corpus>`.
    JoinMissing,
}

/// The map plus why it is the size it is.
#[derive(Debug, Clone)]
pub struct SectionJoinStatus {
    pub map: HashMap<u64, String>,
    pub status: JoinStatus,
    /// Sections declared in the manifest.
    pub sections_total: usize,
    /// Sections that carry at least one chunk id. Less than `sections_total`
    /// is a PARTIAL join — legitimate (a section may hold no chunk) but worth
    /// seeing, because a sudden drop is how a re-ingest silently breaks it.
    pub sections_with_chunks: usize,
}

impl SectionJoinStatus {
    /// The one-line operator sentence, or `None` when there is nothing to
    /// say. Callers log this rather than composing their own wording, so the
    /// repair command is named identically everywhere (§10.6).
    pub fn warning(&self, corpus_hint: &str) -> Option<String> {
        match self.status {
            JoinStatus::JoinMissing => Some(format!(
                "corpus `{corpus_hint}` declares {} section(s) but NO chunk→section join — \
                 citations cannot name a section and section filters silently match nothing. \
                 Repair: `svrn enrich backfill-sections {corpus_hint}`",
                self.sections_total
            )),
            JoinStatus::Present | JoinStatus::NoSectionStructure => None,
        }
    }
}

/// `chunk row id → section id` from a corpus's `chapters.json` — the bridge
/// between what retrieval carries (LanceDB chunk row ids on `ScoredChunk`)
/// and what atoms cite (section ids like `"sec_00001"`). `index_root` is the
/// corpus index dir (the parent of `atlas/`), where `chapters.json` lives.
///
/// An empty map means "don't filter". Callers that need to know WHY it is
/// empty — and anything user-facing does — must use
/// [`chunk_to_section_map_status`] instead; this wrapper cannot tell a
/// structureless corpus from a broken join.
pub fn chunk_to_section_map(index_root: impl AsRef<Path>) -> HashMap<u64, String> {
    chunk_to_section_map_status(index_root).map
}

/// The map, plus whether an empty one is a shape or a fault.
pub fn chunk_to_section_map_status(index_root: impl AsRef<Path>) -> SectionJoinStatus {
    #[derive(serde::Deserialize)]
    struct ChaptersFile {
        #[serde(default)]
        chapters: Vec<ChapterRow>,
    }
    #[derive(serde::Deserialize)]
    struct ChapterRow {
        id: String,
        #[serde(default)]
        chunk_ids: Vec<serde_json::Value>,
    }
    let absent = |status| SectionJoinStatus {
        map: HashMap::new(),
        status,
        sections_total: 0,
        sections_with_chunks: 0,
    };
    let path = index_root.as_ref().join("chapters.json");
    let Ok(bytes) = std::fs::read(&path) else {
        return absent(JoinStatus::NoSectionStructure);
    };
    let Ok(file) = serde_json::from_slice::<ChaptersFile>(&bytes) else {
        return absent(JoinStatus::NoSectionStructure);
    };
    let sections_total = file.chapters.len();
    let mut sections_with_chunks = 0usize;
    let mut map = HashMap::new();
    for ch in file.chapters {
        let mut any = false;
        for ci in &ch.chunk_ids {
            let id = match ci {
                serde_json::Value::Number(n) => n.as_u64(),
                serde_json::Value::String(s) => s.parse::<u64>().ok(),
                _ => None,
            };
            if let Some(id) = id {
                map.insert(id, ch.id.clone());
                any = true;
            }
        }
        if any {
            sections_with_chunks += 1;
        }
    }
    // A manifest with no sections at all is a structureless corpus wearing a
    // file, not a broken join — there is nothing that COULD have been joined.
    let status = if sections_total == 0 {
        JoinStatus::NoSectionStructure
    } else if sections_with_chunks == 0 {
        JoinStatus::JoinMissing
    } else {
        JoinStatus::Present
    };
    SectionJoinStatus {
        map,
        status,
        sections_total,
        sections_with_chunks,
    }
}

/// `section id → human title` from `chapters.json` — e.g. `"sec_00007"` →
/// `"Decision — 2026-03-14 — Guest Policy Revisited"`. Lets a caller match a
/// rule's section against the titles a model cites in its answer (so e.g.
/// supersession provenance fires only for a decision the answer actually
/// relied on). Empty on a missing/unreadable manifest.
pub fn section_titles(index_root: impl AsRef<Path>) -> HashMap<String, String> {
    #[derive(serde::Deserialize)]
    struct ChaptersFile {
        #[serde(default)]
        chapters: Vec<ChapterRow>,
    }
    #[derive(serde::Deserialize)]
    struct ChapterRow {
        id: String,
        #[serde(default)]
        title: String,
    }
    let path = index_root.as_ref().join("chapters.json");
    let Ok(bytes) = std::fs::read(&path) else {
        return HashMap::new();
    };
    let Ok(file) = serde_json::from_slice::<ChaptersFile>(&bytes) else {
        return HashMap::new();
    };
    file.chapters
        .into_iter()
        .filter(|c| !c.title.is_empty())
        .map(|c| (c.id, c.title))
        .collect()
}

// ── Pure builder ─────────────────────────────────────────────

/// Map a folded [`TensionStatus`] to its view disposition.
fn disposition_from_status(s: &TensionStatus) -> TensionDisposition {
    match s {
        TensionStatus::Resolved { by } => TensionDisposition::Resolved { by: by.clone() },
        TensionStatus::Accepted { by } => TensionDisposition::Accepted { by: by.clone() },
        TensionStatus::Dismissed { by } => TensionDisposition::Dismissed { by: by.clone() },
    }
}

/// Decide how a surfaced tension stands, in four steps of decreasing
/// specificity. The order is load-bearing for *living* governance, where
/// the atlas is rebuilt weekly and every edge id is re-minted:
///
/// 1. **Edge-id match** — same build, or a legacy edge-id-only decision.
/// 2. **Endpoint-pair match** — the same two rules, adjudicated under a
///    now-stale edge id. This is what carries a decision across a rebuild.
/// 3. **Mootness** — a tension one of whose rules is superseded/retracted
///    is not a live question. Independently keeps a resolved conflict off
///    the agenda after rebuild (resolve always supersedes a rule) and
///    drops a fresh rule's conflict with already-dead law.
/// 4. **Open** — genuinely awaiting adjudication.
fn tension_disposition(
    edge: &EdgeId,
    rule_a: &AtomId,
    rule_b: &AtomId,
    active: &ActiveSet,
) -> TensionDisposition {
    if let Some(s) = active.tensions.get(edge) {
        return disposition_from_status(s);
    }
    if let Some(s) = active.tension_pairs.get(&PairKey::new(rule_a, rule_b)) {
        return disposition_from_status(s);
    }
    for endpoint in [rule_a, rule_b] {
        if matches!(
            active.rules.get(endpoint),
            Some(RuleStatus::Superseded { .. } | RuleStatus::Retracted { .. })
        ) {
            return TensionDisposition::Moot {
                dead_endpoint: endpoint.clone(),
            };
        }
    }
    TensionDisposition::Open
}

/// Join rule content + surfaced tensions + the act log into a
/// [`GovernanceView`]. Pure; the single source of read-model truth.
///
/// The governed rule set is defined by the **oplog**, not by a heuristic
/// over claim fields: a rule appears here iff some op touched its id
/// (ingest emits `AssertRule` per extracted rule-claim). Claims the log
/// never asserts are simply not governed rules and don't appear.
pub fn build_view(
    rules: &[RuleAtom],
    tensions: &[RuleTension],
    ops: &[Op<GovernanceOpKind>],
) -> GovernanceView {
    build_view_with(rules, tensions, ops, &[], &ChangePolicy::default())
}

/// [`build_view`] with axis 4's declared supersession applied. `build_view`
/// is a shim over it, so the existing tests and the callers with no
/// ontology are untouched (ARCH §10.2): empty `facts` plus a default
/// [`ChangePolicy`] make `derive_active_with_policy` return exactly
/// `derive_active`.
pub fn build_view_with(
    rules: &[RuleAtom],
    tensions: &[RuleTension],
    ops: &[Op<GovernanceOpKind>],
    facts: &[RuleFacts],
    change: &ChangePolicy,
) -> GovernanceView {
    let (active, by_clock) = derive_active_with_policy(ops, facts, change);
    let by_id: BTreeMap<&AtomId, &RuleAtom> = rules.iter().map(|r| (&r.id, r)).collect();

    let mut issues = Vec::new();

    // Rules: one view per oplog-governed rule, joined to its content.
    // `active.rules` is a BTreeMap, so iteration is already id-sorted.
    let mut rule_views = Vec::new();
    for (rid, status) in &active.rules {
        match by_id.get(rid) {
            Some(r) => rule_views.push(RuleView {
                id: rid.clone(),
                text: r.text.clone(),
                deontic: r.deontic.clone(),
                scope: r.scope.clone(),
                citation: r.citation.clone(),
                status: status.clone(),
                superseded_by_clock: by_clock.get(rid).cloned(),
            }),
            None => {
                issues.push(GovernanceIssue::RuleHasNoAtom { rule: rid.clone() });
                rule_views.push(RuleView {
                    id: rid.clone(),
                    text: String::new(),
                    deontic: None,
                    scope: None,
                    citation: None,
                    status: status.clone(),
                    superseded_by_clock: by_clock.get(rid).cloned(),
                });
            }
        }
    }

    // Tensions: attach both rule texts and the disposition.
    let mut tension_views = Vec::new();
    let mut surfaced: BTreeSet<&EdgeId> = BTreeSet::new();
    for t in tensions {
        surfaced.insert(&t.id);
        let text_a = match by_id.get(&t.rule_a) {
            Some(r) => r.text.clone(),
            None => {
                issues.push(GovernanceIssue::TensionEndpointMissing {
                    tension: t.id.clone(),
                    endpoint: t.rule_a.clone(),
                });
                String::new()
            }
        };
        let text_b = match by_id.get(&t.rule_b) {
            Some(r) => r.text.clone(),
            None => {
                issues.push(GovernanceIssue::TensionEndpointMissing {
                    tension: t.id.clone(),
                    endpoint: t.rule_b.clone(),
                });
                String::new()
            }
        };
        let disposition = tension_disposition(&t.id, &t.rule_a, &t.rule_b, &active);
        tension_views.push(TensionView {
            id: t.id.clone(),
            rule_a: t.rule_a.clone(),
            text_a,
            rule_b: t.rule_b.clone(),
            text_b,
            why: t.why.clone(),
            confidence: t.confidence,
            disposition,
        });
    }

    // Dangling adjudications. Edge ids are re-minted on every atlas
    // rebuild, so a bare "adjudicated edge no longer surfaces" test would
    // false-positive on every past decision — the weekly-treadmill bug.
    // Instead: walk the LIVE, winning adjudication for each edge (matched
    // by `by()` == op.id, which the fold sets last-write-wins, so reverted
    // and superseded decisions are skipped) and flag only genuine drift —
    // a legacy decision with no surfaced edge, or an endpoint-carrying
    // decision whose rule atom has vanished. A valid pair simply awaiting
    // re-detection is normal variance, not an issue.
    for op in ops {
        let (edge, endpoints) = match &op.kind {
            GovernanceOpKind::ResolveTension {
                tension, endpoints, ..
            }
            | GovernanceOpKind::DismissTension {
                tension, endpoints, ..
            }
            | GovernanceOpKind::AcceptTension {
                tension, endpoints, ..
            } => (tension, endpoints),
            _ => continue,
        };
        // Only the live, winning adjudication for this edge counts.
        if active.tensions.get(edge).map(TensionStatus::by) != Some(&op.id) {
            continue;
        }
        // Matched to a currently-surfaced edge → fine.
        if surfaced.contains(edge) {
            continue;
        }
        match endpoints {
            // Pair carried: dangling only if an endpoint atom is gone.
            Some((a, b)) if by_id.contains_key(a) && by_id.contains_key(b) => {}
            // Legacy (no endpoints) unsurfaced, or a vanished endpoint atom.
            _ => issues.push(GovernanceIssue::AdjudicatedTensionNotSurfaced {
                tension: edge.clone(),
            }),
        }
    }

    // INV-2: every adjudication must be human-authored (report all).
    for op in ops {
        if !matches!(op.kind, GovernanceOpKind::AssertRule { .. })
            && !op.actor.starts_with("human:")
        {
            issues.push(GovernanceIssue::UnattendedAct { op: op.id.clone() });
        }
    }

    // Agenda order: open tensions first, then by confidence (desc),
    // then by id for a stable tie-break.
    tension_views.sort_by(|a, b| {
        let a_open = matches!(a.disposition, TensionDisposition::Open);
        let b_open = matches!(b.disposition, TensionDisposition::Open);
        b_open
            .cmp(&a_open)
            .then(b.confidence.total_cmp(&a.confidence))
            .then(a.id.as_str().cmp(b.id.as_str()))
    });

    GovernanceView {
        rules: rule_views,
        tensions: tension_views,
        issues,
    }
}

// ── IO adapter (the only atlas-schema-coupled part) ──────────

fn read_rule_atoms(dir: &Path) -> Result<Vec<RuleAtom>> {
    let path = dir.join("atoms.json");
    if !path.exists() {
        return Ok(Vec::new());
    }
    let file = understanding_vocab::read::read_atlas_atoms(dir)
        .map_err(|e| Error::Extraction(format!("governance_view: atoms.json: {e}")))?;
    Ok(file
        .atoms()
        .iter()
        .filter_map(|a| match a {
            AtomEnvelope::Claim(c) => Some(project_claim(c)),
            _ => None,
        })
        .collect())
}

fn project_claim(c: &Claim) -> RuleAtom {
    RuleAtom {
        id: c.id.clone(),
        text: c.content.clone(),
        deontic: declared_deontic(c).or_else(|| c.claim_kind.clone()),
        scope: c.subject.clone().or_else(|| c.attributed_to.clone()),
        citation: c.evidence.first().cloned(),
    }
}

/// Reserved attribute key carrying a directive claim's deontic normal form.
/// Written by corpus-engine's `parse_policy` (which re-exports it) and read by
/// [`declared_deontic`] off the atom — one spelling is the point (§10.6).
pub const ATTR_DEONTIC: &str = "deontic";

/// The declared deontic normal form, or `None`.
///
/// The value is NOT re-normalised here. `parse_policy::validated_choice`
/// already accepted it only when it named one of the claim type's declared
/// modes, so it is in the closed `Deontic` wire set by construction; a second
/// normaliser would be a second answer to the same question (§10.6). Absent
/// key, non-string value, or blank ⇒ `None`, and the caller falls back to
/// `claim_kind` — which is what every undeclared corpus does, unchanged.
fn declared_deontic(c: &Claim) -> Option<String> {
    let raw = c.attributes.get(ATTR_DEONTIC)?.as_str()?.trim();
    if raw.is_empty() {
        return None;
    }
    tracing::debug!(claim = %c.id.as_str(), deontic = %raw, "governance view: declared deontic");
    Some(raw.to_string())
}

fn read_tensions(dir: &Path) -> Result<Vec<RuleTension>> {
    let path = dir.join("edges.json");
    if !path.exists() {
        return Ok(Vec::new());
    }
    let bytes = std::fs::read(&path).map_err(Error::Io)?;
    let file: EdgesFile = serde_json::from_slice(&bytes)
        .map_err(|e| Error::Extraction(format!("governance_view: edges.json: {e}")))?;
    Ok(file
        .edges
        .iter()
        .filter(|e| e.edge_type == EdgeType::Tension)
        .map(project_tension)
        .collect())
}

fn project_tension(e: &Edge) -> RuleTension {
    RuleTension {
        id: e.id.clone(),
        rule_a: e.source.clone(),
        rule_b: e.target.clone(),
        why: e.sub_question.clone(),
        confidence: e.confidence,
    }
}

#[cfg(test)]
#[path = "governance_view/tests.rs"]
mod tests;
