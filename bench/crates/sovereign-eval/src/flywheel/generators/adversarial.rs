// SPDX-License-Identifier: AGPL-3.0-or-later
//! I2 — the adversarial corruption generator: the Stream B core
//! (`VERIFIER_V0.md` §3, `research/verifier-v0/findings/STREAM_B_DESIGN.md`).
//!
//! One implementation, two consumers:
//! - **Eval probes** — [`AdversarialGenerator`] lowers each constructed case
//!   into a [`Probe`] (corrupted → should-not-confirm / `AbsentAdjacent`;
//!   grounded → `Present` with witness), so the existing flywheel verify /
//!   score / capture chain applies unchanged.
//! - **Training pairs** — `svrn bench verifier export` renders the SAME cases
//!   ([`generate_cases`]) to Stream B JSONL: claim, evidence window,
//!   constructed label, corruption kind, site witness, and **span offsets from
//!   day one** (spec §10 lever 2 — trivial now, expensive to retrofit).
//!
//! **Labels by construction.** Every case's label is fixed mechanically before
//! any teacher model writes a word: each corruption is checkable at its known
//! corruption site ([`SiteWitness`] / [`validate_site`]), validated at
//! generation here and re-validated at export against the production checker
//! (`sovereign-core` `value_present_in_chunks`, of which
//! [`det_checks::value_present`] is the pinned port). A teacher that disagrees
//! with a constructed label discards the pair — it never relabels.
//!
//! **Substrate.** The generator stays pure (serde + std + rand): it consumes a
//! HARVEST ARTIFACT ([`HarvestFile`], `claims.json`) — claims already in the
//! production register (extracted through the gate's own
//! `extract_claim_list` seam) with their sealed evidence windows inline —
//! exactly as the I1 generator consumes `atlas/atoms.json`. `(n, seed)` is
//! reproducible bit-for-bit.

use std::path::Path;

use rand::rngs::StdRng;
use rand::seq::SliceRandom;
use rand::{RngCore, SeedableRng};
use serde::{Deserialize, Serialize};

use crate::chaos_monkey::PressureKind;
use crate::flywheel::det_checks::{contains_ci, value_present};
use crate::flywheel::generators::corpus::{claim_query, salient_terms};
use crate::flywheel::probe::{AbsentKind, Oracle, Probe, ProbeSource};
use crate::flywheel::Generator;

/// Schema version of the harvest artifact this generator consumes.
pub const HARVEST_SCHEMA_VERSION: u32 = 1;

/// The harvest artifact (`claims.json`): production-register claims with their
/// sealed evidence windows, plus the optional side tables some corruption
/// kinds need. Self-contained — the generator never touches an index.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HarvestFile {
    pub schema_version: u32,
    /// Provenance: the installed bench corpus the chunks came from
    /// (`chaos-saltgrass` / `chaos-secret-agent`).
    pub corpus_id: String,
    pub items: Vec<HarvestItem>,
    /// Typed entity clusters for entity-swap corruptions (mined from
    /// `named-clusters.json`-style extractions). Each cluster is ONE entity's
    /// surface forms; `etype` groups clusters for same-type swaps.
    #[serde(default)]
    pub entities: Vec<EntityCluster>,
    /// Adjacent-document sources for distractor-absorption corruptions
    /// (e.g. the meridian postmortem beside the Saltgrass ledger).
    #[serde(default)]
    pub distractors: Vec<DistractorDoc>,
}

/// One production-register claim and the evidence window it was extracted
/// against.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HarvestItem {
    pub id: String,
    /// The question the claim's answer addressed — register provenance.
    pub question: String,
    /// One claim as `extract_claim_list` emitted it.
    pub claim: String,
    /// The sealed evidence window (chunk texts) the claim grounds against.
    pub evidence_chunks: Vec<String>,
    #[serde(default)]
    pub evidence_chunk_ids: Vec<String>,
}

/// One entity's surface forms, typed for same-type swapping.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EntityCluster {
    pub etype: String,
    pub surfaces: Vec<String>,
}

/// An adjacent document a distractor-absorption corruption can absorb from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DistractorDoc {
    pub id: String,
    pub text: String,
}

/// The constructed label — fixed mechanically, never by a model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaseLabel {
    Grounded,
    Ungrounded,
}

/// The corruption taxonomy (spec §3 table) plus the hard-grounded half — the
/// timidity red line: the model must learn confident support, not just
/// suspicion, so supported cases are half the job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CorruptionKind {
    /// Value bound to the wrong same-type entity (observed in prod).
    EntitySwap,
    /// Numeric / spelled-number / ordinal perturbation (numeric_audit escapes).
    NumberPerturb,
    /// Real polarity rewrite (not a "categorically false" prefix).
    NegationFlip,
    /// Two true fragments from different chunks fused by a causal connective
    /// neither supports.
    CrossChunkChimera,
    /// Confusion-table garble (0/O, 1/l, rn/m …) of a grounded surface form.
    OcrGarble,
    /// A fact absorbed from an adjacent document as if grounded here.
    DistractorAbsorption,
    /// A plausible specific appended that the window never asserts.
    UnsupportedAddition,
    /// The claim exactly as extracted — the base positive class.
    Verbatim,
    /// Register-variant framing of a grounded claim (paraphrase pressure).
    Reframe,
    /// Two grounded fragments conjoined over the fused window — multi-hop
    /// support without any fabricated relation.
    MultiHopConjunction,
}

impl CorruptionKind {
    pub fn label(self) -> CaseLabel {
        match self {
            CorruptionKind::EntitySwap
            | CorruptionKind::NumberPerturb
            | CorruptionKind::NegationFlip
            | CorruptionKind::CrossChunkChimera
            | CorruptionKind::OcrGarble
            | CorruptionKind::DistractorAbsorption
            | CorruptionKind::UnsupportedAddition => CaseLabel::Ungrounded,
            CorruptionKind::Verbatim
            | CorruptionKind::Reframe
            | CorruptionKind::MultiHopConjunction => CaseLabel::Grounded,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            CorruptionKind::EntitySwap => "entity_swap",
            CorruptionKind::NumberPerturb => "number_perturb",
            CorruptionKind::NegationFlip => "negation_flip",
            CorruptionKind::CrossChunkChimera => "cross_chunk_chimera",
            CorruptionKind::OcrGarble => "ocr_garble",
            CorruptionKind::DistractorAbsorption => "distractor_absorption",
            CorruptionKind::UnsupportedAddition => "unsupported_addition",
            CorruptionKind::Verbatim => "verbatim",
            CorruptionKind::Reframe => "reframe",
            CorruptionKind::MultiHopConjunction => "multi_hop_conjunction",
        }
    }
}

/// Byte-offset range `[start, end)` into [`StreamBCase::claim`] marking a
/// constructed edit — the span axis the export carries from day one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

/// The typed mechanical check that FIXES a case's label at its corruption
/// site. [`validate_site`] re-runs it; the export path re-runs it again with
/// the production checker. The witness is the referee — teachers never are.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "check", rename_all = "snake_case")]
pub enum SiteWitness {
    /// The injected surface must be ABSENT from the evidence window (and, when
    /// `original` is present, the replaced surface must be PRESENT — the
    /// corruption displaced something real).
    InjectedAbsent {
        injected: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        original: Option<String>,
    },
    /// The polarity marker is in the claim while the ORIGINAL claim's terms
    /// ground in the window — the site asserts the opposite polarity.
    PolarityFlip {
        marker: String,
        original_terms: Vec<String>,
    },
    /// Fragment A grounds in `evidence_chunks[..boundary]`, fragment B in
    /// `evidence_chunks[boundary..]`, and the causal connective in neither.
    Chimera {
        connective: String,
        frag_a_terms: Vec<String>,
        frag_b_terms: Vec<String>,
        boundary: usize,
    },
    /// The absorbed value grounds ONLY in the distractor document, never in
    /// the evidence window.
    DistractorOnly {
        value: String,
        distractor_id: String,
        distractor_text: String,
    },
    /// Grounded half: every term grounds in the window.
    Supported { terms: Vec<String> },
}

/// One constructed Stream B case — the unit both consumers share.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StreamBCase {
    pub id: String,
    pub corpus_id: String,
    pub source_item_id: String,
    pub kind: CorruptionKind,
    pub label: CaseLabel,
    /// The (possibly corrupted) claim text.
    pub claim: String,
    /// Register provenance: the question the source claim answered.
    pub question: String,
    pub evidence_chunks: Vec<String>,
    pub evidence_chunk_ids: Vec<String>,
    /// Constructed-edit spans into `claim` (empty for the grounded half —
    /// there is nothing false to localize).
    pub spans: Vec<Span>,
    pub witness: SiteWitness,
}

// ---------------------------------------------------------------------------
// The corruption-site contract
// ---------------------------------------------------------------------------

/// Mechanically re-check a case's label at its corruption site. Sibling of
/// [`crate::flywheel::case::validate_fairness`] (which guards the PROBE
/// lowering); this guards the CASE itself — witness/label coherence, span
/// sanity, and the site condition. Runs at generation here; the export verb
/// runs it again (plus the production `value_present_in_chunks`) at render.
pub fn validate_site(case: &StreamBCase) -> Result<(), String> {
    if case.label != case.kind.label() {
        return Err(format!(
            "case `{}`: label {:?} contradicts kind {:?} (labels are by construction)",
            case.id, case.label, case.kind
        ));
    }
    for s in &case.spans {
        if s.start >= s.end
            || s.end > case.claim.len()
            || !case.claim.is_char_boundary(s.start)
            || !case.claim.is_char_boundary(s.end)
        {
            return Err(format!(
                "case `{}`: span {}..{} out of bounds / off char boundary for claim of {} bytes",
                case.id,
                s.start,
                s.end,
                case.claim.len()
            ));
        }
    }
    if case.label == CaseLabel::Ungrounded && case.spans.is_empty() {
        return Err(format!(
            "case `{}`: corrupted case must localize its corruption site (spec §10 span axis)",
            case.id
        ));
    }
    let ev = &case.evidence_chunks;
    match (&case.label, &case.witness) {
        (CaseLabel::Ungrounded, SiteWitness::InjectedAbsent { injected, original }) => {
            if !contains_ci(&case.claim, injected) {
                return Err(format!(
                    "case `{}`: injected value `{injected}` not in the claim",
                    case.id
                ));
            }
            if value_present(injected, ev) {
                return Err(format!(
                    "case `{}`: injected value `{injected}` is present in the evidence window — not a constructed corruption",
                    case.id
                ));
            }
            if let Some(orig) = original {
                if !value_present(orig, ev) {
                    return Err(format!(
                        "case `{}`: displaced original `{orig}` does not ground in the window",
                        case.id
                    ));
                }
            }
            Ok(())
        }
        (
            CaseLabel::Ungrounded,
            SiteWitness::PolarityFlip {
                marker,
                original_terms,
            },
        ) => {
            if !contains_ci(&case.claim, marker) {
                return Err(format!(
                    "case `{}`: polarity marker `{marker}` not in the claim",
                    case.id
                ));
            }
            if original_terms.is_empty() || !original_terms.iter().all(|t| value_present(t, ev)) {
                return Err(format!(
                    "case `{}`: original claim's terms must ground at the site (the site asserts the opposite polarity)",
                    case.id
                ));
            }
            Ok(())
        }
        (
            CaseLabel::Ungrounded,
            SiteWitness::Chimera {
                connective,
                frag_a_terms,
                frag_b_terms,
                boundary,
            },
        ) => {
            if *boundary == 0 || *boundary >= ev.len() {
                return Err(format!(
                    "case `{}`: chimera boundary {boundary} does not split {} chunks",
                    case.id,
                    ev.len()
                ));
            }
            if ev.iter().any(|c| contains_ci(c, connective)) {
                return Err(format!(
                    "case `{}`: connective `{connective}` appears in a chunk — the fused relation might be supported",
                    case.id
                ));
            }
            let (a, b) = ev.split_at(*boundary);
            if frag_a_terms.is_empty() || !frag_a_terms.iter().all(|t| value_present(t, a)) {
                return Err(format!(
                    "case `{}`: fragment A does not ground in its own chunks",
                    case.id
                ));
            }
            if frag_b_terms.is_empty() || !frag_b_terms.iter().all(|t| value_present(t, b)) {
                return Err(format!(
                    "case `{}`: fragment B does not ground in its own chunks",
                    case.id
                ));
            }
            Ok(())
        }
        (
            CaseLabel::Ungrounded,
            SiteWitness::DistractorOnly {
                value,
                distractor_text,
                ..
            },
        ) => {
            if !value_present(value, std::slice::from_ref(&distractor_text.to_string())) {
                return Err(format!(
                    "case `{}`: absorbed value does not ground in the distractor doc",
                    case.id
                ));
            }
            if value_present(value, ev) {
                return Err(format!(
                    "case `{}`: absorbed value grounds in the evidence window — not a distractor absorption",
                    case.id
                ));
            }
            Ok(())
        }
        (CaseLabel::Grounded, SiteWitness::Supported { terms }) => {
            if terms.is_empty() || !terms.iter().all(|t| value_present(t, ev)) {
                return Err(format!(
                    "case `{}`: grounded case's terms must ALL ground in the window",
                    case.id
                ));
            }
            Ok(())
        }
        (label, witness) => Err(format!(
            "case `{}`: witness {witness:?} is incoherent with label {label:?}",
            case.id
        )),
    }
}

// ---------------------------------------------------------------------------
// Generation
// ---------------------------------------------------------------------------

const UNGROUNDED_KINDS: &[CorruptionKind] = &[
    CorruptionKind::EntitySwap,
    CorruptionKind::NumberPerturb,
    CorruptionKind::NegationFlip,
    CorruptionKind::CrossChunkChimera,
    CorruptionKind::OcrGarble,
    CorruptionKind::DistractorAbsorption,
    CorruptionKind::UnsupportedAddition,
];

const GROUNDED_KINDS: &[CorruptionKind] = &[
    CorruptionKind::Verbatim,
    CorruptionKind::Reframe,
    CorruptionKind::MultiHopConjunction,
];

/// Load a harvest artifact from a file, or a directory holding `claims.json`.
pub fn load_harvest(path: &Path) -> Result<HarvestFile, String> {
    let file = if path.is_dir() {
        path.join("claims.json")
    } else {
        path.to_path_buf()
    };
    let raw = std::fs::read_to_string(&file)
        .map_err(|e| format!("could not read harvest artifact {file:?}: {e}"))?;
    let h: HarvestFile = serde_json::from_str(&raw)
        .map_err(|e| format!("harvest artifact {file:?} is not valid: {e}"))?;
    if h.schema_version != HARVEST_SCHEMA_VERSION {
        return Err(format!(
            "harvest artifact schema_version {} != supported {HARVEST_SCHEMA_VERSION}",
            h.schema_version
        ));
    }
    Ok(h)
}

/// Generate up to `n` validated Stream B cases, alternating constructed labels
/// for the ~50/50 class balance the benchmarks score. Deterministic: same
/// `(n, seed, harvest)` → bit-for-bit identical output. Only cases that pass
/// [`validate_site`] are emitted — fairness by construction, not by hope.
pub fn generate_cases(n: usize, seed: u64, harvest: &HarvestFile) -> Vec<StreamBCase> {
    let mut items: Vec<&HarvestItem> = harvest.items.iter().collect();
    items.sort_by(|a, b| a.id.cmp(&b.id));
    let mut rng = StdRng::seed_from_u64(seed);
    items.shuffle(&mut rng);
    if items.is_empty() {
        return Vec::new();
    }

    let mut out: Vec<StreamBCase> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let (mut u_cursor, mut g_cursor, mut item_cursor) = (0usize, 0usize, 0usize);
    // Attempt budget: generous enough to cycle every (kind, item) pairing a
    // few times, bounded so an unsatisfiable label can never spin forever.
    let budget = n.saturating_mul(64).max(items.len() * 16);
    let mut attempts = 0usize;
    while out.len() < n && attempts < budget {
        attempts += 1;
        let want = if out.len() % 2 == 0 {
            CaseLabel::Ungrounded
        } else {
            CaseLabel::Grounded
        };
        let kind = match want {
            CaseLabel::Ungrounded => {
                let k = UNGROUNDED_KINDS[u_cursor % UNGROUNDED_KINDS.len()];
                u_cursor += 1;
                k
            }
            CaseLabel::Grounded => {
                let k = GROUNDED_KINDS[g_cursor % GROUNDED_KINDS.len()];
                g_cursor += 1;
                k
            }
        };
        let item = items[item_cursor % items.len()];
        item_cursor += 1;
        let Some(case) = build_case(kind, item, &items, item_cursor, harvest, &mut rng) else {
            continue;
        };
        if !seen.insert(case.id.clone()) {
            continue;
        }
        if let Err(e) = validate_site(&case) {
            // A construction that fails its own site contract is a generator
            // bug in the making — surface it, don't silently swallow.
            eprintln!("[i2] dropped case failing site contract: {e}");
            continue;
        }
        out.push(case);
    }
    out
}

fn build_case(
    kind: CorruptionKind,
    item: &HarvestItem,
    items: &[&HarvestItem],
    cursor: usize,
    harvest: &HarvestFile,
    rng: &mut StdRng,
) -> Option<StreamBCase> {
    let case = match kind {
        CorruptionKind::EntitySwap => entity_swap(item, harvest, rng),
        CorruptionKind::NumberPerturb => number_perturb(item, rng),
        CorruptionKind::NegationFlip => negation_flip(item),
        CorruptionKind::CrossChunkChimera => {
            let partner = pick_partner(items, cursor, item)?;
            chimera(item, partner)
        }
        CorruptionKind::OcrGarble => ocr_garble(item),
        CorruptionKind::DistractorAbsorption => distractor_absorption(item, harvest, rng),
        CorruptionKind::UnsupportedAddition => unsupported_addition(item, rng),
        CorruptionKind::Verbatim => verbatim(item),
        CorruptionKind::Reframe => reframe(item, rng),
        CorruptionKind::MultiHopConjunction => {
            let partner = pick_partner(items, cursor, item)?;
            multi_hop(item, partner)
        }
    };
    // Provenance is stamped once, here — builders stay corpus-agnostic.
    case.map(|mut c| {
        c.corpus_id = harvest.corpus_id.clone();
        c
    })
}

/// First item after `start` with an evidence-chunk set disjoint from `me` —
/// the pair kinds need genuinely different sites.
fn pick_partner<'a>(
    items: &[&'a HarvestItem],
    start: usize,
    me: &HarvestItem,
) -> Option<&'a HarvestItem> {
    (0..items.len())
        .map(|k| items[(start + k) % items.len()])
        .find(|c| {
            c.id != me.id
                && (c.evidence_chunk_ids.is_empty()
                    || me.evidence_chunk_ids.is_empty()
                    || c.evidence_chunk_ids
                        .iter()
                        .all(|id| !me.evidence_chunk_ids.contains(id)))
        })
}

// ---- corruption builders ---------------------------------------------------

fn entity_swap(item: &HarvestItem, harvest: &HarvestFile, rng: &mut StdRng) -> Option<StreamBCase> {
    for (ci, cluster) in harvest.entities.iter().enumerate() {
        for surface in &cluster.surfaces {
            let Some((s, e)) = find_word_ci(&item.claim, surface) else {
                continue;
            };
            if !value_present(surface, &item.evidence_chunks) {
                continue;
            }
            // Same-type surfaces from OTHER clusters, absent from both the
            // window (the site condition) and the claim (no self-collision).
            let cands: Vec<&String> = harvest
                .entities
                .iter()
                .enumerate()
                .filter(|(cj, c2)| *cj != ci && c2.etype == cluster.etype)
                .flat_map(|(_, c2)| c2.surfaces.iter())
                .filter(|r| {
                    !value_present(r, &item.evidence_chunks)
                        && find_word_ci(&item.claim, r).is_none()
                })
                .collect();
            if cands.is_empty() {
                continue;
            }
            let injected = cands[pick(rng, cands.len())].clone();
            let claim = splice(&item.claim, s, e, &injected);
            return Some(make_case(
                item,
                CorruptionKind::EntitySwap,
                claim,
                vec![Span {
                    start: s,
                    end: s + injected.len(),
                }],
                SiteWitness::InjectedAbsent {
                    injected,
                    original: Some(surface.clone()),
                },
            ));
        }
    }
    None
}

const NUMBER_WORDS: &[&str] = &[
    "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten", "eleven",
    "twelve", "twenty", "thirty", "forty", "fifty", "hundred", "thousand",
];
const ORDINAL_WORDS: &[&str] = &[
    "first",
    "second",
    "third",
    "fourth",
    "fifth",
    "sixth",
    "seventh",
    "eighth",
    "ninth",
    "tenth",
    "eleventh",
    "twelfth",
    "thirteenth",
    "fourteenth",
    "fifteenth",
    "sixteenth",
    "seventeenth",
    "eighteenth",
    "nineteenth",
    "twentieth",
];

fn number_perturb(item: &HarvestItem, rng: &mut StdRng) -> Option<StreamBCase> {
    // Spelled numbers / ordinals first (bench prose is wordy), digits second.
    for table in [NUMBER_WORDS, ORDINAL_WORDS] {
        for word in table {
            let Some((s, e)) = find_word_ci(&item.claim, word) else {
                continue;
            };
            if !value_present(word, &item.evidence_chunks) {
                continue;
            }
            let cands: Vec<&&str> = table
                .iter()
                .filter(|c| {
                    **c != *word
                        && !value_present(c, &item.evidence_chunks)
                        && find_word_ci(&item.claim, c).is_none()
                })
                .collect();
            if cands.is_empty() {
                continue;
            }
            let injected = cands[pick(rng, cands.len())].to_string();
            let claim = splice(&item.claim, s, e, &injected);
            return Some(make_case(
                item,
                CorruptionKind::NumberPerturb,
                claim,
                vec![Span {
                    start: s,
                    end: s + injected.len(),
                }],
                SiteWitness::InjectedAbsent {
                    injected,
                    original: Some(word.to_string()),
                },
            ));
        }
    }
    // Digit runs (length ≥2 so a bare single digit can't false-positive its
    // way through substring checks).
    let bytes = item.claim.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let s = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            let e = i;
            if e - s < 2 {
                continue;
            }
            let run = &item.claim[s..e];
            if !item.evidence_chunks.iter().any(|c| c.contains(run)) {
                continue;
            }
            let last = bytes[e - 1] - b'0';
            let delta = 1 + (rng.next_u64() % 8) as u8;
            let new_last = (last + delta) % 10;
            let injected = format!("{}{}", &run[..run.len() - 1], new_last);
            if item.evidence_chunks.iter().any(|c| c.contains(&injected)) {
                continue;
            }
            let claim = splice(&item.claim, s, e, &injected);
            let end = s + injected.len();
            return Some(make_case(
                item,
                CorruptionKind::NumberPerturb,
                claim,
                vec![Span { start: s, end }],
                SiteWitness::InjectedAbsent {
                    injected,
                    original: Some(run.to_string()),
                },
            ));
        } else {
            i += 1;
        }
    }
    None
}

/// Real polarity rewrites — auxiliary negations and modal flips, never a
/// "categorically false:" prefix (the spec calls that out as a non-rewrite).
const NEGATION_FLIPS: &[(&str, &str)] = &[
    (" was ", " was not "),
    (" were ", " were not "),
    (" is ", " is not "),
    (" are ", " are not "),
    (" has ", " has not "),
    (" had ", " had not "),
    (" did ", " did not "),
    (" must ", " need not "),
    (" can ", " cannot "),
    (" always ", " never "),
];

fn negation_flip(item: &HarvestItem) -> Option<StreamBCase> {
    let original_terms = salient_terms(&item.claim, 3);
    if original_terms.is_empty()
        || !original_terms
            .iter()
            .all(|t| value_present(t, &item.evidence_chunks))
    {
        return None; // the original must ground for the flip to be a clean label
    }
    for (pat, repl) in NEGATION_FLIPS {
        if let Some(pos) = item.claim.find(pat) {
            let claim = item.claim.replacen(pat, repl, 1);
            return Some(make_case(
                item,
                CorruptionKind::NegationFlip,
                claim,
                vec![Span {
                    start: pos,
                    end: pos + repl.len(),
                }],
                SiteWitness::PolarityFlip {
                    marker: repl.trim().to_string(),
                    original_terms,
                },
            ));
        }
    }
    None
}

const CHIMERA_CONNECTIVE: &str = "which is why";

fn chimera(a: &HarvestItem, b: &HarvestItem) -> Option<StreamBCase> {
    let frag_a_terms = salient_terms(&a.claim, 3);
    let frag_b_terms = salient_terms(&b.claim, 3);
    if frag_a_terms.is_empty()
        || frag_b_terms.is_empty()
        || !frag_a_terms
            .iter()
            .all(|t| value_present(t, &a.evidence_chunks))
        || !frag_b_terms
            .iter()
            .all(|t| value_present(t, &b.evidence_chunks))
    {
        return None;
    }
    let base = trim_period(&a.claim);
    let appended = format!(
        ", {} {}",
        CHIMERA_CONNECTIVE,
        lower_first(trim_period(&b.claim))
    );
    let claim = format!("{base}{appended}.");
    let boundary = a.evidence_chunks.len();
    let mut evidence = a.evidence_chunks.clone();
    evidence.extend(b.evidence_chunks.iter().cloned());
    let mut chunk_ids = a.evidence_chunk_ids.clone();
    chunk_ids.extend(b.evidence_chunk_ids.iter().cloned());
    Some(StreamBCase {
        id: format!(
            "{}:{}+{}",
            CorruptionKind::CrossChunkChimera.as_str(),
            a.id,
            b.id
        ),
        corpus_id: String::new(), // stamped by build_case

        source_item_id: a.id.clone(),
        kind: CorruptionKind::CrossChunkChimera,
        label: CaseLabel::Ungrounded,
        spans: vec![Span {
            start: base.len(),
            end: base.len() + appended.len(),
        }],
        claim,
        question: a.question.clone(),
        evidence_chunks: evidence,
        evidence_chunk_ids: chunk_ids,
        witness: SiteWitness::Chimera {
            connective: CHIMERA_CONNECTIVE.to_string(),
            frag_a_terms,
            frag_b_terms,
            boundary,
        },
    })
}

/// Realistic OCR confusions, applied to ONE grounded surface form.
const OCR_CONFUSIONS: &[(&str, &str)] = &[
    ("rn", "m"),
    ("m", "rn"),
    ("l", "1"),
    ("1", "l"),
    ("O", "0"),
    ("0", "O"),
    ("S", "5"),
    ("5", "S"),
    ("B", "8"),
    ("8", "B"),
];

fn ocr_garble(item: &HarvestItem) -> Option<StreamBCase> {
    for (s, e) in words_with_offsets(&item.claim) {
        let word = &item.claim[s..e];
        if word.chars().count() < 4 || !value_present(word, &item.evidence_chunks) {
            continue;
        }
        for (pat, repl) in OCR_CONFUSIONS {
            if !word.contains(pat) {
                continue;
            }
            let garbled = word.replacen(pat, repl, 1);
            if garbled == word || value_present(&garbled, &item.evidence_chunks) {
                continue;
            }
            let claim = splice(&item.claim, s, e, &garbled);
            let end = s + garbled.len();
            return Some(make_case(
                item,
                CorruptionKind::OcrGarble,
                claim,
                vec![Span { start: s, end }],
                SiteWitness::InjectedAbsent {
                    injected: garbled,
                    original: Some(word.to_string()),
                },
            ));
        }
    }
    None
}

fn distractor_absorption(
    item: &HarvestItem,
    harvest: &HarvestFile,
    rng: &mut StdRng,
) -> Option<StreamBCase> {
    if harvest.distractors.is_empty() {
        return None;
    }
    let doc = &harvest.distractors[pick(rng, harvest.distractors.len())];
    let sentences: Vec<&str> = doc
        .text
        .split(['.', '!', '?'])
        .map(str::trim)
        .filter(|s| (30..=240).contains(&s.len()) && salient_terms(s, 3).len() >= 2)
        .collect();
    if sentences.is_empty() {
        return None;
    }
    let sentence = sentences[pick(rng, sentences.len())];
    if value_present(sentence, &item.evidence_chunks) {
        return None; // absorbed fact must NOT ground in the window
    }
    let claim = format!("{}.", cap_first(sentence));
    let len = claim.len();
    Some(make_case(
        item,
        CorruptionKind::DistractorAbsorption,
        claim,
        vec![Span { start: 0, end: len }],
        SiteWitness::DistractorOnly {
            value: sentence.to_string(),
            distractor_id: doc.id.clone(),
            distractor_text: doc.text.clone(),
        },
    ))
}

/// Plausible specifics with a checkable injected value — filled from a fixed
/// pool so the site check has one surface to key on.
const ADDITIONS: &[(&str, &str)] = &[
    (", as noted in the harbourmaster's report", "harbourmaster"),
    (", witnessed by the tide clerk", "tide clerk"),
    (", according to the quartermaster's tally", "quartermaster"),
    (", confirmed by the ferry master", "ferry master"),
    (", as the almoner later testified", "almoner"),
];

fn unsupported_addition(item: &HarvestItem, rng: &mut StdRng) -> Option<StreamBCase> {
    let start = pick(rng, ADDITIONS.len());
    for k in 0..ADDITIONS.len() {
        let (clause, value) = ADDITIONS[(start + k) % ADDITIONS.len()];
        if value_present(value, &item.evidence_chunks) || contains_ci(&item.claim, value) {
            continue;
        }
        let base = trim_period(&item.claim);
        let claim = format!("{base}{clause}.");
        return Some(make_case(
            item,
            CorruptionKind::UnsupportedAddition,
            claim,
            vec![Span {
                start: base.len(),
                end: base.len() + clause.len(),
            }],
            SiteWitness::InjectedAbsent {
                injected: value.to_string(),
                original: None,
            },
        ));
    }
    None
}

fn verbatim(item: &HarvestItem) -> Option<StreamBCase> {
    let terms = salient_terms(&item.claim, 3);
    if terms.is_empty()
        || !terms
            .iter()
            .all(|t| value_present(t, &item.evidence_chunks))
    {
        return None; // stay fair by construction — an ungroundable claim is no positive
    }
    Some(make_case(
        item,
        CorruptionKind::Verbatim,
        item.claim.clone(),
        Vec::new(),
        SiteWitness::Supported { terms },
    ))
}

const REFRAMES: &[&str] = &[
    "It is recorded that ",
    "According to the sources, ",
    "The record shows that ",
];

fn reframe(item: &HarvestItem, rng: &mut StdRng) -> Option<StreamBCase> {
    let terms = salient_terms(&item.claim, 3);
    if terms.is_empty()
        || !terms
            .iter()
            .all(|t| value_present(t, &item.evidence_chunks))
    {
        return None;
    }
    let prefix = REFRAMES[pick(rng, REFRAMES.len())];
    let claim = format!("{prefix}{}.", lower_first(trim_period(&item.claim)));
    Some(make_case(
        item,
        CorruptionKind::Reframe,
        claim,
        Vec::new(),
        SiteWitness::Supported { terms },
    ))
}

fn multi_hop(a: &HarvestItem, b: &HarvestItem) -> Option<StreamBCase> {
    let mut terms = salient_terms(&a.claim, 2);
    terms.extend(salient_terms(&b.claim, 2));
    let mut evidence = a.evidence_chunks.clone();
    evidence.extend(b.evidence_chunks.iter().cloned());
    if terms.is_empty() || !terms.iter().all(|t| value_present(t, &evidence)) {
        return None;
    }
    let claim = format!(
        "{}, and {}.",
        trim_period(&a.claim),
        lower_first(trim_period(&b.claim))
    );
    let mut chunk_ids = a.evidence_chunk_ids.clone();
    chunk_ids.extend(b.evidence_chunk_ids.iter().cloned());
    Some(StreamBCase {
        id: format!(
            "{}:{}+{}",
            CorruptionKind::MultiHopConjunction.as_str(),
            a.id,
            b.id
        ),
        corpus_id: String::new(), // stamped by build_case

        source_item_id: a.id.clone(),
        kind: CorruptionKind::MultiHopConjunction,
        label: CaseLabel::Grounded,
        claim,
        question: a.question.clone(),
        evidence_chunks: evidence,
        evidence_chunk_ids: chunk_ids,
        spans: Vec::new(),
        witness: SiteWitness::Supported { terms },
    })
}

// ---- shared assembly -------------------------------------------------------

/// Single-item case assembly. `corpus_id` is stamped by [`build_case`] so
/// builders stay corpus-agnostic.
fn make_case(
    item: &HarvestItem,
    kind: CorruptionKind,
    claim: String,
    spans: Vec<Span>,
    witness: SiteWitness,
) -> StreamBCase {
    StreamBCase {
        id: format!("{}:{}", kind.as_str(), item.id),
        corpus_id: String::new(),
        source_item_id: item.id.clone(),
        kind,
        label: kind.label(),
        claim,
        question: item.question.clone(),
        evidence_chunks: item.evidence_chunks.clone(),
        evidence_chunk_ids: item.evidence_chunk_ids.clone(),
        spans,
        witness,
    }
}

// ---- text helpers ----------------------------------------------------------

fn pick(rng: &mut StdRng, len: usize) -> usize {
    (rng.next_u64() % len as u64) as usize
}

fn splice(s: &str, start: usize, end: usize, with: &str) -> String {
    format!("{}{}{}", &s[..start], with, &s[end..])
}

fn trim_period(s: &str) -> &str {
    s.trim().trim_end_matches(['.', '!', '?']).trim_end()
}

fn lower_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(f) => format!("{}{}", f.to_lowercase(), chars.as_str()),
        None => String::new(),
    }
}

fn cap_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(f) => format!("{}{}", f.to_uppercase(), chars.as_str()),
        None => String::new(),
    }
}

/// ASCII-case-insensitive whole-word search returning BYTE offsets. Manual so
/// byte offsets stay valid on the ORIGINAL string (a `to_lowercase` scan can
/// shift byte positions on non-ASCII text).
fn find_word_ci(hay: &str, word: &str) -> Option<(usize, usize)> {
    if word.is_empty() || word.len() > hay.len() {
        return None;
    }
    let h = hay.as_bytes();
    let w = word.as_bytes();
    for i in 0..=(h.len() - w.len()) {
        let end = i + w.len();
        if !hay.is_char_boundary(i) || !hay.is_char_boundary(end) {
            continue;
        }
        if !h[i..end].eq_ignore_ascii_case(w) {
            continue;
        }
        let prev_ok = i == 0
            || !hay[..i]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_alphanumeric());
        let next_ok = end == hay.len()
            || !hay[end..]
                .chars()
                .next()
                .is_some_and(|c| c.is_alphanumeric());
        if prev_ok && next_ok {
            return Some((i, end));
        }
    }
    None
}

/// Alphanumeric word runs with byte offsets.
fn words_with_offsets(s: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut start: Option<usize> = None;
    for (i, c) in s.char_indices() {
        if c.is_alphanumeric() {
            if start.is_none() {
                start = Some(i);
            }
        } else if let Some(st) = start.take() {
            out.push((st, i));
        }
    }
    if let Some(st) = start {
        out.push((st, s.len()));
    }
    out
}

// ---------------------------------------------------------------------------
// The Generator seam (eval-probe consumer)
// ---------------------------------------------------------------------------

/// Lower a constructed case into the flywheel probe vocabulary. A corrupted
/// claim must NOT be confirmed → `AbsentAdjacent` (the honest move is to
/// refuse/abstain: no in-corpus witness supports the corrupted relation). A
/// grounded claim → `Present` with its supported terms as the witness. Either
/// way [`crate::flywheel::case::validate_fairness`] holds by construction.
pub fn case_to_probe(c: &StreamBCase) -> Probe {
    match c.label {
        CaseLabel::Grounded => {
            let gold = match &c.witness {
                SiteWitness::Supported { terms } => terms.clone(),
                _ => salient_terms(&c.claim, 3),
            };
            Probe {
                id: format!("i2:{}", c.id),
                query: claim_query(&c.claim),
                qtype: PressureKind::Present,
                oracle: Oracle::Witness {
                    gold_keywords: gold,
                    supporting_quote: None,
                    distractor_quote: None,
                },
                source: ProbeSource::I2Adversarial,
                note: format!("constructed {} (grounded half)", c.kind.as_str()),
            }
        }
        CaseLabel::Ungrounded => Probe {
            id: format!("i2:{}", c.id),
            query: claim_query(&c.claim),
            qtype: PressureKind::AbsentAdjacent,
            oracle: Oracle::Absent {
                held_out_witness: None,
                kind: AbsentKind::Adjacent,
            },
            source: ProbeSource::I2Adversarial,
            note: format!("constructed {} (corrupted half)", c.kind.as_str()),
        },
    }
}

/// I2 — the adversarial slot the registry has named since day one
/// (`probe.rs` `ProbeSource::I2Adversarial`). `corpus` is the HARVEST
/// artifact path (file or directory holding `claims.json`), not an index
/// root — the artifact is the substrate.
#[derive(Debug, Clone, Copy, Default)]
pub struct AdversarialGenerator;

impl Generator for AdversarialGenerator {
    fn id(&self) -> &'static str {
        "i2_adversarial"
    }

    fn generate(&self, n: usize, seed: u64, corpus: Option<&Path>) -> Vec<Probe> {
        let Some(path) = corpus else {
            eprintln!("[i2] no harvest artifact path given (pass the claims.json path) — nothing to corrupt");
            return Vec::new();
        };
        match load_harvest(path) {
            Ok(harvest) => generate_cases(n, seed, &harvest)
                .iter()
                .map(case_to_probe)
                .collect(),
            Err(e) => {
                eprintln!("[i2] {e}");
                Vec::new()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chaos_monkey::ExpectedAction;

    fn fixture() -> HarvestFile {
        HarvestFile {
            schema_version: HARVEST_SCHEMA_VERSION,
            corpus_id: "chaos-saltgrass".into(),
            items: vec![
                HarvestItem {
                    id: "h1".into(),
                    question: "What did the registrar record about the lock sluice?".into(),
                    claim: "The registrar recorded that the lock sluice was opened at half past eleven on the night of the fourteenth.".into(),
                    evidence_chunks: vec![
                        "The registrar's tick in the margin was copied. It recorded, in eleven words, that the lock sluice had been opened at half past eleven on the night of the fourteenth.".into(),
                    ],
                    evidence_chunk_ids: vec!["c1".into()],
                },
                HarvestItem {
                    id: "h2".into(),
                    question: "What happened to the survey of the Merrow Bell?".into(),
                    claim: "The survey of the Merrow Bell was held at last.".into(),
                    evidence_chunks: vec![
                        "The chronometer recovered and rehung; the survey of the Merrow Bell held at last, an empty hold opened to the light.".into(),
                    ],
                    evidence_chunk_ids: vec!["c2".into()],
                },
            ],
            entities: vec![
                EntityCluster {
                    etype: "vessel".into(),
                    surfaces: vec!["Merrow Bell".into()],
                },
                EntityCluster {
                    etype: "vessel".into(),
                    surfaces: vec!["Saltgrass Maid".into()],
                },
            ],
            distractors: vec![DistractorDoc {
                id: "meridian".into(),
                text: "The Meridian packet lost her rudder off the shoals in March. The pilot was censured by the harbour board.".into(),
            }],
        }
    }

    #[test]
    fn generation_is_deterministic_bit_for_bit() {
        let h = fixture();
        let a = generate_cases(12, 17, &h);
        let b = generate_cases(12, 17, &h);
        assert_eq!(a, b, "(n, seed) must be reproducible bit-for-bit");
        assert!(!a.is_empty());
    }

    #[test]
    fn every_case_passes_the_site_contract_and_probe_fairness() {
        let h = fixture();
        let cases = generate_cases(12, 17, &h);
        assert!(
            cases.len() >= 6,
            "fixture should support most kinds: got {}",
            cases.len()
        );
        for c in &cases {
            validate_site(c).unwrap();
            let p = case_to_probe(c);
            crate::flywheel::case::validate_fairness(&p).unwrap();
            match c.label {
                CaseLabel::Ungrounded => {
                    assert_eq!(p.expected_action(), ExpectedAction::Abstain);
                    assert!(
                        !c.spans.is_empty(),
                        "corrupted case `{}` must carry span offsets (spec §10)",
                        c.id
                    );
                    for s in &c.spans {
                        assert!(s.start < s.end && s.end <= c.claim.len());
                    }
                }
                CaseLabel::Grounded => {
                    assert_eq!(p.expected_action(), ExpectedAction::Answer);
                    assert!(c.spans.is_empty(), "grounded case has nothing to localize");
                }
            }
        }
    }

    #[test]
    fn labels_alternate_toward_class_balance() {
        let h = fixture();
        let cases = generate_cases(10, 3, &h);
        let u = cases
            .iter()
            .filter(|c| c.label == CaseLabel::Ungrounded)
            .count();
        let g = cases.len() - u;
        assert!(
            u >= 2 && g >= 2,
            "both halves present: {u} ungrounded / {g} grounded"
        );
        assert!(u.abs_diff(g) <= 2, "roughly balanced: {u} vs {g}");
    }

    #[test]
    fn taxonomy_coverage_on_the_fixture() {
        let h = fixture();
        let cases = generate_cases(24, 41, &h);
        let kinds: std::collections::HashSet<&str> =
            cases.iter().map(|c| c.kind.as_str()).collect();
        for expect in [
            "entity_swap",
            "number_perturb",
            "negation_flip",
            "cross_chunk_chimera",
            "ocr_garble",
            "distractor_absorption",
            "unsupported_addition",
            "verbatim",
            "reframe",
            "multi_hop_conjunction",
        ] {
            assert!(
                kinds.contains(expect),
                "kind `{expect}` never constructed; got {kinds:?}"
            );
        }
    }

    #[test]
    fn corrupted_claims_actually_differ_and_spans_point_at_the_edit() {
        let h = fixture();
        for c in generate_cases(24, 41, &h) {
            if c.label == CaseLabel::Ungrounded && c.kind != CorruptionKind::DistractorAbsorption {
                let orig = h.items.iter().find(|i| i.id == c.source_item_id).unwrap();
                assert_ne!(c.claim, orig.claim, "`{}` did not change the claim", c.id);
            }
        }
    }

    #[test]
    fn site_contract_rejects_a_grounded_injection() {
        // A "corruption" whose injected value is actually in the window must
        // be rejected — that is the whole point of the contract.
        let h = fixture();
        let item = &h.items[0];
        let bogus = StreamBCase {
            id: "entity_swap:bogus".into(),
            corpus_id: h.corpus_id.clone(),
            source_item_id: item.id.clone(),
            kind: CorruptionKind::EntitySwap,
            label: CaseLabel::Ungrounded,
            claim: item.claim.clone(),
            question: item.question.clone(),
            evidence_chunks: item.evidence_chunks.clone(),
            evidence_chunk_ids: item.evidence_chunk_ids.clone(),
            spans: vec![Span { start: 0, end: 3 }],
            witness: SiteWitness::InjectedAbsent {
                injected: "registrar".into(), // present in the window
                original: None,
            },
        };
        assert!(validate_site(&bogus).is_err());
    }

    #[test]
    fn generator_seam_loads_a_harvest_artifact() {
        let dir = std::env::temp_dir().join("flywheel_adversarial_gen");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("claims.json"),
            serde_json::to_string_pretty(&fixture()).unwrap(),
        )
        .unwrap();
        let g = AdversarialGenerator;
        let a = g.generate(8, 7, Some(&dir));
        let b = g.generate(8, 7, Some(&dir));
        assert_eq!(a, b);
        assert!(!a.is_empty());
        for p in &a {
            assert_eq!(p.source, ProbeSource::I2Adversarial);
            crate::flywheel::case::validate_fairness(p).unwrap();
        }
        assert!(g.generate(8, 7, None).is_empty(), "no artifact → no probes");
    }
}
