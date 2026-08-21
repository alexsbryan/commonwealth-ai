// SPDX-License-Identifier: AGPL-3.0-or-later
//! Duplicated concept SHAPE — the renamed fork a name census structurally
//! cannot see.
//!
//! [`crate::converge::census`] finds `ChatMessage` defined in six crates. It
//! is blind by construction to the OTHER half of the same disease: one concept
//! forked under seven different names. Seven of them were found by hand during
//! this campaign's wave 7 —
//!
//! ```text
//! Citation · DrCitation · SourceCitation · CitationAttribution
//! DraftCitation · ClaimCitation · ReleasedCitation
//! ```
//!
//! — and the census reported `Citation` as a four-crate collision, missing all
//! of the renamed ones. A name census cannot find those, because the evidence
//! is not in the name. It is in the FIELDS.
//!
//! ## The instrument
//!
//! **No type name is ever compared, at any step.** A type's signature is the
//! set of `(field name, normalized field type)` keys it declares; two types in
//! different crates are ranked by how much of that signature they share,
//! weighted by inverse document frequency so a field 300 types carry
//! (`id: String`) is worth almost nothing and a field four types carry
//! (`evidence_id: String`) is worth a lot.
//!
//! ```text
//! score(A,B) = shared_idf_weight / max(weight(A), weight(B))
//! ```
//!
//! The denominator is `max`, not `min`. Containment (`min`) also scores 1.0
//! when a two-field struct is wholly inside a forty-field one, which is a
//! coincidence rather than a fork; the symmetric form is what separates them.
//! Measured on the prototype corpus this was the difference between 94% and
//! 74% precision. [`ShapeMatch::containment`] carries the `min` form too,
//! because it is the right lens for a fork that DROPPED a field, and a reader
//! adjudicating a row wants both.
//!
//! Two gates keep coincidence out of the candidate set before the score is
//! ever computed: at least [`ShapeOptions::min_shared`] shared keys, and at
//! least one shared key rare enough ([`ShapeOptions::rare_df`]) that the match
//! is not held together by universal scaffolding alone.
//!
//! ## Where the field types come from
//!
//! SCIP emits a symbol per declared field, and the field's TYPE is recorded as
//! the references that field symbol makes: `rows: Vec<CensusRow>` is two edges,
//! to `Vec#` and to `CensusRow#`, ordered by column, and the key is written
//! `Vec<CensusRow` — unclosed, because an ordered list of occurrences is not a
//! parse tree and [`FieldKey::ty`] says why that matters. Normalizing to leaf
//! names is what lets a fork match — two crates each referencing their OWN
//! local `Citation` both normalize to `Citation`.
//!
//! Carrying the types is not free and it is worth it. Measured at index head
//! `4f64bdb2` over this workspace, with the same gates and the same threshold:
//!
//! | signature | pairs past the gates | pairs at score >= 0.50 |
//! |---|---:|---:|
//! | field names + field types | 669 | 221 |
//! | field names alone         | 947 | 328 |
//!
//! **42% more rows to adjudicate without the types** (48% more at the
//! reporting threshold), for the same recall on the positive control — the
//! renamed-fork specimen scores 1.000 either way. `--names-only` keeps that
//! arm runnable rather than leaving it as a claim in a commit message.
//!
//! A primitive field carries no symbol (`line: i32` references nothing), so its
//! normalized type is the empty string. That collapses `i32` with `u64` and is
//! the conservative direction — it can only ever merge two keys that a text
//! parser would have kept apart, never invent a match between different field
//! NAMES.
//!
//! ## Honest limitations, stated once
//!
//! - **A shared shape is not a shared concept.** This DISCOVERS; a human
//!   DISPOSITIONS, into `quality/CONCEPTS.toml` with the verbs already there.
//!   **Measured precision at the default threshold: 37/40 = 92.5%**, 95% Wilson
//!   CI [80.1%, 97.4%], zero could-not-judge, over a uniform random sample
//!   (seed 23) of the 226 pairs this verb reported at index head `b0697afb`,
//!   labelled against a rubric written before any label was assigned. So
//!   roughly one row in thirteen is a coincidence and a reader must still
//!   adjudicate.
//!
//!   The three misses are worth knowing, because none of them is fixable by
//!   raising the threshold — they scored 0.596, 0.723 and **0.919**:
//!   `CrateRect` vs `Quad` (both `{x,y,w,h}`; diagram layout vs OCR geometry),
//!   `NodeWork` vs `RaptorHit` (a bench work-unit and a production retrieval
//!   hit over the same RAPTOR node), and `AxisMeans` vs `JudgeScore` (an
//!   AGGREGATE over judgements and a single judgement, sharing the nine-axis
//!   vector). Two of the three are one real shared sub-concept wrapped for two
//!   purposes — a finding a reader can use, scored as a miss because the rubric
//!   asks whether ONE TYPE could replace both.
//! - **Tuple structs and unit structs are invisible.** They declare no named
//!   fields, so they have no signature. So are types with fewer than
//!   [`ShapeOptions::min_fields`] fields, where any match is noise.
//! - **A fork whose two halves share no field NAME is invisible** — rename the
//!   fields as well as the type and this instrument loses it, the same way the
//!   census loses a renamed type. Neither feed subsumes the other; run both.
//! - The graph is the same one every other verb in this crate reads, so it
//!   describes the last indexed commit, not the working tree.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::capability_map::pkg_and_desc;
use crate::converge::SourceScope;
use crate::descriptor::{descriptor_kind, field_owner_and_name, leaf_name, DescriptorKind};
use crate::scip_graph::{ScipRefRecord, ScipSymbolRecord};

// ── Data ──────────────────────────────────────────────────────────────────────

/// One declared field, reduced to what a fork preserves: its name and the
/// shape of its type.
///
/// NOT the field's source text. `Vec<crate::converge::CensusRow>` and
/// `Vec<CensusRow>` are the same key, which is the entire point — a fork that
/// moved to another crate still spells its own local type the same way.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct FieldKey {
    pub name: String,
    /// Leaf names of the type symbols this field references, in source order,
    /// joined by `<` and DELIBERATELY LEFT UNCLOSED: `Vec<CensusRow`, not
    /// `Vec<CensusRow>`.
    ///
    /// SCIP hands over an ordered list of type occurrences, not a parse tree.
    /// Closing the brackets would render `HashMap<String, Vec<u8>>` as
    /// `HashMap<String<Vec<u8>>>` — a plausible spelling that is not the
    /// source's, which is the one failure mode this workspace's instruments
    /// are built to avoid. The unclosed form is unambiguous about being a
    /// sequence and cannot be mistaken for what the developer typed. Same rule
    /// `crate::descriptor` follows for struct-vs-enum: claim exactly what the
    /// data carries.
    ///
    /// Empty for a primitive, which carries no symbol at all.
    pub ty: String,
}

impl FieldKey {
    /// `name: Vec<String>` — the one rendering, so a report and a JSON
    /// consumer never disagree about what a key is (§10.6).
    pub fn render(&self) -> String {
        format!("{}: {}", self.name, if self.ty.is_empty() { "?" } else { &self.ty })
    }
}

/// One side of a match — enough to open the file without a second lookup.
#[derive(Debug, Clone, Serialize)]
pub struct ShapeSide {
    pub krate: String,
    pub name: String,
    pub file: String,
    pub line: i32,
    pub qualified: String,
    /// How many named fields this type declares.
    pub fields: usize,
}

impl ShapeSide {
    pub fn label(&self) -> String {
        format!("{}::{}", self.krate, self.name)
    }
}

/// Two types in different crates that share a field signature.
#[derive(Debug, Clone, Serialize)]
pub struct ShapeMatch {
    /// `shared / max(weight)` — the ranking score. 1.0 is an exact signature.
    pub score: f64,
    /// `shared / min(weight)` — 1.0 when one signature CONTAINS the other.
    /// A fork that dropped a field scores 1.0 here and less than 1.0 above.
    pub containment: f64,
    pub a: ShapeSide,
    pub b: ShapeSide,
    pub shared: Vec<FieldKey>,
    /// Document frequency of the rarest shared key — how much of this match is
    /// carried by something unusual.
    pub rarest_shared_df: usize,
}

/// A connected component over the passing matches. One concept, many names.
#[derive(Debug, Clone, Serialize)]
pub struct ShapeGroup {
    /// `crate::Name` for every type in the component, sorted.
    pub members: Vec<String>,
    pub top_score: f64,
    /// The highest-scoring pair, which is the one to read first.
    pub best: ShapeMatch,
    pub pairs: Vec<ShapeMatch>,
}

/// Knobs, with their measured defaults.
///
/// A struct rather than four positional arguments because every one of them
/// moves the number and a caller passing them in the wrong order would get a
/// plausible wrong answer.
#[derive(Debug, Clone, Serialize)]
pub struct ShapeOptions {
    /// Types with fewer named fields than this have no signature worth
    /// matching.
    pub min_fields: usize,
    /// A pair must share at least this many keys to be scored at all.
    pub min_shared: usize,
    /// At least one shared key must be held by no more than this many types.
    /// A match carried only by `{id: String, name: String}` is by construction
    /// not evidence.
    pub rare_df: usize,
    /// Report matches at or above this score.
    pub threshold: f64,
}

impl ShapeOptions {
    /// The prototype's measured operating point (nc-22c, 27 hand-labelled
    /// groups): 94% precision with the symmetric score, 74% with containment.
    /// Re-measured against this implementation's own output for nc-23.
    pub const DEFAULT_THRESHOLD: f64 = 0.50;
}

impl Default for ShapeOptions {
    fn default() -> Self {
        Self {
            min_fields: 2,
            min_shared: 3,
            rare_df: 20,
            threshold: Self::DEFAULT_THRESHOLD,
        }
    }
}

/// The whole report, carrying the method that produced it.
///
/// Named for its sibling [`crate::converge::Census`] and
/// [`crate::roles::RoleCensus`]: same crate, same graph, same "what is
/// duplicated" question asked along a third axis.
#[derive(Debug, Clone, Serialize)]
pub struct ShapeCensus {
    pub scope: SourceScope,
    pub options: ShapeOptions,
    /// Types with at least `min_fields` named fields — the population.
    pub types_with_fields: usize,
    /// Cross-crate pairs sharing at least one non-ubiquitous key.
    pub candidate_pairs: usize,
    /// …of which passed both gates and were scored.
    pub scored_pairs: usize,
    /// …of which scored at or above the threshold.
    pub matched_pairs: usize,
    pub groups: Vec<ShapeGroup>,
}

// ── Extraction ────────────────────────────────────────────────────────────────

/// A key held by more types than this cannot seed a candidate pair.
///
/// Purely a cost guard on the O(n²)-per-key candidate expansion, and it can
/// only ever drop pairs the `rare_df` gate would have dropped anyway: a pair
/// whose ONLY shared key is held by 400+ types has no rare key by definition.
const MAX_SEED_POSTINGS: usize = 400;

/// Every in-scope type's field signature, keyed by qualified name.
///
/// `refs` supplies the field TYPES: a field symbol's outgoing references are
/// the type it is declared as. Pass an empty slice for a names-only signature —
/// legitimate, and measured as 29% more pairs to adjudicate.
pub fn field_signatures(
    symbols: &[ScipSymbolRecord],
    refs: &[ScipRefRecord],
    scope: &SourceScope,
) -> BTreeMap<String, BTreeSet<FieldKey>> {
    // field symbol -> (owner, field name)
    let mut fields: BTreeMap<&str, (String, String)> = BTreeMap::new();
    for s in symbols {
        if !scope.admits(&s.file_path) || s.qualified_name.contains("/tests/") {
            continue;
        }
        let Some((owner, name)) = field_owner_and_name(&s.qualified_name) else {
            continue;
        };
        // A field of an enum VARIANT is not a struct field signature.
        if descriptor_kind(owner) != DescriptorKind::Type {
            continue;
        }
        fields.insert(
            s.qualified_name.as_str(),
            (owner.to_string(), name.to_string()),
        );
    }

    // field symbol -> the type symbols it references, in source order.
    let mut parts: BTreeMap<&str, Vec<(i32, i32, &str)>> = BTreeMap::new();
    for r in refs {
        let Some((sym, _)) = fields.get_key_value(r.caller_qualified.as_str()) else {
            continue;
        };
        if descriptor_kind(&r.callee_qualified) != DescriptorKind::Type {
            continue;
        }
        parts
            .entry(sym)
            .or_default()
            .push((r.line, r.start_col, leaf_name(&r.callee_qualified)));
    }

    let mut out: BTreeMap<String, BTreeSet<FieldKey>> = BTreeMap::new();
    for (sym, (owner, name)) in &fields {
        let ty = match parts.get_mut(sym) {
            Some(ps) => {
                ps.sort_unstable();
                ps.iter().map(|(_, _, n)| *n).collect::<Vec<_>>().join("<")
            }
            None => String::new(),
        };
        out.entry(owner.clone()).or_default().insert(FieldKey {
            name: name.clone(),
            ty,
        });
    }
    out
}

// ── Verb: shape ───────────────────────────────────────────────────────────────

struct Rec<'a> {
    qualified: &'a str,
    krate: &'a str,
    name: String,
    file: &'a str,
    line: i32,
    keys: &'a BTreeSet<FieldKey>,
    weight: f64,
}

impl Rec<'_> {
    fn side(&self) -> ShapeSide {
        ShapeSide {
            krate: self.krate.to_string(),
            name: self.name.clone(),
            file: self.file.to_string(),
            line: self.line,
            qualified: self.qualified.to_string(),
            fields: self.keys.len(),
        }
    }
}

/// Cross-crate types that share a field signature, grouped.
///
/// `symbols` supplies the location of each type; `sigs` is
/// [`field_signatures`]'s output. No type name is read except to LABEL the
/// output — nothing branches on one.
pub fn shape_census(
    symbols: &[ScipSymbolRecord],
    sigs: &BTreeMap<String, BTreeSet<FieldKey>>,
    scope: &SourceScope,
    opts: &ShapeOptions,
) -> ShapeCensus {
    // ── population ───────────────────────────────────────────────────────────
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    let mut recs: Vec<Rec> = Vec::new();
    for s in symbols {
        if !scope.admits(&s.file_path) || s.qualified_name.contains("/tests/") {
            continue;
        }
        if descriptor_kind(&s.qualified_name) != DescriptorKind::Type {
            continue;
        }
        let Some(keys) = sigs.get(&s.qualified_name) else {
            continue;
        };
        if keys.len() < opts.min_fields || !seen.insert(s.qualified_name.as_str()) {
            continue;
        }
        let Some((pkg, _)) = pkg_and_desc(&s.qualified_name) else {
            continue;
        };
        recs.push(Rec {
            qualified: &s.qualified_name,
            krate: pkg,
            name: leaf_name(&s.qualified_name).to_string(),
            file: &s.file_path,
            line: s.line_start,
            keys,
            weight: 0.0,
        });
    }

    // ── inverse document frequency ───────────────────────────────────────────
    let n = recs.len();
    let mut df: BTreeMap<&FieldKey, usize> = BTreeMap::new();
    for r in &recs {
        for k in r.keys {
            *df.entry(k).or_default() += 1;
        }
    }
    let idf = |k: &FieldKey| -> f64 {
        let d = df.get(k).copied().unwrap_or(1).max(1);
        (n as f64 / d as f64).ln()
    };
    for i in 0..recs.len() {
        recs[i].weight = recs[i].keys.iter().map(idf).sum();
    }

    // ── candidates: only pairs sharing a key worth sharing ───────────────────
    let mut postings: BTreeMap<&FieldKey, Vec<usize>> = BTreeMap::new();
    for (i, r) in recs.iter().enumerate() {
        for k in r.keys {
            postings.entry(k).or_default().push(i);
        }
    }
    let mut cand: BTreeSet<(usize, usize)> = BTreeSet::new();
    for (_, idxs) in postings.iter() {
        if idxs.len() > MAX_SEED_POSTINGS {
            continue;
        }
        for a in 0..idxs.len() {
            for b in (a + 1)..idxs.len() {
                let (i, j) = (idxs[a], idxs[b]);
                if recs[i].krate != recs[j].krate {
                    cand.insert(if i < j { (i, j) } else { (j, i) });
                }
            }
        }
    }

    // ── score ────────────────────────────────────────────────────────────────
    let mut scored = 0usize;
    let mut matches: Vec<ShapeMatch> = Vec::new();
    for (i, j) in &cand {
        let (a, b) = (&recs[*i], &recs[*j]);
        let shared: Vec<&FieldKey> = a.keys.intersection(b.keys).collect();
        if shared.len() < opts.min_shared {
            continue;
        }
        let rarest = shared
            .iter()
            .filter_map(|k| df.get(*k).copied())
            .min()
            .unwrap_or(usize::MAX);
        if rarest > opts.rare_df {
            continue;
        }
        scored += 1;
        let sw: f64 = shared.iter().map(|k| idf(k)).sum();
        let (lo, hi) = (
            a.weight.min(b.weight).max(f64::MIN_POSITIVE),
            a.weight.max(b.weight).max(f64::MIN_POSITIVE),
        );
        let score = sw / hi;
        if score < opts.threshold {
            continue;
        }
        matches.push(ShapeMatch {
            score,
            containment: sw / lo,
            a: a.side(),
            b: b.side(),
            shared: shared.into_iter().cloned().collect(),
            rarest_shared_df: rarest,
        });
    }
    matches.sort_by(|x, y| {
        y.score
            .partial_cmp(&x.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(y.shared.len().cmp(&x.shared.len()))
            .then(x.a.label().cmp(&y.a.label()))
            .then(x.b.label().cmp(&y.b.label()))
    });
    let matched_pairs = matches.len();

    ShapeCensus {
        scope: scope.clone(),
        options: opts.clone(),
        types_with_fields: n,
        candidate_pairs: cand.len(),
        scored_pairs: scored,
        matched_pairs,
        groups: group(matches),
    }
}

/// Connected components over the passing matches, best group first.
fn group(matches: Vec<ShapeMatch>) -> Vec<ShapeGroup> {
    let mut parent: BTreeMap<String, String> = BTreeMap::new();
    fn find(parent: &mut BTreeMap<String, String>, x: &str) -> String {
        let mut cur = x.to_string();
        loop {
            let p = parent.entry(cur.clone()).or_insert_with(|| cur.clone()).clone();
            if p == cur {
                return cur;
            }
            cur = p;
        }
    }
    for m in &matches {
        let (ra, rb) = (
            find(&mut parent, &m.a.label()),
            find(&mut parent, &m.b.label()),
        );
        if ra != rb {
            parent.insert(ra, rb);
        }
    }
    let mut buckets: BTreeMap<String, Vec<ShapeMatch>> = BTreeMap::new();
    for m in matches {
        let root = find(&mut parent, &m.a.label());
        buckets.entry(root).or_default().push(m);
    }
    let mut groups: Vec<ShapeGroup> = buckets
        .into_values()
        .map(|pairs| {
            let members: BTreeSet<String> = pairs
                .iter()
                .flat_map(|m| [m.a.label(), m.b.label()])
                .collect();
            let best = pairs
                .iter()
                .max_by(|x, y| {
                    x.score
                        .partial_cmp(&y.score)
                        .unwrap_or(std::cmp::Ordering::Equal)
                        .then(x.shared.len().cmp(&y.shared.len()))
                })
                .cloned()
                .expect("a bucket is built from at least one match");
            ShapeGroup {
                members: members.into_iter().collect(),
                top_score: best.score,
                best,
                pairs,
            }
        })
        .collect();
    groups.sort_by(|x, y| {
        y.top_score
            .partial_cmp(&x.top_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(y.members.len().cmp(&x.members.len()))
            .then(x.members.join(" ").cmp(&y.members.join(" ")))
    });
    groups
}

// ── Rendering ─────────────────────────────────────────────────────────────────

pub fn render_shape(c: &ShapeCensus, limit: usize) -> String {
    let mut s = String::new();
    s.push_str("scope: ");
    if c.scope.include_prefixes.is_empty() {
        s.push_str("all paths");
    } else {
        s.push_str(&c.scope.include_prefixes.join(", "));
    }
    s.push_str(" minus segments ");
    s.push_str(&c.scope.exclude_segments.join(" "));
    s.push_str(&format!(
        "\ngates: >={} named fields, >={} shared keys, one shared key held by <={} types\n",
        c.options.min_fields, c.options.min_shared, c.options.rare_df
    ));
    s.push_str(&format!(
        "\ntypes carrying a field signature   : {}\n\
         cross-crate candidate pairs        : {}\n\
         pairs past both gates              : {}\n\
         pairs at score >= {:<4}             : {}   in {} group(s)\n\n",
        c.types_with_fields,
        c.candidate_pairs,
        c.scored_pairs,
        c.options.threshold,
        c.matched_pairs,
        c.groups.len()
    ));
    for g in c.groups.iter().take(limit) {
        s.push_str(&format!(
            "{:.3}  {}\n",
            g.top_score,
            g.members.join("  ==  ")
        ));
        s.push_str(&format!(
            "       {}:{}\n       {}:{}\n",
            g.best.a.file, g.best.a.line, g.best.b.file, g.best.b.line
        ));
        let shared: Vec<String> = g.best.shared.iter().map(FieldKey::render).collect();
        s.push_str(&format!("       shared: {}\n", shared.join(", ")));
    }
    if c.groups.len() > limit {
        s.push_str(&format!(
            "\n... {} more (--limit 0 for all)\n",
            c.groups.len() - limit
        ));
    }
    s.push_str(
        "\nNo type name was compared to produce this — the evidence is the fields.\n\
         A shared shape is not a shared concept: this DISCOVERS, a human DISPOSITIONS.\n\
         Duplicated NAME is `svrn code converge census`; duplicated ROLE is `converge roles`.\n",
    );
    s
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn sym(pkg: &str, desc: &str, file: &str, line: i32) -> ScipSymbolRecord {
        ScipSymbolRecord {
            name: leaf_name(desc).to_string(),
            qualified_name: format!("rust-analyzer cargo {pkg} 0.1.0 {desc}"),
            kind: "unknown".into(),
            file_path: file.into(),
            line_start: line,
            line_end: line + 5,
            language: "rust".into(),
        }
    }

    /// A field declaration edge: the field symbol references its type.
    fn field_ty(
        pkg: &str,
        field_desc: &str,
        ty_pkg: &str,
        ty_desc: &str,
        file: &str,
        col: i32,
    ) -> ScipRefRecord {
        ScipRefRecord {
            caller_symbol: field_desc.into(),
            callee_symbol: ty_desc.into(),
            caller_qualified: format!("rust-analyzer cargo {pkg} 0.1.0 {field_desc}"),
            callee_qualified: format!("rust-analyzer cargo {ty_pkg} 0.1.0 {ty_desc}"),
            file_path: file.into(),
            line: 1,
            start_col: col,
            end_line: -1,
            end_col: -1,
            ref_kind: "direct".into(),
        }
    }

    /// Types with fields nothing else shares. IDF is a corpus statistic: with
    /// two records in the world every key has `df == n` and weighs exactly
    /// zero, so a fixture without a population measures nothing. This is the
    /// population — 60 types over three crates, every field name unique, so it
    /// contributes no candidate pair of its own.
    fn background(syms: &mut Vec<ScipSymbolRecord>, refs: &mut Vec<ScipRefRecord>) {
        for i in 0..60 {
            let pkg = format!("bg{}", i % 3);
            let owner = format!("m/Bg{i}#");
            let file = format!("sovereign/crates/{pkg}/src/m{i}.rs");
            syms.push(sym(&pkg, &owner, &file, 1));
            for f in 0..3 {
                let fd = format!("{owner}bg{i}_{f}.");
                syms.push(sym(&pkg, &fd, &file, 1));
                refs.push(field_ty(&pkg, &fd, "alloc", "string/String#", &file, 10));
            }
        }
    }

    /// Two crates, one concept, two names, three identical fields — the
    /// live specimen this verb exists for, reduced.
    fn citation_fixture() -> (Vec<ScipSymbolRecord>, Vec<ScipRefRecord>) {
        let mut syms = vec![
            sym("core", "icd/ClaimCitation#", "sovereign/crates/core/src/icd.rs", 665),
            sym("desktop", "cmds/DrCitation#", "sovereign/crates/desktop/src/cmds.rs", 779),
        ];
        let mut refs = Vec::new();
        for (pkg, owner, file) in [
            ("core", "icd/ClaimCitation#", "sovereign/crates/core/src/icd.rs"),
            ("desktop", "cmds/DrCitation#", "sovereign/crates/desktop/src/cmds.rs"),
        ] {
            for f in ["evidence_id", "url", "chunk_id"] {
                let fd = format!("{owner}{f}.");
                syms.push(sym(pkg, &fd, file, 1));
                refs.push(field_ty(pkg, &fd, "alloc", "string/String#", file, 10));
            }
        }
        // Noise: a third crate whose fields are the universal scaffolding.
        for i in 0..40 {
            let owner = format!("m/Row{i}#");
            let file = format!("sovereign/crates/noise/src/m{i}.rs");
            syms.push(sym("noise", &owner, &file, 1));
            for f in ["id", "name", "path"] {
                let fd = format!("{owner}{f}.");
                syms.push(sym("noise", &fd, &file, 1));
                refs.push(field_ty("noise", &fd, "alloc", "string/String#", &file, 10));
            }
        }
        (syms, refs)
    }

    /// THE POSITIVE CONTROL (§18.4). If this ever stops holding, the number
    /// the verb prints is not a measurement of anything.
    #[test]
    fn a_renamed_fork_scores_1_0_with_no_name_input() {
        let (syms, refs) = citation_fixture();
        let scope = SourceScope::default();
        let sigs = field_signatures(&syms, &refs, &scope);
        let c = shape_census(&syms, &sigs, &scope, &ShapeOptions::default());

        assert_eq!(c.groups.len(), 1, "the 40 scaffolding rows must not group");
        let g = &c.groups[0];
        assert_eq!(g.members, vec!["core::ClaimCitation", "desktop::DrCitation"]);
        assert!(
            (g.top_score - 1.0).abs() < 1e-9,
            "an identical signature is 1.000, got {}",
            g.top_score
        );
        assert_eq!(g.best.shared.len(), 3);
        // …and the two names share no morpheme a census could have keyed on
        // beyond the head noun, which is exactly why the census missed it.
        assert!(!render_shape(&c, 10).contains("Row0"));
    }

    /// The gate that keeps `{id, name, path}` out. Without the rare-key gate
    /// the 40 noise rows above would be 780 cross-crate pairs at 1.000 —
    /// except they are all in ONE crate, so here the test is the direct one:
    /// a universal signature shared across crates still does not match.
    #[test]
    fn a_match_held_together_only_by_universal_fields_is_not_evidence() {
        let scope = SourceScope::default();
        let mut syms = Vec::new();
        let mut refs = Vec::new();
        for i in 0..40 {
            let pkg = format!("k{}", i % 4);
            let owner = format!("m/Row{i}#");
            let file = format!("sovereign/crates/{pkg}/src/m{i}.rs");
            syms.push(sym(&pkg, &owner, &file, 1));
            for f in ["id", "name", "path"] {
                let fd = format!("{owner}{f}.");
                syms.push(sym(&pkg, &fd, &file, 1));
                refs.push(field_ty(&pkg, &fd, "alloc", "string/String#", &file, 10));
            }
        }
        let sigs = field_signatures(&syms, &refs, &scope);
        let c = shape_census(&syms, &sigs, &scope, &ShapeOptions::default());
        assert!(c.candidate_pairs > 0, "they do share keys");
        assert_eq!(
            c.scored_pairs, 0,
            "…but no shared key is rare, so none is evidence"
        );
        assert!(c.groups.is_empty());
    }

    /// `max` in the denominator, and the reason it is there: a small struct
    /// wholly inside a large one is a coincidence, and containment cannot see
    /// the difference.
    #[test]
    fn a_small_signature_swallowed_by_a_large_one_is_ranked_below_a_fork() {
        let scope = SourceScope::default();
        let mut syms = vec![
            sym("a", "m/Small#", "sovereign/crates/a/src/m.rs", 1),
            sym("b", "n/Large#", "sovereign/crates/b/src/n.rs", 1),
        ];
        let mut refs = Vec::new();
        background(&mut syms, &mut refs);
        let add = |syms: &mut Vec<ScipSymbolRecord>,
                       refs: &mut Vec<ScipRefRecord>,
                       pkg: &str,
                       owner: &str,
                       file: &str,
                       fields: &[&str]| {
            for f in fields {
                let fd = format!("{owner}{f}.");
                syms.push(sym(pkg, &fd, file, 1));
                refs.push(field_ty(pkg, &fd, "alloc", "string/String#", file, 10));
            }
        };
        let rare = ["alpha_id", "beta_id", "gamma_id"];
        add(&mut syms, &mut refs, "a", "m/Small#", "sovereign/crates/a/src/m.rs", &rare);
        let big: Vec<String> = (0..12).map(|i| format!("extra{i}")).collect();
        let mut large: Vec<&str> = rare.to_vec();
        large.extend(big.iter().map(String::as_str));
        add(&mut syms, &mut refs, "b", "n/Large#", "sovereign/crates/b/src/n.rs", &large);

        let sigs = field_signatures(&syms, &refs, &scope);
        let opts = ShapeOptions {
            threshold: 0.0,
            ..ShapeOptions::default()
        };
        let c = shape_census(&syms, &sigs, &scope, &opts);
        let m = &c.groups[0].best;
        assert!(
            (m.containment - 1.0).abs() < 1e-9,
            "Small is wholly contained in Large: containment {}",
            m.containment
        );
        assert!(
            m.score < ShapeOptions::DEFAULT_THRESHOLD,
            "…and the symmetric score must put it below the default threshold, got {}",
            m.score
        );
    }

    #[test]
    fn a_field_type_is_the_symbols_it_references_in_source_order() {
        let scope = SourceScope::default();
        let file = "sovereign/crates/a/src/m.rs";
        let syms = vec![
            sym("a", "m/Census#", file, 1),
            sym("a", "m/Census#rows.", file, 2),
            sym("a", "m/Census#total.", file, 3),
        ];
        let refs = vec![
            // `rows: Vec<CensusRow>` — two edges, Vec first by column.
            field_ty("a", "m/Census#rows.", "a", "m/CensusRow#", file, 18),
            field_ty("a", "m/Census#rows.", "alloc", "vec/Vec#", file, 14),
        ];
        let sigs = field_signatures(&syms, &refs, &scope);
        let keys = &sigs["rust-analyzer cargo a 0.1.0 m/Census#"];
        let rows = keys.iter().find(|k| k.name == "rows").unwrap();
        assert_eq!(
            rows.ty, "Vec<CensusRow",
            "unclosed on purpose — an ordered occurrence list is not a parse tree"
        );
        // A primitive references no symbol. That is reported as "unknown
        // type", never guessed at (§18.3).
        let total = keys.iter().find(|k| k.name == "total").unwrap();
        assert_eq!(total.ty, "");
        assert_eq!(total.render(), "total: ?");
    }

    /// Names-only is a legitimate mode (pass no refs) and it costs more
    /// adjudication, which is the measurement that justified carrying types.
    #[test]
    fn dropping_field_types_admits_pairs_the_typed_signature_rejects() {
        let scope = SourceScope::default();
        let file_a = "sovereign/crates/a/src/m.rs";
        let file_b = "sovereign/crates/b/src/n.rs";
        let mut syms = vec![sym("a", "m/A#", file_a, 1), sym("b", "n/B#", file_b, 1)];
        let mut refs = Vec::new();
        background(&mut syms, &mut refs);
        for (pkg, owner, file, ty) in [
            ("a", "m/A#", file_a, "string/String#"),
            ("b", "n/B#", file_b, "collections/HashMap#"),
        ] {
            for f in ["alpha_id", "beta_id", "gamma_id"] {
                let fd = format!("{owner}{f}.");
                syms.push(sym(pkg, &fd, file, 1));
                refs.push(field_ty(pkg, &fd, "std", ty, file, 10));
            }
        }
        let opts = ShapeOptions::default();
        let typed = shape_census(&syms, &field_signatures(&syms, &refs, &scope), &scope, &opts);
        assert_eq!(typed.matched_pairs, 0, "same names, different types");
        let named = shape_census(&syms, &field_signatures(&syms, &[], &scope), &scope, &opts);
        assert_eq!(named.matched_pairs, 1, "names alone cannot tell them apart");
    }

    #[test]
    fn out_of_scope_and_colocated_test_types_carry_no_signature() {
        let scope = SourceScope::default();
        let syms = vec![
            sym("a", "m/Real#", "sovereign/crates/a/src/m.rs", 1),
            sym("a", "m/Real#x.", "sovereign/crates/a/src/m.rs", 2),
            sym("a", "m/Real#y.", "sovereign/crates/a/src/m.rs", 3),
            sym("a", "m/tests/Fixture#", "sovereign/crates/a/src/m.rs", 9),
            sym("a", "m/tests/Fixture#x.", "sovereign/crates/a/src/m.rs", 10),
            sym("a", "m/tests/Fixture#y.", "sovereign/crates/a/src/m.rs", 11),
            sym("a", "e/Helper#", "sovereign/crates/a/tests/e2e.rs", 1),
            sym("a", "e/Helper#x.", "sovereign/crates/a/tests/e2e.rs", 2),
            sym("a", "e/Helper#y.", "sovereign/crates/a/tests/e2e.rs", 3),
        ];
        let sigs = field_signatures(&syms, &[], &scope);
        assert_eq!(sigs.len(), 1);
        assert!(sigs.contains_key("rust-analyzer cargo a 0.1.0 m/Real#"));
    }
}
