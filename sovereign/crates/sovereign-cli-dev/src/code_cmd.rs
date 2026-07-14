// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn code` subcommand — Code Intelligence v1 Phase 1.
//!
//! Ships two commands in v1:
//!
//!   sovereign code index <path> [--corpus-id <id>]
//!       Walks a local repository with tree-sitter, produces one chunk
//!       per symbol, embeds each chunk through the running daemon's
//!       standard embedding model, and writes a LanceDB index under
//!       `~/.sovereign/indexes/{corpus_id}/`. Symbol lookup uses the
//!       SCIP graph + metadata filter pushdown; semantic code search
//!       uses the same embedding space as knowledge retrieval, which
//!       keeps the retrieval surface coherent across corpus kinds.
//!
//!   sovereign code search <query>
//!       Phase-2 placeholder. Prints a friendly message explaining which
//!       tools land in P2.
//!
//! Embeds go through the daemon HTTP endpoint (localhost:9741 by
//! default), not an in-process model load. That keeps the CLI light
//! *and* guarantees every corpus — knowledge or code — is embedded
//! with the same model, so `embedding_dimensions` is consistent
//! across the installation.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use corpus_engine::{CorpusEngine, CorpusSpec, EmbedFn};
use sovereign_core::traits::InferenceProvider;
use sovereign_inference::remote::RemoteApiProvider;

/// Run a `code` subcommand. Returns the exit code.
pub async fn run_code(args: &[String]) -> i32 {
    if args.is_empty() {
        sovereign_cli_shared::help::print(&HELP);
        return 1;
    }
    if matches!(args[0].as_str(), "--help" | "-h" | "help") {
        sovereign_cli_shared::help::print(&HELP);
        return 0;
    }

    match args[0].as_str() {
        "index" => cmd_index(&args[1..]).await,
        "finalize" => cmd_finalize(&args[1..]).await,
        "watch" => cmd_watch(&args[1..]).await,
        "mcp-status" => cmd_mcp_status(&args[1..]).await,
        "search" => cmd_search(&args[1..]).await,
        "brief" => cmd_brief(&args[1..]).await,
        "reflect" => cmd_reflect(&args[1..]).await,
        "capability-map" => cmd_capability_map(&args[1..]).await,
        "arch-report" => crate::arch_report_cmd::run(&args[1..]).await,
        "suggest-seams" => crate::suggest_seams_cmd::run(&args[1..]).await,
        "dry-report" => crate::dry_report_cmd::run(&args[1..]).await,
        "capability-graph" => crate::code_capability_graph::cmd_capability_graph(&args[1..]).await,
        "map" => crate::code_map::cmd_map(&args[1..]).await,
        "facts" => cmd_facts(&args[1..]).await,
        "check-spec" => cmd_check_spec(&args[1..]).await,
        other => {
            eprintln!("Unknown code subcommand: {other}");
            sovereign_cli_shared::help::print(&HELP);
            1
        }
    }
}

// ─── finalize ─────────────────────────────────────────────────
// Recovery hook for ingests that wrote a `<corpus>-partition-local/`
// chunk index but never promoted it into `<corpus>/`. Pre-fix, this
// would silently strand behind a SCIP sidecar; the engine now does
// the right thing on its own, but pre-existing stranded partitions
// need a manual nudge.
async fn cmd_finalize(args: &[String]) -> i32 {
    if args.is_empty() || matches!(args[0].as_str(), "--help" | "-h" | "help") {
        eprintln!(
            "Usage: svrn code finalize <corpus_id>\n\n\
             Promote a stranded `<corpus>-partition-local/` Lance \
             index into the canonical `<corpus>/` location. Safe to \
             rerun; no-ops when there is nothing to promote."
        );
        return if args.is_empty() { 1 } else { 0 };
    }
    let corpus_id = args[0].clone();
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => {
            eprintln!("Cannot resolve home directory");
            return 1;
        }
    };
    let data_dir = home.join(".sovereign").join("indexes");
    let recipes_dir = home.join(".sovereign").join("recipes");

    // `finalise_solo_ingest` only inspects the filesystem — no embed
    // calls. A noop EmbedFn keeps the engine constructable without
    // booting the daemon.
    let noop_embed: corpus_engine::EmbedFn =
        Arc::new(|_text: &str| Box::pin(async move { Ok(vec![0.0_f32; 1]) }));
    let engine = corpus_engine::CorpusEngine::new(recipes_dir, data_dir, noop_embed);
    match engine.finalise_solo_ingest(&corpus_id) {
        Ok(true) => {
            eprintln!("Promoted {corpus_id}-partition-local/ → {corpus_id}/");
            0
        }
        Ok(false) => {
            eprintln!(
                "Nothing to do for '{corpus_id}': either no partition-local dir, \
                 a peer partition is present (use `coordinate_merge`), or canonical \
                 Lance is already finalized."
            );
            0
        }
        Err(e) => {
            eprintln!("finalize failed: {e}");
            1
        }
    }
}

// ─── facts ────────────────────────────────────────────────────
// `sovereign code facts <path>` — extract the deterministic code-fact
// base (construction-field / string-literal / function-definition) and
// write it to ~/.sovereign/indexes/<corpus>/facts.json. The queryable
// substrate for spec↔code drift detection. Rust-only for now.
async fn cmd_facts(args: &[String]) -> i32 {
    if args.is_empty() || matches!(args[0].as_str(), "--help" | "-h" | "help") {
        eprintln!(
            "Usage: sovereign code facts <path> [--corpus-id <id>] [--roots <dir,dir>]\n\n\
             Extract the deterministic code-fact base from a repository and write it to\n\
             ~/.sovereign/indexes/<corpus>/facts.json. Facts: construction-field values\n\
             (data-flow, e.g. `tools: None`), string literals, and function definitions.\n\
             --roots defaults to the whole repo; pass crate src dirs for a monorepo. Rust-only."
        );
        return if args.is_empty() { 1 } else { 0 };
    }

    let mut path: Option<String> = None;
    let mut corpus_id: Option<String> = None;
    let mut roots: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--corpus-id" => {
                i += 1;
                if i < args.len() {
                    corpus_id = Some(args[i].clone());
                }
            }
            "--roots" => {
                i += 1;
                if i < args.len() {
                    roots = args[i].split(',').map(|s| s.trim().to_string()).collect();
                }
            }
            other if !other.starts_with("--") => path = Some(other.to_string()),
            _ => {}
        }
        i += 1;
    }

    let repo = match path {
        Some(p) => PathBuf::from(p),
        None => {
            eprintln!("code facts: missing <path>");
            return 1;
        }
    };
    let repo = match repo.canonicalize() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("code facts: cannot resolve {}: {e}", repo.display());
            return 1;
        }
    };
    let corpus_id = corpus_id.unwrap_or_else(|| {
        repo.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "corpus".to_string())
    });
    if roots.is_empty() {
        roots = vec![".".to_string()];
    }

    let facts = corpus_engine::facts::extract_facts(&repo, &roots);

    let home = match dirs::home_dir() {
        Some(h) => h,
        None => {
            eprintln!("cannot resolve home directory");
            return 1;
        }
    };
    let out_dir = home.join(".sovereign").join("indexes").join(&corpus_id);
    if let Err(e) = std::fs::create_dir_all(&out_dir) {
        eprintln!("cannot create {}: {e}", out_dir.display());
        return 1;
    }
    let out = out_dir.join("facts.json");
    if let Err(e) = facts.write(&out) {
        eprintln!("cannot write facts: {e}");
        return 1;
    }
    println!(
        "code facts [{corpus_id}]: {} construction-fields · {} string-literals · {} function-defs → {}",
        facts.ctor_fields.len(),
        facts.str_lits.len(),
        facts.fn_defs.len(),
        out.display()
    );
    0
}

// ─── check-spec ───────────────────────────────────────────────
// `sovereign code check-spec --corpus <id> --claims <claims.json>` —
// tag each spec claim and check it against the deterministic fact base
// (built by `code facts`), producing cited drift / corroborated /
// unverifiable verdicts. The user-facing surface of the fact-base check.
// Needs the daemon (tagger + embeddings) + facts.json + scip_graph.db
// (+ capability_map.json + fn_vecs for CONFIG/CALLS scoping).

const TAG_SYS: &str = "Classify a spec claim's PRIMARY checkable relation about the code, preferring CONCRETE relations. Check in this order and pick the FIRST that fits:\nCONFIG - asserts a request/struct field or flag is set/unset (e.g. 'exposes tools to the model' => field=tools). ALWAYS give `field` (the bare field name) when you choose CONFIG; if you cannot name a field, do NOT choose CONFIG.\nLITERAL - asserts a VERBATIM string appears: an endpoint, a marker like 'SUMMARY:', a model name. Give `literal` = the exact substring only.\nEXISTS - asserts a named function/type exists / is the named entry. Give `target` (the bare name).\nCALLS - asserts X calls/invokes/reaches Y. Give `target`.\nCAPABILITY / CONTROL / OUT_OF_SCHEMA - only if none of the concrete relations fit.\n`subject` = short description of the code path/situation the claim is about. `expected` = YES if the claim asserts it is present/true, NO if absent/false.\nOutput JSON: {\"relation\":\"...\",\"subject\":\"...\",\"field\":\"\",\"literal\":\"\",\"target\":\"\",\"expected\":\"YES\"}.";

fn clip(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// Load capability-ENTRY vectors (name, embedding) for entry-restricted resolution.
fn load_entries(home: &Path, corpus: &str) -> Vec<(String, Vec<f32>)> {
    let mut entry_names: std::collections::HashSet<String> = std::collections::HashSet::new();
    if let Ok(txt) = std::fs::read_to_string(
        home.join(".sovereign/capabilities")
            .join(corpus)
            .join("capability_map.json"),
    ) {
        if let Ok(cap) = serde_json::from_str::<serde_json::Value>(&txt) {
            if let Some(caps) = cap.get("capabilities").and_then(|c| c.as_array()) {
                for c in caps {
                    if let Some(reps) = c.get("reps").and_then(|r| r.as_array()) {
                        for r in reps {
                            if let Some(s) = r.as_str() {
                                entry_names.insert(s.to_string());
                            }
                        }
                    }
                    if let Some(es) = c.get("entries").and_then(|r| r.as_array()) {
                        for e in es {
                            if let Some(s) = e.as_str() {
                                let short = s
                                    .rsplit('#')
                                    .next()
                                    .unwrap_or(s)
                                    .rsplit(']')
                                    .next()
                                    .unwrap_or(s)
                                    .trim_end_matches("().")
                                    .trim();
                                if !short.is_empty() && !short.contains('(') {
                                    entry_names.insert(short.to_string());
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    let side = match std::fs::read_to_string(
        home.join(".sovereign/specs/_fn_vecs")
            .join(format!("{corpus}.json")),
    ) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    let side: serde_json::Value = match serde_json::from_str(&side) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let dim = side.get("dim").and_then(|d| d.as_u64()).unwrap_or(0) as usize;
    let fns = match side.get("fns").and_then(|f| f.as_array()) {
        Some(f) => f,
        None => return Vec::new(),
    };
    let bin = match std::fs::read(
        home.join(".sovereign/specs/_fn_vecs")
            .join(format!("{corpus}.bin")),
    ) {
        Ok(b) => b,
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::new();
    for (i, fm) in fns.iter().enumerate() {
        let name = fm.get("name").and_then(|n| n.as_str()).unwrap_or("");
        if !entry_names.contains(name) {
            continue;
        }
        let off = i * dim * 4;
        if off + dim * 4 > bin.len() {
            continue;
        }
        let vec: Vec<f32> = (0..dim)
            .map(|j| {
                let b = off + j * 4;
                f32::from_le_bytes([bin[b], bin[b + 1], bin[b + 2], bin[b + 3]])
            })
            .collect();
        out.push((name.to_string(), vec));
    }
    out
}

/// Parse a spec-intel `claims.json` → (statement, conditions) list.
fn load_claims(path: &str) -> Result<Vec<(String, Vec<String>)>, String> {
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    let claim_vals: Vec<serde_json::Value> =
        if let Some(secs) = v.get("sections").and_then(|s| s.as_array()) {
            secs.iter()
                .flat_map(|s| {
                    s.get("claims")
                        .and_then(|c| c.as_array())
                        .cloned()
                        .unwrap_or_default()
                })
                .collect()
        } else {
            v.get("claims")
                .and_then(|c| c.as_array())
                .cloned()
                .unwrap_or_default()
        };
    let mut out = Vec::new();
    for c in claim_vals {
        let stmt = c
            .get("statement")
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .to_string();
        if stmt.is_empty() {
            continue;
        }
        let conds: Vec<String> = c
            .get("conditions")
            .and_then(|x| x.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        out.push((stmt, conds));
    }
    Ok(out)
}

/// Load fuzzy spec-reconcile findings (statement → verdict kind) for the deterministic-first
/// composition — the deterministic fact base answers what it can; the fuzzy path fills the gaps.
fn load_fuzzy(path: &str) -> std::collections::HashMap<String, String> {
    let mut m = std::collections::HashMap::new();
    if let Ok(txt) = std::fs::read_to_string(path) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&txt) {
            if let Some(fs) = v.get("findings").and_then(|f| f.as_array()) {
                for f in fs {
                    if let (Some(s), Some(k)) = (
                        f.get("statement").and_then(|x| x.as_str()),
                        f.get("kind").and_then(|x| x.as_str()),
                    ) {
                        m.insert(s.to_string(), k.to_string());
                    }
                }
            }
        }
    }
    m
}

/// Tag one claim via a daemon chat call → structured [`Tag`].
async fn tag_claim(
    http: &reqwest::Client,
    port: u16,
    model: &str,
    stmt: &str,
    conds: &[String],
) -> corpus_engine::facts_check::Tag {
    let user = format!("CLAIM: {stmt}\nCONDITIONS: {conds:?}");
    let body = serde_json::json!({
        "model": model, "temperature": 0.1, "max_tokens": 300,
        "messages": [{"role": "system", "content": TAG_SYS}, {"role": "user", "content": user}]
    });
    let attempt = async {
        let resp = http
            .post(format!("http://localhost:{port}/v1/chat/completions"))
            .json(&body)
            .send()
            .await
            .ok()?;
        let v: serde_json::Value = resp.json().await.ok()?;
        let content = v
            .get("choices")?
            .as_array()?
            .first()?
            .get("message")?
            .get("content")?
            .as_str()?;
        let start = content.find('{')?;
        let end = content.rfind('}')? + 1;
        serde_json::from_str::<corpus_engine::facts_check::Tag>(&content[start..end]).ok()
    };
    attempt.await.unwrap_or_default()
}

async fn cmd_check_spec(args: &[String]) -> i32 {
    let mut corpus: Option<String> = None;
    let mut claims_path: Option<String> = None;
    let mut fuzzy_path: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--corpus" | "--corpus-id" => {
                i += 1;
                if i < args.len() {
                    corpus = Some(args[i].clone());
                }
            }
            "--claims" => {
                i += 1;
                if i < args.len() {
                    claims_path = Some(args[i].clone());
                }
            }
            "--fuzzy" => {
                i += 1;
                if i < args.len() {
                    fuzzy_path = Some(args[i].clone());
                }
            }
            "--help" | "-h" | "help" => {
                eprintln!(
                    "Usage: sovereign code check-spec --corpus <id> --claims <claims.json> [--fuzzy <spec_findings.json>]\n\n\
                     Tag each spec claim and check it against the deterministic fact base (built by\n\
                     `sovereign code facts`) → cited drift / corroborated / unverifiable verdicts.\n\
                     With --fuzzy (from `enrich spec-reconcile`): deterministic verdicts win; the fuzzy\n\
                     verdict fills abstentions, labeled `fuzzy`. Needs the daemon (tagger + embeddings)\n\
                     + facts.json + scip_graph.db (+ capability_map.json + fn_vecs for CONFIG/CALLS)."
                );
                return 0;
            }
            _ => {}
        }
        i += 1;
    }
    let corpus = match corpus {
        Some(c) => c,
        None => {
            eprintln!("check-spec: --corpus <id> required");
            return 1;
        }
    };
    let claims_path = match claims_path {
        Some(c) => c,
        None => {
            eprintln!("check-spec: --claims <claims.json> required");
            return 1;
        }
    };

    let home = match dirs::home_dir() {
        Some(h) => h,
        None => {
            eprintln!("cannot resolve home directory");
            return 1;
        }
    };
    let idx = home.join(".sovereign").join("indexes").join(&corpus);

    let facts = match corpus_engine::facts::Facts::load(&idx.join("facts.json")) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("check-spec: load {}: {e}\n  run `sovereign code facts <repo> --corpus-id {corpus}` first", idx.join("facts.json").display());
            return 1;
        }
    };
    let graph =
        corpus_engine_scip::scip_graph::ScipGraph::open(&idx.join("scip_graph.db"), &corpus).ok();
    let adj = match &graph {
        Some(g) => Some(corpus_engine::facts_check::build_adjacency(g).await), // load edges once → fast in-memory BFS
        None => None,
    };
    let entries = load_entries(&home, &corpus);
    let claims = match load_claims(&claims_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("check-spec: load claims: {e}");
            return 1;
        }
    };

    let fuzzy = fuzzy_path.as_deref().map(load_fuzzy).unwrap_or_default();

    let cfg = match sovereign_core::setup_config::SetupConfig::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("check-spec: read config: {e}");
            return 1;
        }
    };
    let port = cfg.daemon.client_port;
    let chat_model = cfg
        .models
        .primary
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string();
    let (embed, _) = match build_daemon_embed_fn().await {
        Ok(e) => e,
        Err(e) => {
            eprintln!("check-spec: {e}");
            return 1;
        }
    };
    let http = reqwest::Client::new();

    println!(
        "check-spec [{corpus}]: {} claims · {} fn-defs · scip={} · entries={}\n{}",
        claims.len(),
        facts.fn_defs.len(),
        graph.is_some(),
        entries.len(),
        "=".repeat(72)
    );
    let (mut drift, mut corrob, mut unver, mut fuzzy_used) = (0u32, 0u32, 0u32, 0u32);
    for (stmt, conds) in &claims {
        let tag = tag_claim(&http, port, &chat_model, stmt, conds).await;
        let v = corpus_engine::facts_check::check_claim(
            &facts,
            adj.as_ref(),
            &entries,
            Some(&embed),
            stmt,
            &tag,
        )
        .await;
        // deterministic-first: a cited drift/corroborated wins; on abstention, fall back to the
        // fuzzy spec-reconcile verdict (labeled, lower-confidence) if one exists.
        let (verdict, source, receipt) = match v.kind {
            corpus_engine::facts_check::VerdictKind::Drift => {
                drift += 1;
                ("DRIFT".to_string(), "det", v.receipt)
            }
            corpus_engine::facts_check::VerdictKind::Corroborated => {
                corrob += 1;
                ("corrob".to_string(), "det", v.receipt)
            }
            corpus_engine::facts_check::VerdictKind::Unverifiable => match fuzzy.get(stmt.as_str())
            {
                Some(fk) => {
                    fuzzy_used += 1;
                    (
                        fk.clone(),
                        "fuzzy",
                        format!("deterministic abstained; fuzzy spec-reconcile: {fk}"),
                    )
                }
                None => {
                    unver += 1;
                    ("unverif".to_string(), "—", v.receipt)
                }
            },
        };
        println!(
            "[{:11}|{source:5}|{:9}] {}",
            clip(&verdict, 11),
            clip(&tag.relation, 9),
            clip(stmt, 48)
        );
        println!("     {}", clip(&receipt, 100));
    }
    println!(
        "{}\n{corrob} corroborated · {drift} drift (deterministic, cited) · {fuzzy_used} fuzzy-fallback · {unver} unverifiable  ({} claims)",
        "=".repeat(72),
        claims.len()
    );
    0
}

// ─── reflect ──────────────────────────────────────────────────
// `svrn code reflect` — write a session-end reflection note
// describing what changed during the session. Triggered by Claude
// Code's Stop hook (`.claude/hooks/capture-reflection.sh`).
//
// Captures:
//   - current branch
//   - uncommitted modifications (`git diff HEAD --name-only`)
//   - recent commits in last `--hours` hours (default 4)
// If nothing meaningful changed, exits silently — no point recording
// "the engineer opened a session and did nothing."
//
// Writes via NoteStore::write_reflection_scoped. The brief queries
// `reflection` kind alongside decision/invariant, so a session's
// reflection shows up in the next session's brief automatically.

async fn cmd_reflect(args: &[String]) -> i32 {
    if std::env::var("SOVEREIGN_NO_REFLECTION").as_deref() == Ok("1") {
        return 0;
    }
    if matches!(
        args.first().map(String::as_str),
        Some("--help" | "-h" | "help")
    ) {
        sovereign_cli_shared::help::print(&REFLECT_HELP);
        return 0;
    }

    // ── Args ──────────────────────────────────────────────────
    let mut hours: u64 = 4;
    let mut repo_root_arg: Option<PathBuf> = None;
    let mut feature_id: Option<String> = None;
    let mut content_override: Option<String> = None;
    let mut quiet: bool = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--hours" => {
                hours = args.get(i + 1).and_then(|s| s.parse().ok()).unwrap_or(4);
                i += 2;
            }
            "--repo-root" => {
                repo_root_arg = args.get(i + 1).map(PathBuf::from);
                i += 2;
            }
            "--feature-id" => {
                feature_id = args.get(i + 1).cloned();
                i += 2;
            }
            "--content" => {
                content_override = args.get(i + 1).cloned();
                i += 2;
            }
            "--quiet" => {
                quiet = true;
                i += 1;
            }
            other => {
                eprintln!("error: unrecognised flag {other}");
                return 2;
            }
        }
    }

    // ── Resolve repo + collect session state ─────────────────
    let repo_root = match repo_root_arg.or_else(|| resolve_cwd_repo_root().ok()) {
        Some(p) => p,
        None => return 0, // not in a git repo — silent no-op
    };
    let branch = current_branch(&repo_root).unwrap_or_else(|| "HEAD".into());
    let uncommitted = git_diff_head_names(&repo_root);
    let recent = git_recent_commit_files(&repo_root, hours);

    let session_files: Vec<String> = {
        let mut s: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for f in &uncommitted {
            s.insert(f.clone());
        }
        for f in &recent {
            s.insert(f.clone());
        }
        s.into_iter().collect()
    };

    // ── Bail if nothing meaningful changed ───────────────────
    if content_override.is_none() && session_files.is_empty() {
        if !quiet {
            eprintln!("(reflection: nothing to record — no diff and no recent commits)");
        }
        return 0;
    }

    let content = match content_override {
        Some(c) => c,
        None => format_reflection(
            &repo_root,
            &branch,
            uncommitted.len(),
            recent.len(),
            &session_files,
        ),
    };

    // ── Open NoteStore + write ───────────────────────────────
    let notes_path = home_dir().join(".sovereign").join("notes.db");
    let notes = match corpus_engine_notes::NoteStore::open(&notes_path) {
        Ok(n) => n,
        Err(e) => {
            if !quiet {
                eprintln!(
                    "error: cannot open NoteStore at {}: {e}",
                    notes_path.display()
                );
            }
            return 1;
        }
    };
    let session_id = format!("reflect-{}", chrono::Utc::now().timestamp());
    let scope = if feature_id.is_some() {
        corpus_engine_notes::NoteScope::Feature
    } else {
        corpus_engine_notes::NoteScope::Global
    };
    match notes
        .write_reflection_scoped(
            &content,
            Some("code:reflect"),
            &session_id,
            scope,
            feature_id.as_deref(),
        )
        .await
    {
        Ok(id) => {
            if !quiet {
                eprintln!("✓ reflection saved as {id}");
            }
            0
        }
        Err(e) => {
            if !quiet {
                eprintln!("error: write_reflection failed: {e}");
            }
            1
        }
    }
}

fn format_reflection(
    repo_root: &Path,
    branch: &str,
    uncommitted_count: usize,
    recent_count: usize,
    files: &[String],
) -> String {
    let date = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let repo_name = repo_root
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("repo");
    let mut out = format!(
        "Session {date} on {repo_name} @ {branch}: {uncommitted_count} uncommitted, \
         {recent_count} recent commits. Files touched:\n"
    );
    for f in files.iter().take(15) {
        out.push_str(&format!("  - {f}\n"));
    }
    if files.len() > 15 {
        out.push_str(&format!("  - …+{} more\n", files.len() - 15));
    }
    out
}

fn git_diff_head_names(repo_root: &Path) -> Vec<String> {
    let out = std::process::Command::new("git")
        .args(["diff", "HEAD", "--name-only"])
        .current_dir(repo_root)
        .output();
    let Ok(o) = out else { return Vec::new() };
    if !o.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&o.stdout)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.trim().to_string())
        .collect()
}

fn git_recent_commit_files(repo_root: &Path, hours: u64) -> Vec<String> {
    let since = format!("{hours} hours ago");
    let out = std::process::Command::new("git")
        .args(["log", "--since", &since, "--name-only", "--pretty=format:"])
        .current_dir(repo_root)
        .output();
    let Ok(o) = out else { return Vec::new() };
    if !o.status.success() {
        return Vec::new();
    }
    let mut set: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for line in String::from_utf8_lossy(&o.stdout).lines() {
        let l = line.trim();
        if !l.is_empty() {
            set.insert(l.to_string());
        }
    }
    set.into_iter().collect()
}

const REFLECT_HELP: sovereign_cli_shared::help::Help = sovereign_cli_shared::help::Help {
    command: "svrn code reflect",
    summary: "Write a session-end reflection note describing what changed during the session.",
    sections: &[
        sovereign_cli_shared::help::HelpSection::Usage(
            "svrn code reflect [--hours N] [--repo-root <path>] [--feature-id <id>] \
             [--content <text>] [--quiet]",
        ),
        sovereign_cli_shared::help::HelpSection::Flags(&[
            (
                "--hours N",
                "How far back to scan for recent commits. Default 4.",
            ),
            (
                "--repo-root <path>",
                "Override repo root. Default: cwd's git toplevel.",
            ),
            (
                "--feature-id <id>",
                "Scope the reflection to this ATOS feature. Mirrors SOVEREIGN_FEATURE_ID.",
            ),
            (
                "--content <text>",
                "Use this verbatim instead of the auto-generated session summary.",
            ),
            ("--quiet", "Suppress info output (used by hooks)."),
        ]),
        sovereign_cli_shared::help::HelpSection::Notes(
            "Writes a `reflection` kind note to ~/.sovereign/notes.db via \
             NoteStore::write_reflection_scoped. The next session's brief queries \
             reflection alongside decision/invariant so this surfaces automatically. \
             Honors SOVEREIGN_NO_REFLECTION=1 (hard opt-out).",
        ),
    ],
};

// ─── brief ────────────────────────────────────────────────────
// `svrn code brief` — assemble a working-set brief for the
// current session. Uses the same machinery the daemon's
// /v1/brief/working_set endpoint will use; this command exists
// for direct testing without going through the HTTP boundary,
// and as the offline fallback when the daemon isn't reachable.

async fn cmd_brief(args: &[String]) -> i32 {
    use sovereign_tools::code::brief::{assemble_brief, BriefInputs};
    use sovereign_tools::code::working_set::{detect_working_set, Strategy};

    if matches!(
        args.first().map(String::as_str),
        Some("--help" | "-h" | "help")
    ) {
        sovereign_cli_shared::help::print(&BRIEF_HELP);
        return 0;
    }

    // ── Parse args ────────────────────────────────────────────
    let mut strategy_kind = "branch".to_string();
    let mut hours: u64 = 24;
    let mut budget_tokens: usize = 1500;
    let mut repo_root: Option<PathBuf> = None;
    let mut atlas_id: Option<String> = None;
    let mut feature_id: Option<String> = None;
    let mut output: Option<PathBuf> = None;
    let mut explicit_files: Vec<PathBuf> = Vec::new();
    let mut telemetry_log: Option<PathBuf> = None;
    let mut inquiries_dir_arg: Option<PathBuf> = None;
    let started_at = std::time::Instant::now();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--inquiries-dir" => {
                inquiries_dir_arg = args.get(i + 1).map(PathBuf::from);
                i += 2;
            }
            "--telemetry-log" => {
                telemetry_log = args.get(i + 1).map(PathBuf::from);
                i += 2;
            }
            "--strategy" => {
                strategy_kind = args.get(i + 1).cloned().unwrap_or_default();
                i += 2;
            }
            "--hours" => {
                hours = args.get(i + 1).and_then(|s| s.parse().ok()).unwrap_or(24);
                i += 2;
            }
            "--budget" => {
                budget_tokens = args.get(i + 1).and_then(|s| s.parse().ok()).unwrap_or(1500);
                i += 2;
            }
            "--repo-root" => {
                repo_root = args.get(i + 1).map(PathBuf::from);
                i += 2;
            }
            "--atlas-id" => {
                atlas_id = args.get(i + 1).cloned();
                i += 2;
            }
            "--feature-id" => {
                feature_id = args.get(i + 1).cloned();
                i += 2;
            }
            "--output" => {
                output = args.get(i + 1).map(PathBuf::from);
                i += 2;
            }
            "--file" => {
                if let Some(v) = args.get(i + 1) {
                    explicit_files.push(PathBuf::from(v));
                }
                i += 2;
            }
            other => {
                eprintln!("error: unrecognised flag {other}");
                return 2;
            }
        }
    }

    // ── Resolve repo root (CWD's toplevel by default) ────────
    let repo_root = match repo_root {
        Some(p) => p,
        None => match resolve_cwd_repo_root() {
            Ok(p) => p,
            Err(e) => {
                eprintln!("error: cannot resolve repo root: {e}");
                return 1;
            }
        },
    };

    // ── Working set ───────────────────────────────────────────
    let strategy = match strategy_kind.as_str() {
        "branch" => Strategy::default_branch_diff(),
        "recent" => Strategy::RecentCommits { hours },
        "explicit" => Strategy::Explicit(explicit_files),
        other => {
            eprintln!("error: --strategy must be one of: branch, recent, explicit (got `{other}`)");
            return 2;
        }
    };
    let working_set = match detect_working_set(&repo_root, strategy) {
        Ok(ws) => ws,
        Err(e) => {
            eprintln!("error: working-set detection failed: {e}");
            return 1;
        }
    };

    // ── Notes store ───────────────────────────────────────────
    let notes_path = home_dir().join(".sovereign").join("notes.db");
    let notes = match corpus_engine_notes::NoteStore::open(&notes_path) {
        Ok(n) => n,
        Err(e) => {
            eprintln!(
                "error: cannot open NoteStore at {}: {e}",
                notes_path.display()
            );
            return 1;
        }
    };

    // ── Atlas dir ─────────────────────────────────────────────
    // Convention: <atlas-id>-self-atlas under ~/.sovereign/indexes,
    // or just <atlas-id> if explicitly named with the suffix already.
    let atlas_dir = atlas_id.as_ref().and_then(|id| {
        let name = if id.ends_with("-self-atlas") {
            id.clone()
        } else {
            format!("{id}-self-atlas")
        };
        let candidate = home_dir()
            .join(".sovereign")
            .join("indexes")
            .join(&name)
            .join("atlas");
        if candidate.join("atoms.json").exists() {
            Some(candidate)
        } else {
            None
        }
    });

    // ── Repo + branch labels ──────────────────────────────────
    let repo_name = repo_root
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("repo")
        .to_string();
    let branch_name = current_branch(&repo_root).unwrap_or_else(|| "HEAD".into());

    // ── Inquiries dir ────────────────────────────────────────
    // Default: <repo_root>/inquiries/. Falls through to None when
    // the directory doesn't exist (the brief just skips the
    // "Principles for this area" section).
    let inquiries_dir = inquiries_dir_arg.unwrap_or_else(|| repo_root.join("inquiries"));
    let inquiries_dir_opt: Option<&Path> = if inquiries_dir.is_dir() {
        Some(inquiries_dir.as_path())
    } else {
        None
    };

    // ── Drift dir ────────────────────────────────────────────
    // The brief reads the drift fingerprint + report sidecar to
    // render a "Drift posture" section. Defaults to
    // ~/.sovereign/drift/; falls through to None if neither the
    // fingerprint nor the report exists yet (`render_drift_posture`
    // is itself robust to the empty case).
    let drift_dir_path = home_dir().join(".sovereign").join("drift");
    let drift_dir_opt: Option<&Path> = if drift_dir_path.exists() {
        Some(drift_dir_path.as_path())
    } else {
        None
    };

    // ── Assemble ──────────────────────────────────────────────
    let inputs = BriefInputs {
        working_set: &working_set,
        repo_root: Some(&repo_root),
        atlas_dir: atlas_dir.as_deref(),
        inquiries_dir: inquiries_dir_opt,
        repo_name: &repo_name,
        branch_name: &branch_name,
        budget_tokens,
        feature_id: feature_id.as_deref(),
        drift_dir: drift_dir_opt,
    };
    let brief = match assemble_brief(inputs, &notes).await {
        Ok(b) => b,
        Err(e) => {
            eprintln!("error: brief assembly failed: {e}");
            return 1;
        }
    };

    match output {
        Some(p) => {
            if let Err(e) = std::fs::write(&p, &brief) {
                eprintln!("error: write {}: {e}", p.display());
                emit_brief_telemetry(
                    &telemetry_log,
                    started_at,
                    &working_set,
                    &brief,
                    Some(&format!("write_failed: {e}")),
                );
                return 1;
            }
            eprintln!("✓ wrote {}", p.display());
        }
        None => {
            print!("{brief}");
        }
    }
    emit_brief_telemetry(&telemetry_log, started_at, &working_set, &brief, None);
    0
}

/// Append one JSONL line per brief invocation. Empty when no
/// `--telemetry-log` flag is set; never fatal — telemetry must not
/// break the brief.
fn emit_brief_telemetry(
    log_path: &Option<PathBuf>,
    started_at: std::time::Instant,
    working_set: &[PathBuf],
    brief: &str,
    error: Option<&str>,
) {
    let Some(path) = log_path else { return };
    let elapsed_ms = started_at.elapsed().as_millis();
    // Count `^## ` headings — that's the canonical section marker.
    let sections_rendered = brief.lines().filter(|l| l.starts_with("## ")).count();
    let output_lines = brief.lines().count();
    // Cheap byte-count proxy for tokens; we don't need precision.
    // Real estimator lives in sovereign_tools::knowledge_view::tokens
    // but importing it here is a circular-dep risk; this approximation
    // is fine for log-trend purposes.
    let output_tokens = brief.split_whitespace().count() * 13 / 10;
    let ts = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let line = format!(
        "{{\"ts\":\"{ts}\",\"elapsed_ms\":{elapsed_ms},\"output_lines\":{output_lines},\"output_tokens\":{output_tokens},\"working_set_size\":{},\"sections_rendered\":{sections_rendered},\"error\":{}}}\n",
        working_set.len(),
        error.map(|e| format!("\"{}\"", e.replace('\\', "\\\\").replace('"', "\\\""))).unwrap_or_else(|| "null".into()),
    );
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = f.write_all(line.as_bytes());
    }
}

fn resolve_cwd_repo_root() -> Result<PathBuf, String> {
    let cwd = std::env::current_dir().map_err(|e| format!("cwd: {e}"))?;
    let out = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(&cwd)
        .output()
        .map_err(|e| format!("git: {e}"))?;
    if !out.status.success() {
        return Err(format!("{} is not a git repository", cwd.display()));
    }
    Ok(PathBuf::from(
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
    ))
}

// Re-export from `sovereign-cli-shared::repo` so `daemon_cmd` and other
// in-crate callers keep working through the existing `code_cmd::current_branch`
// path. The new home is the canonical spot.
pub(crate) use sovereign_cli_shared::repo::current_branch;

// ─── capability-map ───────────────────────────────────────────
// Derive a clustered "what does this codebase do" map from the SCIP call graph.
// Pure graph work (no model) — the deterministic foundation the narration +
// reconciliation phases build on. Writes a JSON map + a scannable markdown
// inventory under ~/.sovereign/capabilities/<corpus>/.

async fn cmd_capability_map(args: &[String]) -> i32 {
    use corpus_engine_scip::{build_capability_map, MapOptions, ProviderKind};

    let mut corpus_id: Option<String> = None;
    let mut jaccard: f64 = 0.5;
    let mut provider = ProviderKind::Heuristic;
    let mut out_dir: Option<PathBuf> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--jaccard" => {
                i += 1;
                match args.get(i).and_then(|s| s.parse::<f64>().ok()) {
                    Some(v) => jaccard = v,
                    None => {
                        eprintln!("error: --jaccard requires a number");
                        return 1;
                    }
                }
            }
            "--provider" => {
                i += 1;
                match args.get(i).map(String::as_str) {
                    Some("heuristic") => provider = ProviderKind::Heuristic,
                    Some("fallback") => provider = ProviderKind::Fallback,
                    _ => {
                        eprintln!("error: --provider must be heuristic|fallback");
                        return 1;
                    }
                }
            }
            "--out" => {
                i += 1;
                out_dir = args.get(i).map(PathBuf::from);
                if out_dir.is_none() {
                    eprintln!("error: --out requires a value");
                    return 1;
                }
            }
            flag if flag.starts_with('-') => {
                eprintln!("error: unknown flag {flag}");
                return 1;
            }
            positional => {
                if corpus_id.is_none() {
                    corpus_id = Some(positional.to_string());
                }
            }
        }
        i += 1;
    }

    let corpus_id = match corpus_id {
        Some(c) => c,
        None => {
            eprintln!(
                "usage: sovereign code capability-map <corpus-id> \
                 [--provider heuristic|fallback] [--jaccard 0.5] [--out <dir>]"
            );
            return 1;
        }
    };

    let db_path = home_dir()
        .join(".sovereign")
        .join("indexes")
        .join(&corpus_id)
        .join("scip_graph.db");
    if !db_path.exists() {
        eprintln!(
            "error: no SCIP graph at {} — run `svrn project init` first",
            db_path.display()
        );
        return 1;
    }

    let graph = match corpus_engine_scip::ScipGraph::open(&db_path, &corpus_id) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("error: cannot open SCIP graph: {e}");
            return 1;
        }
    };
    let symbols = match graph.iter_all_symbols().await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: reading symbols: {e}");
            return 1;
        }
    };
    let refs = match graph.iter_all_refs().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: reading refs: {e}");
            return 1;
        }
    };

    let opts = MapOptions {
        jaccard,
        provider,
        ..Default::default()
    };
    let map = build_capability_map(&symbols, &refs, &opts);

    let out_dir = out_dir.unwrap_or_else(|| {
        home_dir()
            .join(".sovereign")
            .join("capabilities")
            .join(&corpus_id)
    });
    if let Err(e) = std::fs::create_dir_all(&out_dir) {
        eprintln!("error: cannot create {}: {e}", out_dir.display());
        return 1;
    }
    let json_path = out_dir.join("capability_map.json");
    let md_path = out_dir.join("capability_map.md");
    match serde_json::to_string_pretty(&map) {
        Ok(s) => {
            if let Err(e) = std::fs::write(&json_path, s) {
                eprintln!("error: writing {}: {e}", json_path.display());
                return 1;
            }
        }
        Err(e) => {
            eprintln!("error: serializing map: {e}");
            return 1;
        }
    }
    if let Err(e) = std::fs::write(
        &md_path,
        corpus_engine_scip::capability_map::render_markdown(&corpus_id, &map),
    ) {
        eprintln!("error: writing {}: {e}", md_path.display());
        return 1;
    }

    let s = &map.stats;
    println!("Capability map for {corpus_id}:");
    println!(
        "  {} capabilities from {} entry points ({} multi-entry)",
        s.capabilities, s.roots, s.multi_entry
    );
    println!(
        "  substrate: {} first-party call edges ({} external, {} type/module, {} test dropped)",
        s.substrate.kept_edges,
        s.substrate.dropped_external,
        s.substrate.dropped_nonfunction,
        s.substrate.dropped_test
    );
    println!("  wrote {}", json_path.display());
    println!("  wrote {}", md_path.display());
    println!("\nLargest capabilities:");
    for c in map.capabilities.iter().filter(|c| c.n_entries > 1).take(12) {
        println!(
            "  [{:>2} entries, {:>4} core] {}  — {}",
            c.n_entries,
            c.n_core,
            c.label,
            c.reps.join(", ")
        );
    }
    0
}

fn home_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"))
}

const BRIEF_HELP: sovereign_cli_shared::help::Help = sovereign_cli_shared::help::Help {
    command: "svrn code brief",
    summary: "Assemble a working-set brief (markdown) for the current session.",
    sections: &[
        sovereign_cli_shared::help::HelpSection::Usage(
            "svrn code brief [--strategy {branch|recent|explicit}] [--hours N] \
             [--budget N] [--repo-root <path>] [--atlas-id <id>] [--feature-id <id>] \
             [--output <md>] [--file <path>]...",
        ),
        sovereign_cli_shared::help::HelpSection::Flags(&[
            (
                "--strategy",
                "branch (default; diff vs default branch), recent (last N hours), or explicit",
            ),
            ("--hours N", "Window for `recent` strategy. Default 24."),
            ("--budget N", "Token budget for the brief. Default 1500."),
            (
                "--repo-root <path>",
                "Override the git repo root. Default: cwd's toplevel.",
            ),
            (
                "--atlas-id <id>",
                "Structural-atlas corpus id (e.g. `sovereign`). The brief reads atoms from \
                 ~/.sovereign/indexes/<id>-self-atlas/atlas/. If absent, the structural section \
                 is skipped.",
            ),
            (
                "--feature-id <id>",
                "ATOS feature id, used to scope notes. Mirrors SOVEREIGN_FEATURE_ID env var.",
            ),
            ("--output <md>", "Write to this path instead of stdout."),
            (
                "--file <path>",
                "(For --strategy explicit) Add a file to the working set. Repeat for multiple.",
            ),
        ]),
        sovereign_cli_shared::help::HelpSection::Notes(
            "Reads notes from ~/.sovereign/notes.db. Reads atoms + archaeology sidecar from \
             ~/.sovereign/indexes/<id>-self-atlas/atlas/ when --atlas-id is given. Walks git \
             history for the recent-activity section.",
        ),
    ],
};

const HELP: sovereign_cli_shared::help::Help = sovereign_cli_shared::help::Help {
    command: "svrn code",
    summary: "Code intelligence tooling: index a repository, watch for changes, check MCP.",
    sections: &[
        sovereign_cli_shared::help::HelpSection::Usage("svrn code <subcommand> [args]"),
        sovereign_cli_shared::help::HelpSection::Subcommands(&[
            ("index <path>", "Index a local repository with tree-sitter"),
            (
                "finalize <id>",
                "Promote a stranded <id>-partition-local/ to canonical",
            ),
            (
                "watch <corpus-id>",
                "Run a filesystem watcher that re-indexes on save",
            ),
            (
                "mcp-status",
                "Ping the local MCP server and list exposed tools",
            ),
            (
                "search <query>",
                "(placeholder) Use the Sovereign chat or MCP for now",
            ),
            (
                "capability-map <corpus-id>",
                "Derive a capability map (what the codebase does) from the SCIP call graph",
            ),
            (
                "arch-report [corpus-id]",
                "Architecture posture: god-crate fan-in, coupling carriers, declared↔observed \
                 deltas, layer-map violations, temporal coupling; persists for arch_posture \
                 (--no-git skips history; --root <path> overrides the workspace root)",
            ),
            (
                "capability-graph <corpus-id>",
                "Emit a self-contained interactive graph.html — nodes coloured by code-vs-docs finding; \
                 --layout force (call structure, default) | meaning (UMAP of capability embeddings)",
            ),
            (
                "map <path>",
                "One-shot: index → summarize → derive → narrate → reconcile a codebase (+ optional --spec)",
            ),
            (
                "facts <path>",
                "Extract the deterministic code-fact index → ~/.sovereign/indexes/<corpus>/facts.json",
            ),
            (
                "check-spec",
                "Diff spec claims against the code-fact index — corroborated / gap / todo per claim",
            ),
            (
                "brief",
                "Assemble a working-set brief for the current session (offline fallback for /v1/brief)",
            ),
            (
                "reflect",
                "Record a session reflection note (branch + diff + recent commits) to notes.db",
            ),
        ]),
        sovereign_cli_shared::help::HelpSection::Notes(
            "`index` and `watch` take --corpus-id <id>, --data-dir <dir>, --root <path>.\n\
             `mcp-status` accepts --url <url> to override http://localhost:9741/mcp.",
        ),
    ],
};

// ─── index ────────────────────────────────────────────────────

async fn cmd_index(args: &[String]) -> i32 {
    let mut path_arg: Option<PathBuf> = None;
    let mut corpus_id: Option<String> = None;
    let mut data_dir: Option<PathBuf> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--corpus-id" => {
                i += 1;
                corpus_id = args.get(i).cloned();
                if corpus_id.is_none() {
                    eprintln!("error: --corpus-id requires a value");
                    return 1;
                }
            }
            "--data-dir" => {
                i += 1;
                data_dir = args.get(i).map(PathBuf::from);
                if data_dir.is_none() {
                    eprintln!("error: --data-dir requires a value");
                    return 1;
                }
            }
            flag if flag.starts_with('-') => {
                eprintln!("warning: unknown flag '{flag}' — ignored");
            }
            p => {
                path_arg = Some(PathBuf::from(p));
            }
        }
        i += 1;
    }

    let Some(path) = path_arg else {
        eprintln!("error: missing <path>");
        sovereign_cli_shared::help::print(&HELP);
        return 1;
    };

    let abs_path = match path.canonicalize() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: cannot resolve path {}: {e}", path.display());
            return 1;
        }
    };

    let corpus_id = corpus_id.unwrap_or_else(|| {
        abs_path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "codebase".to_string())
    });

    let data_dir = data_dir
        .or_else(default_data_dir)
        .unwrap_or_else(|| PathBuf::from("./sovereign-indexes"));

    match rebuild_code_corpus(&abs_path, &corpus_id, &data_dir).await {
        Ok(stats) => {
            eprintln!();
            eprintln!(
                "✓ Indexed {} chunks in {}s",
                stats.chunks_created, stats.duration_secs
            );
            eprintln!(
                "  Corpus: {}  ({} KB on disk)",
                stats.corpus_id,
                stats.index_size_bytes / 1024,
            );
            eprintln!("  Location: {}/{}", data_dir.display(), stats.corpus_id);
            0
        }
        Err(e) => {
            eprintln!();
            eprintln!("✗ Indexing failed: {e}");
            1
        }
    }
}

/// Full rebuild of a code corpus's LanceDB index. Shared between
/// `svrn code index` and `svrn project refresh` so both
/// surfaces write exactly the same thing: an ephemeral code-extract
/// recipe, embedded through the running daemon, ingested to
/// `<data_dir>/<corpus_id>/`.
///
/// Bails early (error, never zero-vector fallback) when the daemon
/// is unreachable — see the `build_daemon_embed_fn` docstring for
/// rationale.
pub async fn rebuild_code_corpus(
    root: &std::path::Path,
    corpus_id: &str,
    data_dir: &std::path::Path,
) -> std::result::Result<corpus_engine::IngestResult, String> {
    std::fs::create_dir_all(data_dir)
        .map_err(|e| format!("cannot create data dir {}: {e}", data_dir.display()))?;

    // A `rebuild` is a rebuild. Clear prior LanceDB state so
    // `create_empty_table` doesn't trip with `Table 'chunks' already
    // exists`. Keep the SCIP graph DB (`scip_graph.db*`) intact —
    // it's owned by the daemon's Reindexer on a parallel cadence,
    // and wiping it here would race with a just-nudged rebuild.
    //
    // Two targets: the canonical `<corpus>/` directory AND every
    // `<corpus>-partition-*/` sibling. The engine writes new ingests
    // into a partition directory and only renames to canonical at
    // finalize; a stale partition from a prior run would make
    // `create_empty_table` collide on the second pass.
    let target = data_dir.join(corpus_id);
    if target.exists() {
        clear_lancedb_artifacts(&target).map_err(|e| {
            format!(
                "cannot clear existing LanceDB index at {}: {e}",
                target.display()
            )
        })?;
    }
    clear_partitions_for(data_dir, corpus_id).map_err(|e| {
        format!(
            "cannot clear partition dirs under {}: {e}",
            data_dir.display()
        )
    })?;

    // Vector ANN enabled — every corpus on this node shares one
    // embedding model so the `embedding_dimensions` is consistent
    // across knowledge + code indexes. Symbol lookup still uses
    // metadata filter pushdown; vector search is additive.
    let recipe_toml = format!(
        r#"[corpus]
id = "{corpus_id}"
name = "{corpus_id}"
description = "Local code corpus generated by `svrn code index`"
license = "private"
mesh_sharing = false
size_compressed_gb = 0
size_indexed_gb = 0

[acquire]
type = "local_file"
path = "{path}"

[extract]
type = "code"
context_lines = 3
max_lines_per_chunk = 150

[chunk]
type = "passthrough"

[index]
fts = true
vector = true
"#,
        corpus_id = corpus_id,
        path = root.display(),
    );

    let tempdir = tempfile_dir().map_err(|e| format!("cannot create temp dir: {e}"))?;
    let recipe_path = tempdir.join(format!("{corpus_id}.toml"));
    std::fs::write(&recipe_path, recipe_toml)
        .map_err(|e| format!("cannot write ephemeral recipe: {e}"))?;

    let (embed, embed_model_name) = build_daemon_embed_fn().await.map_err(|e| {
        format!(
            "{e}\n\n`svrn code index` / `svrn project refresh` now embed via the daemon \
             so code corpora share the standard embedding model. Start the daemon with \
             `svrn daemon run` and re-run this command."
        )
    })?;
    // Pass the embed model stem through so `_corpus_meta.json`
    // records exactly what produced the vectors (not the engine's
    // legacy default). See `corpus-engine::with_embedding_model`
    // rationale.
    let engine = CorpusEngine::new(tempdir.clone(), data_dir.to_path_buf(), embed)
        .with_embedding_model(&embed_model_name);

    eprintln!("Indexing {} as corpus '{corpus_id}'", root.display());
    eprintln!("Index directory: {}", data_dir.display());
    eprintln!();

    let spec = CorpusSpec::RecipePath(recipe_path);
    engine
        .ingest(&spec, None)
        .await
        .map_err(|e| format!("ingest failed: {e}"))
}

// ─── watch (P3) ───────────────────────────────────────────────

async fn cmd_watch(args: &[String]) -> i32 {
    let mut corpus_id: Option<String> = None;
    let mut root_override: Option<PathBuf> = None;
    let mut data_dir: Option<PathBuf> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--root" => {
                i += 1;
                root_override = args.get(i).map(PathBuf::from);
                if root_override.is_none() {
                    eprintln!("error: --root requires a value");
                    return 1;
                }
            }
            "--data-dir" => {
                i += 1;
                data_dir = args.get(i).map(PathBuf::from);
                if data_dir.is_none() {
                    eprintln!("error: --data-dir requires a value");
                    return 1;
                }
            }
            flag if flag.starts_with('-') => {
                eprintln!("warning: unknown flag '{flag}' — ignored");
            }
            v => {
                if corpus_id.is_none() {
                    corpus_id = Some(v.to_string());
                } else {
                    eprintln!("warning: ignoring extra positional arg '{v}'");
                }
            }
        }
        i += 1;
    }

    let Some(corpus_id) = corpus_id else {
        eprintln!("error: missing <corpus-id>");
        return 1;
    };

    let data_dir = data_dir
        .or_else(default_data_dir)
        .unwrap_or_else(|| PathBuf::from("./sovereign-indexes"));

    // Open the index to discover the source_path unless the caller
    // overrode it. Doing this via CorpusIndex means the meta-file
    // schema is the single source of truth.
    let index_path = data_dir.join(&corpus_id);
    if !index_path.exists() {
        eprintln!(
            "error: no index for corpus '{corpus_id}' at {}",
            index_path.display()
        );
        eprintln!("Run `svrn code index <path> --corpus-id {corpus_id}` first.");
        return 1;
    }

    let index = match corpus_engine::CorpusIndex::open(&index_path).await {
        Ok(i) => i,
        Err(e) => {
            eprintln!("error: cannot open index: {e}");
            return 1;
        }
    };

    let root = match root_override {
        Some(p) => p,
        None => match index.source_path() {
            Some(p) => p,
            None => {
                eprintln!(
                    "error: corpus '{corpus_id}' has no recorded source_path. \
                     Re-index with `svrn code index <path>`, or pass `--root <path>`."
                );
                return 1;
            }
        },
    };

    if !root.exists() {
        eprintln!(
            "error: source root '{}' does not exist. Use --root to override.",
            root.display()
        );
        return 1;
    }
    drop(index); // Watcher owns its own CorpusIndex handle via the engine.

    let embed: EmbedFn = Arc::new(|_text: &str| {
        Box::pin(async {
            Ok::<Vec<f32>, corpus_engine::Error>(vec![0.0; corpus_engine::DEFAULT_EMBED_DIM])
        })
    });
    let recipes_dir = data_dir.clone(); // unused placeholder — engine requires one
    let engine = Arc::new(corpus_engine::CorpusEngine::new(
        recipes_dir,
        data_dir.clone(),
        embed,
    ));

    eprintln!("Watching {} for corpus '{corpus_id}'", root.display());
    eprintln!("Press Ctrl-C to stop.");

    let watcher = corpus_engine::update::watch::CodeWatcher::new(
        Arc::clone(&engine),
        corpus_id.clone(),
        root.clone(),
    );

    let handle = match watcher.start().await {
        Ok(h) => h,
        Err(e) => {
            eprintln!("error: failed to start watcher: {e}");
            return 1;
        }
    };

    // Keep the process alive until Ctrl-C. The watcher handle aborts
    // its background task on drop.
    match tokio::signal::ctrl_c().await {
        Ok(()) => {
            eprintln!("\nShutting down watcher...");
            handle.abort();
            0
        }
        Err(e) => {
            eprintln!("error: failed to install ctrl-c handler: {e}");
            1
        }
    }
}

// ─── mcp-status (P4) ──────────────────────────────────────────

async fn cmd_mcp_status(args: &[String]) -> i32 {
    let mut url = "http://localhost:9741/mcp".to_string();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--url" => {
                i += 1;
                match args.get(i) {
                    Some(v) => url = v.clone(),
                    None => {
                        eprintln!("error: --url requires a value");
                        return 1;
                    }
                }
            }
            flag if flag.starts_with('-') => {
                eprintln!("warning: unknown flag '{flag}' — ignored");
            }
            _ => {}
        }
        i += 1;
    }

    eprintln!("MCP endpoint: {url}");
    let client = reqwest::Client::new();

    // Step 1 — initialize handshake.
    let init_body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {}
    });
    let init_res = match client.post(&url).json(&init_body).send().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: cannot reach MCP server: {e}");
            eprintln!("  Is `sovereign-server` running? Start it with:");
            eprintln!("    sovereign-server --config sovereign-server.toml");
            return 1;
        }
    };
    if !init_res.status().is_success() {
        eprintln!("error: initialize returned HTTP {}", init_res.status());
        return 1;
    }
    let init_json: serde_json::Value = match init_res.json().await {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: initialize response not JSON: {e}");
            return 1;
        }
    };
    let version = init_json["result"]["protocolVersion"]
        .as_str()
        .unwrap_or("?");
    let server_name = init_json["result"]["serverInfo"]["name"]
        .as_str()
        .unwrap_or("?");
    let server_version = init_json["result"]["serverInfo"]["version"]
        .as_str()
        .unwrap_or("?");
    println!("  ✓ initialize");
    println!("    protocolVersion: {version}");
    println!("    serverInfo:      {server_name} v{server_version}");

    // Step 2 — tools/list.
    let list_body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/list",
        "params": {}
    });
    let message_url = format!("{url}/message");
    let list_res = match client.post(&message_url).json(&list_body).send().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: tools/list failed: {e}");
            return 1;
        }
    };
    let list_json: serde_json::Value = match list_res.json().await {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: tools/list response not JSON: {e}");
            return 1;
        }
    };
    let tools = list_json["result"]["tools"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    println!("  ✓ tools/list  ({} exposed)", tools.len());
    for tool in &tools {
        let name = tool["name"].as_str().unwrap_or("?");
        let desc = tool["description"]
            .as_str()
            .unwrap_or("")
            .lines()
            .next()
            .unwrap_or("");
        println!("      {name} — {desc}");
    }

    if tools.is_empty() {
        eprintln!();
        eprintln!("warning: no tools exposed. Rebuild with --features treesitter");
        eprintln!("         and make sure a code corpus is indexed.");
        return 1;
    }

    eprintln!();
    eprintln!("To wire Claude Code, add to ~/.claude/settings.json:");
    eprintln!("  {{");
    eprintln!("    \"mcpServers\": {{");
    eprintln!("      \"sovereign\": {{");
    eprintln!("        \"type\": \"http\",");
    eprintln!("        \"url\": \"{url}\"");
    eprintln!("      }}");
    eprintln!("    }}");
    eprintln!("  }}");
    0
}

// ─── search (P2 placeholder) ──────────────────────────────────

async fn cmd_search(args: &[String]) -> i32 {
    let query = args.join(" ");
    eprintln!(
        "`svrn code search` ships in Code Intelligence Phase 2.\n\n\
         Phase 2 adds five Sovereign tools wired to the corpus you indexed:\n\
           symbol_lookup  — exact symbol name → file:line (always correct)\n\
           code_search    — semantic search (approximate, labelled as such)\n\
           recent_changes — files modified within the last N hours\n\
           find_callees   — what does this function call? (SCIP graph)\n\
           find_callers   — what calls this function? (SCIP graph)\n\n\
         In the meantime, index with `svrn code index <path>` — the\n\
         on-disk LanceDB table is already populated and queryable from\n\
         tools that open it directly.\n\n\
         Your query: {query}"
    );
    0
}

// ─── helpers ──────────────────────────────────────────────────

fn default_data_dir() -> Option<PathBuf> {
    // Mirrors project_cmd::default_data_dir; both just wrap
    // `util::dirs::sovereign_indexes()` but keep the Option return so
    // existing `.or_else(default_data_dir)` callers stay stable.
    let p = sovereign_cli_shared::dirs::sovereign_indexes();
    if p == std::path::Path::new(".") {
        None
    } else {
        Some(p)
    }
}

/// Remove every entry in `dir` that belongs to the LanceDB index
/// (the `_corpus_meta.json`, the `.lance` table dirs, the `_indices`
/// directory, any FTS/vector build scratch). Preserve anything named
/// `scip_graph.db*` — the daemon's Reindexer owns those.
///
/// If the directory ends up empty after clearing, remove the directory
/// itself. Reason: `finalise_solo_ingest` promotes
/// `<corpus>-partition-<node>/` to canonical `<corpus>/` via rename,
/// and that rename is skipped when the canonical path already
/// exists — even if empty. An empty leftover would silently leave
/// the fresh ingest stranded in the partition path.
///
/// Returns the first IO error encountered; partial cleanup is fine
/// since the next `create_empty_table` will still succeed against
/// whatever's left as long as the `chunks` table itself is gone.
fn clear_lancedb_artifacts(dir: &std::path::Path) -> std::io::Result<()> {
    let mut any_kept = false;
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with("scip_graph.db") {
            any_kept = true;
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            std::fs::remove_dir_all(&path)?;
        } else {
            std::fs::remove_file(&path)?;
        }
    }
    if !any_kept {
        // Swallow the error — a racing observer could have created
        // a file between our last read and this rmdir. The next
        // ingest step will create a fresh partition either way.
        let _ = std::fs::remove_dir(dir);
    }
    Ok(())
}

/// Remove every `<corpus_id>-partition-*` directory under `root`.
/// Called before a full rebuild so stale partition-of-self /
/// partition-of-peer dirs don't collide with the fresh ingest's
/// `create_empty_table` call.
///
/// Non-partition siblings (other corpora, arbitrary files) are
/// untouched. A missing `root` is not an error — first-ever
/// rebuild on a machine with no indexes yet is a normal state.
fn clear_partitions_for(root: &std::path::Path, corpus_id: &str) -> std::io::Result<()> {
    let prefix = format!("{corpus_id}-partition-");
    let entries = match std::fs::read_dir(root) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with(&prefix) {
            std::fs::remove_dir_all(entry.path())?;
        }
    }
    Ok(())
}

fn tempfile_dir() -> std::io::Result<PathBuf> {
    // Avoid pulling in the `tempfile` crate — sovereign-cli doesn't
    // already use it, and a one-shot per-run dir is enough. Use the
    // system temp dir plus a pid-derived suffix for uniqueness.
    let base = std::env::temp_dir();
    let suffix = format!("sovereign-code-{}", std::process::id());
    let path = base.join(suffix);
    std::fs::create_dir_all(&path)?;
    Ok(path)
}

/// Build an `EmbedFn` that POSTs to the running daemon's
/// `/v1/embeddings` endpoint with the daemon's configured embed
/// model. Returns `(EmbedFn, embed_model_stem)` — the stem is the
/// filename of the GGUF without the `.gguf` suffix, matching what
/// the daemon advertises on `/v1/models`.
///
/// Returns `Err(message)` when the daemon is unreachable or the
/// embed model can't be resolved.
///
/// Using the daemon (rather than loading a model in-process) keeps
/// `svrn code index` lightweight — no GPU/RAM for llama.cpp
/// — and guarantees code corpora land in the same embedding space
/// as knowledge corpora.
async fn build_daemon_embed_fn() -> std::result::Result<(EmbedFn, String), String> {
    let cfg = sovereign_core::setup_config::SetupConfig::load()
        .map_err(|e| format!("read ~/.sovereign/config.toml: {e}"))?;
    let port = cfg.daemon.client_port;
    let endpoint = format!("http://localhost:{port}/v1");
    let embed_model = cfg
        .models
        .embed
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| "SetupConfig.models.embed has no filename stem".to_string())?
        .to_string();

    // Probe before we return — a daemon-down failure 40 minutes
    // into a 10k-file reindex is much worse than an up-front bail.
    let probe = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
        .map_err(|e| format!("http client build: {e}"))?;
    let probe_url = format!("{endpoint}/models");
    match probe.get(&probe_url).send().await {
        Ok(r) if r.status().is_success() => {}
        Ok(r) => {
            return Err(format!(
                "daemon at :{port} returned {} from /v1/models",
                r.status()
            ));
        }
        Err(_) => {
            return Err(format!("daemon unreachable at localhost:{port}"));
        }
    }

    // `RemoteApiProvider` is constructed with the embed model as
    // its single `model_id`. Its `InferenceProvider::embed` sends
    // `{"model": "<embed_model>", "input": "<text>"}` to
    // `/embeddings`, which is the exact contract we want.
    let provider: Arc<dyn InferenceProvider> =
        Arc::new(RemoteApiProvider::new(&endpoint, None, &embed_model, 8192));
    let f = sovereign_tools::corpus::inference_to_embed_fn(provider);
    Ok((f, embed_model))
}
