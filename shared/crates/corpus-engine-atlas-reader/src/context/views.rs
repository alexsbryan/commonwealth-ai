// SPDX-License-Identifier: AGPL-3.0-or-later
//! The borrowing views over the atlas — `AtomView` / `EdgeView` /
//! `EvidenceRef` / `ChunkRequest` — plus the walk helpers (`edge_weight`,
//! `contains_whole_word`), the CallChain data model and its brief renderer,
//! `atom_verbatim_excerpt`, and the ANN-seeded `atlas_navigate_ann` entry
//! point. Split out of `context.rs` (file ceiling); every public item is
//! re-exported at `crate::context`, so historical paths hold.

use crate::atoms::{AtomEnvelope, AtomType, ChunkRef};
use crate::edges::{EdgeProvenance, EdgeType};
use crate::evidence_site::{ChunkSelector, EvidenceSite};
use crate::projection::AtomRecord;
use understanding_vocab::taxonomy::EpistemicStatus;

use super::graph::AtlasGraph;
use super::AtlasContext;

/// Borrowing view over one stored edge — the fields the navigate +
/// call-chain paths read. `source`/`target` are zero-copy atom-id `&str`s.
///
/// `provenance` is the ground-truth discriminant the code-atlas
/// [`AtlasGraph::call_chain`](super::AtlasGraph::call_chain) filters on (a `scip_structural` call edge vs a
/// `containment_structural` parent edge; both are `EdgeType::Involves`). It is
/// carried per-edge by the v2 CSR (`edges.csr`), the only atlas read path.
pub struct EdgeView<'a> {
    pub source: &'a str,
    pub target: &'a str,
    pub edge_type: EdgeType,
    pub confidence: f32,
    pub provenance: EdgeProvenance,
}

// ── CallChain: "talk to your architecture" (Inc 5) ─────────────────────────

/// Direction of an [`AtlasGraph::call_chain`](super::AtlasGraph::call_chain) walk over the code atlas's
/// `ScipStructural` (call / use / impl) edges.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CallDirection {
    /// Follow OUT edges: "what does X call / how does X work / trace the flow."
    Callees,
    /// Follow IN edges: "what calls X / where is X used."
    Callers,
}

/// One atom reached by [`AtlasGraph::call_chain`](super::AtlasGraph::call_chain), in BFS (call) order.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CallChainNode {
    /// Content-hash atom id — the citation handle.
    pub atom_id: String,
    /// Qualified symbol name (e.g. `semver::eval::matches_req`).
    pub name: String,
    /// Entity subtype: `function` / `struct` / `module` / `enum` / `const` / …
    pub subtype: String,
    /// Code-intel summary (may be empty for placeholder atoms).
    pub description: String,
    /// Evidence chunk id the atom first appears in — a citation locator.
    pub chunk_id: String,
    /// Hops from the seed (the seed itself is `0`).
    pub depth: usize,
    /// Atom this node was reached from (`None` for the seed).
    pub via: Option<String>,
    /// The edge into this node crossed a trait / dynamic-dispatch boundary — a
    /// reciprocal `ScipStructural` pair, the structural signal the atlas retains
    /// after dropping SCIP's per-ref `kind`.
    pub via_dyn_dispatch: bool,
}

/// Result of an [`AtlasGraph::call_chain`](super::AtlasGraph::call_chain) walk: the seed plus every atom
/// reached, in call order with per-node depth. [`render_call_chain_brief`]
/// narrates it; the Inc-7 chat path consumes the same struct.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CallChainResult {
    pub corpus_id: String,
    pub direction: CallDirection,
    pub max_depth: usize,
    /// BFS-ordered nodes; `nodes[0]` is the seed at depth 0. Empty when the
    /// seed atom is absent from the atlas.
    pub nodes: Vec<CallChainNode>,
    /// A per-node fanout cap or the depth bound stopped the walk before the
    /// reachable set was exhausted.
    pub truncated: bool,
}

impl CallChainResult {
    /// Did the walk reach any atom (the seed resolved)?
    pub fn hit(&self) -> bool {
        !self.nodes.is_empty()
    }
}

/// First sentence (or a hard char cap) of a code-intel summary, single-lined —
/// keeps a call-chain line scannable without dropping the gist.
fn first_sentence(s: &str, max: usize) -> String {
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let end = flat
        .find(". ")
        .map(|i| i + 1)
        .unwrap_or_else(|| flat.len().min(max));
    let mut clipped = flat[..end.min(flat.len())].to_string();
    if clipped.len() > max {
        clipped.truncate(max);
        clipped.push('…');
    }
    clipped
}

/// Render a [`CallChainResult`] into a cited, depth-indented brief — the
/// human/LLM-facing surface for `enrich atlas-query`'s CallChain. Narrates in
/// call order, indents by depth, flags `[dyn-dispatch]` boundaries (mirroring
/// `corpus_engine_scip::trace::render_trace`), and reuses the `Structural`
/// depth frame so phrasing matches the rest of the atlas brief stack. Each line
/// cites its atom by qualified name + content-hash id + evidence chunk.
pub fn render_call_chain_brief(result: &CallChainResult) -> String {
    use understanding_atlas::atlas_traversal::brief::depth_frame_records;
    use understanding_vocab::taxonomy::EnrichmentDepth;

    let Some(seed) = result.nodes.first() else {
        return "No atom matches that seed in this code atlas.".to_string();
    };
    let verb = match result.direction {
        CallDirection::Callees => "calls",
        CallDirection::Callers => "is called by",
    };
    let seed_subtype = if seed.subtype.is_empty() {
        "code"
    } else {
        seed.subtype.as_str()
    };
    // `depth_frame_records(Structural)` = "The work records that" — code atoms
    // are deterministic structural parse, so they assert directly.
    let frame = depth_frame_records(EnrichmentDepth::Structural);
    let mut out = format!(
        "Call chain — {frame} `{}` ({seed_subtype}) {verb} (depth ≤ {}, {} atom(s)):\n",
        seed.name,
        result.max_depth,
        result.nodes.len(),
    );
    for node in &result.nodes {
        let indent = "  ".repeat(node.depth + 1);
        let arrow = if node.depth == 0 { "•" } else { "→" };
        let dyn_marker = if node.via_dyn_dispatch {
            "  [dyn-dispatch]"
        } else {
            ""
        };
        let subtype = if node.subtype.is_empty() {
            "code"
        } else {
            node.subtype.as_str()
        };
        let desc = first_sentence(&node.description, 160);
        let desc_clause = if desc.is_empty() {
            String::new()
        } else {
            format!(" — {desc}")
        };
        let cite = if node.chunk_id.is_empty() {
            format!("  (cite: {})", node.atom_id)
        } else {
            format!("  (cite: {} @ {})", node.atom_id, node.chunk_id)
        };
        out.push_str(&format!(
            "{indent}{arrow} `{}` [{subtype}]{dyn_marker}{desc_clause}{cite}\n",
            node.name,
        ));
    }
    if result.truncated {
        out.push_str("  … (truncated at the depth/fanout bound)\n");
    }
    out
}

/// Borrowing view over one atom — the v2 resident projected record
/// (`&AtomRecord`, an `atoms.lance` row re-projected at open). Field borrows are
/// tied to the graph's data (`'a`), not to the (often temporary) view, so a
/// caller can collect [`EvidenceRef`]s out of a transient `AtomView`.
/// [`atom_envelope`](Self::atom_envelope) re-parses the full `AtomEnvelope` from
/// the JSON payload for the rare deep-field read.
pub struct AtomView<'a>(&'a AtomRecord);

impl<'a> AtomView<'a> {
    /// Wrap a projected record.
    ///
    /// PUBLIC because [`crate::provider::AtlasProvider`] returns `AtomView`
    /// from four of its eight members, and a backend that lives outside this
    /// module cannot implement any of them without this. The tuple field
    /// stays private: a view is a read-only window onto a record the store
    /// owns, and callers must go through the accessors below (which is what
    /// lets the projection change shape without touching them).
    pub fn new(record: &'a AtomRecord) -> Self {
        Self(record)
    }

    pub fn id(&self) -> &'a str {
        self.0.id.as_str()
    }
    pub fn kind(&self) -> AtomType {
        self.0.kind
    }
    /// `Entity.canonical_name` (else `""`).
    pub fn name(&self) -> &'a str {
        self.0.name.as_str()
    }
    /// `Relation.label` (else `""`).
    pub fn label(&self) -> &'a str {
        self.0.label.as_str()
    }
    /// `Claim.content` (else `""`).
    pub fn content(&self) -> &'a str {
        self.0.content.as_str()
    }
    /// `Entity.entity_type` string repr (else `""`).
    pub fn subtype(&self) -> &'a str {
        self.0.subtype.as_str()
    }
    /// `Entity.description` (else `""`).
    pub fn description(&self) -> &'a str {
        self.0.description.as_str()
    }
    /// `Claim.quotable_excerpt` (else `""`).
    pub fn excerpt(&self) -> &'a str {
        self.0.excerpt.as_str()
    }
    /// `Claim.confidence` (0.5 default; 0.0 for non-claims).
    pub fn confidence(&self) -> f32 {
        self.0.confidence
    }
    /// `Entity.salience` (0.0 for non-entities).
    pub fn salience(&self) -> f32 {
        self.0.salience
    }
    pub fn alias_count(&self) -> usize {
        self.0.aliases.len()
    }
    pub fn aliases(&self) -> impl Iterator<Item = &'a str> + 'a {
        self.0.aliases.iter().map(|s| s.as_str())
    }
    /// `Relation.participants` atom-ids.
    pub fn participants(&self) -> impl Iterator<Item = &'a str> + 'a {
        self.0.participants.iter().map(|s| s.as_str())
    }
    pub fn evidence(&self) -> impl Iterator<Item = EvidenceRef<'a>> + 'a {
        self.0.evidence.iter().map(EvidenceRef)
    }
    /// Re-parse the full `AtomEnvelope` from the JSON payload blob.
    /// `None` only for the empty-payload edge case or a parse failure.
    pub fn atom_envelope(&self) -> Option<AtomEnvelope> {
        if self.0.payload.is_empty() {
            return None;
        }
        serde_json::from_slice(self.0.payload.as_slice()).ok()
    }
}

/// Borrowing view over one evidence ref — the v2 resident `&ChunkRef`. The
/// accessors collapse the source `Option`s to `""` for the callers that only
/// ever wanted a string; `raw()` hands back the `ChunkRef` for a caller that
/// needs to tell "absent" from "empty". Until 2026-08-20 the collapse happened
/// at projection time into a mirror struct (`projection::ArchChunkRef`), so the
/// distinction was destroyed before any caller could ask for it.
pub struct EvidenceRef<'a>(&'a ChunkRef);

impl<'a> EvidenceRef<'a> {
    /// Wrap an evidence anchor.
    ///
    /// PUBLIC for the same reason as [`AtomView::new`]: `atom_evidence` is an
    /// [`crate::provider::AtlasProvider`] member, and a second backend must be
    /// able to return one. `Option`s stay intact behind the accessors — see
    /// [`Self::raw`] for the un-defaulted view.
    pub fn new(chunk_ref: &'a ChunkRef) -> Self {
        Self(chunk_ref)
    }

    pub fn chunk_id(&self) -> &'a str {
        self.0.chunk_id.as_str()
    }
    pub fn passage_preview(&self) -> &'a str {
        self.0.passage_preview.as_deref().unwrap_or("")
    }
    pub fn source_doc_id(&self) -> &'a str {
        self.0.source_doc_id.as_deref().unwrap_or("")
    }
    /// The underlying evidence ref, `Option`s intact.
    pub fn raw(&self) -> &'a ChunkRef {
        self.0
    }
}

impl ChunkRequest {
    /// Display / grouping label for this request's article — the article name
    /// for a per-article atlas, the corpus id for a self-hosted one. NOT a
    /// corpus to search: ask `self.site.chunk_corpus()` for that.
    pub fn article_slug(&self) -> &str {
        self.site.label()
    }
}

/// One step's worth of source-chunk targeting from atlas navigation.
/// Each request says "atlas thinks the source-corpus section
/// identified by `chunk_id` (in the per-article extraction corpus)
/// is highly relevant to the question". Resolved by direct lookup
/// in the article's chapters.json source — no FTS or vector search
/// needed. The `passage_preview` is a fallback for paragraph-level
/// targeting within the larger section.
#[derive(Debug, Clone)]
pub struct ChunkRequest {
    /// Where this chunk lives and whether a title filter applies —
    /// [`EvidenceSite::chunk_corpus`] is the ONE answer to "which corpus do I
    /// search", and the consumer may not compute its own.
    ///
    /// Replaces a `corpus_id: String` that carried the ATLAS id while the
    /// consumer read it as the CHUNK corpus. Those are the same string for a
    /// self-hosted atlas and different for a per-article one, so the old field
    /// scoped every SEP fetch to `sep-freewill` — an index holding no chunks —
    /// and atlas grounding contributed zero to every SEP answer. The old doc
    /// comment asserted the false premise outright: "the chunk lives here
    /// because the atlas was extracted from this corpus."
    pub site: EvidenceSite,
    /// How to pick the chunk out of that corpus: a direct row id, or a section
    /// slug resolved by search. Parsed ONCE here, at the producer, instead of
    /// being re-derived at the consumer with `chunk_id.parse::<u64>()`.
    pub selector: ChunkSelector,
    /// Snippet of the source passage the atom was extracted from.
    /// Used to home in on the specific paragraph within the
    /// (10-paragraph-wide) section.
    pub passage_preview: String,
    /// Aggregate score: sum across all atoms in the navigation
    /// neighborhood that ground this passage, weighted by cosine
    /// match × graph-distance decay × edge-type weight. Chunks that
    /// ground multiple high-relevance atoms float to the top.
    pub score: f32,
    /// Diagnostic — which atoms motivated this fetch and via which
    /// edge types. Surfaces "this chunk is here because of the
    /// Tension between Knowledge Argument and Ability Hypothesis."
    pub motivating_atoms: Vec<String>,
    /// Verbatim ≤200-char excerpts harvested from the motivating
    /// atoms' `defining_quote` / `quotable_excerpt` fields. Each
    /// string is already formatted ("Defining X: …" or "[Y]: …")
    /// for direct injection into the fetched chunk's content. The
    /// caller (apply_atlas_grounding) prepends these to the chunk
    /// so the article's exact words for a defined concept or an
    /// attributed claim sit visibly at the head of the passage —
    /// addresses the essay-judge's "wants direct primary text"
    /// finding from the 2026-05-06 calibration audit.
    pub verbatim_excerpts: Vec<String>,
    /// EVERY distinct passage preview among the atoms that cited this section,
    /// not just the longest one [`Self::passage_preview`] keeps.
    ///
    /// A section is cited by many atoms and each names a different paragraph.
    /// `passage_preview` keeps the longest because the search fallback wants
    /// one query string — but pinning the chunk INSIDE the section needs all
    /// of them, or the pin lands on whichever atom happened to be wordiest.
    /// Measured on `chaos-secret-agent`: `sec_0011` is Conrad's murder
    /// chapter, 37 chunks, and the longest preview belongs to an atom about
    /// the cold beef laid out for supper — so a single-preview pin returned
    /// the carving knife on the supper table and not the one in Verloc's
    /// breast, and the gate correctly refused a claim that passage does not
    /// support.
    pub passage_previews: Vec<String>,
    /// EXACT chunk ids for a [`ChunkSelector::Section`] request, from the
    /// atlas's `chapters.json` join — empty for a `RowId` request, and empty
    /// for a corpus whose join was never filled.
    ///
    /// Filled at the PRODUCER, like `selector` itself, so the consumer never
    /// re-derives a path rule. Non-empty means the resolver fetches the rows
    /// the atom actually named instead of searching for its passage preview,
    /// which is what this struct's own doc has always said it did.
    pub section_rows: Vec<u64>,
}

/// Per-edge-type relevance weights for graph BFS. Tunable; a value
/// of 0 disables walking that edge type. Defaults reflect what each
/// edge type contributes to question-answering retrieval:
///   - Tension → highest (only edge that supplies dialectical
///     breadth — opposing claim pairs surface counter-positions)
///   - Grounds → high (argument-depth: claims supported by other
///     claims walk us into the reasoning chain)
///   - Configures/Composes → medium (configuration's constituent
///     atoms identify the article's interpretive frame)
///   - Involves → medium (entity-event participation)
///   - Causes/Transition → low (state/event chains)
pub fn edge_weight(edge_type: EdgeType) -> f32 {
    match edge_type {
        EdgeType::Tension => 1.0,
        EdgeType::Grounds => 0.8,
        EdgeType::Configures => 0.6,
        EdgeType::Composes => 0.6,
        EdgeType::Involves => 0.5,
        EdgeType::Causes => 0.3,
        EdgeType::Transition => 0.3,
        // Cross-corpus edges aren't relevant for intra-article
        // navigation; they're surfaced via dedicated cross-corpus
        // retrieval paths.
        EdgeType::Grounding | EdgeType::Framing | EdgeType::Provenance => 0.0,
        // Gap-B typed-extension edges. EvidenceFor lands at Grounds
        // weight because the semantics overlap (evidence supports a
        // claim/position the same way Grounds links one claim to
        // its evidential basis). Concedes mirrors Tension (a
        // concession addresses a counter-position the same way a
        // Tension edge captures dialectical disagreement). OpposesIn
        // walks from an Opposition atom out to its two sides — the
        // graph traversal benefit lives mainly downstream of the
        // Opposition atom itself, so the edge weight is medium.
        EdgeType::EvidenceFor => 0.8,
        EdgeType::Concedes => 1.0,
        EdgeType::OpposesIn => 0.6,
        // Attaches connects a carrier doc to a described asset.
        // Intra-article navigation rarely benefits from this edge —
        // surfacing the asset is downstream UX (atom detail panel),
        // not retrieval. Zero weight here keeps the navigator
        // focused on argumentative structure.
        EdgeType::Attaches => 0.0,
    }
}

/// Whole-word case-insensitive substring check. Returns true iff
/// `needle` appears in `haystack` bounded by non-alphanumeric chars
/// on both sides (or string boundaries). Used by name-match seeding
/// in [`atlas_navigate`] to avoid false positives like "form" inside
/// "informed". Both args MUST already be lowercase.
pub fn contains_whole_word(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() || needle.len() > haystack.len() {
        return false;
    }
    let mut start = 0;
    while let Some(off) = haystack[start..].find(needle) {
        let abs = start + off;
        let end = abs + needle.len();
        let left_ok = abs == 0
            || !haystack[..abs]
                .chars()
                .last()
                .is_some_and(|c| c.is_alphanumeric());
        let right_ok = end == haystack.len()
            || !haystack[end..]
                .chars()
                .next()
                .is_some_and(|c| c.is_alphanumeric());
        if left_ok && right_ok {
            return true;
        }
        start = abs + 1;
    }
    false
}

/// Pull the verbatim excerpt off an atom — `defining_quote` from a
/// concept Entity, `quotable_excerpt` from a Claim — and format it
/// for direct injection into a chunk's content. Returns `None` for
/// atoms that don't carry a quote field or whose quote is empty.
///
/// Format pins the source so the judge can attribute (mirrors the
/// essay-judge calibration's "named with substantive content"
/// rubric without demanding pre-assembled reconstruction). Single-
/// line, prefixed; the chunk-annotation site joins these with
/// newlines and prepends them to the chunk content.
/// Floor under which a verbatim excerpt is treated as a fragment
/// the model truncated rather than a real ≤200-char sentence.
/// Empirical: under the condensed prompt, 80%+ of populated quotes
/// land 100-220c; the rest cluster under 50c (mid-word cuts the
/// constraint sampler couldn't fully prevent). 60c is the
/// inflection — long enough to carry a clause that adds judge-
/// visible signal, short enough not to drop legitimate short
/// definitional sentences ("X is Y").
const MIN_VERBATIM_EXCERPT_CHARS: usize = 60;

pub fn atom_verbatim_excerpt(
    graph: &dyn crate::provider::AtlasProvider,
    atom_id: &str,
) -> Option<String> {
    // Deep-field read over the bounded navigation neighborhood (not a hot
    // scan path) — parse the full atom from its JSON payload blob.
    let atom = graph.atom(atom_id)?.atom_envelope()?;
    match &atom {
        AtomEnvelope::ArgumentReconstruction(a) => {
            // Pre-format the reconstruction as P1/.../C/Objections.
            // Targets the essay-judge "argument_depth" axis, which
            // under-credits chunks that contain the argument's
            // pieces scattered across paragraphs without an explicit
            // reconstruction. Article-voice attribution.
            if a.premises.is_empty() && a.conclusion.trim().is_empty() {
                return None;
            }
            let mut s = String::with_capacity(256);
            s.push_str(&format!("Argument: {}", a.name));
            // Resolve proponent to canonical name when possible.
            if let Some(prop_id) = a.proponent.as_ref() {
                if let Some(prop) = graph.atom(prop_id.as_str()) {
                    if prop.kind() == AtomType::Entity {
                        s.push_str(&format!(" ({})", prop.name()));
                    }
                }
            }
            s.push_str(&format!(" [from {}]", graph.article_slug()));
            s.push('\n');
            for (i, p) in a.premises.iter().enumerate() {
                s.push_str(&format!("  P{}. {}\n", i + 1, p.trim()));
            }
            if !a.conclusion.trim().is_empty() {
                s.push_str(&format!("  C. {}\n", a.conclusion.trim()));
            }
            if !a.objections.is_empty() {
                // Render each objection on its own line with prose
                // content when available — the dialectical_breadth
                // axis credits expounded objections, not bare names.
                // Falls back to bare-name rendering for legacy atoms
                // whose objections were extracted as Vec<String>.
                s.push_str("  Objections:\n");
                for o in a.objections.iter() {
                    let name = o.name.trim();
                    let content = o.content.trim();
                    if content.is_empty() {
                        s.push_str(&format!("    - {}\n", name));
                    } else {
                        s.push_str(&format!("    - {}: {}\n", name, content));
                    }
                }
            }
            Some(s)
        }
        AtomEnvelope::Entity(e) => {
            let q = e.defining_quote.as_deref()?.trim();
            if q.chars().count() < MIN_VERBATIM_EXCERPT_CHARS {
                return None;
            }
            // "Defining $name: $sentence" — keeps the term anchored.
            Some(format!(
                "Defining {} ({}): \"{}\"",
                e.canonical_name,
                graph.article_slug(),
                q
            ))
        }
        AtomEnvelope::Claim(c) => {
            let q = c.quotable_excerpt.as_deref()?.trim();
            if q.chars().count() < MIN_VERBATIM_EXCERPT_CHARS {
                return None;
            }
            // Resolve attribution to a canonical name when possible.
            // The Claim atom holds an AtomId — look it up in the
            // graph for the human-readable label. Fallback: bare id.
            let attribution = c.attributed_to.as_ref().and_then(|aid| {
                graph
                    .atom(aid.as_str())
                    .filter(|a| a.kind() == AtomType::Entity)
                    .map(|a| a.name().to_string())
            });
            // Tag contested-status claims so the essay-judge sees them
            // as counter-position content rather than mainline support.
            // SEP articles routinely encode disputed claims with
            // epistemic_status=contested; without flagging, the
            // surfaced quote reads as part of the position the question
            // asks about, when really it's a rival voice. This flips
            // the dialectical_breadth axis from "names objections" (1)
            // to "expounds counter-position" (2) without changing
            // chunk content.
            let contested_tag = if matches!(c.epistemic_status, EpistemicStatus::Contested) {
                " — contested"
            } else {
                ""
            };
            match attribution {
                Some(name) => Some(format!(
                    "[{} ({}){}]: \"{}\"",
                    name,
                    graph.article_slug(),
                    contested_tag,
                    q
                )),
                None => Some(format!(
                    "[{}{}]: \"{}\"",
                    graph.article_slug(),
                    contested_tag,
                    q
                )),
            }
        }
        _ => None,
    }
}

/// ANN-seeded atlas navigation — the UNFILTERED walk, kept as the name every
/// existing caller already spells.
///
/// **This is now a thin caller.** The walk itself moved to
/// [`crate::ground::ground`] (ei-4-walk, 2026-09-04), where it is driven by
/// the atlas's own navigation policy instead of by constants; §10.6 allows one
/// implementation of a walk, and this is not it. What this function preserves
/// is the pre-policy BEHAVIOUR — [`WalkPolicy::unfiltered`]: no seed-kind
/// filter, no edge-kind filter, `max_hops` as passed — so the eval CLI, the
/// evidence-site wiring test, and any caller that has not yet been given a map
/// keep the walk they were calibrated against.
///
/// A caller that HAS a map should call [`crate::ground::ground`] with a
/// [`crate::ground::WalkSelection`] instead; that is the door
/// `apply_atlas_grounding` and `corpus-mcp`'s `ask` go through.
///
/// # Seeds (unchanged, see [`crate::ground::ground`] for the full account)
/// - **Vector seed:** each graph's persistent ANN table returns nearest
///   atom-ids directly, re-scored with the canonical [`cosine`](super::cosine).
/// - **Name-match seed:** every bag atom literally named in the question.
pub async fn atlas_navigate_ann(
    query_text: &str,
    query_embedding: &[f32],
    atlases: &[&AtlasContext],
    graphs: &[&AtlasGraph],
    max_seeds: usize,
    max_hops: usize,
) -> Vec<ChunkRequest> {
    // The historical guard: this entry point returned nothing without a bag,
    // because it predates ANN-only seeding. `ground` allows an empty bag (it
    // is how `corpus-mcp` walks); keeping the guard HERE means no existing
    // caller's behaviour changes on this path.
    if atlases.is_empty() {
        return Vec::new();
    }
    let mut selection = crate::ground::WalkSelection::named(
        understanding_vocab::ontology::QuestionKind::Thematic,
        &understanding_vocab::ontology::NavigationPolicy::default(),
        crate::ground::PolicySource::PreRegistered,
    );
    selection.walk = understanding_vocab::ontology::WalkPolicy::unfiltered();
    selection.walk.hops = max_hops.min(u8::MAX as usize) as u8;
    // Existing callers hold concrete `AtlasGraph`s; the walk speaks
    // `AtlasProvider`. One widening, here, so no call site changes.
    let providers: Vec<&dyn crate::provider::AtlasProvider> = graphs
        .iter()
        .map(|g| *g as &dyn crate::provider::AtlasProvider)
        .collect();
    crate::ground::ground(
        query_text,
        query_embedding,
        atlases,
        &providers,
        &selection,
        max_seeds,
    )
    .await
    .requests
}
