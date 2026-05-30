//! Per-pair merge signals for [`super::multi_origin`].
//!
//! Each signal answers: "does this evidence support that `left` and
//! `right` refer to the same real-world entity?" Pure functions over
//! the typed [`Entity`] pair, returning a [`MergeSignal`] tag the
//! reconciliation policy folds into its decision.
//!
//! Adding a signal: implement [`MergeSignalCheck`], slot it into
//! [`default_signals`]'s ordered list, name it via the
//! [`MergeSignal`] enum's `Other(String)` variant if it doesn't yet
//! map to a known kind, and call out the new tag in the docstring
//! below so audit-log readers can interpret the new symbol.

use serde::{Deserialize, Serialize};

use crate::enrichment::atlas::atoms::Entity;

/// Tag for a signal that fired on a candidate pair. Round-trips
/// through the oplog so an auditor can reconstruct exactly which
/// signals supported a merge.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeSignal {
    /// Canonical names + aliases agree under a fold (lowercase + ASCII
    /// punctuation collapse).
    NameSimilarity,
    /// One of the surface forms is an email address that resolves to
    /// the other party's company / domain.
    EmailHeader,
    /// Person + organisation + role all triangulate (Ken Lay @ Enron
    /// CEO ↔ Kenneth Lay @ Enron CEO).
    OrgRole,
    /// The calibrated judge confirmed the merge after the
    /// reconciliation policy escalated.
    JudgeConfirmed,
    /// Same email-thread root in the carrier doc's metadata (Phase 2
    /// thread_id matches → the two mentions are in the same
    /// conversation).
    ThreadRoot,
    Other(String),
}

impl MergeSignal {
    pub fn as_str(&self) -> &str {
        match self {
            MergeSignal::NameSimilarity => "name_similarity",
            MergeSignal::EmailHeader => "email_header",
            MergeSignal::OrgRole => "org_role",
            MergeSignal::JudgeConfirmed => "judge_confirmed",
            MergeSignal::ThreadRoot => "thread_root",
            MergeSignal::Other(s) => s.as_str(),
        }
    }
}

/// Trait every signal implements.
pub trait MergeSignalCheck: Send + Sync {
    fn check(&self, left: &Entity, right: &Entity) -> bool;
    fn signal(&self) -> MergeSignal;
}

/// Name similarity — exact fold-match on canonical_name OR alias-set
/// intersection. The simplest signal; usually the necessary
/// precondition that other signals refine.
pub struct NameSimilaritySignal {
    /// Minimum length the folded name must reach before this signal
    /// fires. Guards against `"J" == "j"` matches.
    pub min_chars: usize,
}

impl Default for NameSimilaritySignal {
    fn default() -> Self {
        Self { min_chars: 3 }
    }
}

impl MergeSignalCheck for NameSimilaritySignal {
    fn check(&self, left: &Entity, right: &Entity) -> bool {
        if left.entity_type != right.entity_type {
            return false;
        }
        let lf = fold_name(&left.canonical_name);
        let rf = fold_name(&right.canonical_name);
        if lf.len() < self.min_chars || rf.len() < self.min_chars {
            return false;
        }
        if lf == rf {
            return true;
        }
        // Initial + surname fold: "J. Skilling" → "j skilling"; we
        // also try to match against "jeff skilling" by treating
        // single-letter tokens at the start as initials.
        if initial_surname_matches(&lf, &rf) || initial_surname_matches(&rf, &lf) {
            return true;
        }
        // Common-prefix nickname: "Ken Lay" ↔ "Kenneth Lay" — same
        // surname, one first-name is a 3+ char prefix of the other.
        if shared_surname_with_prefix_first_name(&lf, &rf) {
            return true;
        }
        // Alias overlap — restricted to identity-grade (email)
        // aliases. **Load-bearing restriction.** business_email
        // Phase 1b stuffs bare given-name aliases ("John", "Ken",
        // "Mr. Lay") into entity alias lists; an unrestricted
        // intersection fired this clause for any two people who merely
        // shared a first name. With same-origin `needed = 1`, that
        // chained 362 distinct people — Lay, Skilling, Fastow and
        // ~360 others — into one transitively-merged cluster on
        // enron-sample-multi-wide (2026-05-30), cratering B³ precision
        // (the gold execs collapsed into a single mega-cluster). An
        // email address is a unique identifier; a bare given name is
        // not. Email-sharing pairs still fire here (in addition to
        // EmailHeaderSignal), so a cross-origin email match keeps its
        // two-signal vote under `cross_origin_required_signals`.
        let l_aliases: std::collections::BTreeSet<String> = left
            .aliases
            .iter()
            .filter(|s| s.contains('@'))
            .map(|s| fold_name(s))
            .collect();
        let r_aliases: std::collections::BTreeSet<String> = right
            .aliases
            .iter()
            .filter(|s| s.contains('@'))
            .map(|s| fold_name(s))
            .collect();
        !l_aliases.is_disjoint(&r_aliases)
    }

    fn signal(&self) -> MergeSignal {
        MergeSignal::NameSimilarity
    }
}

/// Email-header signal: one entity's canonical name OR an alias is an
/// email address, and the local part / domain align with the other
/// entity's affiliation token.
pub struct EmailHeaderSignal;

impl MergeSignalCheck for EmailHeaderSignal {
    fn check(&self, left: &Entity, right: &Entity) -> bool {
        // Shared *exact* email address — an identity-grade signal.
        //
        // We deliberately do NOT infer identity from email-local-part ↔
        // name string matching (the former `email_matches_name` path).
        // business_email Phase 1b conflates multiple correspondents'
        // addresses into a single atom's alias bag — one "Kenneth Lay"
        // atom on enron-sample-multi-wide carried `chairman.ken@`,
        // `.fred@enron.com`, AND a third party's `judys.knepshield@` —
        // so a surname/initial match between a stray local-part and any
        // same-surnamed person chained Lay, Skilling, Fastow and every
        // org into one 2,013-atom cluster (train B³ precision 0.26). An
        // exact shared address is unique; a fuzzy local-part match is
        // not robust on noisy extraction. 2026-05-30 sweep: dropping the
        // fuzzy path moved train B³ from 0.26/0.41 to 1.00/0.80 with
        // zero gold over-merge. If a future, cleaner corpus wants
        // name↔email inference back, reintroduce it behind a per-policy
        // flag — never as an unconditional default.
        let lemails = collect_emails(left);
        if lemails.is_empty() {
            return false;
        }
        let remails = collect_emails(right);
        !lemails.is_disjoint(&remails)
    }

    fn signal(&self) -> MergeSignal {
        MergeSignal::EmailHeader
    }
}

/// Org + role signal: same affiliation AND same role string.
pub struct OrgRoleSignal;

impl MergeSignalCheck for OrgRoleSignal {
    fn check(&self, left: &Entity, right: &Entity) -> bool {
        // Empty strings count as missing — emitting Some("") instead
        // of None is what business_email's Phase 1b output does when
        // the model leaves an "omit" field as a literal "". Without
        // the empty-string guard, every entity at the same employer
        // with no role specified ("Enron Corp" + "") collapses into
        // one transitively-merged cluster — observed 2026-05-29 on
        // enron-sample-multi-tiny train: 86 Enron employees including
        // Lay, Skilling, and Fastow folded into a single canonical,
        // dropping tuned B³ precision from 1.000 (conv) to 0.593.
        match (&left.affiliation, &right.affiliation, &left.role, &right.role) {
            (Some(la), Some(ra), Some(lr), Some(rr))
                if !la.is_empty()
                    && !ra.is_empty()
                    && !lr.is_empty()
                    && !rr.is_empty() =>
            {
                fold_name(la) == fold_name(ra) && fold_name(lr) == fold_name(rr)
            }
            _ => false,
        }
    }

    fn signal(&self) -> MergeSignal {
        MergeSignal::OrgRole
    }
}

/// Thread-root signal: both entities' provenance `source_doc_id`s
/// share the same email thread root (i.e. the two mentions are in
/// the same conversation). Read by [`super::multi_origin::reconcile`]
/// from a caller-supplied lookup; defaults off when no lookup is
/// installed.
pub struct ThreadRootSignal {
    pub thread_of: std::sync::Arc<dyn Fn(&str) -> Option<String> + Send + Sync>,
}

impl MergeSignalCheck for ThreadRootSignal {
    fn check(&self, left: &Entity, right: &Entity) -> bool {
        let l = &left.provenance.source_doc_id;
        let r = &right.provenance.source_doc_id;
        if l.is_empty() || r.is_empty() {
            return false;
        }
        match ((self.thread_of)(l), (self.thread_of)(r)) {
            (Some(lt), Some(rt)) => lt == rt,
            _ => false,
        }
    }

    fn signal(&self) -> MergeSignal {
        MergeSignal::ThreadRoot
    }
}

/// Default signal stack used by [`super::reconcile`] when no caller
/// custom-builds one.
pub fn default_signals() -> Vec<Box<dyn MergeSignalCheck>> {
    vec![
        Box::new(NameSimilaritySignal::default()),
        Box::new(EmailHeaderSignal),
        Box::new(OrgRoleSignal),
    ]
}

// ── Helpers ──────────────────────────────────────────────────

pub(crate) fn fold_name(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_space = false;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            prev_space = false;
        } else if c.is_whitespace() || c == '.' || c == ',' || c == '-' || c == '_' {
            if !prev_space && !out.is_empty() {
                out.push(' ');
                prev_space = true;
            }
        } else if c == '@' {
            out.push('@');
            prev_space = false;
        } else if c == '<' || c == '>' || c == '"' || c == '\'' {
            // strip
        } else {
            out.push(c);
            prev_space = false;
        }
    }
    out.trim().to_string()
}

fn shared_surname_with_prefix_first_name(a: &str, b: &str) -> bool {
    // a, b are already folded. Tokens are split on whitespace; we
    // ignore middle initials (single-char tokens between first and
    // surname).
    let a_tokens: Vec<&str> = a.split_whitespace().filter(|t| t.len() > 1).collect();
    let b_tokens: Vec<&str> = b.split_whitespace().filter(|t| t.len() > 1).collect();
    if a_tokens.len() < 2 || b_tokens.len() < 2 {
        return false;
    }
    let a_first = a_tokens.first().copied().unwrap_or("");
    let b_first = b_tokens.first().copied().unwrap_or("");
    let a_surname = a_tokens.last().copied().unwrap_or("");
    let b_surname = b_tokens.last().copied().unwrap_or("");
    if a_surname != b_surname {
        return false;
    }
    // First-name prefix match: one must start with the other, and
    // the shorter must be ≥3 chars to keep "K" / "Ka" from creating
    // spurious matches.
    let (short, long) = if a_first.len() <= b_first.len() {
        (a_first, b_first)
    } else {
        (b_first, a_first)
    };
    short.len() >= 3 && long.starts_with(short)
}

fn initial_surname_matches(short: &str, long: &str) -> bool {
    // "j skilling" + "jeff skilling" → true (initial matches first
    // letter of first token).
    let s_tokens: Vec<&str> = short.split_whitespace().collect();
    let l_tokens: Vec<&str> = long.split_whitespace().collect();
    if s_tokens.len() != 2 || l_tokens.len() < 2 {
        return false;
    }
    if s_tokens[0].len() != 1 {
        return false;
    }
    let initial = s_tokens[0].chars().next().unwrap();
    let first = l_tokens.first().and_then(|t| t.chars().next());
    let surname_l = s_tokens[1];
    let surname_long = l_tokens.last().unwrap();
    Some(initial) == first && surname_l == *surname_long
}

pub(crate) fn collect_emails(e: &Entity) -> std::collections::BTreeSet<String> {
    let mut out = std::collections::BTreeSet::new();
    if let Some(stripped) = strip_to_email(&e.canonical_name) {
        out.insert(stripped);
    }
    for a in &e.aliases {
        if let Some(stripped) = strip_to_email(a) {
            out.insert(stripped);
        }
    }
    out
}

fn strip_to_email(s: &str) -> Option<String> {
    let trimmed = s.trim_matches(|c: char| c == '<' || c == '>' || c.is_whitespace());
    if trimmed.contains('@') && !trimmed.contains(' ') {
        Some(trimmed.to_ascii_lowercase())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enrichment::atlas::atoms::{AtomId, ChunkRef, Provenance};
    use crate::enrichment::pipeline::atlas::{EnrichmentDepth, EntityType};

    fn ent(name: &str, et: EntityType) -> Entity {
        Entity {
            id: AtomId::from_raw("entity-001"),
            canonical_name: name.into(),
            aliases: Vec::new(),
            entity_type: et,
            first_appearance: ChunkRef::new("sec-001", None),
            description: String::new(),
            defining_quote: None,
            salience: 0.5,
            enrichment_depth: EnrichmentDepth::Extracted,
            affiliation: None,
            role: None,
            participants: Vec::new(),
            provenance: Provenance::default(),
            concept_kind: None,
        }
    }

    #[test]
    fn name_similarity_collapses_initial_to_full() {
        let l = ent("Jeff Skilling", EntityType::Person);
        let r = ent("J. Skilling", EntityType::Person);
        assert!(NameSimilaritySignal::default().check(&l, &r));
    }

    #[test]
    fn name_similarity_rejects_different_entity_types() {
        let l = ent("Dynegy", EntityType::Institution);
        let r = ent("Dynegy", EntityType::Person);
        assert!(!NameSimilaritySignal::default().check(&l, &r));
    }

    #[test]
    fn name_similarity_ignores_shared_bare_given_name_alias() {
        // Two distinct people who merely share a polluted bare given
        // name in their alias lists must NOT merge. This is the
        // 362-atom mega-cluster guard: pre-fix, a shared "John"/"Ken"
        // alias fired NameSimilarity and (same-origin, needed=1)
        // chained Lay + Skilling + Fastow + ~360 others into one
        // cluster on enron-sample-multi-wide.
        let mut l = ent("Kenneth Lay", EntityType::Person);
        l.aliases.push("John".into());
        let mut r = ent("Jeff Skilling", EntityType::Person);
        r.aliases.push("John".into());
        assert!(
            !NameSimilaritySignal::default().check(&l, &r),
            "distinct surnames sharing a bare given-name alias must not merge"
        );
    }

    #[test]
    fn name_similarity_still_merges_on_shared_email_alias() {
        // The restriction keeps identity-grade (email) alias overlap
        // as a firing path — two atoms with unlike surface names but
        // the same email alias are the same person.
        let mut l = ent("The Chairman", EntityType::Person);
        l.aliases.push("klay@enron.com".into());
        let mut r = ent("Board Chair", EntityType::Person);
        r.aliases.push("<klay@enron.com>".into());
        assert!(
            NameSimilaritySignal::default().check(&l, &r),
            "shared email alias is an identity-grade merge signal"
        );
    }

    #[test]
    fn email_header_matches_full_address() {
        let mut l = ent("Ken Lay", EntityType::Person);
        l.aliases.push("klay@enron.com".into());
        let mut r = ent("klay@enron.com", EntityType::Person);
        r.aliases.push("klay@enron.com".into());
        assert!(EmailHeaderSignal.check(&l, &r));
    }

    #[test]
    fn email_header_no_longer_infers_identity_from_local_part() {
        // The email-local-part ↔ name heuristic was removed: on noisy
        // extraction it chained unrelated same-surname people through
        // polluted alias bags. An email on one side and a bare name on
        // the other is no longer an EmailHeader match. The two still
        // reconcile — through NameSimilarity's nickname-prefix path.
        let mut l = ent("Ken Lay", EntityType::Person);
        l.aliases.push("ken.lay@enron.com".into());
        let r = ent("Kenneth Lay", EntityType::Person);
        assert!(!EmailHeaderSignal.check(&l, &r));
        assert!(NameSimilaritySignal::default().check(&l, &r));
    }

    #[test]
    fn email_header_does_not_bridge_same_surname_distinct_addresses() {
        // The 2,013-atom mega-cluster guard. Two distinct people who
        // share a surname but hold different addresses must NOT merge:
        // pre-fix, "kenneth.lay@enron.com" matched "Linda Lay" via the
        // surname-only fallback and bridged the whole exec roster.
        let mut ken = ent("Kenneth Lay", EntityType::Person);
        ken.aliases.push("kenneth.lay@enron.com".into());
        let mut linda = ent("Linda Lay", EntityType::Person);
        linda.aliases.push("linda.lay@enron.com".into());
        assert!(!EmailHeaderSignal.check(&ken, &linda));
        assert!(!NameSimilaritySignal::default().check(&ken, &linda));
    }

    #[test]
    fn org_role_requires_both_affiliation_and_role_match() {
        let mut l = ent("Ken Lay", EntityType::Person);
        l.affiliation = Some("Enron".into());
        l.role = Some("CEO".into());
        let mut r = ent("Kenneth Lay", EntityType::Person);
        r.affiliation = Some("Enron".into());
        r.role = Some("CEO".into());
        assert!(OrgRoleSignal.check(&l, &r));

        r.role = Some("CFO".into());
        assert!(!OrgRoleSignal.check(&l, &r));
    }

    #[test]
    fn org_role_rejects_empty_string_role() {
        // Empty-string role from Phase 1b output ("role":"") was
        // matching transitively across every same-employer entity,
        // collapsing 86 distinct Enron employees into one cluster on
        // enron-sample-multi-tiny train. Lock the empty-string guard
        // so a future refactor can't quietly drop it.
        let mut l = ent("Ken Lay", EntityType::Person);
        l.affiliation = Some("Enron".into());
        l.role = Some(String::new());
        let mut r = ent("Jeff Skilling", EntityType::Person);
        r.affiliation = Some("Enron".into());
        r.role = Some(String::new());
        assert!(!OrgRoleSignal.check(&l, &r));

        // Same guard applies to affiliation.
        l.role = Some("CEO".into());
        r.role = Some("CEO".into());
        l.affiliation = Some(String::new());
        r.affiliation = Some(String::new());
        assert!(!OrgRoleSignal.check(&l, &r));
    }

    #[test]
    fn fold_name_collapses_punctuation_and_case() {
        assert_eq!(fold_name("Kenneth L. Lay"), "kenneth l lay");
        assert_eq!(fold_name("KEN-LAY"), "ken lay");
        assert_eq!(fold_name("<klay@enron.com>"), "klay@enron com");
    }
}
