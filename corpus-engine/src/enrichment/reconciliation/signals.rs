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
use crate::enrichment::pipeline::atlas::EntityType;

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
        // Normalize "Last, First" roster form → "First Last" (persons
        // only) so org-chart / Reports-To / headcount entries reconcile
        // with the body "First Last" form.
        let lf = fold_name(&person_display_name(left));
        let rf = fold_name(&person_display_name(right));
        if lf.len() < self.min_chars || rf.len() < self.min_chars {
            return false;
        }
        if lf == rf {
            return true;
        }
        // Corporate-suffix normalization (organisations only). "El Paso",
        // "El Paso Corp." and "El Paso Corporation" fold to three
        // distinct strings and share no email, so nothing else merges
        // them — they sit as singletons and cap org recall. Stripping a
        // leading "The" and the trailing run of legal-form suffixes
        // (Inc / Corp / Corporation / Cos / LLP / …) collapses the
        // variants while keeping distinct bases apart: "El Paso Japan
        // Co." → "el paso japan" ≠ "el paso", and "Williams Industries"
        // (Industries is not a legal suffix) never collapses into
        // "Williams". Org-only — a person's surname is never a legal
        // suffix. Guarded on a ≥3-char normalized form so degenerate
        // names ("Holdings Inc" → "") can't match.
        if left.entity_type == EntityType::Institution
            && right.entity_type == EntityType::Institution
        {
            let ln = strip_org_suffixes(&lf);
            if ln.len() >= 3 && ln == strip_org_suffixes(&rf) {
                return true;
            }
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
        } else if c == '&' {
            // Normalize "&" to the word "and" so "JPMorgan Chase & Co."
            // and "JPMorgan Chase and Co." fold identically. Without this
            // the ampersand survives as its own token and the org-suffix
            // strip leaves "...chase &" vs "...chase and" — two strings
            // that never merge (observed under-merge: org-jpmc sat in 3
            // clusters, 2026-05-30 test-split diagnose). Faithful: "AT&T"
            // → "at and t" matches a written-out "AT and T"; it conflates
            // no genuinely distinct organisations.
            if !out.is_empty() && !prev_space {
                out.push(' ');
            }
            out.push_str("and ");
            prev_space = true;
        } else if c == '<' || c == '>' || c == '"' || c == '\'' {
            // strip
        } else {
            out.push(c);
            prev_space = false;
        }
    }
    out.trim().to_string()
}

/// Strip a leading "the" and the trailing run of legal-form suffixes
/// from an already-folded organisation name, so surface variants of one
/// company collapse: "el paso corp" / "el paso corporation" / "el paso"
/// → "el paso". Only the *trailing* run is removed, so a distinct base
/// survives ("el paso japan co" → "el paso japan"). Never strips to
/// empty — at least one token is always kept.
pub(crate) fn strip_org_suffixes(folded: &str) -> String {
    const SUFFIXES: &[&str] = &[
        "inc",
        "incorporated",
        "corp",
        "corporation",
        "co",
        "company",
        "companies",
        "cos",
        "llp",
        "llc",
        "ltd",
        "limited",
        "lp",
        "plc",
        "gmbh",
        "ag",
        "sa",
        "nv",
        "group",
        "holdings",
    ];
    let mut toks: Vec<&str> = folded.split_whitespace().collect();
    if toks.first() == Some(&"the") && toks.len() > 1 {
        toks.remove(0);
    }
    while toks.len() > 1 && SUFFIXES.contains(toks.last().unwrap()) {
        toks.pop();
    }
    // Drop a now-dangling trailing connector. "JPMorgan Chase & Co." folds
    // to "jpmorgan chase and co"; stripping "co" leaves "...chase and",
    // which would never match the plain "JPMorgan Chase". Removing the
    // dangling "and" collapses all three forms to "jpmorgan chase".
    if toks.len() > 1 && toks.last() == Some(&"and") {
        toks.pop();
    }
    toks.join(" ")
}

/// For a Person whose canonical name is in "Last, First" roster form
/// ("O'Brien, James", "Lay, Kenneth", "Chen, Katherine"), return the
/// natural "First Last" order so it reconciles with the same person
/// written "First Last". Org charts, headcount rosters, and
/// Reports-To / Manager columns use the comma form; without this they
/// never merge with the body / email "First Last" form. Identity for
/// non-Person atoms (an organisation's comma — "Salesforce.com, Inc." —
/// must NOT be reordered) and for names without a single plausible
/// "Last, First" comma.
fn person_display_name(e: &Entity) -> String {
    if e.entity_type == EntityType::Person {
        if let Some(swapped) = lastfirst_swapped(&e.canonical_name) {
            return swapped;
        }
    }
    e.canonical_name.clone()
}

fn lastfirst_swapped(raw: &str) -> Option<String> {
    let mut parts = raw.splitn(2, ',');
    let last = parts.next()?.trim();
    let first = parts.next()?.trim();
    if last.is_empty() || first.is_empty() || first.contains(',') {
        return None;
    }
    // Guard: real "Last, First" halves are short (1-3 tokens). A longer
    // half is probably a descriptive string with an incidental comma.
    if last.split_whitespace().count() > 3 || first.split_whitespace().count() > 3 {
        return None;
    }
    Some(format!("{first} {last}"))
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
    // Only *standalone* email aliases are harvested for merging — one
    // address, no surrounding text. This is identity-grade and the
    // proven-safe path (train/test B³ precision 1.0). Mining addresses
    // out of blob aliases (forwarded/quoted header lines) was tried
    // 2026-05-30 and reverted: even coherence-guarded, a Person atom with
    // a descriptive multi-token "name" matched almost any address and
    // became a cross-type merge bridge (a 1,116-atom over-merge spanning
    // Causey + Houston + Lay + Dynegy). Cleaning blob-packed aliases is a
    // job for extraction/persistence-time hygiene, not the merge signal.
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
    fn name_similarity_merges_corporate_suffix_variants() {
        let full = ent("El Paso Corporation", EntityType::Institution);
        let abbr = ent("El Paso Corp.", EntityType::Institution);
        let bare = ent("El Paso", EntityType::Institution);
        assert!(NameSimilaritySignal::default().check(&full, &abbr));
        assert!(NameSimilaritySignal::default().check(&bare, &full));
        // "The Williams Companies" ↔ "Williams" via leading-the + suffix.
        let the_co = ent("The Williams Companies", EntityType::Institution);
        let williams = ent("Williams", EntityType::Institution);
        assert!(NameSimilaritySignal::default().check(&the_co, &williams));
    }

    #[test]
    fn name_similarity_org_norm_keeps_distinct_bases_apart() {
        // Suffix stripping must not over-merge: a non-suffix trailing
        // token or a different base stays distinct. (Gold note:
        // Williams Companies must NOT collapse into Williams Industries.)
        let williams = ent("Williams", EntityType::Institution);
        let industries = ent("Williams Industries", EntityType::Institution);
        assert!(!NameSimilaritySignal::default().check(&williams, &industries));
        let elpaso = ent("El Paso Corp.", EntityType::Institution);
        let japan = ent("El Paso Japan Co.", EntityType::Institution);
        assert!(!NameSimilaritySignal::default().check(&elpaso, &japan));
    }

    #[test]
    fn name_similarity_merges_last_comma_first_roster_form() {
        // Org charts / Reports-To / headcount columns write
        // "Last, First"; it must reconcile with the body "First Last".
        let roster = ent("O'Brien, James", EntityType::Person);
        let body = ent("James O'Brien", EntityType::Person);
        assert!(NameSimilaritySignal::default().check(&roster, &body));
        let chen_roster = ent("Chen, Katherine", EntityType::Person);
        let chen = ent("Katherine Chen", EntityType::Person);
        assert!(NameSimilaritySignal::default().check(&chen_roster, &chen));
        // An ORGANISATION's comma ("Salesforce.com, Inc.") must NOT be
        // reordered — the swap is person-only.
        let sf = ent("Salesforce.com, Inc.", EntityType::Institution);
        let acme = ent("Inc. Acme", EntityType::Institution);
        assert!(!NameSimilaritySignal::default().check(&sf, &acme));
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

    #[test]
    fn fold_name_normalizes_ampersand_to_and() {
        // The org-jpmc under-merge: "& Co." vs "and Co." must fold alike.
        assert_eq!(
            fold_name("JPMorgan Chase & Co."),
            fold_name("JPMorgan Chase and Co.")
        );
        assert_eq!(fold_name("JPMorgan Chase & Co."), "jpmorgan chase and co");
        // No-space ampersand still gets word boundaries: "AT&T" → "at and t".
        assert_eq!(fold_name("AT&T"), "at and t");
    }

    #[test]
    fn name_similarity_merges_ampersand_against_written_and() {
        let amp = ent("JPMorgan Chase & Co.", EntityType::Institution);
        let written = ent("JPMorgan Chase and Co.", EntityType::Institution);
        assert!(
            NameSimilaritySignal::default().check(&amp, &written),
            "'& Co.' and 'and Co.' name the same org"
        );
    }

    #[test]
    fn ampersand_normalization_does_not_overmerge_distinct_orgs() {
        let pg = ent("Procter & Gamble", EntityType::Institution);
        let jpm = ent("JPMorgan Chase & Co.", EntityType::Institution);
        assert!(!NameSimilaritySignal::default().check(&pg, &jpm));
    }

    #[test]
    fn collect_emails_harvests_standalone_only() {
        // Standalone email aliases are identity-grade and harvested; a
        // blob alias packing a third party's address is NOT mined for
        // merging (that path bridged distinct entities and was reverted).
        let mut lay = ent("Kenneth Lay", EntityType::Person);
        lay.aliases.push("kenneth.lay@enron.com".into());
        lay.aliases
            .push("Fwd from Lynda.L.Phinney@williams.com re: budget".into());
        let emails = collect_emails(&lay);
        assert!(emails.contains("kenneth.lay@enron.com"));
        assert!(
            !emails.contains("lynda.l.phinney@williams.com"),
            "blob-packed address must not be harvested for merging, got {emails:?}"
        );
    }
}
