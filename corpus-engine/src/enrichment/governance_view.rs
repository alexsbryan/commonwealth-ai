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
use crate::enrichment::atlas::atoms::{AtomEnvelope, AtomId, AtomsFile, ChunkRef, Claim};
use crate::enrichment::atlas::edges::{Edge, EdgeId, EdgeType, EdgesFile};
use crate::enrichment::atlas::read_atlas_ontology;
use crate::enrichment::ontology::ChangePolicy;
use crate::error::{Error, Result};
use crate::oplog::{Op, OpId, Oplog};

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
    let bytes = std::fs::read(&path).map_err(Error::Io)?;
    let file: AtomsFile = serde_json::from_slice(&bytes)
        .map_err(|e| Error::Extraction(format!("governance_view: atoms.json: {e}")))?;
    Ok(file
        .atoms
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

/// The declared deontic normal form, or `None`.
///
/// The value is NOT re-normalised here. `parse_policy::validated_choice`
/// already accepted it only when it named one of the claim type's declared
/// modes, so it is in the closed `Deontic` wire set by construction; a second
/// normaliser would be a second answer to the same question (§10.6). Absent
/// key, non-string value, or blank ⇒ `None`, and the caller falls back to
/// `claim_kind` — which is what every undeclared corpus does, unchanged.
fn declared_deontic(c: &Claim) -> Option<String> {
    let raw = c
        .attributes
        .get(crate::enrichment::pipeline::pipelines::parse_policy::ATTR_DEONTIC)?
        .as_str()?
        .trim();
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
mod tests {
    use super::*;

    fn rule(n: usize, text: &str) -> RuleAtom {
        RuleAtom {
            id: AtomId::claim(n),
            text: text.into(),
            deontic: Some("forbids".into()),
            scope: Some(AtomId::entity(99)),
            citation: Some(ChunkRef::new(format!("chunk-{n}"), Some(text.into()))),
        }
    }
    fn tension(n: usize, a: usize, b: usize, why: &str, conf: f32) -> RuleTension {
        RuleTension {
            id: EdgeId::new(n),
            rule_a: AtomId::claim(a),
            rule_b: AtomId::claim(b),
            why: Some(why.into()),
            confidence: conf,
        }
    }
    fn op(kind: GovernanceOpKind, ts: i64, actor: &str) -> Op<GovernanceOpKind> {
        Op::new(kind, ts, actor)
    }
    fn assert_rule(n: usize, ts: i64) -> Op<GovernanceOpKind> {
        op(
            GovernanceOpKind::AssertRule {
                rule: AtomId::claim(n),
                source_doc: None,
            },
            ts,
            "ingest",
        )
    }

    #[test]
    fn rules_join_status_and_content() {
        let ops = vec![
            assert_rule(1, 1000),
            op(
                GovernanceOpKind::Supersede {
                    new_rule: AtomId::claim(2),
                    old_rules: vec![AtomId::claim(1)],
                    rationale: String::new(),
                },
                1001,
                "human:alex",
            ),
        ];
        let rules = vec![rule(1, "old rule"), rule(2, "new rule")];
        let view = build_view(&rules, &[], &ops);

        let active: Vec<_> = view.active_rules().collect();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].id, AtomId::claim(2));
        assert_eq!(active[0].text, "new rule");
        assert_eq!(active[0].deontic.as_deref(), Some("forbids"));

        let old = view
            .rules
            .iter()
            .find(|r| r.id == AtomId::claim(1))
            .unwrap();
        assert!(matches!(old.status, RuleStatus::Superseded { .. }));
        assert_eq!(old.text, "old rule");
        assert!(view.issues.is_empty());
    }

    /// Like `rule`, but with an explicit *section* id citation (an atom's
    /// evidence `chunk_id` is a section id like `"sec_00001"`, not a chunk
    /// row id — see [`chunk_to_section_map`] for the bridge to row ids).
    fn rule_at(n: usize, section: &str, text: &str) -> RuleAtom {
        RuleAtom {
            id: AtomId::claim(n),
            text: text.into(),
            deontic: Some("forbids".into()),
            scope: Some(AtomId::entity(99)),
            citation: Some(ChunkRef::new(section.to_string(), Some(text.into()))),
        }
    }

    #[test]
    fn dead_law_sections_are_the_superseded_rules_sections() {
        // claim 1 (sec-a) superseded by claim 2 (sec-b); claim 3 (sec-c)
        // active and untouched.
        let ops = vec![
            assert_rule(1, 1000),
            assert_rule(3, 1000),
            op(
                GovernanceOpKind::Supersede {
                    new_rule: AtomId::claim(2),
                    old_rules: vec![AtomId::claim(1)],
                    rationale: String::new(),
                },
                1001,
                "human:alex",
            ),
        ];
        let rules = vec![
            rule_at(1, "sec-a", "guests may stay two nights"),
            rule_at(2, "sec-b", "no overnight guests"),
            rule_at(3, "sec-c", "quiet hours begin at 10pm"),
        ];
        let dead = build_view(&rules, &[], &ops).dead_law_sections();
        assert!(
            dead.contains("sec-a"),
            "the superseded rule's section is dead law"
        );
        assert!(
            !dead.contains("sec-b"),
            "the active successor's section is kept"
        );
        assert!(
            !dead.contains("sec-c"),
            "an untouched active section is kept"
        );
        assert_eq!(dead.len(), 1);
    }

    #[test]
    fn dead_law_sections_flags_a_section_mixing_live_and_dead() {
        // claim 2 (sec-a) superseded; claim 1 (sec-a) STILL ACTIVE in the
        // same section. The aggressive RL-3 choice flags sec-a wholesale —
        // chunk-level retrieval can't excise one rule's sentence from a
        // chunk it shares, so the amended section is dropped and the
        // superseding decision (sec-b, kept) carries the current rule.
        let ops = vec![
            assert_rule(1, 1000),
            assert_rule(2, 1000),
            op(
                GovernanceOpKind::Supersede {
                    new_rule: AtomId::claim(3),
                    old_rules: vec![AtomId::claim(2)],
                    rationale: String::new(),
                },
                1001,
                "human:alex",
            ),
        ];
        let rules = vec![
            rule_at(1, "sec-a", "members must accompany daytime visitors"),
            rule_at(2, "sec-a", "a guest may stay two nights"),
            rule_at(3, "sec-b", "overnight guests are not permitted"),
        ];
        let dead = build_view(&rules, &[], &ops).dead_law_sections();
        assert!(
            dead.contains("sec-a"),
            "a section with any superseded rule is dead-law wholesale"
        );
        assert!(!dead.contains("sec-b"), "the successor's section is kept");
    }

    #[test]
    fn open_tensions_carry_both_texts_and_rank_by_confidence() {
        let rules = vec![
            rule(1, "no guests in common areas after 11"),
            rule(2, "guests may stay two nights"),
            rule(3, "quiet hours begin at 10"),
        ];
        let tensions = vec![
            tension(2, 1, 3, "weak overlap", 0.4),
            tension(1, 1, 2, "overnight vs curfew?", 0.9),
        ];
        let ops = vec![
            assert_rule(1, 1000),
            assert_rule(2, 1001),
            assert_rule(3, 1002),
        ];
        let view = build_view(&rules, &tensions, &ops);

        let open: Vec<_> = view.open_tensions().collect();
        assert_eq!(open.len(), 2);
        // Highest confidence first.
        assert_eq!(open[0].id, EdgeId::new(1));
        assert_eq!(open[0].text_a, "no guests in common areas after 11");
        assert_eq!(open[0].text_b, "guests may stay two nights");
        assert_eq!(open[0].why.as_deref(), Some("overnight vs curfew?"));
        assert_eq!(open[1].id, EdgeId::new(2));
        assert!(view.issues.is_empty());
    }

    #[test]
    fn accepted_tension_leaves_the_open_set() {
        let rules = vec![rule(1, "a"), rule(2, "b")];
        let tensions = vec![tension(1, 1, 2, "why", 0.9)];
        let ops = vec![
            assert_rule(1, 1000),
            assert_rule(2, 1001),
            op(
                GovernanceOpKind::AcceptTension {
                    tension: EdgeId::new(1),
                    rationale: "intentional".into(),
                    endpoints: None,
                },
                1002,
                "human:alex",
            ),
        ];
        let view = build_view(&rules, &tensions, &ops);
        assert_eq!(view.open_tensions().count(), 0);
        assert!(matches!(
            view.tensions[0].disposition,
            TensionDisposition::Accepted { .. }
        ));
    }

    #[test]
    fn resolved_tension_shows_resolved_disposition() {
        let rules = vec![rule(1, "a"), rule(2, "b")];
        let tensions = vec![tension(1, 1, 2, "why", 0.9)];
        let supersede = op(
            GovernanceOpKind::Supersede {
                new_rule: AtomId::claim(2),
                old_rules: vec![AtomId::claim(1)],
                rationale: String::new(),
            },
            1002,
            "human:alex",
        );
        let ops = vec![
            assert_rule(1, 1000),
            assert_rule(2, 1001),
            supersede.clone(),
            op(
                GovernanceOpKind::ResolveTension {
                    tension: EdgeId::new(1),
                    via: supersede.id.clone(),
                    endpoints: Some((AtomId::claim(1), AtomId::claim(2))),
                    rationale: String::new(),
                },
                1003,
                "human:alex",
            ),
        ];
        let view = build_view(&rules, &tensions, &ops);
        assert_eq!(view.open_tensions().count(), 0);
        assert!(matches!(
            view.tensions[0].disposition,
            TensionDisposition::Resolved { .. }
        ));
    }

    #[test]
    fn dismissed_tension_leaves_the_open_set() {
        let rules = vec![rule(1, "a"), rule(2, "b")];
        let tensions = vec![tension(1, 1, 2, "why", 0.9)];
        let ops = vec![
            assert_rule(1, 1000),
            assert_rule(2, 1001),
            op(
                GovernanceOpKind::DismissTension {
                    tension: EdgeId::new(1),
                    endpoints: Some((AtomId::claim(1), AtomId::claim(2))),
                    rationale: "detector noise".into(),
                },
                1002,
                "human:alex",
            ),
        ];
        let view = build_view(&rules, &tensions, &ops);
        assert_eq!(view.open_tensions().count(), 0);
        assert!(matches!(
            view.tensions[0].disposition,
            TensionDisposition::Dismissed { .. }
        ));
        assert!(view.issues.is_empty());
    }

    /// covers: EN-19
    ///
    /// The clause's first half: an adjudication recorded on the endpoint RULE PAIR
    /// survives a rebuild that re-mints every edge id.
    #[test]
    fn pair_matched_disposition_survives_rebuild_edge_ids() {
        // Week 1: accept the conflict between rules 1 and 2, adjudicating
        // edge-0001 and recording the endpoint pair. Week 2: the atlas is
        // rebuilt and the same conflict re-surfaces under a NEW edge id
        // (edge-0005). The decision must carry over via the pair map, and
        // no false "not surfaced" issue may fire.
        let rules = vec![rule(1, "a"), rule(2, "b")];
        let rebuilt_tensions = vec![tension(5, 1, 2, "why", 0.9)];
        let ops = vec![
            assert_rule(1, 1000),
            assert_rule(2, 1001),
            op(
                GovernanceOpKind::AcceptTension {
                    tension: EdgeId::new(1),
                    rationale: "both can stand".into(),
                    endpoints: Some((AtomId::claim(1), AtomId::claim(2))),
                },
                1002,
                "human:alex",
            ),
        ];
        let view = build_view(&rules, &rebuilt_tensions, &ops);
        assert_eq!(view.tensions[0].id, EdgeId::new(5));
        assert!(
            matches!(
                view.tensions[0].disposition,
                TensionDisposition::Accepted { .. }
            ),
            "the re-minted edge inherits the pair's accepted disposition"
        );
        assert_eq!(view.open_tensions().count(), 0);
        assert!(
            view.issues.is_empty(),
            "a pair re-detected under a new edge id is not drift"
        );
    }

    /// covers: EN-19
    ///
    /// The clause's mootness half: a conflict whose rule has been superseded is
    /// not open.
    #[test]
    fn tension_with_superseded_endpoint_is_moot_not_open() {
        // Rule 1 was superseded by rule 2. A tension the detector surfaces
        // between the dead rule 1 and a live rule 3 is not a live question —
        // it is moot, off the agenda, with no adjudication needed.
        let rules = vec![rule(1, "dead"), rule(2, "successor"), rule(3, "live")];
        let tensions = vec![tension(7, 1, 3, "stale overlap", 0.8)];
        let ops = vec![
            assert_rule(1, 1000),
            assert_rule(3, 1001),
            op(
                GovernanceOpKind::Supersede {
                    new_rule: AtomId::claim(2),
                    old_rules: vec![AtomId::claim(1)],
                    rationale: String::new(),
                },
                1002,
                "human:alex",
            ),
        ];
        let view = build_view(&rules, &tensions, &ops);
        assert_eq!(view.open_tensions().count(), 0);
        assert!(matches!(
            view.tensions[0].disposition,
            TensionDisposition::Moot { .. }
        ));
        if let TensionDisposition::Moot { dead_endpoint } = &view.tensions[0].disposition {
            assert_eq!(dead_endpoint, &AtomId::claim(1));
        }
    }

    /// covers: EN-19
    ///
    /// The clause's last half: ONLY a genuinely dangling decision — one whose rule
    /// text was edited away — surfaces as an issue. A pair awaiting re-detection
    /// is weekly variance.
    #[test]
    fn vanished_pair_is_not_an_issue_but_missing_endpoint_atom_is() {
        // (a) A pair decision whose edge the detector simply didn't
        //     re-surface this rebuild — both rule atoms still exist — is
        //     normal weekly variance, NOT an issue.
        let rules = vec![rule(1, "a"), rule(2, "b")];
        let ops = vec![
            assert_rule(1, 1000),
            assert_rule(2, 1001),
            op(
                GovernanceOpKind::AcceptTension {
                    tension: EdgeId::new(1),
                    rationale: "both can stand".into(),
                    endpoints: Some((AtomId::claim(1), AtomId::claim(2))),
                },
                1002,
                "human:alex",
            ),
        ];
        let view = build_view(&rules, &[], &ops);
        assert!(
            view.issues.is_empty(),
            "a valid pair awaiting re-detection is not drift"
        );

        // (b) A pair decision one of whose rule atoms has vanished (the
        //     rule text was edited into a new atom) IS drift needing
        //     attention.
        let ops_edited = vec![
            assert_rule(1, 1000),
            op(
                GovernanceOpKind::AcceptTension {
                    tension: EdgeId::new(1),
                    rationale: "both can stand".into(),
                    endpoints: Some((AtomId::claim(1), AtomId::claim(9))),
                },
                1002,
                "human:alex",
            ),
        ];
        let view_edited = build_view(&[rule(1, "a")], &[], &ops_edited);
        assert!(view_edited
            .issues
            .contains(&GovernanceIssue::AdjudicatedTensionNotSurfaced {
                tension: EdgeId::new(1)
            }));
    }

    #[test]
    fn governed_rule_without_atom_is_an_issue() {
        let ops = vec![assert_rule(5, 1000)];
        let view = build_view(&[], &[], &ops);
        assert!(view.issues.contains(&GovernanceIssue::RuleHasNoAtom {
            rule: AtomId::claim(5)
        }));
        // Still listed (with empty text) so it's visible, not vanished.
        assert_eq!(view.rules.len(), 1);
        assert_eq!(view.rules[0].text, "");
    }

    #[test]
    fn tension_endpoint_missing_is_an_issue() {
        let rules = vec![rule(1, "a")];
        let tensions = vec![tension(1, 1, 7, "x", 0.5)];
        let ops = vec![assert_rule(1, 1000)];
        let view = build_view(&rules, &tensions, &ops);
        assert!(view
            .issues
            .contains(&GovernanceIssue::TensionEndpointMissing {
                tension: EdgeId::new(1),
                endpoint: AtomId::claim(7),
            }));
        assert_eq!(view.tensions[0].text_b, "");
    }

    #[test]
    fn adjudication_without_surfaced_edge_is_an_issue() {
        // Case (a): a legacy edge-id-only decision whose edge no longer
        // surfaces is genuine drift — nothing to re-match against.
        let ops = vec![op(
            GovernanceOpKind::AcceptTension {
                tension: EdgeId::new(9),
                rationale: "x".into(),
                endpoints: None,
            },
            1000,
            "human:alex",
        )];
        let view = build_view(&[], &[], &ops);
        assert!(view
            .issues
            .contains(&GovernanceIssue::AdjudicatedTensionNotSurfaced {
                tension: EdgeId::new(9)
            }));
    }

    #[test]
    fn unattended_adjudication_is_an_issue() {
        let forged = op(
            GovernanceOpKind::Supersede {
                new_rule: AtomId::claim(2),
                old_rules: vec![AtomId::claim(1)],
                rationale: String::new(),
            },
            1000,
            "ingest", // not human:
        );
        let view = build_view(&[], &[], &[forged.clone()]);
        assert!(view.issues.contains(&GovernanceIssue::UnattendedAct {
            op: forged.id.clone()
        }));
    }

    #[test]
    fn from_atlas_dir_reads_and_joins_real_atlas_files() {
        use crate::enrichment::atlas::edges::{EdgeProvenance, EdgesFile};
        use crate::enrichment::pipeline::atlas::{
            ClaimScope, DiscourseAct, EnrichmentDepth, EpistemicStatus,
        };

        let dir = tempfile::tempdir().unwrap();

        // Two rule Claims → atoms.json.
        let make_claim = |n: usize, content: &str| Claim {
            attributes: Default::default(),
            subject: None,
            id: AtomId::claim(n),
            content: content.into(),
            discourse_act: DiscourseAct::Enact,
            epistemic_status: EpistemicStatus::Confident,
            scope: ClaimScope::Contextual,
            evidence: vec![ChunkRef::new(format!("chunk-{n}"), None)],
            quotable_excerpt: None,
            attributed_to: Some(AtomId::entity(1)),
            confidence: None,
            anchor: None,
            claim_kind: Some("forbids".into()),
            concession_outcome: None,
            evidence_kind: None,
            enrichment_depth: EnrichmentDepth::Extracted,
        };
        let atoms = AtomsFile::new(vec![
            AtomEnvelope::Claim(make_claim(1, "old rule")),
            AtomEnvelope::Claim(make_claim(2, "new rule")),
        ]);
        std::fs::write(
            dir.path().join("atoms.json"),
            serde_json::to_vec(&atoms).unwrap(),
        )
        .unwrap();

        // One Tension edge between them → edges.json.
        let edges = EdgesFile::new(vec![Edge {
            id: EdgeId::new(1),
            edge_type: EdgeType::Tension,
            source: AtomId::claim(1),
            target: AtomId::claim(2),
            evidence: Vec::new(),
            trigger_event: None,
            sub_question: Some("why?".into()),
            confidence: 0.8,
            provenance: EdgeProvenance::Derived,
        }]);
        std::fs::write(
            dir.path().join("edges.json"),
            serde_json::to_vec(&edges).unwrap(),
        )
        .unwrap();

        // Both rules asserted by ingest → governance_oplog.jsonl.
        let log = Oplog::<GovernanceOpKind>::new(dir.path());
        log.append(&assert_rule(1, 1000)).unwrap();
        log.append(&assert_rule(2, 1001)).unwrap();

        let view = GovernanceView::from_atlas_dir(dir.path()).unwrap();
        assert_eq!(view.active_rules().count(), 2);
        let open: Vec<_> = view.open_tensions().collect();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].text_a, "old rule");
        assert_eq!(open[0].text_b, "new rule");
        assert_eq!(open[0].why.as_deref(), Some("why?"));
        assert!(view.issues.is_empty());
    }

    /// ontology-v1 P5. The projection reads what the DECLARED claim carries:
    /// `subject` as the scope (the coin being dated, not the scholar dating
    /// it) and the validated `deontic` attribute as the force.
    #[test]
    fn project_claim_prefers_subject_and_the_declared_deontic() {
        use crate::enrichment::pipeline::atlas::{
            ClaimScope, DiscourseAct, EnrichmentDepth, EpistemicStatus,
        };

        let base =
            |subject: Option<AtomId>, attrs: serde_json::Map<String, serde_json::Value>| Claim {
                attributes: attrs,
                subject,
                id: AtomId::claim(1),
                content: "the mancus was struck 805/810".into(),
                discourse_act: DiscourseAct::Assert,
                epistemic_status: EpistemicStatus::Confident,
                scope: ClaimScope::Contextual,
                evidence: vec![ChunkRef::new("chunk-1", None)],
                quotable_excerpt: None,
                // The VOICE: the scholar making the attribution.
                attributed_to: Some(AtomId::entity(7)),
                confidence: None,
                anchor: None,
                claim_kind: Some("attribution".into()),
                concession_outcome: None,
                evidence_kind: None,
                enrichment_depth: EnrichmentDepth::Extracted,
            };

        // Declared: subject wins over attributed_to, deontic off the attribute.
        let mut attrs = serde_json::Map::new();
        attrs.insert("deontic".into(), serde_json::Value::String("forbid".into()));
        let declared = project_claim(&base(Some(AtomId::entity(42)), attrs));
        assert_eq!(declared.scope, Some(AtomId::entity(42)));
        assert_ne!(declared.scope, Some(AtomId::entity(7)));
        assert_eq!(declared.deontic.as_deref(), Some("forbid"));

        // I5: no subject and no deontic attribute — an undeclared corpus's
        // claim projects exactly as it did before ontology v1.
        let undeclared = project_claim(&base(None, serde_json::Map::new()));
        assert_eq!(undeclared.scope, Some(AtomId::entity(7)));
        assert_eq!(undeclared.deontic.as_deref(), Some("attribution"));

        // A blank or non-string reserved value is an absence, not a value:
        // it falls back rather than projecting an empty deontic.
        let mut blank = serde_json::Map::new();
        blank.insert("deontic".into(), serde_json::Value::String("  ".into()));
        assert_eq!(
            project_claim(&base(None, blank)).deontic.as_deref(),
            Some("attribution")
        );
        let mut wrong_type = serde_json::Map::new();
        wrong_type.insert("deontic".into(), serde_json::json!(3));
        assert_eq!(
            project_claim(&base(None, wrong_type)).deontic.as_deref(),
            Some("attribution")
        );
    }

    #[test]
    fn empty_atlas_dir_yields_empty_view() {
        let dir = tempfile::tempdir().unwrap();
        let view = GovernanceView::from_atlas_dir(dir.path()).unwrap();
        assert_eq!(view, GovernanceView::default());
    }

    fn write_manifest(dir: &std::path::Path, body: &str) {
        std::fs::write(dir.join("chapters.json"), body).unwrap();
    }

    /// The distinction the whole surface exists for: an empty map because the
    /// corpus has no sections, vs an empty map because its join was never
    /// written. Conflating these is what hid a broken join across 1779 of
    /// 1788 corpora.
    /// covers: ST-25
    ///
    /// Value 1 of the trichotomy: no structure declared.
    #[test]
    fn an_absent_manifest_is_no_structure_not_a_missing_join() {
        let dir = tempfile::tempdir().unwrap();
        let got = chunk_to_section_map_status(dir.path());
        assert_eq!(got.status, JoinStatus::NoSectionStructure);
        assert!(got.map.is_empty());
        assert!(
            got.warning("x").is_none(),
            "a structureless corpus must not nag"
        );
    }

    /// covers: ST-25
    ///
    /// Value 2: structure declared but join missing. Conflating this with value 1
    /// is what lets a broken corpus read as a flat one.
    #[test]
    fn sections_with_no_chunk_ids_are_a_missing_join_and_say_so() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(
            dir.path(),
            r#"{"corpus_id":"c","schema_version":1,"chapters":[
                 {"id":"sec_0001","title":"CHAPTER I","first_line":"","word_count":1,"chunk_ids":[]},
                 {"id":"sec_0002","title":"CHAPTER II","first_line":"","word_count":1,"chunk_ids":[]}]}"#,
        );
        let got = chunk_to_section_map_status(dir.path());
        assert_eq!(got.status, JoinStatus::JoinMissing);
        assert_eq!(got.sections_total, 2);
        assert_eq!(got.sections_with_chunks, 0);
        let w = got
            .warning("chaos-saltgrass")
            .expect("a fault must be reportable");
        assert!(
            w.contains("backfill-sections chaos-saltgrass"),
            "must name the repair: {w}"
        );
    }

    /// covers: ST-25
    ///
    /// Value 3: join present.
    #[test]
    fn a_populated_join_is_present_and_silent() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(
            dir.path(),
            r#"{"corpus_id":"c","schema_version":1,"chapters":[
                 {"id":"sec_0001","title":"I","first_line":"","word_count":1,"chunk_ids":[1,2]},
                 {"id":"sec_0002","title":"II","first_line":"","word_count":1,"chunk_ids":[]}]}"#,
        );
        let got = chunk_to_section_map_status(dir.path());
        assert_eq!(got.status, JoinStatus::Present);
        assert_eq!(
            got.sections_with_chunks, 1,
            "a partial join is still present"
        );
        assert_eq!(got.map.get(&2).map(String::as_str), Some("sec_0001"));
        assert!(got.warning("x").is_none());
    }

    /// A manifest declaring zero sections has nothing that COULD be joined.
    /// covers: ST-25
    ///
    /// Value 1 again, by the other door — an empty chapter list rather than an
    /// absent manifest. Both must land on `NoSectionStructure`.
    #[test]
    fn an_empty_chapter_list_is_no_structure_not_a_fault() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(
            dir.path(),
            r#"{"corpus_id":"c","schema_version":1,"chapters":[]}"#,
        );
        assert_eq!(
            chunk_to_section_map_status(dir.path()).status,
            JoinStatus::NoSectionStructure
        );
    }

    /// The legacy wrapper keeps its "empty means don't filter" contract.
    #[test]
    fn the_map_only_accessor_still_degrades_to_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert!(chunk_to_section_map(dir.path()).is_empty());
    }
}
