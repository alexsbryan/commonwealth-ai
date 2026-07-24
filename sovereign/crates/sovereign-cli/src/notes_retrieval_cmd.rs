//! `sovereign notes retrieval-audit` — the measurement half of the E2/P4
//! rational-forgetting instrument (MEMORY_MODEL §5 E2, principle P4).
//!
//! The `inject-notes` UserPromptSubmit hook fires a 10–14 KB block of notes
//! into context on *every* prompt, ranked purely by semantic relevance — and
//! until now with **zero** measurement of whether any injected note was ever
//! actually used. E2's mandate is "measure before tuning": establish the
//! current ranker's injection **hit-rate** as a baseline before replacing it
//! with a need-probability (recency × retrieval-frequency) ranker.
//!
//! This command is the read side. It joins two purely-local sources:
//!   1. the retrieval log the hook appends per injection
//!      (`~/.sovereign/retrieval-log/<session>.jsonl`) — WHAT entered context;
//!   2. the Claude Code session transcript
//!      (`~/.claude/projects/<enc-cwd>/<session>.jsonl`) — what the agent then
//!      DID.
//! Like `cache-audit`, it reads only local files — no daemon, no network, no
//! mutation.
//!
//! ## What "used" means (and its honest bias)
//!
//! We cannot observe intent, only co-occurrence, so a note counts as used when
//! its distinctive anchors reappear in the agent's own downstream actions
//! (assistant text + tool-call inputs; tool *results* are excluded — those are
//! the environment talking, not the agent choosing). Three signals, reported
//! separately so the metric is glassbox rather than a single opaque number:
//!   - **symbol hit** — a note `symbols[]` entry reappears downstream (strong);
//!   - **file hit** — a note `files[]` basename reappears downstream (strong);
//!   - **content hit** — ≥2 distinct `terms[]` (identifier-shaped tokens the
//!     hook pre-extracted) reappear (softer; the only signal available for the
//!     ~5-in-8 notes that carry no symbols/files).
//!
//! Two rates fall out, and both are honest about their denominator:
//!   - `strong` = anchor-used ÷ notes-that-have-an-anchor (high-confidence floor)
//!   - `any`    = (anchor OR content)-used ÷ all-injected-notes (headline)
//!
//! This is an **upper bound** on true usage: a note may share a term with text
//! the agent would have written regardless. That bias is constant across ranker
//! versions, so the before/after comparison the E2 gate needs stays valid.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::cache_audit_cmd::resolve_transcript_dir;

/// Minimum needle length for a substring match to count. Guards against a short
/// token ("id", "fn") matching inside unrelated words. Real symbols/terms are
/// identifier-length; the hook only logs terms of length ≥5.
const MIN_MATCH_LEN: usize = 4;

// ── Retrieval-log records (the subset we consume) ──────────────────────────

#[derive(Debug, Deserialize)]
struct LoggedNote {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    symbols: Vec<String>,
    #[serde(default)]
    files: Vec<String>,
    #[serde(default)]
    terms: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct InjectionRecord {
    #[serde(default)]
    session_id: String,
    #[serde(default)]
    notes: Vec<LoggedNote>,
}

// ── Per-note verdict ───────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct NoteUsage {
    id: String,
    kind: String,
    /// How many prompts injected this note (retrieval frequency — the raw
    /// signal the future need-probability ranker will consume).
    injections: u64,
    has_anchor: bool,
    symbol_hit: bool,
    file_hit: bool,
    /// Number of distinct distinctive-terms that reappeared downstream.
    term_hits: usize,
}

impl NoteUsage {
    fn anchor_used(&self) -> bool {
        self.symbol_hit || self.file_hit
    }
    fn content_used(&self) -> bool {
        self.term_hits >= 2
    }
    fn used_any(&self) -> bool {
        self.anchor_used() || self.content_used()
    }
}

/// Rolled-up result for one session's log × transcript join.
#[derive(Debug)]
struct SessionAudit {
    session_id: String,
    /// Present iff a matching transcript was found — otherwise the log records
    /// injections we cannot correlate (session never produced a transcript, or
    /// it lives under a different project dir).
    transcript_found: bool,
    notes: Vec<NoteUsage>,
}

impl SessionAudit {
    fn injected(&self) -> usize {
        self.notes.len()
    }
    fn anchored(&self) -> usize {
        self.notes.iter().filter(|n| n.has_anchor).count()
    }
    fn anchor_used(&self) -> usize {
        self.notes.iter().filter(|n| n.anchor_used()).count()
    }
    fn used_any(&self) -> usize {
        self.notes.iter().filter(|n| n.used_any()).count()
    }
}

// ── Downstream evidence (what the agent did) ───────────────────────────────

/// One lowercased blob of the agent's own actions: assistant text blocks plus
/// the serialized `input` of every tool call. Tool *results* are deliberately
/// excluded — they are the environment's output, and counting them would let a
/// `Read` echoing a file match the very note that named it, inflating usage.
struct Evidence {
    blob: String,
}

impl Evidence {
    fn contains(&self, needle: &str) -> bool {
        let n = needle.trim().to_lowercase();
        if n.len() < MIN_MATCH_LEN {
            return false;
        }
        self.blob.contains(&n)
    }

    /// A file counts if its basename reappears — paths get rewritten
    /// (absolute vs. repo-relative) but the leaf is stable.
    fn contains_file(&self, path: &str) -> bool {
        let base = path.rsplit('/').next().unwrap_or(path);
        self.contains(base)
    }
}

fn build_evidence(transcript_path: &Path) -> Option<Evidence> {
    let text = std::fs::read_to_string(transcript_path).ok()?;
    let mut blob = String::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let obj: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let msg = match obj.get("message") {
            Some(m) if m.is_object() => m,
            _ => continue,
        };
        // Only the agent's own turns count as "use". The human's prompt tokens
        // matching a note means the note was RELEVANT (why it was injected),
        // not that it was used — including them would inflate every rate.
        let role = msg
            .get("role")
            .and_then(|r| r.as_str())
            .or_else(|| obj.get("type").and_then(|t| t.as_str()))
            .unwrap_or("");
        if role != "assistant" {
            continue;
        }
        let content = match msg.get("content").and_then(|c| c.as_array()) {
            Some(c) => c,
            None => continue,
        };
        for block in content {
            match block.get("type").and_then(|t| t.as_str()) {
                Some("text") => {
                    if let Some(t) = block.get("text").and_then(|t| t.as_str()) {
                        blob.push_str(t);
                        blob.push('\n');
                    }
                }
                Some("tool_use") => {
                    // The tool INPUT is the agent's action (which file it chose
                    // to read, which symbol it looked up, what it grepped for).
                    if let Some(input) = block.get("input") {
                        blob.push_str(&input.to_string());
                        blob.push('\n');
                    }
                }
                _ => {}
            }
        }
    }
    blob.make_ascii_lowercase();
    Some(Evidence { blob })
}

// ── Join ───────────────────────────────────────────────────────────────────

fn load_injections(log_path: &Path) -> Vec<InjectionRecord> {
    let text = match std::fs::read_to_string(log_path) {
        Ok(t) => t,
        Err(_) => return Vec::new(),
    };
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<InjectionRecord>(l).ok())
        .collect()
}

/// Aggregate a session's injection records, then score each unique note against
/// its downstream evidence. A note re-injected across N prompts is ONE note for
/// hit-rate purposes (with `injections = N` retained as the frequency signal).
fn audit_session(
    session_id: &str,
    records: &[InjectionRecord],
    evidence: Option<&Evidence>,
) -> SessionAudit {
    // note_id -> accumulator. Preserve first-seen order for stable output.
    let mut order: Vec<String> = Vec::new();
    let mut acc: BTreeMap<String, (String, u64, Vec<String>, Vec<String>, Vec<String>)> =
        BTreeMap::new();
    for rec in records {
        for n in &rec.notes {
            // Fall back to a synthetic key when a note predates id logging, so
            // anchorless legacy records still aggregate rather than vanish.
            let key = n.id.clone().unwrap_or_else(|| {
                format!(
                    "anon:{}:{}",
                    n.kind,
                    n.terms.first().cloned().unwrap_or_default()
                )
            });
            let entry = acc.entry(key.clone()).or_insert_with(|| {
                order.push(key.clone());
                (n.kind.clone(), 0, Vec::new(), Vec::new(), Vec::new())
            });
            entry.1 += 1;
            for s in &n.symbols {
                if !entry.2.contains(s) {
                    entry.2.push(s.clone());
                }
            }
            for f in &n.files {
                if !entry.3.contains(f) {
                    entry.3.push(f.clone());
                }
            }
            for t in &n.terms {
                if !entry.4.contains(t) {
                    entry.4.push(t.clone());
                }
            }
        }
    }

    let notes = order
        .into_iter()
        .map(|key| {
            let (kind, injections, symbols, files, terms) = acc.remove(&key).unwrap();
            let has_anchor = !symbols.is_empty() || !files.is_empty();
            let (symbol_hit, file_hit, term_hits) = match evidence {
                Some(ev) => (
                    symbols.iter().any(|s| ev.contains(s)),
                    files.iter().any(|f| ev.contains_file(f)),
                    terms.iter().filter(|t| ev.contains(t)).count(),
                ),
                // No transcript to correlate against: every hit is unknown, so
                // score nothing as used rather than guessing.
                None => (false, false, 0),
            };
            NoteUsage {
                id: key,
                kind,
                injections,
                has_anchor,
                symbol_hit,
                file_hit,
                term_hits,
            }
        })
        .collect();

    SessionAudit {
        session_id: session_id.to_string(),
        transcript_found: evidence.is_some(),
        notes,
    }
}

// ── Driver ─────────────────────────────────────────────────────────────────

fn retrieval_log_dir(override_dir: Option<&str>) -> Result<PathBuf, String> {
    if let Some(d) = override_dir {
        return Ok(PathBuf::from(d));
    }
    let home = dirs::home_dir().ok_or_else(|| "could not locate the home directory".to_string())?;
    Ok(home.join(".sovereign").join("retrieval-log"))
}

struct Opts {
    project: Option<String>,
    transcript_dir: Option<String>,
    log_dir: Option<String>,
    session: Option<String>,
    json: bool,
}

fn parse_opts(args: &[String]) -> Result<Opts, String> {
    let mut o = Opts {
        project: None,
        transcript_dir: None,
        log_dir: None,
        session: None,
        json: false,
    };
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--project" => {
                o.project = Some(args.get(i + 1).ok_or("--project needs a value")?.clone());
                i += 2;
            }
            "--dir" => {
                o.transcript_dir = Some(args.get(i + 1).ok_or("--dir needs a value")?.clone());
                i += 2;
            }
            "--log-dir" => {
                o.log_dir = Some(args.get(i + 1).ok_or("--log-dir needs a value")?.clone());
                i += 2;
            }
            "--session" => {
                o.session = Some(args.get(i + 1).ok_or("--session needs a value")?.clone());
                i += 2;
            }
            "--format" => {
                o.json = args.get(i + 1).map(|s| s == "json").unwrap_or(false);
                i += 2;
            }
            other => return Err(format!("unknown flag: {other}")),
        }
    }
    Ok(o)
}

/// Enumerate `<session>.jsonl` retrieval logs, honoring `--session`.
fn collect_log_files(log_dir: &Path, session: Option<&str>) -> Result<Vec<PathBuf>, String> {
    let mut out = Vec::new();
    let entries = std::fs::read_dir(log_dir).map_err(|e| {
        format!(
            "no retrieval logs at {} ({e}).\n\
             The inject-notes hook writes them per injection; run some prompts first, \
             or pass --log-dir <path>.",
            log_dir.display()
        )
    })?;
    for entry in entries.flatten() {
        let path = entry.path();
        let stem = match path.file_stem().and_then(|s| s.to_str()) {
            Some(s) => s,
            None => continue,
        };
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        if let Some(want) = session {
            if stem != want {
                continue;
            }
        }
        out.push(path);
    }
    out.sort();
    Ok(out)
}

pub async fn run(args: &[String]) -> i32 {
    if crate::util::help::wants_help(args) {
        print_help();
        return 0;
    }
    let opts = match parse_opts(args) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("retrieval-audit: {e}");
            return 2;
        }
    };

    let log_dir = match retrieval_log_dir(opts.log_dir.as_deref()) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("retrieval-audit: {e}");
            return 2;
        }
    };
    let transcript_dir =
        match resolve_transcript_dir(opts.project.as_deref(), opts.transcript_dir.as_deref()) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("retrieval-audit: {e}");
                return 2;
            }
        };
    let log_files = match collect_log_files(&log_dir, opts.session.as_deref()) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("retrieval-audit: {e}");
            return 1;
        }
    };
    if log_files.is_empty() {
        eprintln!(
            "retrieval-audit: no matching retrieval logs in {}.",
            log_dir.display()
        );
        return 1;
    }

    let mut audits = Vec::new();
    for log_path in &log_files {
        let session_id = log_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        let records = load_injections(log_path);
        if records.is_empty() {
            continue;
        }
        let transcript = transcript_dir.join(format!("{session_id}.jsonl"));
        let evidence = build_evidence(&transcript);
        audits.push(audit_session(&session_id, &records, evidence.as_ref()));
    }

    if audits.is_empty() {
        eprintln!("retrieval-audit: logs found but none carried injection records.");
        return 1;
    }

    if opts.json {
        print_json(&audits);
    } else {
        print_table(&audits);
    }
    0
}

fn rate(num: usize, den: usize) -> f64 {
    if den == 0 {
        0.0
    } else {
        100.0 * num as f64 / den as f64
    }
}

fn print_table(audits: &[SessionAudit]) {
    println!("Notes retrieval audit — injected-note hit-rate (E2/P4 baseline)\n");
    println!(
        "{:<10}  {:>4}  {:>8}  {:>10}  {:>8}",
        "session", "inj", "anchored", "strong%", "any%"
    );
    println!("{}", "-".repeat(48));

    let (mut t_inj, mut t_anch, mut t_anch_used, mut t_used_any) = (0usize, 0usize, 0usize, 0usize);
    let mut missing = 0usize;
    for a in audits {
        let inj = a.injected();
        let anch = a.anchored();
        let strong = rate(a.anchor_used(), anch);
        let any = rate(a.used_any(), inj);
        let flag = if a.transcript_found {
            ""
        } else {
            " (no transcript)"
        };
        if !a.transcript_found {
            missing += 1;
        }
        println!(
            "{:<10}  {:>4}  {:>8}  {:>9.0}%  {:>7.0}%{}",
            &a.session_id.chars().take(10).collect::<String>(),
            inj,
            anch,
            strong,
            any,
            flag
        );
        // Only sessions with a transcript contribute to the fleet rate — a
        // missing transcript means "unknown", not "zero used".
        if a.transcript_found {
            t_inj += inj;
            t_anch += anch;
            t_anch_used += a.anchor_used();
            t_used_any += a.used_any();
        }
    }

    println!("{}", "-".repeat(48));
    let fleet_strong = rate(t_anch_used, t_anch);
    let fleet_any = rate(t_used_any, t_inj);
    println!(
        "{:<10}  {:>4}  {:>8}  {:>9.0}%  {:>7.0}%",
        "FLEET", t_inj, t_anch, fleet_strong, fleet_any
    );
    println!(
        "\nstrong = anchor(symbol|file) matches ÷ anchored notes  ·  \
         any = (anchor|content) matches ÷ all injected"
    );
    // Ceiling warning. On a session working squarely ON a note's topic, the
    // note's distinctive terms flood the transcript and `any%` pegs at ~100% —
    // measuring nothing (a saturated metric cannot show a ranker improving).
    // Say so, and point the operator at the discriminating floor. Calibrating
    // the content signal (rarity/IDF-weight terms so shared-topic words stop
    // counting) is the follow-up — deferred until fleet logs reveal the
    // `any%` distribution, per E2's own measure-before-tuning rule.
    if fleet_any >= 95.0 && fleet_strong < fleet_any {
        println!(
            "\n! any% is saturated ({fleet_any:.0}%): content-term overlap can't \
             discriminate when the session is on-topic. Use strong% ({fleet_strong:.0}%) \
             as the gate metric until content-matching is rarity-weighted (E2 follow-up)."
        );
    }
    if missing > 0 {
        println!(
            "{missing} session(s) had a retrieval log but no matching transcript — \
             excluded from the fleet rate (shown as (no transcript))."
        );
    }
    println!(
        "Upper bound: co-occurrence, not proven use. Constant bias across ranker \
         versions — compare this number before/after the need-probability ranker."
    );
}

fn json_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

fn print_json(audits: &[SessionAudit]) {
    let mut out = String::from("{\"sessions\":[");
    for (si, a) in audits.iter().enumerate() {
        if si > 0 {
            out.push(',');
        }
        out.push_str(&format!(
            "{{\"session_id\":\"{}\",\"transcript_found\":{},\"injected\":{},\"anchored\":{},\
             \"anchor_used\":{},\"used_any\":{},\"notes\":[",
            json_escape(&a.session_id),
            a.transcript_found,
            a.injected(),
            a.anchored(),
            a.anchor_used(),
            a.used_any()
        ));
        for (ni, n) in a.notes.iter().enumerate() {
            if ni > 0 {
                out.push(',');
            }
            out.push_str(&format!(
                "{{\"id\":\"{}\",\"kind\":\"{}\",\"injections\":{},\"has_anchor\":{},\
                 \"symbol_hit\":{},\"file_hit\":{},\"term_hits\":{},\"used_any\":{}}}",
                json_escape(&n.id),
                json_escape(&n.kind),
                n.injections,
                n.has_anchor,
                n.symbol_hit,
                n.file_hit,
                n.term_hits,
                n.used_any()
            ));
        }
        out.push_str("]}");
    }
    out.push_str("]}");
    println!("{out}");
}

fn print_help() {
    println!(
        "sovereign notes retrieval-audit — injected-note hit-rate (E2/P4 baseline)\n\n\
         Joins the inject-notes retrieval log (~/.sovereign/retrieval-log/<session>.jsonl)\n\
         against Claude Code transcripts to measure whether injected notes are actually\n\
         used downstream. Reads only local files — no daemon, no network.\n\n\
         USAGE:\n\
         \x20 sovereign notes retrieval-audit [flags]\n\n\
         FLAGS:\n\
         \x20 --project <path>   Project whose transcripts to correlate (default: cwd)\n\
         \x20 --dir <path>       Transcript directory override (skips cwd encoding)\n\
         \x20 --log-dir <path>   Retrieval-log directory override (default ~/.sovereign/retrieval-log)\n\
         \x20 --session <id>     Audit a single session id\n\
         \x20 --format json      Machine-readable output\n\n\
         METRICS:\n\
         \x20 strong = anchor(symbol|file) matches / anchored notes (high-confidence floor)\n\
         \x20 any    = (anchor|content) matches / all injected notes (headline)\n"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write(path: &Path, body: &str) {
        let mut f = std::fs::File::create(path).unwrap();
        f.write_all(body.as_bytes()).unwrap();
    }

    fn evidence_from(blob: &str) -> Evidence {
        Evidence {
            blob: blob.to_lowercase(),
        }
    }

    #[test]
    fn symbol_and_file_hits_are_detected_case_insensitively() {
        let ev = evidence_from("I'll call ReindexFile now and open src/foo/bar.rs");
        assert!(ev.contains("reindexfile"));
        assert!(ev.contains("ReindexFile"));
        assert!(ev.contains_file("crates/foo/bar.rs")); // basename match across path rewrite
        assert!(!ev.contains("unrelated_symbol"));
    }

    #[test]
    fn short_needles_do_not_match_inside_words() {
        let ev = evidence_from("the session identifier is here");
        // "id" is below MIN_MATCH_LEN and must not match inside "identifier".
        assert!(!ev.contains("id"));
    }

    #[test]
    fn anchor_used_beats_content_used_in_classification() {
        let n = NoteUsage {
            id: "n1".into(),
            kind: "invariant".into(),
            injections: 3,
            has_anchor: true,
            symbol_hit: true,
            file_hit: false,
            term_hits: 0,
        };
        assert!(n.anchor_used());
        assert!(n.used_any());
        assert!(!n.content_used());
    }

    #[test]
    fn content_hit_requires_two_distinct_terms() {
        let one = NoteUsage {
            id: "n".into(),
            kind: "decision".into(),
            injections: 1,
            has_anchor: false,
            symbol_hit: false,
            file_hit: false,
            term_hits: 1,
        };
        assert!(!one.content_used(), "a single term is coincidence, not use");
        let two = NoteUsage {
            term_hits: 2,
            ..one.clone()
        };
        assert!(two.content_used());
    }

    #[test]
    fn reinjection_counts_once_for_hitrate_but_sums_frequency() {
        // Same note injected across two prompts.
        let rec = |()| InjectionRecord {
            session_id: "s".into(),
            notes: vec![LoggedNote {
                id: Some("dup".into()),
                kind: "invariant".into(),
                symbols: vec!["CorpusEngine".into()],
                files: vec![],
                terms: vec![],
            }],
        };
        let records = vec![rec(()), rec(())];
        let ev = evidence_from("touching CorpusEngine today");
        let audit = audit_session("s", &records, Some(&ev));
        assert_eq!(
            audit.injected(),
            1,
            "one unique note despite two injections"
        );
        assert_eq!(audit.notes[0].injections, 2, "frequency preserved");
        assert!(audit.notes[0].symbol_hit);
        assert_eq!(audit.anchor_used(), 1);
    }

    #[test]
    fn missing_transcript_scores_nothing_as_used() {
        let records = vec![InjectionRecord {
            session_id: "s".into(),
            notes: vec![LoggedNote {
                id: Some("n".into()),
                kind: "decision".into(),
                symbols: vec!["Foo".into()],
                files: vec![],
                terms: vec!["something".into()],
            }],
        }];
        let audit = audit_session("s", &records, None);
        assert!(!audit.transcript_found);
        assert_eq!(audit.injected(), 1);
        assert_eq!(
            audit.used_any(),
            0,
            "no evidence => unknown => not counted used"
        );
    }

    #[test]
    fn end_to_end_join_over_synthetic_log_and_transcript() {
        let dir = std::env::temp_dir().join(format!("retr-audit-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let log_dir = dir.join("logs");
        let tx_dir = dir.join("tx");
        std::fs::create_dir_all(&log_dir).unwrap();
        std::fs::create_dir_all(&tx_dir).unwrap();

        let sid = "abcd1234-session";
        // Log: two notes injected. Note A anchored on symbol used downstream;
        // note B anchorless with two terms, only ONE of which appears.
        let log = r#"{"session_id":"abcd1234-session","notes":[{"id":"A","kind":"invariant","symbols":["ReindexFile"],"files":[],"terms":[]},{"id":"B","kind":"decision","symbols":[],"files":[],"terms":["lancedb","checkpoint_resume"]}]}"#;
        write(&log_dir.join(format!("{sid}.jsonl")), log);

        // Transcript: assistant calls a tool referencing ReindexFile and writes
        // text mentioning lancedb (one term for B → below the 2-term bar).
        let tx = "{\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"Let me look at lancedb.\"},{\"type\":\"tool_use\",\"name\":\"symbols\",\"input\":{\"name\":\"ReindexFile\"}}]}}\n";
        write(&tx_dir.join(format!("{sid}.jsonl")), tx);

        let records = load_injections(&log_dir.join(format!("{sid}.jsonl")));
        let ev = build_evidence(&tx_dir.join(format!("{sid}.jsonl")));
        let audit = audit_session(sid, &records, ev.as_ref());

        assert!(audit.transcript_found);
        assert_eq!(audit.injected(), 2);
        assert_eq!(audit.anchored(), 1, "only A has an anchor");
        assert_eq!(audit.anchor_used(), 1, "A's symbol was used");
        // B has only one of two terms downstream → not content-used.
        assert_eq!(audit.used_any(), 1, "A used; B below content bar");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn user_prompt_tokens_do_not_count_as_agent_use() {
        let dir = std::env::temp_dir().join(format!("retr-audit-role-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let tx = dir.join("role.jsonl");
        // Only a USER message mentions the symbol; assistant never acts on it.
        write(
            &tx,
            "{\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"tell me about ReindexFile\"}]}}\n",
        );
        let ev = build_evidence(&tx).unwrap();
        assert!(
            !ev.contains("reindexfile"),
            "user tokens are relevance, not use"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
