//! The multi-origin merge primitive.
//!
//! Given a `Vec<Entity>` whose atoms carry [`Provenance`] (AD-4),
//! produce [`ReconciledEntity`]s — same canonical id across multiple
//! surface-form mentions, with the merge signals that fired recorded
//! on each one.
//!
//! The merger is **non-destructive**: every merge writes an
//! [`oplog::OplogEntry`] capturing inputs / output / signals so the
//! operator can replay or reverse it via `split`.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::oplog::OplogEntry;
use super::signals::{
    collect_emails, default_signals, fold_name, strip_org_suffixes, MergeSignal,
    MergeSignalCheck,
};
use crate::enrichment::atlas::atoms::{AtomId, Entity, Provenance};
use crate::enrichment::pipeline::atlas::EntityType;

/// Policy knobs for the merger. Mirrors the
/// `[enrichment.reconciliation]` TOML schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReconciliationPolicy {
    /// Minimum overlap-similarity for a name match to count as a
    /// candidate. Today the
    /// [`super::signals::NameSimilaritySignal`] uses an exact
    /// fold-match; future versions may swap a real similarity
    /// function here.
    #[serde(default = "default_name_similarity_threshold")]
    pub name_similarity_threshold: f32,
    /// Minimum *distinct* signals required for a cross-origin merge.
    /// Two same-origin mentions (both LLM batch, both column header)
    /// can merge on `name_similarity` alone; two cross-origin
    /// mentions need a second signal (`email_header`, `org_role`, or
    /// `judge_confirmed`).
    ///
    /// Default 2 — Phase 5 tunes against the train split. Set to 1
    /// to recover the legacy single-signal behaviour.
    #[serde(default = "default_cross_origin_required_signals")]
    pub cross_origin_required_signals: u8,
    /// When `true`, the policy escalates uncertain candidates to the
    /// calibrated judge (`corpus-engine/assets/judges/business_entity_v1/`).
    /// The judge is owned by the runner — this primitive captures the
    /// outcome via `judge_callback`.
    #[serde(default = "default_true")]
    pub judge_when_uncertain: bool,
    /// Trial count fed into the judge harness when escalation fires.
    /// Matches `sovereign_agent_bench::judge_multi::run_judge_trials`'s
    /// `trials` parameter.
    #[serde(default = "default_judge_trials")]
    pub judge_trials: u8,
}

impl Default for ReconciliationPolicy {
    fn default() -> Self {
        Self {
            name_similarity_threshold: default_name_similarity_threshold(),
            cross_origin_required_signals: default_cross_origin_required_signals(),
            judge_when_uncertain: default_true(),
            judge_trials: default_judge_trials(),
        }
    }
}

fn default_name_similarity_threshold() -> f32 {
    0.85
}
fn default_cross_origin_required_signals() -> u8 {
    2
}
fn default_judge_trials() -> u8 {
    3
}
fn default_true() -> bool {
    true
}

/// Output record per canonical entity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReconciledEntity {
    pub canonical_id: AtomId,
    pub canonical_name: String,
    /// Every (surface_form, provenance) the merger collapsed under
    /// `canonical_id`. Surface forms intentionally hold the verbatim
    /// canonical_name from the input atom — the recipe-author can
    /// inspect them for atypical surface variants.
    pub surface_forms: Vec<(String, Provenance)>,
    pub signals_fired: Vec<MergeSignal>,
    /// The atom ids the merger collapsed; the oplog already carries
    /// these but we surface them here so a runtime read of the
    /// reconciled atlas doesn't require also opening the oplog.
    pub source_atom_ids: Vec<AtomId>,
}

/// Outcome bundle the runner consumes.
#[derive(Debug, Clone, Default)]
pub struct ReconciliationOutcome {
    pub entities: Vec<ReconciledEntity>,
    /// One [`OplogEntry`] per merge the primitive performed. The
    /// caller persists these into
    /// `<corpus_index>/atlas/reconciliation_oplog.jsonl` so the
    /// audit trail survives the process exiting.
    pub oplog_entries: Vec<OplogEntry>,
}

/// Run the merger over `entities` with `policy`. The signal stack is
/// the default ([`super::signals::default_signals`]) when the caller
/// doesn't custom-build one. Equality is **transitive** in the
/// reconciler — a -> b -> c chain collapses into a single canonical
/// id even if a and c have no direct pairwise signal.
pub fn reconcile(entities: Vec<Entity>, policy: &ReconciliationPolicy) -> ReconciliationOutcome {
    let signals = default_signals();
    reconcile_with_signals(entities, policy, &signals)
}

/// Same as [`reconcile`] but with a caller-supplied signal stack —
/// lets tests + Phase 5 tuning swap in extra signals
/// (`ThreadRootSignal`, judge-confirmation post-hook).
pub fn reconcile_with_signals(
    entities: Vec<Entity>,
    policy: &ReconciliationPolicy,
    signals: &[Box<dyn MergeSignalCheck>],
) -> ReconciliationOutcome {
    let n = entities.len();
    if n == 0 {
        return ReconciliationOutcome::default();
    }
    // Union-find over indices.
    let mut parent: Vec<usize> = (0..n).collect();
    let mut signal_log: HashMap<(usize, usize), Vec<MergeSignal>> = HashMap::new();

    // Candidate-blocked pairwise scan. The naive form is O(n²) — fine
    // for one mailbox, but enron-sample-multi-wide (18,833 atoms) is
    // 177M pairs and minutes of wall-clock. `candidate_pairs` returns a
    // *superset* of every pair on which a signal could fire (every
    // firing pair shares at least one blocking key — see its doc), so
    // iterating it instead of all i<j is behaviour-identical while
    // cutting the scan to ~274K pairs (~650×). The full signal check +
    // cross-origin gate below still decides each candidate; blocking
    // only skips pairs that provably cannot fire. Pairs are sorted, so
    // iteration order (and thus the oplog) matches the naive scan.
    for (i, j) in candidate_pairs(&entities) {
        let mut fired: Vec<MergeSignal> = Vec::new();
        for signal in signals {
            if signal.check(&entities[i], &entities[j]) {
                fired.push(signal.signal());
            }
        }
        if fired.is_empty() {
            continue;
        }
        let cross_origin =
            entities[i].provenance.signal_kind != entities[j].provenance.signal_kind;
        let needed = if cross_origin {
            policy.cross_origin_required_signals as usize
        } else {
            1
        };
        if fired.len() < needed {
            tracing::debug!(
                left = %entities[i].canonical_name,
                right = %entities[j].canonical_name,
                cross_origin,
                fired_count = fired.len(),
                needed,
                "reconciliation: candidate rejected by signal-count gate"
            );
            continue;
        }
        tracing::debug!(
            left = %entities[i].canonical_name,
            right = %entities[j].canonical_name,
            cross_origin,
            signals = ?fired.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
            "reconciliation: merging candidate pair"
        );
        signal_log.insert((i, j), fired);
        union(&mut parent, i, j);
    }

    // Group indices by their root.
    let mut clusters: HashMap<usize, Vec<usize>> = HashMap::new();
    for i in 0..n {
        clusters.entry(find(&mut parent, i)).or_default().push(i);
    }

    let mut out_entities = Vec::with_capacity(clusters.len());
    let mut oplog_entries = Vec::new();

    for (root, members) in clusters {
        let canonical_idx = pick_canonical(&entities, &members);
        let canonical_id = entities[canonical_idx].id.clone();
        let canonical_name = entities[canonical_idx].canonical_name.clone();
        let mut surface_forms: Vec<(String, Provenance)> =
            Vec::with_capacity(members.len());
        let mut signals_fired: Vec<MergeSignal> = Vec::new();
        let mut source_atom_ids: Vec<AtomId> = Vec::with_capacity(members.len());
        for &m in &members {
            surface_forms.push((
                entities[m].canonical_name.clone(),
                entities[m].provenance.clone(),
            ));
            source_atom_ids.push(entities[m].id.clone());
        }
        // Collect signals from the merge pairs participating in this
        // cluster.
        for &m in &members {
            for &other in &members {
                if m < other {
                    if let Some(s) = signal_log.get(&(m, other)) {
                        for sig in s {
                            if !signals_fired.contains(sig) {
                                signals_fired.push(sig.clone());
                            }
                        }
                    }
                }
            }
        }

        if members.len() > 1 {
            oplog_entries.push(OplogEntry::merge(
                source_atom_ids.clone(),
                canonical_id.clone(),
                signals_fired.clone(),
                None,
                format!(
                    "merged {} mentions of {} via {} signal(s)",
                    members.len(),
                    canonical_name,
                    signals_fired.len()
                ),
            ));
        }

        let _ = root;
        out_entities.push(ReconciledEntity {
            canonical_id,
            canonical_name,
            surface_forms,
            signals_fired,
            source_atom_ids,
        });
    }

    out_entities.sort_by(|a, b| a.canonical_name.cmp(&b.canonical_name));
    ReconciliationOutcome {
        entities: out_entities,
        oplog_entries,
    }
}

/// Reverse a prior merge by splitting `canonical_id` into the original
/// atoms. Returns the [`OplogEntry`] the caller must persist.
pub fn split_atom(
    canonical_id: AtomId,
    into: Vec<AtomId>,
    rationale: impl Into<String>,
) -> OplogEntry {
    OplogEntry::split(canonical_id, into, rationale)
}

// ── Union-find helpers ───────────────────────────────────────

fn find(parent: &mut Vec<usize>, mut x: usize) -> usize {
    while parent[x] != x {
        parent[x] = parent[parent[x]];
        x = parent[x];
    }
    x
}

fn union(parent: &mut Vec<usize>, a: usize, b: usize) {
    let ra = find(parent, a);
    let rb = find(parent, b);
    if ra != rb {
        parent[ra] = rb;
    }
}

/// Surname blocking keys for an already-folded name. The merge signals
/// that key on a surname are `NameSimilarity::initial_surname` (surname
/// = the final token) and `nickname_prefix` (surname = the final token
/// of length > 1, single-char middle initials filtered). Returning both
/// covers both. Email-shaped names (`@`) yield none — their final token
/// is a domain TLD ("com"), never a surname, and they block via the
/// `c:`/`e:` keys instead; skipping them drops the single largest
/// spurious bucket on real corpora.
fn surname_keys(folded: &str) -> Vec<String> {
    if folded.contains('@') {
        return Vec::new();
    }
    let toks: Vec<&str> = folded.split_whitespace().collect();
    let mut keys: Vec<String> = Vec::new();
    if let Some(last) = toks.last() {
        keys.push((*last).to_string());
    }
    if let Some(last_big) = toks.iter().rev().find(|t| t.chars().count() > 1) {
        let s = (*last_big).to_string();
        if !keys.contains(&s) {
            keys.push(s);
        }
    }
    keys
}

/// Generate a superset of every `(i, j)` pair (i < j) on which a
/// [`MergeSignalCheck`] from the default stack could fire, by bucketing
/// entities under shared blocking keys and emitting all within-bucket
/// pairs. Every firing pair shares at least one key, so none is lost;
/// the caller still runs the full signal check on each candidate. This
/// is the blocking the naive O(n²) scan deferred — it turns the
/// full-corpus reconcile from minutes into sub-second.
///
/// One key per firing path:
/// - `c:<fold(canonical)>` — `NameSimilarity` exact fold-match
/// - `s:<surname>`         — `NameSimilarity` initial-surname / nickname
///   and `EmailHeader` email↔name. Sourced from the folded canonical,
///   each folded alias, and each email local-part's last dotted token
///   (so an address blocks with the name it encodes).
/// - `e:<email>`           — `EmailHeader` shared-address and the
///   (email-restricted) `NameSimilarity` alias overlap
/// - `o:<aff>|<role>`      — `OrgRole` (both non-empty)
///
/// The returned vector is sorted so iteration order — and the resulting
/// oplog — is deterministic and matches the old ascending i<j scan.
fn candidate_pairs(entities: &[Entity]) -> Vec<(usize, usize)> {
    use std::collections::{HashMap, HashSet};
    let mut buckets: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, e) in entities.iter().enumerate() {
        let fc = fold_name(&e.canonical_name);
        if !fc.is_empty() {
            buckets.entry(format!("c:{fc}")).or_default().push(i);
            for k in surname_keys(&fc) {
                buckets.entry(format!("s:{k}")).or_default().push(i);
            }
            // Org-suffix-normalized key so corporate-form variants
            // ("El Paso" / "El Paso Corp." / "El Paso Corporation")
            // become candidates — they share no canon-fold or surname
            // key otherwise. Matches the NameSimilarity org branch.
            if e.entity_type == EntityType::Institution {
                let on = strip_org_suffixes(&fc);
                if on.chars().count() >= 3 {
                    buckets.entry(format!("o:{on}")).or_default().push(i);
                }
            }
        }
        for a in &e.aliases {
            for k in surname_keys(&fold_name(a)) {
                buckets.entry(format!("s:{k}")).or_default().push(i);
            }
        }
        for em in collect_emails(e) {
            buckets.entry(format!("e:{em}")).or_default().push(i);
            if let Some(local) = em.split('@').next() {
                if let Some(last) = local
                    .split(|c| c == '.' || c == '_' || c == '-')
                    .filter(|t| !t.is_empty())
                    .last()
                {
                    buckets.entry(format!("s:{last}")).or_default().push(i);
                }
            }
        }
        if let (Some(aff), Some(role)) = (&e.affiliation, &e.role) {
            if !aff.is_empty() && !role.is_empty() {
                buckets
                    .entry(format!("o:{}|{}", fold_name(aff), fold_name(role)))
                    .or_default()
                    .push(i);
            }
        }
    }
    let mut pairs: HashSet<(usize, usize)> = HashSet::new();
    for members in buckets.values() {
        for a in 0..members.len() {
            for b in (a + 1)..members.len() {
                let (x, y) = (members[a], members[b]);
                pairs.insert(if x < y { (x, y) } else { (y, x) });
            }
        }
    }
    let mut out: Vec<(usize, usize)> = pairs.into_iter().collect();
    out.sort_unstable();
    out
}

fn pick_canonical(entities: &[Entity], members: &[usize]) -> usize {
    // Prefer the longest canonical_name (typically the most
    // explicit form) — ties broken by lowest source index for
    // determinism.
    let mut best = members[0];
    for &m in members.iter().skip(1) {
        let cur_len = entities[m].canonical_name.chars().count();
        let best_len = entities[best].canonical_name.chars().count();
        if cur_len > best_len || (cur_len == best_len && m < best) {
            best = m;
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enrichment::atlas::atoms::{ChunkRef, SignalKind};
    use crate::enrichment::pipeline::atlas::{EnrichmentDepth, EntityType};

    fn ent(name: &str, id: &str, sk: SignalKind, doc: &str) -> Entity {
        Entity {
            id: AtomId::from_raw(id),
            canonical_name: name.into(),
            aliases: Vec::new(),
            entity_type: EntityType::Person,
            first_appearance: ChunkRef::new("sec-001", None),
            description: String::new(),
            defining_quote: None,
            salience: 0.5,
            enrichment_depth: EnrichmentDepth::Extracted,
            affiliation: None,
            role: None,
            participants: Vec::new(),
            provenance: Provenance::new("ext", doc, sk),
            concept_kind: None,
        }
    }

    #[test]
    fn three_lay_surface_forms_collapse_to_one() {
        let entities = vec![
            ent("Ken Lay", "entity-001", SignalKind::EmailHeader, "msg-1"),
            ent("Kenneth L. Lay", "entity-002", SignalKind::EmailHeader, "msg-1"),
            ent("Kenneth Lay", "entity-003", SignalKind::LlmBatch, "msg-2"),
        ];
        // Same-origin merge needs 1 signal; cross-origin needs 2 by
        // default. Make the third a cross-origin candidate by setting
        // signal_kind = LlmBatch; with default policy it requires 2
        // signals → won't merge with names alone. Drop required
        // signals to 1 to test transitivity in a controlled way.
        let mut policy = ReconciliationPolicy::default();
        policy.cross_origin_required_signals = 1;
        let outcome = reconcile(entities, &policy);
        assert_eq!(outcome.entities.len(), 1);
        let lay = &outcome.entities[0];
        assert_eq!(lay.surface_forms.len(), 3);
        // Canonical name picked as the longest form.
        assert_eq!(lay.canonical_name, "Kenneth L. Lay");
        // Oplog has one merge entry.
        assert_eq!(outcome.oplog_entries.len(), 1);
    }

    #[test]
    fn cross_origin_signal_gate_blocks_weak_merge() {
        let entities = vec![
            ent("Ken Lay", "entity-001", SignalKind::EmailHeader, "msg-1"),
            ent("Kenneth Lay", "entity-002", SignalKind::LlmBatch, "msg-2"),
        ];
        // Default policy requires 2 cross-origin signals; only
        // name_similarity fires → keep separate.
        let outcome = reconcile(entities, &ReconciliationPolicy::default());
        assert_eq!(outcome.entities.len(), 2);
    }

    #[test]
    fn split_reverses_a_merge() {
        let entry = split_atom(
            AtomId::from_raw("entity-fused"),
            vec![
                AtomId::from_raw("entity-a"),
                AtomId::from_raw("entity-b"),
            ],
            "operator reversed",
        );
        assert!(matches!(
            entry.op,
            super::super::oplog::OpKind::Split
        ));
        assert_eq!(entry.split_outputs.len(), 2);
    }

    #[test]
    fn singletons_pass_through_unchanged() {
        let entities = vec![ent(
            "Lone Wolf",
            "entity-001",
            SignalKind::LlmBatch,
            "msg-1",
        )];
        let outcome = reconcile(entities, &ReconciliationPolicy::default());
        assert_eq!(outcome.entities.len(), 1);
        assert!(outcome.oplog_entries.is_empty());
    }

    #[test]
    fn empty_input_is_safe() {
        let outcome = reconcile(Vec::new(), &ReconciliationPolicy::default());
        assert!(outcome.entities.is_empty());
        assert!(outcome.oplog_entries.is_empty());
    }

    #[test]
    fn candidate_pairs_is_superset_of_every_firing_pair() {
        // The blocking invariant: `candidate_pairs` may emit pairs that
        // do not fire, but must never DROP one that does. If it ever
        // misses a firing pair, recall regresses silently — this is the
        // guard. Fixture exercises every signal path (exact fold,
        // nickname-prefix, initial-surname, email-alias overlap,
        // email↔name, OrgRole).
        let signals = default_signals();
        let mut entities = vec![
            ent("Ken Lay", "e1", SignalKind::LlmBatch, "m1"),
            ent("Kenneth Lay", "e2", SignalKind::LlmBatch, "m1"),
            ent("K. Lay", "e3", SignalKind::LlmBatch, "m1"),
            ent("Jeff Skilling", "e4", SignalKind::LlmBatch, "m1"),
            ent("J. Skilling", "e5", SignalKind::LlmBatch, "m1"),
        ];
        entities[0].aliases.push("klay@enron.com".into());
        entities[1].aliases.push("kenneth.lay@enron.com".into());
        let mut a = ent("Dynegy", "e6", SignalKind::ColumnHeader, "m2");
        a.entity_type = EntityType::Institution;
        a.affiliation = Some("Energy".into());
        a.role = Some("Counterparty".into());
        let mut b = ent("Dynegy Inc.", "e7", SignalKind::ColumnHeader, "m2");
        b.entity_type = EntityType::Institution;
        b.affiliation = Some("Energy".into());
        b.role = Some("Counterparty".into());
        entities.push(a);
        entities.push(b);

        let cands: std::collections::HashSet<(usize, usize)> =
            candidate_pairs(&entities).into_iter().collect();
        let n = entities.len();
        let mut fired_any = false;
        for i in 0..n {
            for j in (i + 1)..n {
                if signals.iter().any(|s| s.check(&entities[i], &entities[j])) {
                    fired_any = true;
                    assert!(
                        cands.contains(&(i, j)),
                        "blocking dropped firing pair ({i},{j}): {} / {}",
                        entities[i].canonical_name,
                        entities[j].canonical_name
                    );
                }
            }
        }
        assert!(fired_any, "fixture should produce at least one firing pair");
    }
}
