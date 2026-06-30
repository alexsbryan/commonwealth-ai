// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn enrich spec-reconcile <corpus> --spec=<spec-stem>` — reconcile the
//! *conditioned claims* a spec makes (extracted by `enrich spec-intel`) against
//! what the code in a corpus actually does (summarized by `enrich code-intel`).
//!
//! This is the spec-side analogue of `capability-reconcile`: where that verb asks
//! "do the architecture docs describe everything the code does?", this asks "does
//! the code do everything the spec claims?" — claim by claim, condition by
//! condition.
//!
//! The pipeline is a faithful port of the validated Python prototype
//! (`scratch/spec_diff{3,6}.py`):
//!
//!   1. **Recall (function-level, union).** Embed every function's
//!      `summary + " asks: " + asks.join(" ")` once and the claim's
//!      `statement + " " + conditions.join(" ")`. For each claim, take the top-K
//!      functions by cosine UNION any function whose name overlaps a
//!      `referenced_entities` token (len ≥ 5) — the candidate set (cap ~10).
//!   2. **Confidence floor.** If the best cosine is below [`FLOOR`] AND no symbol
//!      hit, the claim is `Unverifiable` — we have no evidence to adjudicate
//!      against, so we say so rather than guess.
//!   3. **Strict per-condition adjudication.** One grammar-constrained chat call
//!      per claim decides, for EACH condition, whether a candidate function
//!      DIRECTLY implements it (topically adjacent does NOT count), then renders a
//!      verdict: entails / partial / contradicts / unrelated.
//!   4. **Classify + write.** entails→Corroborated, partial→Todo,
//!      contradicts→Drift, unrelated→Gap (+ Unverifiable from the floor). Findings
//!      are written to `<data_dir>/specs/<spec-stem>/spec_findings.{md,json}`
//!      (+ a freshness fingerprint over the inputs), grouped by kind.
//!
//! Glassbox: the per-claim verdict + cited functions are printed live, the JSON
//! artifact carries the full per-condition trace, and the 29k-function embedding
//! is cached to `<data_dir>/specs/_fn_vecs/<corpus>.bin` (raw little-endian f32 +
//! a sidecar guarding on the code-intel cache size) so re-runs are cheap.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

use corpus_engine::enrichment::code_intel::SymbolEnrichment;
use corpus_engine::enrichment::pipeline::{ChatCompletionFn, ChatPrompt};
use corpus_engine::types::EmbedFn;
use futures::stream::{self, StreamExt};
use serde::{Deserialize, Serialize};

use super::config::EnrichConfig;
use super::inference_client::{probe_daemon, DaemonInferenceClient};
use sovereign_cli_shared::help::{self, Help, HelpSection};
use sovereign_tools::code::drift_posture::write_fingerprint;

/// Top functions by cosine per claim (prototype `K = 8`).
const K: usize = 8;
/// Max symbol-name-overlap hits folded into the candidate set (prototype `[:4]`).
const SYM_CAP: usize = 4;
/// Max candidate functions handed to the judge (prototype `BUNDLE = 10`).
const BUNDLE: usize = 10;
/// Below this best cosine (and with no symbol hit) a claim is Unverifiable —
/// we have no confident evidence, so we abstain rather than adjudicate
/// (prototype `FLOOR = 0.45`).
const FLOOR: f32 = 0.45;
/// First N chars of each function summary shown in the evidence bundle
/// (prototype `summary[:140]`).
const SUMMARY_CLIP: usize = 140;
/// Concurrency for the one-time function-embedding pass.
const EMBED_CONCURRENCY: usize = 16;
/// Concurrency for the per-claim adjudication chat calls (mirror
/// capability-reconcile's `LLM_CONCURRENCY`).
const LLM_CONCURRENCY: usize = 8;
/// Output-token budget for one verdict (prototype `max_tokens = 600`).
const MAX_OUTPUT_TOKENS: u32 = 600;
/// Low temperature — deterministic adjudication (prototype `temperature = 0.1`).
const TEMPERATURE: f32 = 0.1;
/// Phase id carried on every prompt so the chat client can route this pass to an
/// operator-declared model and the daemon heartbeat logs are labelled.
const PHASE_ID: &str = "spec_reconcile";

/// The strict adjudication system prompt — **verbatim from `spec_diff6.py`'s
/// `ADJ_SYSTEM`**. It is load-bearing: `entails` requires every condition to be
/// DIRECTLY implemented (not topically adjacent), `contradicts` is reserved for
/// code that does the OPPOSITE (never for mere silence), and `unrelated` covers
/// the silence case.
const ADJ_SYSTEM: &str = r#"You decide whether code satisfies a spec claim. You get the claim + its conditions and a set of candidate functions (each with file:line and a summary). For EACH condition decide met/unmet and cite the function (file:line) for met ones — no citation means unmet. A condition is met ONLY if a cited function DIRECTLY implements the specific behavior it states; a function merely operating in the same general area does NOT count — mark those unmet. Verdict: 'entails' (every condition directly met), 'partial' (some met), 'contradicts' (a function actively implements the OPPOSITE of the claim — reserve this, not for silence), 'unrelated' (none of these functions address the claim). Output ONLY JSON: {"per_condition":[{"condition":"..","met":true,"evidence":"<fn> (<file>:<line>)"}],"verdict":"entails|partial|contradicts|unrelated","note":"<=20 words"}."#;

const HELP: Help = Help {
    command: "svrn enrich spec-reconcile",
    summary: "Reconcile a spec's conditioned claims against what the corpus code actually does: corroborated / todo / drift / gap / unverifiable.",
    sections: &[
        HelpSection::Usage(
            "svrn enrich spec-reconcile <corpus-id> --spec=<spec-stem> [--render-only]",
        ),
        HelpSection::Flags(&[
            (
                "<corpus-id>",
                "An installed CODE corpus with a code-intel cache (run `svrn enrich code-intel <corpus>` first). id, name, or unique substring.",
            ),
            (
                "--spec=<stem>",
                "Spec stem under <data_dir>/specs/<stem>/ — i.e. a spec already processed by `svrn enrich spec-intel`, whose claims.json this reconciles.",
            ),
            (
                "--render-only",
                "Re-render spec_findings.md from the existing JSON — no embedding, no LLM, no daemon.",
            ),
        ]),
        HelpSection::Notes(
            "Requires the daemon at localhost:9741. Findings are written to \
             <data_dir>/specs/<spec-stem>/spec_findings.{md,json}. The 29k-function \
             embedding is cached to <data_dir>/specs/_fn_vecs/<corpus>.bin and reused \
             until the code-intel cache changes.",
        ),
    ],
};

// ── on-disk shapes read from sibling caches ───────────────────

/// One conditioned claim — mirrors `spec_intel.rs`'s `Claim`. We read only the
/// fields we adjudicate against; `section_hash` and any future fields are ignored
/// by serde's default lenient parsing.
#[derive(Debug, Clone, Deserialize)]
struct Claim {
    statement: String,
    #[serde(default)]
    conditions: Vec<String>,
    #[serde(default)]
    referenced_entities: Vec<String>,
    #[serde(default)]
    normativity: String,
    #[serde(default)]
    source: String,
}

#[derive(Debug, Deserialize)]
struct SectionClaims {
    #[serde(default)]
    title: String,
    #[serde(default)]
    claims: Vec<Claim>,
}

#[derive(Debug, Deserialize)]
struct ClaimsCache {
    // `spec` (the spec stem) is also present in the file but we derive that from
    // the `--spec` arg; serde ignores the extra key.
    #[serde(default)]
    sections: Vec<SectionClaims>,
}

// ── findings artifact (mirrors capability_reconcile.rs) ────────

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum FindingKind {
    /// Code does the OPPOSITE of the claim (verdict `contradicts`).
    Drift,
    /// No candidate function addresses the claim (verdict `unrelated`).
    Gap,
    /// Some conditions met, some not (verdict `partial`).
    Todo,
    /// No confident candidate — best cosine below the floor and no symbol hit.
    Unverifiable,
    /// Every condition directly implemented (verdict `entails`).
    Corroborated,
}

/// Per-condition adjudication result mirrored into the finding.
#[derive(Debug, Serialize, Deserialize, Clone)]
struct PerConditionFinding {
    condition: String,
    met: bool,
    evidence: String,
}

/// One reconciled claim.
#[derive(Debug, Serialize, Deserialize, Clone)]
struct SpecFinding {
    kind: FindingKind,
    /// The claim's one-sentence statement.
    statement: String,
    /// `contract` (validated finding) or `proposal` (planned behavior).
    normativity: String,
    /// Section title the claim came from (provenance).
    source: String,
    /// Matched function(s) as `name (file:line)` — the evidence neighborhood.
    matched: Vec<String>,
    /// Per-condition met/evidence trace from the judge.
    per_condition: Vec<PerConditionFinding>,
    /// The judge's ≤20-word note (or the floor's abstention reason).
    note: String,
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct FindingSet {
    corpus_id: String,
    spec: String,
    corroborated: usize,
    todo: usize,
    drift: usize,
    gap: usize,
    unverifiable: usize,
    findings: Vec<SpecFinding>,
}

// ── the model's verdict (grammar-constrained JSON) ─────────────

#[derive(Debug, Deserialize)]
struct VerdictCondition {
    #[serde(default)]
    condition: String,
    #[serde(default)]
    met: bool,
    #[serde(default)]
    evidence: String,
}

#[derive(Debug, Deserialize)]
struct SpecVerdict {
    #[serde(default)]
    per_condition: Vec<VerdictCondition>,
    #[serde(default)]
    verdict: String,
    #[serde(default)]
    note: String,
}

impl SpecVerdict {
    /// The prototype's `or {"verdict": "unrelated", "note": "[parse-fail]"}`
    /// fallback — an unparseable response is treated as `unrelated` (→ Gap).
    fn parse_fail(note: &str) -> Self {
        Self {
            per_condition: Vec::new(),
            verdict: "unrelated".to_string(),
            note: note.to_string(),
        }
    }
}

/// JSON Schema forcing the verdict shape, handed to the daemon as
/// `response_format.json_schema` (llguidance-enforced) — the structural fix for
/// the prototype's lenient `parse_json` fallback path.
fn verdict_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "per_condition": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "condition": { "type": "string" },
                        "met": { "type": "boolean" },
                        "evidence": { "type": "string" }
                    },
                    "required": ["condition", "met", "evidence"],
                    "additionalProperties": false
                }
            },
            "verdict": {
                "type": "string",
                "enum": ["entails", "partial", "contradicts", "unrelated"]
            },
            "note": { "type": "string" }
        },
        "required": ["per_condition", "verdict", "note"],
        "additionalProperties": false
    })
}

/// Map a model verdict string onto a finding kind (prototype `KIND`).
fn classify(verdict: &str) -> FindingKind {
    match verdict {
        "entails" => FindingKind::Corroborated,
        "partial" => FindingKind::Todo,
        "contradicts" => FindingKind::Drift,
        _ => FindingKind::Gap, // "unrelated" + any unexpected value
    }
}

// ── function-vector cache (the embed-once optimisation) ────────

/// Function metadata aligned 1:1 with the cached vectors — enough to score
/// (name) and to cite (file:line) + render (summary). Stored in the sidecar so a
/// cache hit is self-describing without re-reading the code-intel cache.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct FnMeta {
    name: String,
    file: String,
    line: u32,
    summary: String,
}

impl FnMeta {
    fn from_enrichment(e: &SymbolEnrichment) -> Self {
        Self {
            name: e.meta.name.clone(),
            file: e.meta.file_path.clone(),
            line: e.meta.line_start,
            summary: e.summary.clone(),
        }
    }
    /// The text embedded for recall: `summary + " asks: " + asks.join(" ")`.
    fn embed_text(e: &SymbolEnrichment) -> String {
        format!("{} asks: {}", e.summary, e.asks.join(" "))
    }
}

/// Sidecar describing the `.bin` matrix: a `guard` (the code-intel cache file's
/// byte length — any change re-embeds), the matrix shape, and the per-row meta.
#[derive(Debug, Serialize, Deserialize)]
struct FnVecSidecar {
    guard: u64,
    dim: usize,
    count: usize,
    fns: Vec<FnMeta>,
}

fn fn_vec_paths(data_dir: &Path, corpus_id: &str) -> (PathBuf, PathBuf) {
    let dir = data_dir.join("specs").join("_fn_vecs");
    (
        dir.join(format!("{corpus_id}.bin")),
        dir.join(format!("{corpus_id}.json")),
    )
}

/// Load cached function vectors iff the guard + embedding dim still match.
/// `None` signals "re-embed": absent, corrupt, stale (cache changed), or a
/// different embedding model (dim mismatch).
fn load_fn_vecs(
    data_dir: &Path,
    corpus_id: &str,
    guard: u64,
    dim: usize,
) -> Option<(Vec<FnMeta>, Vec<Vec<f32>>)> {
    let (bin_path, side_path) = fn_vec_paths(data_dir, corpus_id);
    let side: FnVecSidecar = serde_json::from_str(&fs::read_to_string(&side_path).ok()?).ok()?;
    if side.guard != guard || side.dim != dim || side.fns.len() != side.count {
        return None;
    }
    let bytes = fs::read(&bin_path).ok()?;
    if bytes.len() != side.count * side.dim * 4 {
        return None;
    }
    let vecs: Vec<Vec<f32>> = bytes
        .chunks_exact(side.dim * 4)
        .map(|row| {
            row.chunks_exact(4)
                .map(|b| f32::from_le_bytes(b.try_into().expect("chunks_exact(4)")))
                .collect()
        })
        .collect();
    Some((side.fns, vecs))
}

/// Persist function vectors as a raw little-endian f32 matrix + sidecar.
fn save_fn_vecs(
    data_dir: &Path,
    corpus_id: &str,
    guard: u64,
    dim: usize,
    fns: &[FnMeta],
    vecs: &[Vec<f32>],
) -> std::io::Result<()> {
    let (bin_path, side_path) = fn_vec_paths(data_dir, corpus_id);
    if let Some(parent) = bin_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut buf = Vec::with_capacity(vecs.len() * dim * 4);
    for v in vecs {
        for &x in v {
            buf.extend_from_slice(&x.to_le_bytes());
        }
    }
    fs::write(&bin_path, &buf)?;
    let side = FnVecSidecar {
        guard,
        dim,
        count: vecs.len(),
        fns: fns.to_vec(),
    };
    fs::write(
        &side_path,
        serde_json::to_string(&side).map_err(std::io::Error::other)?,
    )?;
    Ok(())
}

// ── pure helpers ──────────────────────────────────────────────

/// Cosine *similarity* (1.0 = identical, 0.0 = orthogonal/degenerate). Local
/// because `clustering::cosine_distance` (which returns `1 - similarity`) is
/// private to that module. A zero-length / zero-norm vector yields 0.0 — the
/// graceful-degradation path for a function whose embed failed.
fn cosine_sim(a: &[f32], b: &[f32]) -> f32 {
    let (mut dot, mut na, mut nb) = (0.0f64, 0.0f64, 0.0f64);
    for (&x, &y) in a.iter().zip(b.iter()) {
        let (x, y) = (x as f64, y as f64);
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    let denom = na.sqrt() * nb.sqrt();
    if denom < 1e-12 {
        0.0
    } else {
        (dot / denom) as f32
    }
}

/// `referenced_entities` → lowercase identifier tokens of length ≥ 5 (prototype
/// `ent_tokens`). Split on any non `[A-Za-z0-9_]` char.
fn ent_tokens(claim: &Claim) -> HashSet<String> {
    let mut toks = HashSet::new();
    for e in &claim.referenced_entities {
        for t in e.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')) {
            if t.len() >= 5 {
                toks.insert(t.to_lowercase());
            }
        }
    }
    toks
}

/// Parse the verdict JSON. Fast path: the whole response is the object (the
/// grammar-constrained common case). Tolerant fallback: strip a code fence and
/// slice the outermost `{...}` span (mirrors `spec_intel::parse_claims`).
fn parse_verdict(text: &str) -> Option<SpecVerdict> {
    let t = text.trim();
    if let Ok(v) = serde_json::from_str::<SpecVerdict>(t) {
        return Some(v);
    }
    let start = t.find('{')?;
    let end = t.rfind('}')?;
    if end <= start {
        return None;
    }
    serde_json::from_str::<SpecVerdict>(&t[start..=end]).ok()
}

fn parse_flag(args: &[String], key: &str) -> Option<String> {
    let eq = format!("{key}=");
    for (i, a) in args.iter().enumerate() {
        if let Some(v) = a.strip_prefix(&eq) {
            return Some(v.to_string());
        }
        if a == key {
            return args.get(i + 1).cloned();
        }
    }
    None
}

/// The recall plan for one claim, computed offline (CPU-only) before any chat
/// call. `Adjudicate` carries the candidate function indices; `Unverifiable`
/// carries the best cosine for the abstention note.
enum Plan {
    Unverifiable { best: f32 },
    Adjudicate { cand: Vec<usize> },
}

/// Compute the candidate set for one claim: top-K by cosine UNION up to
/// [`SYM_CAP`] symbol-name overlaps, deduped, capped at [`BUNDLE`]. Applies the
/// confidence floor.
fn plan_claim(claim: &Claim, claim_vec: &[f32], fns: &[FnMeta], fn_vecs: &[Vec<f32>]) -> Plan {
    let sims: Vec<f32> = fn_vecs.iter().map(|fv| cosine_sim(claim_vec, fv)).collect();
    let mut order: Vec<usize> = (0..sims.len()).collect();
    order.sort_by(|&a, &b| {
        sims[b]
            .partial_cmp(&sims[a])
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let top: Vec<usize> = order.iter().copied().take(K).collect();
    let best = top.first().map(|&i| sims[i]).unwrap_or(0.0);

    // Symbol-name overlap: any function whose lowercased name contains a
    // referenced-entity token (len ≥ 5). Substring containment subsumes equality.
    let toks = ent_tokens(claim);
    let mut sym: Vec<usize> = Vec::new();
    if !toks.is_empty() {
        for (i, f) in fns.iter().enumerate() {
            let name_lc = f.name.to_lowercase();
            if toks.iter().any(|t| name_lc.contains(t.as_str())) {
                sym.push(i);
                if sym.len() >= SYM_CAP {
                    break;
                }
            }
        }
    }

    if best < FLOOR && sym.is_empty() {
        return Plan::Unverifiable { best };
    }

    let mut cand: Vec<usize> = Vec::with_capacity(BUNDLE);
    let mut seen: HashSet<usize> = HashSet::new();
    for &i in sym.iter().chain(top.iter()) {
        if seen.insert(i) {
            cand.push(i);
            if cand.len() >= BUNDLE {
                break;
            }
        }
    }
    Plan::Adjudicate { cand }
}

/// Render the candidate evidence bundle handed to the judge (prototype format).
fn render_bundle(cand: &[usize], fns: &[FnMeta]) -> String {
    cand.iter()
        .map(|&j| {
            let f = &fns[j];
            let summary: String = f.summary.chars().take(SUMMARY_CLIP).collect();
            format!("- {} ({}:{}): {}", f.name, f.file, f.line, summary)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// One strict per-condition adjudication call. Never panics — a chat or parse
/// failure degrades to the prototype's `unrelated` fallback (→ Gap).
async fn adjudicate(chat: &ChatCompletionFn, claim: &Claim, bundle: &str) -> SpecVerdict {
    let user = format!(
        "CLAIM: {}\nCONDITIONS: {:?}\n\nCANDIDATE FUNCTIONS:\n{}",
        claim.statement, claim.conditions, bundle
    );
    let prompt = ChatPrompt::new(ADJ_SYSTEM, user)
        .with_response_schema("SpecVerdict", verdict_schema())
        .with_phase_id(PHASE_ID)
        .with_temperature(TEMPERATURE)
        .with_max_output_tokens(MAX_OUTPUT_TOKENS);
    match (chat)(&prompt).await {
        Ok(raw) => parse_verdict(&raw).unwrap_or_else(|| SpecVerdict::parse_fail("[parse-fail]")),
        Err(e) => SpecVerdict::parse_fail(&format!("[chat-error: {e}]")),
    }
}

// ── markdown render ───────────────────────────────────────────

fn tally_line(set: &FindingSet) -> String {
    format!(
        "{} corroborated · {} todo · {} drift · {} gap · {} unverifiable",
        set.corroborated, set.todo, set.drift, set.gap, set.unverifiable
    )
}

fn render_group(s: &mut String, title: &str, kind: FindingKind, set: &FindingSet) {
    let items: Vec<&SpecFinding> = set.findings.iter().filter(|f| f.kind == kind).collect();
    s.push_str(&format!("## {} ({})\n\n", title, items.len()));
    for f in items {
        let norm = if f.normativity.is_empty() {
            "?"
        } else {
            f.normativity.as_str()
        };
        s.push_str(&format!("- **{}** ({}) — {}\n", f.statement, norm, f.note));
        if !f.matched.is_empty() {
            s.push_str(&format!("  - matched: {}\n", f.matched.join(", ")));
        }
        for pc in &f.per_condition {
            let mk = if pc.met { "OK" } else { "MISS" };
            let ev = if pc.met {
                pc.evidence.as_str()
            } else {
                "(none)"
            };
            s.push_str(&format!("  - [{}] {} -> {}\n", mk, pc.condition, ev));
        }
        if !f.source.is_empty() {
            s.push_str(&format!("  - _from §{}_\n", f.source));
        }
    }
    s.push('\n');
}

fn render_markdown(set: &FindingSet) -> String {
    let mut s = String::new();
    s.push_str(&format!(
        "# {} — Spec Reconciliation (claims vs code) [{}]\n\n",
        set.spec, set.corpus_id
    ));
    s.push_str(&format!(
        "_Conditioned claims reconciled against what the code actually does — {}. \
         Regenerate with `svrn enrich spec-reconcile {} --spec={}`._\n\n",
        tally_line(set),
        set.corpus_id,
        set.spec,
    ));
    // Most-actionable first: real contradictions, then silences, then partials.
    render_group(&mut s, "⚠ Drift — code does the OPPOSITE of the claim", FindingKind::Drift, set);
    render_group(&mut s, "Gap — no code addresses the claim", FindingKind::Gap, set);
    render_group(&mut s, "Todo — partially implemented", FindingKind::Todo, set);
    render_group(&mut s, "Unverifiable — no confident candidate", FindingKind::Unverifiable, set);
    render_group(&mut s, "✓ Corroborated — every condition implemented", FindingKind::Corroborated, set);
    s
}

fn recount(findings: Vec<SpecFinding>, corpus_id: &str, spec: &str) -> FindingSet {
    let count = |k: FindingKind| findings.iter().filter(|f| f.kind == k).count();
    FindingSet {
        corpus_id: corpus_id.to_string(),
        spec: spec.to_string(),
        corroborated: count(FindingKind::Corroborated),
        todo: count(FindingKind::Todo),
        drift: count(FindingKind::Drift),
        gap: count(FindingKind::Gap),
        unverifiable: count(FindingKind::Unverifiable),
        findings,
    }
}

// ── driver ────────────────────────────────────────────────────

pub async fn cmd_spec_reconcile(args: &[String]) -> i32 {
    if help::wants_help(args) {
        help::print(&HELP);
        return 0;
    }

    let Some(query) = args.iter().find(|a| !a.starts_with('-')) else {
        eprintln!("error: missing <corpus-id>");
        eprintln!();
        help::print(&HELP);
        return 2;
    };
    let Some(spec_stem) = parse_flag(args, "--spec") else {
        eprintln!("error: missing --spec=<spec-stem>");
        eprintln!();
        help::print(&HELP);
        return 2;
    };

    let data_dir = sovereign_core::setup_config::SetupConfig::load()
        .map(|c| c.data.dir)
        .unwrap_or_else(|_| dirs::home_dir().unwrap_or_default().join(".sovereign"));
    let indexes_dir = data_dir.join("indexes");
    let corpus_id = match crate::corpus_resolve::resolve_corpus_id(&indexes_dir, query) {
        Ok(id) => id,
        Err(msg) => {
            eprintln!("error: {msg}");
            return 1;
        }
    };

    // Corpus-scoped — matches spec-intel's write path; two repos' same-named specs
    // (both `README`) no longer collide at specs/<stem>/.
    let cache_dir = data_dir.join("specs").join(&corpus_id).join(&spec_stem);
    let claims_path = cache_dir.join("claims.json");
    let findings_md = cache_dir.join("spec_findings.md");
    let findings_json = cache_dir.join("spec_findings.json");

    // --render-only: regenerate the markdown view from the existing FindingSet
    // JSON (the source of truth) without embedding, LLM, or daemon.
    if args.iter().any(|a| a == "--render-only") {
        let raw = match fs::read_to_string(&findings_json) {
            Ok(r) => r,
            Err(e) => {
                eprintln!(
                    "error: reading {} (run the full reconcile first): {e}",
                    findings_json.display()
                );
                return 1;
            }
        };
        let set: FindingSet = match serde_json::from_str(&raw) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("error: parsing {}: {e}", findings_json.display());
                return 1;
            }
        };
        if let Err(e) = fs::write(&findings_md, render_markdown(&set)) {
            eprintln!("error: writing {}: {e}", findings_md.display());
            return 1;
        }
        println!(
            "spec-reconcile: re-rendered {} → {}",
            tally_line(&set),
            findings_md.display()
        );
        return 0;
    }

    // ── load the two sibling caches ──
    let claims: Vec<Claim> = match fs::read_to_string(&claims_path) {
        Ok(s) => match serde_json::from_str::<ClaimsCache>(&s) {
            Ok(cache) => cache
                .sections
                .into_iter()
                .flat_map(|sec| {
                    let title = sec.title;
                    sec.claims.into_iter().map(move |mut c| {
                        if c.source.is_empty() {
                            c.source = title.clone();
                        }
                        c
                    })
                })
                .collect(),
            Err(e) => {
                eprintln!("error: parsing {}: {e}", claims_path.display());
                return 1;
            }
        },
        Err(e) => {
            eprintln!(
                "error: reading {} (run `svrn enrich spec-intel <spec.md>` first): {e}",
                claims_path.display()
            );
            return 1;
        }
    };
    if claims.is_empty() {
        eprintln!("error: no claims in {} — nothing to reconcile", claims_path.display());
        return 1;
    }

    let code_intel_path = indexes_dir.join(&corpus_id).join("code_intel_cache.json");
    let enrichments: Vec<SymbolEnrichment> = match fs::read_to_string(&code_intel_path) {
        Ok(s) => match serde_json::from_str(&s) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("error: parsing {}: {e}", code_intel_path.display());
                return 1;
            }
        },
        Err(e) => {
            eprintln!(
                "error: reading {} (run `svrn enrich code-intel {}` first): {e}",
                code_intel_path.display(),
                corpus_id
            );
            return 1;
        }
    };
    if enrichments.is_empty() {
        eprintln!(
            "error: empty code-intel cache {} — run `svrn enrich code-intel {}`",
            code_intel_path.display(),
            corpus_id
        );
        return 1;
    }
    let cache_guard = fs::metadata(&code_intel_path).map(|m| m.len()).unwrap_or(0);

    // ── daemon + closures ──
    let cfg = match EnrichConfig::require(&corpus_id) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    if !probe_daemon(&cfg.base_url).await {
        eprintln!(
            "error: daemon is not responding at {} — start it first",
            cfg.base_url
        );
        return 2;
    }
    let client = match DaemonInferenceClient::from_enrich_config(&cfg) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: building daemon client: {e}");
            return 1;
        }
    };
    let (embed, chat) = client.into_closures();

    println!(
        "spec-reconcile: model={}  spec={}  corpus={}  claims={}  functions={}",
        cfg.chat_model,
        spec_stem,
        corpus_id,
        claims.len(),
        enrichments.len(),
    );

    // ── embed the claims (cheap; also fixes the embedding dim) ──
    let claim_texts: Vec<String> = claims
        .iter()
        .map(|c| format!("{} {}", c.statement, c.conditions.join(" ")))
        .collect();
    let mut claim_vecs = embed_all(&embed, &claim_texts, "claim", None).await;
    let dim = claim_vecs.iter().map(|v| v.len()).max().unwrap_or(0);
    if dim == 0 {
        eprintln!("error: every claim embedding failed — is the embed model loaded?");
        return 1;
    }
    normalize_dim(&mut claim_vecs, dim);

    // ── get-or-build the function vectors (embed-once, cached) ──
    let (fns, mut fn_vecs) = match load_fn_vecs(&data_dir, &corpus_id, cache_guard, dim) {
        Some((fns, vecs)) => {
            println!(
                "spec-reconcile: reusing cached vectors for {} functions (guard={cache_guard}, dim={dim})",
                fns.len()
            );
            (fns, vecs)
        }
        None => {
            let fns: Vec<FnMeta> = enrichments.iter().map(FnMeta::from_enrichment).collect();
            let texts: Vec<String> = enrichments.iter().map(FnMeta::embed_text).collect();
            println!(
                "spec-reconcile: embedding {} functions (one-time; cached to specs/_fn_vecs/{}.bin)…",
                texts.len(),
                corpus_id
            );
            let t0 = Instant::now();
            let mut vecs = embed_all(&embed, &texts, "function", Some(texts.len())).await;
            normalize_dim(&mut vecs, dim);
            println!(
                "spec-reconcile: embedded {} functions in {:.1}s",
                vecs.len(),
                t0.elapsed().as_secs_f32()
            );
            if let Err(e) = save_fn_vecs(&data_dir, &corpus_id, cache_guard, dim, &fns, &vecs) {
                eprintln!("warning: could not cache function vectors: {e}");
            }
            (fns, vecs)
        }
    };
    normalize_dim(&mut fn_vecs, dim); // defensive: cached rows are already dim-correct

    // ── recall plans (CPU-only) ──
    let plans: Vec<Plan> = claims
        .iter()
        .zip(claim_vecs.iter())
        .map(|(claim, cv)| plan_claim(claim, cv, &fns, &fn_vecs))
        .collect();

    // ── strict adjudication (concurrent over the verifiable claims) ──
    let adj_inputs: Vec<(usize, Vec<usize>)> = plans
        .iter()
        .enumerate()
        .filter_map(|(i, p)| match p {
            Plan::Adjudicate { cand } => Some((i, cand.clone())),
            Plan::Unverifiable { .. } => None,
        })
        .collect();
    println!(
        "spec-reconcile: {} verifiable, {} unverifiable; adjudicating per-condition…",
        adj_inputs.len(),
        claims.len() - adj_inputs.len(),
    );

    let claims_ref = &claims;
    let fns_ref = &fns;
    let verdicts: HashMap<usize, SpecVerdict> = stream::iter(adj_inputs.into_iter().map(
        |(i, cand)| {
            let chat = chat.clone();
            async move {
                let bundle = render_bundle(&cand, fns_ref);
                let v = adjudicate(&chat, &claims_ref[i], &bundle).await;
                (i, v)
            }
        },
    ))
    .buffer_unordered(LLM_CONCURRENCY)
    .collect()
    .await;

    // ── assemble findings in claim order (glassbox per-claim print) ──
    let mut findings: Vec<SpecFinding> = Vec::with_capacity(claims.len());
    for (i, claim) in claims.iter().enumerate() {
        let norm = if claim.normativity.is_empty() {
            "?"
        } else {
            claim.normativity.as_str()
        };
        let stmt84: String = claim.statement.chars().take(84).collect();
        let finding = match &plans[i] {
            Plan::Unverifiable { best } => {
                println!("[UNVERIFIABLE] ({norm}) {stmt84}");
                SpecFinding {
                    kind: FindingKind::Unverifiable,
                    statement: claim.statement.clone(),
                    normativity: claim.normativity.clone(),
                    source: claim.source.clone(),
                    matched: Vec::new(),
                    per_condition: Vec::new(),
                    note: format!("best fn sim {best:.2} < {FLOOR}; no symbol hit"),
                }
            }
            Plan::Adjudicate { cand } => {
                let (kind, note, per_condition) = match verdicts.get(&i) {
                    Some(v) => {
                        let pcs = v
                            .per_condition
                            .iter()
                            .map(|pc| PerConditionFinding {
                                condition: pc.condition.clone(),
                                met: pc.met,
                                evidence: pc.evidence.clone(),
                            })
                            .collect();
                        (classify(&v.verdict), v.note.clone(), pcs)
                    }
                    // A verdict can only be missing if the stream dropped it — treat
                    // as a Gap so the claim still surfaces, with a visible reason.
                    None => (FindingKind::Gap, "[missing-verdict]".to_string(), Vec::new()),
                };
                let matched: Vec<String> = cand
                    .iter()
                    .take(6)
                    .map(|&j| format!("{} ({}:{})", fns[j].name, fns[j].file, fns[j].line))
                    .collect();
                println!("[{kind:?}] ({norm}) {stmt84}");
                SpecFinding {
                    kind,
                    statement: claim.statement.clone(),
                    normativity: claim.normativity.clone(),
                    source: claim.source.clone(),
                    matched,
                    per_condition,
                    note,
                }
            }
        };
        findings.push(finding);
    }

    let set = recount(findings, &corpus_id, &spec_stem);

    // ── write artifact + freshness fingerprint over the inputs ──
    if let Err(e) = fs::create_dir_all(&cache_dir) {
        eprintln!("error: creating {}: {e}", cache_dir.display());
        return 1;
    }
    if let Err(e) = fs::write(&findings_md, render_markdown(&set)) {
        eprintln!("error: writing {}: {e}", findings_md.display());
        return 1;
    }
    match serde_json::to_string_pretty(&set) {
        Ok(j) => {
            if let Err(e) = fs::write(&findings_json, j) {
                eprintln!("error: writing {}: {e}", findings_json.display());
                return 1;
            }
        }
        Err(e) => {
            eprintln!("error: serializing findings: {e}");
            return 1;
        }
    }
    // Fingerprint the inputs the findings derive from (claims + code-intel), so a
    // future `spec_posture` can report staleness — mirrors capability-reconcile.
    if let Err(e) = write_fingerprint(
        &cache_dir,
        &[claims_path.clone(), code_intel_path.clone()],
        &findings_md,
    ) {
        eprintln!("warning: could not write fingerprint: {e}");
    }

    println!(
        "=== spec-reconcile: {} → {} ===",
        tally_line(&set),
        findings_md.display()
    );
    0
}

/// Embed `texts` one-at-a-time through the injected `EmbedFn`, order-preserving
/// (so vectors stay aligned with their inputs), with bounded concurrency. A
/// failed embed yields an empty vector — [`normalize_dim`] later replaces it with
/// zeros so the matrix stays a fixed-stride block (and that row simply never
/// matches). `progress_total`, when set, prints periodic progress for the big
/// function pass.
async fn embed_all(
    embed: &EmbedFn,
    texts: &[String],
    label: &str,
    progress_total: Option<usize>,
) -> Vec<Vec<f32>> {
    let done = Arc::new(AtomicUsize::new(0));
    stream::iter(texts.iter().map(|t| {
        let embed = embed.clone();
        let t = t.clone();
        let done = done.clone();
        async move {
            let out = match (embed)(&t).await {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("warning: {label} embed failed: {e}");
                    Vec::new()
                }
            };
            if let Some(total) = progress_total {
                let n = done.fetch_add(1, Ordering::Relaxed) + 1;
                if n % 1000 == 0 || n == total {
                    eprintln!("  embedded {n}/{total} {label}s");
                }
            }
            out
        }
    }))
    .buffered(EMBED_CONCURRENCY)
    .collect::<Vec<Vec<f32>>>()
    .await
}

/// Force every vector to exactly `dim` elements: a failed embed (empty) or any
/// off-dim row becomes a zero vector (→ 0 similarity). Keeps the on-disk matrix a
/// clean fixed-stride block and cosine well-defined.
fn normalize_dim(vecs: &mut [Vec<f32>], dim: usize) {
    for v in vecs.iter_mut() {
        if v.len() != dim {
            *v = vec![0.0f32; dim];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claim_with_entities(entities: &[&str]) -> Claim {
        Claim {
            statement: "s".into(),
            conditions: vec![],
            referenced_entities: entities.iter().map(|s| s.to_string()).collect(),
            normativity: "contract".into(),
            source: String::new(),
        }
    }

    #[test]
    fn ent_tokens_splits_and_filters_short() {
        let c = claim_with_entities(&["CorpusEngine::reindex_file", "ab", "Foo.bar_baz"]);
        let toks = ent_tokens(&c);
        // len >= 5, lowercased: "corpusengine", "reindex_file", "bar_baz".
        assert!(toks.contains("corpusengine"));
        assert!(toks.contains("reindex_file"));
        assert!(toks.contains("bar_baz"));
        // "ab" and "Foo" (len 3) are dropped.
        assert!(!toks.contains("ab"));
        assert!(!toks.contains("foo"));
    }

    #[test]
    fn cosine_sim_basic() {
        assert!((cosine_sim(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < 1e-6);
        assert!(cosine_sim(&[1.0, 0.0], &[0.0, 1.0]).abs() < 1e-6);
        // zero / empty vector is degenerate → 0.0 (graceful-degradation path).
        assert_eq!(cosine_sim(&[1.0, 2.0], &[]), 0.0);
        assert_eq!(cosine_sim(&[0.0, 0.0], &[1.0, 1.0]), 0.0);
    }

    #[test]
    fn classify_maps_verdicts() {
        assert_eq!(classify("entails"), FindingKind::Corroborated);
        assert_eq!(classify("partial"), FindingKind::Todo);
        assert_eq!(classify("contradicts"), FindingKind::Drift);
        assert_eq!(classify("unrelated"), FindingKind::Gap);
        assert_eq!(classify("garbage"), FindingKind::Gap);
    }

    #[test]
    fn parse_verdict_plain_and_fenced() {
        let plain = r#"{"per_condition":[{"condition":"c","met":true,"evidence":"f (a.rs:1)"}],"verdict":"entails","note":"ok"}"#;
        let v = parse_verdict(plain).expect("plain");
        assert_eq!(v.verdict, "entails");
        assert_eq!(v.per_condition.len(), 1);
        assert!(v.per_condition[0].met);

        let fenced = "```json\n{\"per_condition\":[],\"verdict\":\"unrelated\",\"note\":\"n\"}\n```";
        let v = parse_verdict(fenced).expect("fenced");
        assert_eq!(v.verdict, "unrelated");

        assert!(parse_verdict("not json").is_none());
    }

    #[test]
    fn plan_claim_floors_when_no_evidence() {
        // One function whose vector is orthogonal to the claim → best cosine 0,
        // and no symbol hit → Unverifiable.
        let fns = vec![FnMeta {
            name: "alpha".into(),
            file: "a.rs".into(),
            line: 1,
            summary: "x".into(),
        }];
        let fn_vecs = vec![vec![0.0f32, 1.0]];
        let claim = claim_with_entities(&[]);
        match plan_claim(&claim, &[1.0, 0.0], &fns, &fn_vecs) {
            Plan::Unverifiable { best } => assert!(best < FLOOR),
            Plan::Adjudicate { .. } => panic!("expected Unverifiable"),
        }
    }

    #[test]
    fn plan_claim_symbol_hit_survives_floor() {
        // Orthogonal vector (cosine 0) but the claim names the function → the
        // symbol hit rescues it past the floor.
        let fns = vec![FnMeta {
            name: "reindex_file".into(),
            file: "a.rs".into(),
            line: 1,
            summary: "x".into(),
        }];
        let fn_vecs = vec![vec![0.0f32, 1.0]];
        let claim = claim_with_entities(&["reindex_file"]);
        match plan_claim(&claim, &[1.0, 0.0], &fns, &fn_vecs) {
            Plan::Adjudicate { cand } => assert_eq!(cand, vec![0]),
            Plan::Unverifiable { .. } => panic!("symbol hit should survive the floor"),
        }
    }

    #[test]
    fn fn_vec_roundtrip_via_tempdir() {
        let dir = tempfile::tempdir().unwrap();
        let fns = vec![FnMeta {
            name: "f".into(),
            file: "x.rs".into(),
            line: 7,
            summary: "does a thing".into(),
        }];
        let vecs = vec![vec![1.0f32, 2.0, 3.0]];
        save_fn_vecs(dir.path(), "c1", 42, 3, &fns, &vecs).unwrap();
        let (got_fns, got_vecs) = load_fn_vecs(dir.path(), "c1", 42, 3).expect("hit");
        assert_eq!(got_fns.len(), 1);
        assert_eq!(got_fns[0].name, "f");
        assert_eq!(got_vecs, vecs);
        // Guard mismatch (cache changed) → miss; dim mismatch (model changed) → miss.
        assert!(load_fn_vecs(dir.path(), "c1", 99, 3).is_none());
        assert!(load_fn_vecs(dir.path(), "c1", 42, 4).is_none());
    }

    #[test]
    fn normalize_dim_zeroes_bad_rows() {
        let mut vecs = vec![vec![1.0f32, 2.0], vec![], vec![9.0f32]];
        normalize_dim(&mut vecs, 2);
        assert_eq!(vecs[0], vec![1.0, 2.0]);
        assert_eq!(vecs[1], vec![0.0, 0.0]);
        assert_eq!(vecs[2], vec![0.0, 0.0]);
    }
}
