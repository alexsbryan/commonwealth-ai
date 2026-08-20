// SPDX-License-Identifier: AGPL-3.0-or-later
//! Role convergence — duplicated *purpose*, the third thing neither sibling
//! feed can see.
//!
//! Three discovery feeds, and they do not overlap:
//!
//!   1. [`crate::converge::census`]  duplicated NAME      — six `ChatMessage`
//!   2. `sovereign_tools::code::dry_report`  duplicated BEHAVIOUR — bodies
//!      that hash alike
//!   3. this module            duplicated ROLE      — `AuditReport`,
//!      `DriftReport` and `StalenessSummary` all answering "how much should
//!      you trust this result", sharing no name and no body
//!
//! The census keys on names, so it is silent on (3) by construction:
//! `AuditReport` and `DriftReport` are not the same name, and neither are
//! `StalenessSummary` and `lags_graph`. This module keys on two things a name
//! census cannot reach.
//!
//! ## Membership is DERIVED, never curated
//!
//! The order that commissioned this named its own kill condition: *"role
//! membership cannot be decided without a hand-maintained list of member
//! types per role. A registry someone must curate is not an instrument."*
//! So there is no member list here, and adding a type to a role is not an
//! edit anyone makes — it is a consequence of what the type is called and
//! what fields it declares.
//!
//! Two signals, both read straight off the graph:
//!
//! - **Head noun** ([`head_noun`]) — the last CamelCase segment of the type
//!   name. `AuditReport`, `DriftReport`, `ArchReport`, `DryReport` and
//!   `FieldglassReport` are one role because they end in one word. This
//!   needs no table at all: every type in the graph gets a role for free,
//!   and a role appears the moment a second type shares its head noun.
//!
//! - **Field signature** ([`type_fields`]) — the set of field names a type
//!   declares. This is the signal with no name attached: a type carrying
//!   `generated_at` is answering a freshness question whatever it is called,
//!   and [`RoleRow::carriers_unnamed`] counts exactly the ones whose name
//!   gives them away to nobody. That count is the size of the blind spot
//!   the name census has.
//!
//! [`FAMILIES`] is the one place a human wrote anything down, and it is a
//! list of ~25 MORPHEMES, not of types — a closed set, which is what
//! `ARCH_PRINCIPLES` §2 says an enum is for. Its members are unbounded and
//! graph-derived; nobody maintains them. Every morpheme in it is quoted from
//! a table already published in `quality/campaigns/noun-convergence.toml` or
//! in `quality/NOUN_CONVERGENCE.md` §10.1, so this module introduces no new
//! taxonomy of its own.
//!
//! ## This is a mirror, not a gate
//!
//! Nothing here has a threshold to breach, an exit code to fail, or a
//! baseline to ratchet. §10.7 retired two proposed ratchets on the grounds
//! that both would sit red on one crate indefinitely and be switched off
//! inside a week. [`ADOPTION_REACH`] is a REPORTING cut inherited from
//! §10.1's own table, not a bar: it decides which column a row is counted
//! in, and no caller is expected to act on it.
//!
//! ## Honest limitations, stated once
//!
//! - A shared head noun is not a shared concept, exactly as a shared name is
//!   not. `GateVerdict` and `SpendVerdict` are both `Verdict`-role and are
//!   mostly correct as distinct local types. This DISCOVERS; a human
//!   DISPOSITIONS, into the same `quality/CONCEPTS.toml` with the same verbs
//!   the census already feeds.
//! - Field rows come from the SCIP export, which emits a symbol per declared
//!   field. Validated exactly against `RegistrySnapshot` on 2026-08-20: four
//!   fields declared, four rows in the graph. It does NOT count struct
//!   literal initializers, which is the difference between this module's
//!   numbers and a `grep` over the same source — see the note on
//!   [`FRESHNESS_FIELDS`].
//! - Reach is computed with the same rule as [`crate::converge::dossier`]'s
//!   `user_crates`, deliberately: one decider per number (§10.6). SCIP misses
//!   macro-expanded references and some dynamic dispatch, so every reach is a
//!   floor and every adoption share is therefore a floor too.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::capability_map::pkg_and_desc;
use crate::converge::{SourceScope, TypeDef};
use crate::descriptor::{descriptor_kind, DescriptorKind};
use crate::scip_graph::{ScipRefRecord, ScipSymbolRecord};

/// Distinct referencing crates at which a type counts as ADOPTED rather than
/// local.
///
/// Three, because that is the cut `NOUN_CONVERGENCE.md` §10.1's own table
/// used (`reach >= 3`) and this module exists to re-derive that table. One
/// implementation of the threshold, in one place, so the role rows and the
/// family rows can never be counted against different bars (§10.6).
///
/// It is a reporting cut, NOT a gate. Nothing exits non-zero on it.
pub const ADOPTION_REACH: usize = 3;

/// One concept family: a set of head nouns plus a set of field names, both
/// quoted from tables published before this module existed.
#[derive(Debug, Clone, Copy)]
pub struct Family {
    pub name: &'static str,
    /// Type-name head nouns that belong to this family.
    pub heads: &'static [&'static str],
    /// Field names that mark a type as ANSWERING this family's question,
    /// whatever the type is called.
    pub fields: &'static [&'static str],
}

/// The thirteen spellings `NOUN_CONVERGENCE.md` §10.1 enumerated for the
/// freshness concern, quoted verbatim so the re-derivation is against that
/// list and not against a fresh guess.
///
/// §10.1 reported "172 hand-written freshness fields in 13 spellings". That
/// figure counts every LINE in the repo matching `<name>:`, which is mostly
/// struct-literal initializers, JSON keys and comments — measured 2026-08-20,
/// a bare substring grep over all `.rs` returns 175 while the number of field
/// DECLARATIONS in first-party production code is 26. The spellings were
/// right; the count was of something else.
pub const FRESHNESS_FIELDS: &[&str] = &[
    "age_secs",
    "stale",
    "generated_at",
    "built_at",
    "age_days",
    "age_hours",
    "freshness",
    "indexed_at",
    "as_of",
    "staleness",
    "lags_graph",
    "computed_at",
    "commits_behind",
];

/// The three families `NOUN_CONVERGENCE.md` §10.1 named.
///
/// `heads` for the first family is the campaign ladder's own published
/// adjudication row — `*Result *Outcome *Verdict *Status -> Judgement` in
/// `quality/campaigns/noun-convergence.toml` — plus the category's own name.
/// Nothing here was invented for this module.
pub const FAMILIES: &[Family] = &[
    Family {
        name: "verdict / judgement",
        heads: &[
            "Verdict",
            "Judgement",
            "Judgment",
            "Outcome",
            "Result",
            "Status",
        ],
        fields: &["verdict", "outcome", "judgement", "passed", "failed"],
    },
    Family {
        name: "citation / provenance",
        heads: &[
            "Citation",
            "Source",
            "Provenance",
            "Origin",
            "Evidence",
            "Attribution",
            "Custody",
        ],
        fields: &[
            "citation",
            "citations",
            "provenance",
            "origin",
            "evidence",
            "attribution",
        ],
    },
    Family {
        name: "freshness / staleness",
        heads: &["Freshness", "Staleness", "Age"],
        fields: FRESHNESS_FIELDS,
    },
];

// ── Data ──────────────────────────────────────────────────────────────────────

/// The highest-reach member of a role — §10.1's `best` column.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RoleBest {
    pub name: String,
    pub krate: String,
    pub reach: usize,
}

/// One role's population and adoption.
///
/// Named parallel to [`crate::converge::CensusRow`], which is the sibling
/// feed's row in the same crate. The campaign ladder files `*Row` under
/// `Record` at rung 5; this joins that queue rather than opening a new one.
#[derive(Debug, Clone, Serialize)]
pub struct RoleRow {
    /// Head noun, or family name for a [`FAMILIES`] row.
    pub role: String,
    /// Types belonging to this role.
    pub population: usize,
    /// Distinct crates defining at least one member.
    pub crates: usize,
    /// Members reaching [`ADOPTION_REACH`] or more distinct crates.
    pub adopted: usize,
    /// `adopted / population`, as a share in 0.0..=1.0.
    pub adoption: f64,
    pub best: Option<RoleBest>,
    /// Types DECLARING one of this role's field names. Zero for a head-noun
    /// row, which has no field morphemes of its own.
    pub carriers: usize,
    /// Carriers whose head noun is NOT this role — the members a name census
    /// structurally cannot see. This is the blind-spot measurement.
    pub carriers_unnamed: usize,
}

/// The role tier's report. One shape for both halves; the family rows are
/// [`RoleRow`]s too, so a caller renders one table twice.
#[derive(Debug, Clone, Serialize)]
pub struct RoleCensus {
    pub scope: SourceScope,
    /// Every first-party production top-level type definition considered —
    /// the same denominator [`crate::converge::Census`] reports.
    pub total_type_defs: usize,
    /// Types for which the graph carries at least one declared field.
    pub types_with_fields: usize,
    /// Declared field rows in scope.
    pub field_defs: usize,
    /// Head-noun roles, ranked by population descending.
    pub roles: Vec<RoleRow>,
    /// [`FAMILIES`], in declaration order.
    pub families: Vec<RoleRow>,
}

// ── Derivation ────────────────────────────────────────────────────────────────

/// The last CamelCase segment of a type name — its role.
///
/// `AuditReport` -> `Report`. `HTTPResponse` -> `Response` (an acronym run
/// breaks at the last capital before a lowercase, not at every capital).
/// `Verdict` -> `Verdict`, so a single-word type is its own role and the
/// function is total: every type has one, and none is defaulted.
pub fn head_noun(name: &str) -> &str {
    let b = name.as_bytes();
    let mut start = 0usize;
    for i in 1..b.len() {
        if !b[i].is_ascii_uppercase() {
            continue;
        }
        // A boundary is either lower/digit -> Upper (`AuditR`), or the end of
        // an acronym run, Upper -> Upper followed by lower (`HTTPRe`).
        let prev_breaks = b[i - 1].is_ascii_lowercase() || b[i - 1].is_ascii_digit();
        let next_lower = i + 1 < b.len() && b[i + 1].is_ascii_lowercase();
        if prev_breaks || next_lower {
            start = i;
        }
    }
    &name[start..]
}

/// Declared fields per type, keyed by the owning type's qualified name.
///
/// The graph emits `…path/Type#field.` for every declared field, so the
/// owning type's qualified name is the prefix through the last `#` — which is
/// exactly [`TypeDef::qualified`], letting the two join without a second
/// parse of anything.
pub fn type_fields(
    symbols: &[ScipSymbolRecord],
    scope: &SourceScope,
) -> BTreeMap<String, BTreeSet<String>> {
    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for s in symbols {
        if !scope.admits(&s.file_path) {
            continue;
        }
        if descriptor_kind(&s.qualified_name) != DescriptorKind::Field {
            continue;
        }
        // Colocated `mod tests` sits under a `/tests/` descriptor segment even
        // when the FILE is production — the same clause `type_defs` carries.
        if s.qualified_name.contains("/tests/") {
            continue;
        }
        let Some(hash) = s.qualified_name.rfind('#') else {
            continue;
        };
        let owner = &s.qualified_name[..=hash];
        let field = s.qualified_name[hash + 1..].trim_end_matches('.');
        if field.is_empty() {
            continue;
        }
        out.entry(owner.to_string())
            .or_default()
            .insert(field.to_string());
    }
    out
}

/// Distinct first-party crates referencing each type, keyed by qualified name.
///
/// Deliberately the same rule as [`crate::converge::dossier`]'s `user_crates`
/// — an in-scope reference site whose CALLER package is first-party — so a
/// reach printed here and a reach printed by `converge noun` are the same
/// number computed once (§10.6). Types with no in-scope reference are absent
/// from the map, which is reach 0 and is reported as such, never defaulted.
pub fn reach_index(
    defs: &[TypeDef],
    refs: &[ScipRefRecord],
    scope: &SourceScope,
) -> BTreeMap<String, BTreeSet<String>> {
    let by_qualified: BTreeMap<&str, ()> =
        defs.iter().map(|d| (d.qualified.as_str(), ())).collect();
    let first_party: BTreeSet<&str> = defs.iter().map(|d| d.krate.as_str()).collect();

    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for r in refs {
        if !by_qualified.contains_key(r.callee_qualified.as_str()) || !scope.admits(&r.file_path) {
            continue;
        }
        let Some((pkg, _)) = pkg_and_desc(&r.caller_qualified) else {
            continue;
        };
        if !first_party.contains(pkg) {
            continue;
        }
        out.entry(r.callee_qualified.clone())
            .or_default()
            .insert(pkg.to_string());
    }
    out
}

// ── The verb ──────────────────────────────────────────────────────────────────

/// Build one [`RoleRow`] from an explicit member set.
fn row(
    role: &str,
    members: &[&TypeDef],
    reach: &BTreeMap<String, BTreeSet<String>>,
    carriers: usize,
    carriers_unnamed: usize,
) -> RoleRow {
    let reach_of = |d: &TypeDef| reach.get(&d.qualified).map(BTreeSet::len).unwrap_or(0);
    let crates: BTreeSet<&str> = members.iter().map(|d| d.krate.as_str()).collect();
    let adopted = members
        .iter()
        .filter(|d| reach_of(d) >= ADOPTION_REACH)
        .count();
    let best = members
        .iter()
        .max_by(|a, b| {
            reach_of(a)
                .cmp(&reach_of(b))
                .then_with(|| b.name.cmp(&a.name))
        })
        .map(|d| RoleBest {
            name: d.name.clone(),
            krate: d.krate.clone(),
            reach: reach_of(d),
        });
    RoleRow {
        role: role.to_string(),
        population: members.len(),
        crates: crates.len(),
        adopted,
        adoption: if members.is_empty() {
            0.0
        } else {
            adopted as f64 / members.len() as f64
        },
        best,
        carriers,
        carriers_unnamed,
    }
}

/// The role census: per role, the population and the adoption share.
///
/// `min_population` drops the long tail of one-off head nouns from the
/// ranked table; it does not affect [`RoleCensus::total_type_defs`] or the
/// family rows, which are always computed over everything.
pub fn roles(
    defs: &[TypeDef],
    fields: &BTreeMap<String, BTreeSet<String>>,
    reach: &BTreeMap<String, BTreeSet<String>>,
    scope: &SourceScope,
    min_population: usize,
) -> RoleCensus {
    // ── head-noun roles ──
    let mut by_role: BTreeMap<&str, Vec<&TypeDef>> = BTreeMap::new();
    for d in defs {
        by_role.entry(head_noun(&d.name)).or_default().push(d);
    }

    let mut role_rows: Vec<RoleRow> = by_role
        .iter()
        .filter(|(_, ds)| ds.len() >= min_population)
        .map(|(role, ds)| row(role, ds, reach, 0, 0))
        .collect();
    role_rows.sort_by(|a, b| {
        b.population
            .cmp(&a.population)
            .then(b.adopted.cmp(&a.adopted))
            .then(a.role.cmp(&b.role))
    });

    // ── families ──
    let families = FAMILIES
        .iter()
        .map(|f| {
            let members: Vec<&TypeDef> = defs
                .iter()
                .filter(|d| f.heads.contains(&head_noun(&d.name)))
                .collect();
            // A carrier declares one of the family's field names. The
            // unnamed ones are the finding: their head noun puts them in some
            // OTHER role, so no name-keyed census will ever group them here.
            let mut carriers = 0usize;
            let mut carriers_unnamed = 0usize;
            for d in defs {
                let Some(fs) = fields.get(&d.qualified) else {
                    continue;
                };
                if !f.fields.iter().any(|m| fs.contains(*m)) {
                    continue;
                }
                carriers += 1;
                if !f.heads.contains(&head_noun(&d.name)) {
                    carriers_unnamed += 1;
                }
            }
            row(f.name, &members, reach, carriers, carriers_unnamed)
        })
        .collect();

    RoleCensus {
        scope: scope.clone(),
        total_type_defs: defs.len(),
        types_with_fields: fields.len(),
        field_defs: fields.values().map(BTreeSet::len).sum(),
        roles: role_rows,
        families,
    }
}

// ── Rendering ─────────────────────────────────────────────────────────────────

fn pct(x: f64) -> String {
    format!("{:.0}%", x * 100.0)
}

fn render_row(r: &RoleRow, out: &mut String) {
    let best = r
        .best
        .as_ref()
        .map(|b| format!("{} ({})", b.name, b.reach))
        .unwrap_or_else(|| "-".to_string());
    out.push_str(&format!(
        "  {:<26} {:>6} {:>7} {:>9} {:>6}  {}\n",
        r.role,
        r.population,
        r.crates,
        pct(r.adoption),
        r.adopted,
        best
    ));
}

pub fn render_roles(c: &RoleCensus, limit: usize) -> String {
    let mut out = String::new();

    out.push_str("scope: ");
    if c.scope.include_prefixes.is_empty() {
        out.push_str("all paths");
    } else {
        out.push_str(&c.scope.include_prefixes.join(", "));
    }
    out.push_str(" minus segments ");
    out.push_str(&c.scope.exclude_segments.join(" "));
    out.push_str("\n\n");

    out.push_str(&format!(
        "{} first-party production types · {} with declared fields · {} field declarations\n",
        c.total_type_defs, c.types_with_fields, c.field_defs
    ));
    out.push_str(&format!(
        "adoption = share of a role's types reaching >= {ADOPTION_REACH} distinct crates. \
         A MIRROR, not a gate.\n\n"
    ));

    out.push_str("role — by head noun, derived from the graph, no list maintained\n");
    out.push_str(&format!(
        "  {:<26} {:>6} {:>7} {:>9} {:>6}  {}\n",
        "", "types", "crates", "adoption", "n", "best (reach)"
    ));
    for r in c.roles.iter().take(limit) {
        render_row(r, &mut out);
    }
    if c.roles.len() > limit {
        out.push_str(&format!(
            "  … {} more roles (--limit 0 for all)\n",
            c.roles.len() - limit
        ));
    }

    out.push_str("\nconcept family — head nouns plus the fields that mark the concern\n");
    out.push_str(&format!(
        "  {:<26} {:>6} {:>7} {:>9} {:>6}  {}\n",
        "", "types", "crates", "adoption", "n", "best (reach)"
    ));
    for r in &c.families {
        render_row(r, &mut out);
    }

    out.push_str("\nfield carriers — types answering the question under another name\n");
    for r in &c.families {
        out.push_str(&format!(
            "  {:<26} {:>4} carriers, {:>4} of them named for some other role\n",
            r.role, r.carriers, r.carriers_unnamed
        ));
    }

    out.push_str(
        "\nThis DISCOVERS; a human DISPOSITIONS, into quality/CONCEPTS.toml with the\n\
         verbs already defined there. A shared head noun is not a shared concept.\n\
         Adjacent feeds: `svrn code converge census` (name) · `svrn code dry-report`\n\
         (behaviour).\n",
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn td(name: &str, krate: &str, qualified: &str) -> TypeDef {
        TypeDef {
            name: name.to_string(),
            krate: krate.to_string(),
            file: format!("{krate}/src/lib.rs"),
            line: 1,
            qualified: qualified.to_string(),
        }
    }

    #[test]
    fn head_noun_splits_on_the_last_camel_boundary() {
        assert_eq!(head_noun("AuditReport"), "Report");
        assert_eq!(head_noun("DriftReport"), "Report");
        assert_eq!(head_noun("StalenessSummary"), "Summary");
        // A single word is its own role — the function is total.
        assert_eq!(head_noun("Verdict"), "Verdict");
        // An acronym run breaks once, at the last capital before a lowercase.
        assert_eq!(head_noun("HTTPResponse"), "Response");
        assert_eq!(head_noun("S3Client"), "Client");
        // Degenerate inputs still return something, never panic.
        assert_eq!(head_noun(""), "");
        assert_eq!(head_noun("X"), "X");
        assert_eq!(head_noun("HTTP"), "HTTP");
    }

    #[test]
    fn a_role_groups_types_that_share_no_name() {
        let defs = vec![
            td("AuditReport", "a", "q a/AuditReport#"),
            td("DriftReport", "b", "q b/DriftReport#"),
            td("ArchReport", "c", "q c/ArchReport#"),
        ];
        let c = roles(
            &defs,
            &BTreeMap::new(),
            &BTreeMap::new(),
            &SourceScope::default(),
            1,
        );
        let report = c.roles.iter().find(|r| r.role == "Report").unwrap();
        assert_eq!(report.population, 3);
        assert_eq!(report.crates, 3);
        // No references supplied, so nothing is adopted — reported, not defaulted.
        assert_eq!(report.adopted, 0);
        assert_eq!(report.adoption, 0.0);
    }

    /// §18.4: an instrument that cannot notice an addition is not measuring.
    #[test]
    fn planting_a_new_member_moves_the_number() {
        let scope = SourceScope::default();
        let mut defs = vec![
            td("AuditReport", "a", "q a/AuditReport#"),
            td("DriftReport", "b", "q b/DriftReport#"),
        ];
        let before = roles(&defs, &BTreeMap::new(), &BTreeMap::new(), &scope, 1);
        let before_pop = before
            .roles
            .iter()
            .find(|r| r.role == "Report")
            .unwrap()
            .population;

        defs.push(td("FieldglassReport", "c", "q c/FieldglassReport#"));
        let after = roles(&defs, &BTreeMap::new(), &BTreeMap::new(), &scope, 1);
        let after_pop = after
            .roles
            .iter()
            .find(|r| r.role == "Report")
            .unwrap()
            .population;

        assert_eq!(before_pop, 2);
        assert_eq!(after_pop, 3, "a planted member must move the population");
        assert_eq!(after.total_type_defs, before.total_type_defs + 1);
    }

    #[test]
    fn adoption_counts_members_at_or_above_the_reach_cut() {
        let defs = vec![
            td("GateVerdict", "a", "q a/GateVerdict#"),
            td("SpendVerdict", "b", "q b/SpendVerdict#"),
        ];
        let mut reach: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        reach.insert(
            "q a/GateVerdict#".to_string(),
            ["x", "y", "z"].iter().map(|s| s.to_string()).collect(),
        );
        reach.insert(
            "q b/SpendVerdict#".to_string(),
            ["x"].iter().map(|s| s.to_string()).collect(),
        );
        let c = roles(&defs, &BTreeMap::new(), &reach, &SourceScope::default(), 1);
        let v = c.roles.iter().find(|r| r.role == "Verdict").unwrap();
        assert_eq!(v.population, 2);
        assert_eq!(v.adopted, 1, "reach 3 is adopted, reach 1 is not");
        assert_eq!(v.best.as_ref().unwrap().name, "GateVerdict");
        assert_eq!(v.best.as_ref().unwrap().reach, 3);
    }

    /// The whole point of the tier: a type whose NAME says nothing is still
    /// counted, because of the field it declares.
    #[test]
    fn a_field_carrier_is_seen_though_its_name_hides_it() {
        let defs = vec![
            // Head noun `Summary` — no name-keyed census puts this in the
            // freshness family.
            td("StalenessSummary", "a", "q a/StalenessSummary#"),
            td("Freshness", "b", "q b/Freshness#"),
        ];
        let mut fields: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        fields.insert(
            "q a/StalenessSummary#".to_string(),
            ["generated_at", "name"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
        );
        let c = roles(&defs, &fields, &BTreeMap::new(), &SourceScope::default(), 1);
        let fam = c
            .families
            .iter()
            .find(|r| r.role == "freshness / staleness")
            .unwrap();
        // By name, only `Freshness` is a member.
        assert_eq!(fam.population, 1);
        // By field, `StalenessSummary` is a carrier, and an unnamed one.
        assert_eq!(fam.carriers, 1);
        assert_eq!(fam.carriers_unnamed, 1);
    }

    #[test]
    fn type_fields_keys_on_the_owning_type_and_skips_colocated_tests() {
        let sym = |q: &str, f: &str| ScipSymbolRecord {
            name: String::new(),
            qualified_name: q.to_string(),
            kind: String::new(),
            file_path: f.to_string(),
            line_start: 1,
            line_end: 1,
            language: "rust".to_string(),
        };
        let symbols = vec![
            sym(
                "rust-analyzer cargo ce 0.1.0 registry/RegistrySnapshot#generated_at.",
                "corpus-engine/src/registry.rs",
            ),
            sym(
                "rust-analyzer cargo ce 0.1.0 registry/RegistrySnapshot#entries.",
                "corpus-engine/src/registry.rs",
            ),
            // Colocated test module — excluded by the descriptor clause.
            sym(
                "rust-analyzer cargo ce 0.1.0 registry/tests/Fixture#generated_at.",
                "corpus-engine/src/registry.rs",
            ),
            // Out of scope by path segment.
            sym(
                "rust-analyzer cargo ce 0.1.0 vend/Thing#generated_at.",
                "vendor/x/src/lib.rs",
            ),
        ];
        let f = type_fields(&symbols, &SourceScope::default());
        assert_eq!(f.len(), 1, "one owning type, keyed by its qualified name");
        let owner = "rust-analyzer cargo ce 0.1.0 registry/RegistrySnapshot#";
        let got = f
            .get(owner)
            .expect("owner key is the type's qualified name");
        assert!(got.contains("generated_at"));
        assert!(got.contains("entries"));
        assert_eq!(got.len(), 2);
    }

    #[test]
    fn every_family_morpheme_is_lowercase_and_nonempty() {
        // The field morphemes are matched against raw field names, so a
        // capitalised entry would silently never match — a substitution the
        // caller would never be told about (§18.3).
        for f in FAMILIES {
            for m in f.fields {
                assert!(!m.is_empty(), "{} has an empty field morpheme", f.name);
                assert_eq!(
                    *m,
                    m.to_ascii_lowercase(),
                    "{} field morpheme `{m}` must be lowercase",
                    f.name
                );
            }
            for h in f.heads {
                assert!(
                    h.starts_with(|c: char| c.is_ascii_uppercase()),
                    "{} head `{h}` must be a type-name segment",
                    f.name
                );
                assert_eq!(
                    head_noun(h),
                    *h,
                    "{} head `{h}` must be its own head noun",
                    f.name
                );
            }
        }
    }
}
