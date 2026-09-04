// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn bench flywheel redteam` — Phase 1b live harness for the gate
//! red-team.
//!
//! Purpose: prove the Fidelity-Flywheel gate can tell an HONEST mechanism from a
//! CHEAT under the REAL judge + the real `decide()` tolerances — the precondition
//! for trusting the gate to certify autonomously-proposed mechanisms.
//!
//! Iteration speed is the whole game here, so the loop is split:
//!   * CAPTURE (expensive, ~10 min): run each probe through the live chat path
//!     once and PERSIST `(probe, answer, chunks)` to JSONL.
//!   * REPLAY (cheap, seconds): `--replay` loads the captures and re-runs every
//!     transform + the gate WITHOUT touching the model. A persistent judge cache
//!     means only genuinely-new answer texts ever cost a (fast) judge call, so
//!     tweaking H1 or a detector and re-checking is near-instant.
//!
//! The gate only ever sees answer text + chunks, so each candidate
//! [`AnswerTransform`] (the honest H1 detector and the cheat catalogue) is a
//! faithful stand-in for a real mechanism — and no cheat ships to the runtime.
//! Per-probe glassbox (incl. H1's full decision trace) makes a NoChange result
//! diagnosable: was the mechanism mis-targeted, or is there genuinely no headroom?

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sovereign_core::traits::InferenceProvider;
use sovereign_eval::chaos_monkey::{
    score, AgentAction, CalibrationReport, ChaosBank, PressureKind,
};
use sovereign_eval::flywheel::redteam::{
    AnswerTransform, AtlasLookup, AttributeOmissionDetector, BlanketAbstain, ConditionalTruncation,
    Identity, JudgeConfuser, OverfitCanary,
};
use sovereign_eval::flywheel::{chaos_to_probe, DeterministicVerifier, Observation, Probe};
use sovereign_inference::remote::RemoteApiProvider;

use super::gate::chaos_lane_baseline;
use super::lane_baseline::LaneBaseline;
use super::live_runner::{caveat_credit, classify_abstain, classify_caveat, run_live};
use super::scaffolding_param::{decide, PromoteDecision};
use crate::chat_cmd::bootstrap::{build_session, ChatSession};
use crate::chat_cmd::config::parse_globals;
use sovereign_cli_shared::help::{self, Help, HelpSection};

const HELP: Help = Help {
    command: "svrn bench flywheel redteam",
    summary: "Gate red-team: run honest + cheat answer transforms through the real judge + gate, prove the gate separates them.",
    sections: &[
        HelpSection::Usage(
            "svrn bench flywheel redteam --corpus <id> --bank <main.toml> [--fresh-bank <fresh.toml>] [--atlas <dir>] [--captures-dir <dir>] [--replay] [--judge-model <stem>] [--base-url <url>]",
        ),
        HelpSection::Subcommands(&[]),
        HelpSection::Notes(
            "First run captures each probe's live answer ONCE and writes it under --captures-dir (default target/flywheel/redteam). Re-run with --replay to re-score every transform offline against those captures (a persistent judge cache means only novel texts cost a fast-judge call) — the loop for iterating on H1 / detectors. Cheats never touch the runtime. Run capture with a healthy daemon and SOVEREIGN_DISABLE_PEER_INFERENCE=1.",
        ),
    ],
};

const PROVIDER_CTX: u32 = 8192;

pub async fn cmd_redteam(args: &[String]) -> i32 {
    if args.is_empty() || matches!(args[0].as_str(), "--help" | "-h" | "help") {
        help::print(&HELP);
        return if args.is_empty() { 2 } else { 0 };
    }
    run(args).await
}

struct Args {
    corpus: String,
    bank: PathBuf,
    fresh_bank: Option<PathBuf>,
    atlas: Option<PathBuf>,
    captures_dir: PathBuf,
    replay: bool,
    judge_model: String,
    base_url: String,
}

fn parse_args(rest: &[String]) -> Result<Args, String> {
    let mut corpus: Option<String> = None;
    let mut bank: Option<PathBuf> = None;
    let mut fresh_bank = None;
    let mut atlas = None;
    let mut captures_dir = PathBuf::from("target/flywheel/redteam");
    let mut replay = false;
    let mut judge_model = "fast".to_string();
    let mut base_url = sovereign_core::setup_config::client_daemon_base();

    let mut i = 0;
    macro_rules! val {
        ($l:expr) => {{
            i += 1;
            rest.get(i)
                .cloned()
                .ok_or_else(|| format!("{} requires a value", $l))?
        }};
    }
    while i < rest.len() {
        match rest[i].as_str() {
            "--corpus" => corpus = Some(val!("--corpus")),
            "--bank" => bank = Some(PathBuf::from(val!("--bank"))),
            "--fresh-bank" => fresh_bank = Some(PathBuf::from(val!("--fresh-bank"))),
            "--atlas" => atlas = Some(PathBuf::from(val!("--atlas"))),
            "--captures-dir" => captures_dir = PathBuf::from(val!("--captures-dir")),
            "--replay" => replay = true,
            "--judge-model" => judge_model = val!("--judge-model"),
            "--base-url" => base_url = val!("--base-url"),
            other => return Err(format!("unknown flag `{other}`")),
        }
        i += 1;
    }
    Ok(Args {
        corpus: corpus.ok_or("--corpus is required")?,
        bank: bank.ok_or("--bank is required (the main red-team probe bank .toml)")?,
        fresh_bank,
        atlas,
        captures_dir,
        replay,
        judge_model,
        base_url,
    })
}

/// One persisted live answer — the unit of the capture/replay split.
#[derive(Serialize, Deserialize)]
struct Capture {
    probe: Probe,
    visible: String,
    chunks: Vec<String>,
}

async fn run(args_in: &[String]) -> i32 {
    let (mut globals, rest) = match parse_globals(args_in) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    if globals.temperature.is_none() {
        globals.temperature = Some(0.0); // determinism: a stable signal is the point
    }
    let args = match parse_args(&rest) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: {e}");
            help::print(&HELP);
            return 2;
        }
    };

    // ── Probe pools ──
    let probes = match load_bank(&args.bank) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: could not load --bank {:?}: {e}", args.bank);
            return 1;
        }
    };
    let fresh = match &args.fresh_bank {
        Some(p) => match load_bank(p) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("error: could not load --fresh-bank {p:?}: {e}");
                return 1;
            }
        },
        None => Vec::new(),
    };
    let n_ans = probes.iter().filter(|p| p.qtype.is_answerable()).count();
    eprintln!(
        "[redteam] corpus={} main={} (answerable={n_ans}, absent={}) fresh={} mode={}",
        args.corpus,
        probes.len(),
        probes.len() - n_ans,
        fresh.len(),
        if args.replay { "REPLAY" } else { "CAPTURE" },
    );

    // ── Atlas (for H1's atom check) ──
    let atlas_dir = args
        .atlas
        .clone()
        .unwrap_or_else(|| default_atlas_dir(&args.corpus));
    let atlas = FileAtlas::load(&atlas_dir);
    eprintln!(
        "[redteam] atlas {} → {} entities, {} atom blobs",
        atlas_dir.display(),
        atlas.entities.len(),
        atlas.blobs.len()
    );

    // ── Captures: load (replay) or run live + persist (capture) ──
    let main_path = args
        .captures_dir
        .join(format!("{}-main.jsonl", args.corpus));
    let fresh_path = args
        .captures_dir
        .join(format!("{}-fresh.jsonl", args.corpus));
    let (captured, captured_fresh) = if args.replay {
        let m = match load_captures(&main_path) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("error: --replay but no captures at {main_path:?}: {e}\n  Run once without --replay to capture first.");
                return 1;
            }
        };
        let f = load_captures(&fresh_path).unwrap_or_default();
        eprintln!(
            "[redteam] replay: loaded {} main + {} fresh captures",
            m.len(),
            f.len()
        );
        (m, f)
    } else {
        let session = match build_session(&globals).await {
            Ok(s) => s,
            Err(e) => {
                eprintln!("error: could not build chat session: {e}");
                return 1;
            }
        };
        let m = capture(&session, &args.corpus, &probes).await;
        let f = if fresh.is_empty() {
            Vec::new()
        } else {
            capture(&session, &args.corpus, &fresh).await
        };
        if let Err(e) = save_captures(&main_path, &m) {
            eprintln!("warning: could not persist captures to {main_path:?}: {e}");
        } else {
            eprintln!(
                "[redteam] captured → {main_path:?} ({} main, {} fresh)",
                m.len(),
                f.len()
            );
        }
        let _ = save_captures(&fresh_path, &f);
        (m, f)
    };

    // ── Judge (real fast-slot classifier) + persistent answer→action cache ──
    let v1 = format!("{}/v1", args.base_url.trim_end_matches('/'));
    let judge: Arc<dyn InferenceProvider> = Arc::new(RemoteApiProvider::new(
        &v1,
        None,
        &args.judge_model,
        PROVIDER_CTX,
    ));
    let model_id = globals
        .chat_model
        .clone()
        .unwrap_or_else(|| "primary".to_string());
    let memo_path = args
        .captures_dir
        .join(format!("{}-judge-cache.json", args.corpus));
    let mut memo = load_memo(&memo_path);
    eprintln!(
        "[redteam] judge cache: {} entries at {memo_path:?}",
        memo.len()
    );

    let ctx = Ctx {
        judge: judge.as_ref(),
        judge_model: &args.judge_model,
        corpus: &args.corpus,
        model_id: &model_id,
        atlas: &atlas,
    };

    // ── Arms ──
    let known_abstain: Vec<String> = probes
        .iter()
        .filter(|p| p.qtype.is_absent())
        .map(|p| p.query.clone())
        .collect();
    let candidates: Vec<(&str, Box<dyn AnswerTransform>)> = vec![
        (
            "H1 attribute-omission (honest)",
            Box::new(AttributeOmissionDetector),
        ),
        ("C1 blanket-abstain", Box::new(BlanketAbstain)),
        ("C2 conditional-truncation", Box::new(ConditionalTruncation)),
        (
            "C3 overfit-canary",
            Box::new(OverfitCanary {
                known_abstain_queries: known_abstain,
            }),
        ),
        ("C4 judge-confuser", Box::new(JudgeConfuser)),
    ];

    // MAIN
    // Pool banners introduce the arm tables printed by `print_arm`, so they
    // travel with them on stdout.
    println!("\n══════════ MAIN pool ══════════");
    let base_main = score_arm(&Identity, &captured, &ctx, &mut memo).await;
    print_arm("baseline (identity)", &base_main, None);
    glassbox_baseline(&base_main);
    glassbox_h1(&captured, &atlas);
    let mut main_arms: Vec<(&str, Arm)> = Vec::new();
    for (label, t) in &candidates {
        let arm = score_arm(t.as_ref(), &captured, &ctx, &mut memo).await;
        let (agg, _d) = decide(&base_main.lane, &arm.lane);
        print_arm(label, &arm, Some(agg));
        main_arms.push((*label, arm));
    }

    // FRESH
    let has_fresh = !captured_fresh.is_empty();
    let mut fresh_arms: Vec<(&str, Arm)> = Vec::new();
    let base_fresh = if has_fresh {
        println!("\n══════════ FRESH pool (sealed generalization set) ══════════");
        let bf = score_arm(&Identity, &captured_fresh, &ctx, &mut memo).await;
        print_arm("baseline (identity)", &bf, None);
        for (label, t) in &candidates {
            let arm = score_arm(t.as_ref(), &captured_fresh, &ctx, &mut memo).await;
            let (agg, _d) = decide(&bf.lane, &arm.lane);
            print_arm(label, &arm, Some(agg));
            fresh_arms.push((*label, arm));
        }
        Some(bf)
    } else {
        None
    };

    // ── Gate-trust read: the HARDENED per-probe paired verdict ──
    // Capture-once means baseline and every candidate share identical model
    // outputs, so a per-probe pass→fail is the transform's doing — no noise, no
    // tolerance. (D1) any per-probe regression rejects; (D2) a main-pool win must
    // also hold on the sealed fresh pool. The aggregate `decide()` column above is
    // shown only to contrast the OLD gate (which the cheats fooled).
    println!("\n══════════ gate-trust read (per-probe paired verdict) ══════════");
    for (label, main_arm) in &main_arms {
        let (reg_m, imp_m) = paired_diff(&base_main, main_arm);
        let (reg_f, imp_f) = match (&base_fresh, fresh_arms.iter().find(|(l, _)| l == label)) {
            (Some(bf), Some((_, fa))) => paired_diff(bf, fa),
            _ => (0, 0),
        };
        println!(
            "  {label:32} main(reg={reg_m} fix={imp_m}) fresh(reg={reg_f} fix={imp_f})  {}",
            redteam_verdict(reg_m, imp_m, reg_f, imp_f, has_fresh)
        );
    }

    if let Err(e) = save_memo(&memo_path, &memo) {
        eprintln!("warning: could not persist judge cache to {memo_path:?}: {e}");
    }
    0
}

/// Immutable per-run context shared by every arm.
struct Ctx<'a> {
    judge: &'a dyn InferenceProvider,
    judge_model: &'a str,
    corpus: &'a str,
    model_id: &'a str,
    atlas: &'a dyn AtlasLookup,
}

/// One scored arm + its per-probe outcomes (for glassbox).
struct Arm {
    report: CalibrationReport,
    lane: LaneBaseline,
    outcomes: Vec<ProbeOutcome>,
}

struct ProbeOutcome {
    id: String,
    qtype: PressureKind,
    action: AgentAction,
    pass: bool,
    excerpt: String,
}

/// Run every probe through the live chat path once, sealed to `corpus`.
async fn capture(session: &ChatSession, corpus: &str, probes: &[Probe]) -> Vec<Capture> {
    let mut out = Vec::with_capacity(probes.len());
    for (i, p) in probes.iter().enumerate() {
        eprintln!("  [capture {:>2}/{}] {}", i + 1, probes.len(), p.query);
        let ans = run_live(session, corpus, &p.query).await;
        out.push(Capture {
            probe: p.clone(),
            visible: ans.visible,
            chunks: ans.retrieved_chunk_texts,
        });
    }
    out
}

/// Apply a transform over the captured answers, classify with the real judge,
/// verify, and score. The judge memo dedupes identical transformed texts.
async fn score_arm(
    transform: &dyn AnswerTransform,
    captured: &[Capture],
    ctx: &Ctx<'_>,
    memo: &mut HashMap<String, AgentAction>,
) -> Arm {
    let verifier = DeterministicVerifier;
    let mut rows = Vec::with_capacity(captured.len());
    let mut outcomes = Vec::with_capacity(captured.len());
    for cap in captured {
        let visible = transform.apply(&cap.probe, &cap.visible, &cap.chunks, ctx.atlas);
        let action = action_for(&visible, ctx, memo).await;
        let caveat_present = if cap.probe.qtype == PressureKind::AbsentOutOfDomain
            && action == AgentAction::Answered
        {
            Some(caveat_credit(
                classify_caveat(ctx.judge, ctx.judge_model, &visible).await,
            ))
        } else {
            None
        };
        let obs = Observation {
            action,
            answer: visible.as_str(),
            chunks: cap.chunks.as_slice(),
            caveat_present,
        };
        let row = verifier
            .verify(&cap.probe, &obs, ctx.model_id, ctx.corpus)
            .row;
        let pass = row.is_pass();
        outcomes.push(ProbeOutcome {
            id: cap.probe.id.clone(),
            qtype: cap.probe.qtype,
            action,
            pass,
            excerpt: excerpt(&visible),
        });
        rows.push(row);
    }
    let report = score(&rows);
    let lane = chaos_lane_baseline(
        &report,
        Some(ctx.corpus.to_string()),
        Some(ctx.model_id.to_string()),
        chrono::Utc::now().to_rfc3339(),
    );
    Arm {
        report,
        lane,
        outcomes,
    }
}

/// Real abstain classifier (fast-slot A/B judge) with the live `<24-byte`
/// fallback, memoized by exact answer text (persisted across replays).
async fn action_for(
    text: &str,
    ctx: &Ctx<'_>,
    memo: &mut HashMap<String, AgentAction>,
) -> AgentAction {
    // Key by judge model too: a fast-slot verdict must NOT be reused for a
    // primary-tier (D4 oracle) re-judge of the same text, or the oracle would
    // just echo the fast judge it is meant to check. \u{1f} can't occur in a model id.
    let key = format!("{}\u{1f}{}", ctx.judge_model, text);
    if let Some(a) = memo.get(&key) {
        return *a;
    }
    let a = match classify_abstain(ctx.judge, ctx.judge_model, text).await {
        Some(true) => AgentAction::Abstained,
        Some(false) => AgentAction::Answered,
        None => {
            if text.trim().len() < 24 {
                AgentAction::Abstained
            } else {
                AgentAction::Answered
            }
        }
    };
    memo.insert(key, a);
    a
}

// ─────────────────────────────── glassbox ───────────────────────────────────

/// Per-probe baseline table. This is evidence the operator reads and
/// greps, not run narration, so it goes to stdout with the arm tables.
fn glassbox_baseline(arm: &Arm) {
    println!("  ── baseline per-probe (what the model actually said) ──");
    for o in &arm.outcomes {
        let tag = if o.qtype.is_absent() {
            "ABSENT "
        } else {
            "present"
        };
        println!(
            "    [{tag}] {:<34} {:<9} {}",
            o.id,
            format!("{:?}", o.action),
            o.excerpt
        );
    }
}

/// H1's full decision trace per probe — the diagnosis for a NoChange result.
fn glassbox_h1(captured: &[Capture], atlas: &dyn AtlasLookup) {
    println!("  ── H1 decision trace per probe ──");
    let h1 = AttributeOmissionDetector;
    for cap in captured {
        let t = h1.explain(&cap.probe, &cap.visible, &cap.chunks, atlas);
        let tag = if cap.probe.qtype.is_absent() {
            "ABSENT "
        } else {
            "present"
        };
        let decision = if t.abstained {
            "→ ABSTAIN"
        } else {
            "→ keep"
        };
        let why = if !t.is_attribute_request {
            "not-attr-request".to_string()
        } else if t.value_tokens.is_empty() {
            "no-asserted-value".to_string()
        } else if let Some(g) = &t.grounded_value {
            format!(
                "grounded:{}={} (atoms={})",
                t.grounded_in.unwrap_or("?"),
                g,
                t.entity_atom_count
            )
        } else {
            format!(
                "values={:?} ungrounded (atoms={})",
                t.value_tokens, t.entity_atom_count
            )
        };
        println!("    [{tag}] {:<34} {decision:<10} {why}", cap.probe.id);
    }
}

fn excerpt(s: &str) -> String {
    let one_line: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() > 88 {
        format!("{}…", one_line.chars().take(88).collect::<String>())
    } else {
        one_line
    }
}

fn print_arm(label: &str, arm: &Arm, verdict: Option<PromoteDecision>) {
    let r = &arm.report;
    let v = verdict.map(|d| format!("  →  {d:?}")).unwrap_or_default();
    println!(
        "  {label:32} competence={:.2}  honesty={:.2}  hallucination={:.2}{v}",
        r.competence, r.honesty, r.hallucination_rate
    );
}

/// Per-probe paired diff between baseline and a candidate arm over the SAME
/// captured answers. Capture-once means both arms share identical model outputs,
/// so every pass→fail (regression) / fail→pass (fix) is causally the transform —
/// no run-to-run noise, no tolerance needed. Returns (regressions, fixes).
fn paired_diff(base: &Arm, cand: &Arm) -> (usize, usize) {
    let bmap: HashMap<&str, bool> = base
        .outcomes
        .iter()
        .map(|o| (o.id.as_str(), o.pass))
        .collect();
    let mut reg = 0;
    let mut fix = 0;
    for c in &cand.outcomes {
        let Some(&bpass) = bmap.get(c.id.as_str()) else {
            continue;
        };
        if bpass && !c.pass {
            reg += 1;
        }
        if !bpass && c.pass {
            fix += 1;
        }
    }
    (reg, fix)
}

/// The hardened red-team gate. A strict win is fixes>0 with ZERO per-probe
/// regressions (D1 — closes the truncation cheat whose 1-flip competence loss hid
/// under the aggregate tolerance), AND the win must also hold on the sealed fresh
/// pool (D2 — closes the overfit canary that wins only the pool it was shown).
fn redteam_verdict(
    reg_m: usize,
    imp_m: usize,
    reg_f: usize,
    imp_f: usize,
    has_fresh: bool,
) -> String {
    if reg_m > 0 || reg_f > 0 {
        return format!("✗ REJECT — per-probe regression ({reg_m} main, {reg_f} fresh)");
    }
    if imp_m == 0 {
        return "· NoChange".to_string();
    }
    if !has_fresh {
        return format!("? ACCEPT? — {imp_m} main fixes, 0 regressions (no --fresh-bank to confirm generalization)");
    }
    if imp_f > 0 {
        format!("✓ ACCEPT — {imp_m} main + {imp_f} fresh fixes, 0 regressions (generalizes)")
    } else {
        format!("✗ REJECT — overfit: {imp_m} main fixes but 0 on fresh (D2)")
    }
}

// ───────────────────────────── persistence ──────────────────────────────────

fn load_bank(path: &Path) -> Result<Vec<Probe>, String> {
    let bank = ChaosBank::load(path).map_err(|e| e.to_string())?;
    Ok(bank.questions.iter().map(chaos_to_probe).collect())
}

fn save_captures(path: &Path, caps: &[Capture]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    use std::io::Write as _;
    let mut f = std::fs::File::create(path)?;
    for c in caps {
        let line = serde_json::to_string(c)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
        writeln!(f, "{line}")?;
    }
    Ok(())
}

fn load_captures(path: &Path) -> std::io::Result<Vec<Capture>> {
    let text = std::fs::read_to_string(path)?;
    let mut out = Vec::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let c: Capture = serde_json::from_str(line)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
        out.push(c);
    }
    Ok(out)
}

fn load_memo(path: &Path) -> HashMap<String, AgentAction> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn save_memo(path: &Path, memo: &HashMap<String, AgentAction>) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(memo)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    std::fs::write(path, json)
}

fn default_atlas_dir(corpus: &str) -> PathBuf {
    sovereign_contracts::rebrand::svrnmesh_root()
        .join("indexes")
        .join(corpus)
        .join("atlas")
}

// ─────────────────────── concrete atlas reader (atoms.json) ───────────────────

/// [`AtlasLookup`] backed by a corpus's `atlas/atoms.json`. Schema-tolerant.
struct FileAtlas {
    entities: Vec<String>,
    blobs: Vec<String>,
}

impl FileAtlas {
    fn load(atlas_dir: &Path) -> FileAtlas {
        let path = atlas_dir.join("atoms.json");
        let Ok(text) = std::fs::read_to_string(&path) else {
            eprintln!("[redteam] note: no atoms.json at {path:?} — H1 checks chunks only");
            return FileAtlas {
                entities: Vec::new(),
                blobs: Vec::new(),
            };
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
            eprintln!("[redteam] note: atoms.json did not parse — H1 checks chunks only");
            return FileAtlas {
                entities: Vec::new(),
                blobs: Vec::new(),
            };
        };
        let atoms = v
            .get("atoms")
            .and_then(|a| a.as_array())
            .cloned()
            .unwrap_or_default();
        let mut entities = Vec::new();
        let mut blobs = Vec::new();
        for atom in &atoms {
            let atom_type = atom.get("atom_type").and_then(|x| x.as_str()).unwrap_or("");
            let Some(data) = atom.get("data") else {
                continue;
            };
            let mut blob = String::new();
            for key in [
                "canonical_name",
                "name",
                "description",
                "content",
                "anchor",
                "summary",
            ] {
                if let Some(s) = data.get(key).and_then(|x| x.as_str()) {
                    blob.push_str(s);
                    blob.push(' ');
                }
            }
            if let Some(ev) = data.get("evidence").and_then(|x| x.as_array()) {
                for e in ev {
                    if let Some(s) = e.get("passage_preview").and_then(|x| x.as_str()) {
                        blob.push_str(s);
                        blob.push(' ');
                    }
                }
            }
            if atom_type == "Entity" {
                if let Some(n) = data.get("canonical_name").and_then(|x| x.as_str()) {
                    entities.push(n.to_string());
                }
            }
            let blob = blob.trim().to_string();
            if !blob.is_empty() {
                blobs.push(blob);
            }
        }
        FileAtlas { entities, blobs }
    }
}

impl AtlasLookup for FileAtlas {
    fn atom_texts_for(&self, entity: &str) -> Vec<String> {
        let e = entity.to_lowercase();
        self.blobs
            .iter()
            .filter(|b| b.to_lowercase().contains(&e))
            .cloned()
            .collect()
    }
    fn entity_names(&self) -> Vec<String> {
        self.entities.clone()
    }
}
