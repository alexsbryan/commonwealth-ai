// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn deep-research "<question>"` — drive the THIN local-only research
//! loop (order deep-research-t1a) end to end from the shipped CLI.
//!
//! This file is the VERB, not the loop and not the port. Both of those
//! live in sovereign-core: `deep_research::run` is the loop, and
//! `deep_research::port::LiveResearchPort` is the production surface
//! (real estate, real network, real daemon) that used to live here. It
//! moved because a port is runtime, not a host — while it sat in this
//! crate, every other surface that wanted deep research had to SPAWN
//! this binary, and config does not cross a process boundary.
//!
//! What stays here is what only a CLI has: argument parsing and
//! validation, the launch banner, the resume sidecar, the post-run
//! estate ingest, and the summary. Nothing in this file re-implements a
//! loop step or a port method.
//!
//! `--backend mock --mock-deck DIR` (the P5 drill surface): the port's
//! search/fetch legs are served from the deck directory (`deck.toml` +
//! body files, the deep-research search gym's format) instead of the
//! network — the loop's `web_backend` is the mock's closed-set id, so a
//! run can be flown against a planted source with the real daemon still
//! doing the drafting (`MockDraftSurface::Delegated`). Additive: the
//! default path is unchanged.
//!
//! `--resume DIR` (order deep-research-t3a): an interrupted run
//! restores its state from `<DIR>/checkpoint.json` and continues at the
//! next round — ledger continuity included. The checkpoint's frozen
//! config is the identity: flags the operator did NOT pass inherit the
//! checkpoint's values (bare `--resume DIR` is the canonical shape), and
//! every explicitly-passed flag is verified against the frozen config
//! flag-by-flag (a conflicting one refuses, naming the flag); the
//! backend identity comes from the launch sidecar
//! (`resume-input.json`). The verb also closes every run by ingesting
//! its fetched evidence into `dr-estate-<run_id>` — the local cache a
//! later run's `--corpora` reads before the web leg.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use oicp_client::RemoteApiProvider;
use sovereign_core::deep_research::icd::ICD_VERSION;
use sovereign_core::deep_research::launch::{self, ResumeInput};
use sovereign_core::deep_research::port::build_port;
use sovereign_core::deep_research::{read_checkpoint, resume, run, RunOutcome, SearchSource};
use sovereign_core::traits::InferenceProvider;
use sovereign_core::types::Custody;

/// `svrn deep-research "<question>" [--run-dir DIR] [--max-rounds N]
/// [--corpora id1,id2] [--code-set-k N] [--eps-quota F] [--search N]
/// [--fetch N] [--backend auto|mock] [--mock-deck DIR]
/// [--search-source mock|corpus|web] [--consent public-web|peer|personal]
/// [--resume DIR]`
pub async fn cmd_deep_research(args: &[String]) -> i32 {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!(
            "Usage: svrn deep-research \"<question>\" [--run-dir DIR] [--max-rounds N] \
             [--corpora id1,id2] [--code-set-k N] [--eps-quota F] [--search N] [--fetch N] \
             [--backend auto|mock] [--mock-deck DIR] [--search-source mock|corpus|web] \
             [--consent public-web|peer|personal] [--resume DIR]"
        );
        return 0;
    }
    let mut question: Option<String> = None;
    let mut run_dir = std::env::temp_dir().join("deep-research-runs");
    let mut run_dir_explicit = false;
    let mut resume_dir: Option<PathBuf> = None;
    let mut max_rounds = 3u32;
    let mut corpora: Vec<String> = Vec::new();
    // drb1-t1: the admission thresholds default from the ONE decider
    // (acquisition::{DEFAULT_CODE_SET_K, DEFAULT_EPS_QUOTA}) — the
    // charter, the flags, and the replay harness read the same consts.
    let mut code_set_k = sovereign_core::deep_research::acquisition::DEFAULT_CODE_SET_K;
    let mut eps_quota = sovereign_core::deep_research::acquisition::DEFAULT_EPS_QUOTA;
    // Greedy acquisition (2026-08-24). AIQ's own `resource_limits` are up
    // to 20 research queries and 100 source-tool calls per job, and its
    // InfoRecall lead over every frontier entry is a breadth result, not a
    // scorer trick. Ours were 4 and 4 — a toy budget on a command named
    // "deep research". Measured cost of the old shape on a logged DRB-I
    // flight: a mean of 5 distinct sources per task, and 17.6% of the
    // evidence available for those tasks ever reaching the window.
    //
    // These are CEILINGS, not spend: the round split
    // (`budget::round_allowance_cap`) still divides them across rounds,
    // the decider still refuses past them, and `--search` / `--fetch`
    // still override. Nothing here makes a run spend more than the
    // evidence it can actually find.
    let mut search_allowance = 20u32;
    let mut fetch_allowance = 100u32;
    // Which flags the operator ACTUALLY passed (order deep-research-t3a):
    // a `--resume` inherits the checkpoint's frozen values for flags that
    // were NOT passed — only explicitly-passed flags are verified against
    // the frozen config, and a conflicting one refuses, naming it. The
    // fresh-launch path never reads these.
    let mut max_rounds_explicit = false;
    let mut corpora_explicit = false;
    let mut code_set_k_explicit = false;
    let mut eps_quota_explicit = false;
    let mut search_allowance_explicit = false;
    let mut fetch_allowance_explicit = false;
    let mut search_source_explicit = false;
    let mut backend_explicit = false;
    let mut mock_deck_explicit = false;
    // The P5 drill surface (additive; default `auto` = the real
    // network). `--backend mock` serves search/fetch from the deck
    // directory, drafts via the real daemon.
    let mut backend = "auto".to_string();
    let mut mock_deck_dir: Option<PathBuf> = None;
    // The acquisition search source (t1g rung 2; rung 3 = web, order
    // deep-research-t2a): a closed set, decided once here — `mock`
    // (default), `corpus`, or `web`.
    let mut search_source = SearchSource::Mock;
    // The run-scoped consent grant's release floor (order
    // deep-research-t2a): `None` = default-deny — the web leg refuses
    // non-public-web payloads without a grant. The grant itself is
    // built once the run id exists (frozen into the charter, FR-3).
    let mut consent_floor: Option<Custody> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--backend" => {
                i += 1;
                backend = args.get(i).cloned().unwrap_or_default();
                backend_explicit = true;
            }
            "--search-source" => {
                i += 1;
                match SearchSource::parse(args.get(i).map(String::as_str).unwrap_or_default()) {
                    Some(s) => search_source = s,
                    None => {
                        eprintln!(
                            "deep-research: unknown search source {:?} — the closed set is \
                             mock | corpus | web",
                            args.get(i).map(String::as_str).unwrap_or_default()
                        );
                        return 1;
                    }
                }
                search_source_explicit = true;
            }
            "--consent" => {
                i += 1;
                let s = args.get(i).map(String::as_str).unwrap_or_default();
                match Custody::parse_wire(s) {
                    Some(c) if c != Custody::Unknown => consent_floor = Some(c),
                    _ => {
                        eprintln!(
                            "deep-research: unknown consent class {:?} — the closed set is \
                             public-web | peer | personal",
                            s
                        );
                        return 1;
                    }
                }
            }
            "--mock-deck" => {
                i += 1;
                mock_deck_dir = Some(PathBuf::from(args.get(i).cloned().unwrap_or_default()));
                mock_deck_explicit = true;
            }
            "--run-dir" => {
                i += 1;
                run_dir = PathBuf::from(args.get(i).cloned().unwrap_or_default());
                run_dir_explicit = true;
            }
            // T3a: resume an interrupted run from its run dir. The
            // checkpoint's frozen config is the identity — the flags
            // below are verified against it, not applied to it.
            "--resume" => {
                i += 1;
                let p = PathBuf::from(args.get(i).cloned().unwrap_or_default());
                if p.as_os_str().is_empty() {
                    eprintln!("deep-research: --resume requires a run dir argument (--resume DIR)");
                    return 1;
                }
                resume_dir = Some(p);
            }
            "--max-rounds" => {
                i += 1;
                max_rounds = args
                    .get(i)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(max_rounds);
                max_rounds_explicit = true;
            }
            "--corpora" => {
                i += 1;
                corpora = args
                    .get(i)
                    .map(|s| {
                        s.split(',')
                            .map(|c| c.trim().to_string())
                            .filter(|c| !c.is_empty())
                            .collect()
                    })
                    .unwrap_or_default();
                corpora_explicit = true;
            }
            "--code-set-k" => {
                i += 1;
                code_set_k = args
                    .get(i)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(code_set_k);
                code_set_k_explicit = true;
            }
            "--eps-quota" => {
                i += 1;
                eps_quota = args
                    .get(i)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(eps_quota);
                eps_quota_explicit = true;
            }
            "--search" => {
                i += 1;
                search_allowance = args
                    .get(i)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(search_allowance);
                search_allowance_explicit = true;
            }
            "--fetch" => {
                i += 1;
                fetch_allowance = args
                    .get(i)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(fetch_allowance);
                fetch_allowance_explicit = true;
            }
            s if question.is_none() => question = Some(s.to_string()),
            _ => {
                eprintln!("Usage: svrn deep-research \"<question>\" [--run-dir DIR] [--max-rounds N] [--corpora id1,id2] [--code-set-k N] [--eps-quota F] [--search N] [--fetch N] [--backend auto|mock] [--mock-deck DIR] [--search-source mock|corpus|web] [--consent public-web|peer|personal] [--resume DIR]");
                return 1;
            }
        }
        i += 1;
    }
    // The backend is a closed set: a misspelled or unregistered backend
    // must refuse, never silently route (§18.3 — the mock itself
    // refuses any other backend id).
    if backend != "auto" && backend != "mock" {
        eprintln!("deep-research: unknown backend {backend:?} — the closed set is auto | mock");
        return 1;
    }
    if backend == "mock" && mock_deck_dir.is_none() {
        eprintln!("deep-research: --backend mock requires --mock-deck DIR");
        return 1;
    }
    if backend != "mock" && mock_deck_dir.is_some() {
        eprintln!("deep-research: --mock-deck requires --backend mock (no silent substitution)");
        return 1;
    }
    // The corpus source acquires from the estate's corpus-search
    // surface: a run that asks for the corpus source without naming
    // any corpus would search nothing — refused loudly, never a
    // silent empty.
    if search_source == SearchSource::Corpus && corpora.is_empty() {
        eprintln!("deep-research: --search-source corpus requires --corpora id1,id2");
        return 1;
    }
    // A run whose acquisition leg CANNOT acquire is refused here, at
    // launch, rather than after it has spent its round budget.
    //
    // `LiveResearchPort::web_search` calls `egress::verify` on every
    // query with `user_formed: false`; with no `ConsentGrant` that
    // refuses, every round returns zero hits, and the loop lands an
    // honest-but-empty report. `LiveResearchPort::new` already PRINTS the
    // posture at launch and names the flag; what it did not do is stop, so
    // the run spent its whole budget to deliver an empty report the operator
    // could have been spared. A warning that is followed by doing it anyway
    // is not a refusal. `--backend
    // mock` is exempt: it serves search and fetch from the deck and
    // never reaches the egress boundary. The corpus source is exempt:
    // `estate_search` is local.
    let needs_egress = backend != "mock" && search_source != SearchSource::Corpus;
    if needs_egress && consent_floor.is_none() {
        eprintln!(
            "deep-research: refusing to start — the {} search source releases queries to the \n\
             open web, and this run carries no consent grant, so every query would be refused \n\
             at the egress boundary and the run would produce an empty report.\n\
             \n\
             Grant it:      --consent public-web\n\
             Or stay local: --search-source corpus --corpora <id>",
            search_source.as_str()
        );
        return 2;
    }
    // A fresh launch needs a question; a resume refuses one (the
    // checkpoint's question is the frozen identity). Naming both a run
    // dir and a resume dir would name two run dirs — refused.
    if question.is_none() && resume_dir.is_none() {
        eprintln!(
            "Usage: svrn deep-research \"<question>\" [--run-dir DIR] [--max-rounds N] \
             [--corpora id1,id2] [--code-set-k N] [--eps-quota F] [--search N] [--fetch N] \
             [--backend auto|mock] [--mock-deck DIR] [--search-source mock|corpus|web] \
             [--consent public-web|peer|personal] [--resume DIR]"
        );
        return 1;
    }
    if resume_dir.is_some() && run_dir_explicit {
        eprintln!(
            "deep-research: --run-dir cannot be combined with --resume — the resumed run dir \
             is the --resume argument"
        );
        return 1;
    }

    // Daemon + models: the loop is local-only, but it still needs the
    // local daemon's embed + draft surface. The ONE read (the fresh
    // launch reads the same accessor through `launch::prepare`).
    let (endpoint, draft_model, embed_model) = match launch::daemon_targets() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("deep-research: {e}");
            return 1;
        }
    };

    // T3a: the resume gate. The checkpoint's frozen config is the
    // identity; the operator's flags are verified against it
    // flag-by-flag (each mismatch refuses, naming the flag). The
    // question, consent grant, and allowances come from the
    // checkpoint — nothing is re-decided.
    if let Some(resume_dir) = resume_dir {
        // Only EXPLICITLY-passed flags are verified against the frozen
        // config (the checkpoint's values are the default for flags the
        // operator did not pass — bare `--resume DIR` inherits the whole
        // config). A conflicting explicit flag refuses below, naming it.
        return match resume_run_inner(
            &resume_dir,
            &draft_model,
            &embed_model,
            &endpoint,
            max_rounds_explicit.then_some(max_rounds),
            corpora_explicit.then_some(corpora.as_slice()),
            code_set_k_explicit.then_some(code_set_k),
            eps_quota_explicit.then_some(eps_quota),
            search_allowance_explicit.then_some(search_allowance),
            fetch_allowance_explicit.then_some(fetch_allowance),
            search_source_explicit.then_some(search_source),
            consent_floor,
            backend_explicit.then_some(backend.as_str()),
            mock_deck_explicit.then_some(mock_deck_dir.as_deref()),
            question.as_deref(),
        )
        .await
        {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("deep-research: --resume refused: {e}");
                1
            }
        };
    }

    // A fresh launch was validated to carry a question above; the
    // resume branch already returned for the resume shape.
    let question = question.expect("a fresh launch has a question (validated above)");

    // The ONE launch (sovereign-core `deep_research::launch::prepare`):
    // the daemon targets, the run id + dir, the consent grant, the
    // provider, the port, the RunConfig and the resume sidecar. The verb
    // supplies only what an operator typed; nothing here re-decides a
    // runtime default, and the desktop launches through the same call.
    let launch = match launch::prepare(launch::LaunchOptions {
        question: question.clone(),
        runs_base: run_dir,
        max_rounds,
        code_set_k,
        eps_quota,
        search_allowance,
        fetch_allowance,
        estate_corpus_ids: corpora.clone(),
        search_source,
        backend: backend.clone(),
        mock_deck_dir: mock_deck_dir.clone(),
        consent_floor,
    })
    .await
    {
        Ok(l) => l,
        Err(e) => {
            eprintln!("deep-research: {e}");
            return 1;
        }
    };

    eprintln!("deep-research: run {} — {question}", launch.run_id);
    eprintln!("deep-research: run dir {}", launch.run_dir.display());
    eprintln!("deep-research: web backend {}", launch.web_backend);
    eprintln!("deep-research: search source {}", search_source.as_str());
    if backend == "mock" {
        eprintln!(
            "deep-research: mock deck {} (search/fetch served from the deck; drafts delegated)",
            mock_deck_dir.as_deref().expect("validated above").display()
        );
    }
    if search_source == SearchSource::Corpus {
        eprintln!("deep-research: corpus source over: {}", corpora.join(", "));
    }
    eprintln!(
        "deep-research: daemon {} (draft {}, embed {})",
        launch.endpoint, launch.draft_model, launch.embed_model
    );

    let mut outcome = match run(
        launch.config.clone(),
        launch.port.clone(),
        launch.provider.clone(),
        Arc::new(AtomicBool::new(false)),
    )
    .await
    {
        Ok(o) => o,
        Err(e) => {
            eprintln!("deep-research: run failed: {e}");
            return 1;
        }
    };
    // Closing the run is not optional: the fetched evidence lands in
    // `dr-estate-<run_id>` (retrieval-visible without a manual ritual)
    // and the clean RACE page is written beside report.md. Either
    // failure fails the verb loudly — never in silence.
    if let Err(e) = launch::close(&mut outcome, &launch.provider, &launch.embed_model).await {
        eprintln!("deep-research: {e}");
        return 1;
    }
    print_summary(&outcome);
    0
}

/// `--resume DIR` (order deep-research-t3a): restore an interrupted
/// run's state from its checkpoint and continue at the next round.
/// Every refusal is typed and names what was withheld:
///   - the checkpoint envelope (read_checkpoint — malformed /
///     inconsistent / foreign),
///   - a passed question (the checkpoint's question is the frozen
///     identity),
///   - the launch sidecar (resume-input.json — unreadable, malformed,
///     or belonging to another run),
///   - each EXPLICITLY-passed flag that conflicts with the checkpoint's
///     frozen config (--max-rounds, --search, --fetch, --code-set-k,
///     --eps-quota, --corpora, --search-source, --consent) or the
///     sidecar (--backend, --mock-deck). A flag the operator did NOT
///     pass inherits the checkpoint's frozen value — bare `--resume
///     DIR` is the canonical resume shape.
/// The core's resume_start adds the charter-hash, config-identity,
/// ledger-continuity, and live-lock gates behind this surface.
async fn resume_run_inner(
    resume_dir: &Path,
    draft_model: &str,
    embed_model: &str,
    endpoint: &str,
    max_rounds: Option<u32>,
    corpora: Option<&[String]>,
    code_set_k: Option<usize>,
    eps_quota: Option<f64>,
    search_allowance: Option<u32>,
    fetch_allowance: Option<u32>,
    search_source: Option<SearchSource>,
    consent_floor: Option<Custody>,
    backend: Option<&str>,
    mock_deck_dir: Option<Option<&Path>>,
    question: Option<&str>,
) -> Result<(), String> {
    let cp = read_checkpoint(resume_dir)?;
    let mut c = cp.config.clone();
    // The operator's named dir IS the state home (order
    // deep-research-t3a, measured red: the core anchored on
    // cp.config.run_dir — the LAUNCH dir — so a `--resume` of a COPY
    // resumed and closed the ORIGINAL run, and a tampered copy's
    // deadbeef checkpoint was never even read). `run_dir` is a
    // location, not an identity field (the charter — the identity —
    // never included it; config_mismatch does not compare it): the
    // checkpoint records where the run LAUNCHED, `--resume <dir>`
    // anchors where it CONTINUES. All state reads/writes below go to
    // the named dir.
    c.run_dir = resume_dir.to_path_buf();

    if let Some(q) = question {
        return Err(format!(
            "a question argument ({q:?}) substitutes for the checkpoint's frozen question — \
             resume without one"
        ));
    }

    // The launch sidecar: the run's backend identity. A run launched
    // before the sidecar existed has no verifiable identity — refused,
    // never assumed.
    let sidecar_path = resume_dir.join("resume-input.json");
    let raw = std::fs::read_to_string(&sidecar_path).map_err(|e| {
        format!(
            "{sidecar_path:?} is unreadable ({e}) — the run's backend identity cannot be \
             verified (a run launched before the sidecar existed cannot be resumed)"
        )
    })?;
    let sidecar: ResumeInput =
        serde_json::from_str(&raw).map_err(|e| format!("{sidecar_path:?} is malformed: {e}"))?;
    if sidecar.icd != "resume_input" || sidecar.version != ICD_VERSION {
        return Err(format!(
            "{sidecar_path:?} is not a resume sidecar (icd {:?}, version {}) — foreign or \
             tampered",
            sidecar.icd, sidecar.version
        ));
    }
    if sidecar.run_id != c.run_id {
        return Err(format!(
            "{sidecar_path:?} belongs to run {} but the checkpoint is run {} — mismatched run \
             dir",
            sidecar.run_id, c.run_id
        ));
    }

    // Flag-by-flag identity — an EXPLICITLY-passed flag is verified
    // against the frozen config; a flag the operator did NOT pass
    // inherits the checkpoint's value (bare `--resume DIR` resumes with
    // the exact state the run was interrupted with). Each refusal names
    // the flag AND the checkpoint's value, so the operator sees exactly
    // what to drop.
    if let Some(max_rounds) = max_rounds {
        if max_rounds != c.max_rounds {
            return Err(format!(
                "--max-rounds {max_rounds} differs from the checkpoint's {} — a resume keeps \
                 the frozen config",
                c.max_rounds
            ));
        }
    }
    if let Some(search_allowance) = search_allowance {
        if search_allowance != c.web_search_allowance {
            return Err(format!(
                "--search {search_allowance} differs from the checkpoint's {} — a resume keeps \
                 the frozen budget",
                c.web_search_allowance
            ));
        }
    }
    if let Some(fetch_allowance) = fetch_allowance {
        if fetch_allowance != c.web_fetch_allowance {
            return Err(format!(
                "--fetch {fetch_allowance} differs from the checkpoint's {} — a resume keeps \
                 the frozen budget",
                c.web_fetch_allowance
            ));
        }
    }
    if let Some(code_set_k) = code_set_k {
        if code_set_k != c.code_set_k {
            return Err(format!(
                "--code-set-k {code_set_k} differs from the checkpoint's {} — a resume keeps \
                 the frozen config",
                c.code_set_k
            ));
        }
    }
    if let Some(eps_quota) = eps_quota {
        if eps_quota != c.eps_quota {
            return Err(format!(
                "--eps-quota {eps_quota} differs from the checkpoint's {} — a resume keeps the \
                 frozen config",
                c.eps_quota
            ));
        }
    }
    if let Some(corpora) = corpora {
        if corpora != c.estate_corpus_ids.as_slice() {
            return Err(format!(
                "--corpora {} differs from the checkpoint's {} — a resume keeps the frozen \
                 corpus set",
                corpora.join(","),
                c.estate_corpus_ids.join(",")
            ));
        }
    }
    if let Some(search_source) = search_source {
        if search_source != c.search_source {
            return Err(format!(
                "--search-source {} differs from the checkpoint's {} — a resume keeps the \
                 frozen source",
                search_source.as_str(),
                c.search_source.as_str()
            ));
        }
    }
    if let Some(backend) = backend {
        if backend != sidecar.backend {
            return Err(format!(
                "--backend {backend} differs from the run's recorded {} — the backend is part \
                 of the run's identity",
                sidecar.backend
            ));
        }
    }
    match mock_deck_dir {
        Some(Some(given)) => match sidecar.mock_deck_dir.as_deref() {
            Some(recorded) if given.to_string_lossy() != recorded => {
                return Err(format!(
                    "--mock-deck {} differs from the run's recorded {recorded} — the deck is \
                     part of the run's identity",
                    given.display()
                ));
            }
            None => {
                return Err(
                    "--mock-deck was given but the run's sidecar records no deck — the run did \
                     not launch from a mock deck"
                        .to_string(),
                );
            }
            _ => {}
        },
        // Omitted: the sidecar's recorded deck IS the identity — the
        // port is rebuilt from it below, never from the operator's flags.
        _ => {}
    }
    // The consent grant is frozen in the checkpoint (FR-3): a
    // contradicting flag refuses; an omitted flag keeps the grant.
    match (consent_floor, &c.consent) {
        (Some(f), Some(g)) if f != g.release_floor => {
            return Err(format!(
                "--consent {} differs from the checkpoint's frozen {} — the grant is part of \
                 the run's identity",
                f.as_str(),
                g.release_floor.as_str()
            ));
        }
        (Some(_), None) => {
            return Err(
                "--consent was given but the checkpoint's run has no consent grant — resume \
                 without it"
                    .to_string(),
            );
        }
        _ => {}
    }

    let run_id = c.run_id.clone();
    eprintln!(
        "deep-research: resume {run_id} — continuing at round {}",
        cp.written_after_round + 1
    );
    eprintln!("deep-research: run dir {}", resume_dir.display());
    eprintln!("deep-research: question {}", c.question);
    eprintln!("deep-research: web backend {}", c.web_backend);
    if sidecar.backend == "mock" {
        eprintln!(
            "deep-research: mock deck {} (search/fetch served from the deck; drafts delegated)",
            sidecar.mock_deck_dir.as_deref().unwrap_or("?")
        );
    }
    if c.search_source == SearchSource::Corpus {
        eprintln!(
            "deep-research: corpus source over: {}",
            c.estate_corpus_ids.join(", ")
        );
    }
    eprintln!("deep-research: daemon {endpoint} (draft {draft_model}, embed {embed_model})");

    let provider: Arc<dyn InferenceProvider> =
        Arc::new(RemoteApiProvider::new(endpoint, None, draft_model, 8192));
    // The port is rebuilt from the SIDECAR's identity + the
    // checkpoint's config — never from the operator's flags (those
    // were verified equal above).
    let (port, _web_backend) = build_port(
        &sidecar.backend,
        sidecar.mock_deck_dir.as_deref().map(Path::new),
        c.search_source,
        &c.estate_corpus_ids,
        provider.clone(),
        c.consent.clone(),
    )
    .await?;
    let mut outcome = resume(c, port, provider.clone(), Arc::new(AtomicBool::new(false)))
        .await
        .map_err(|e| format!("resume failed: {e}"))?;
    // Same close sequence as a fresh launch (estate ingest + the RACE
    // page) — one implementation, never a second one that can drift.
    launch::close(&mut outcome, &provider, embed_model).await?;
    print_summary(&outcome);
    Ok(())
}

fn print_summary(outcome: &RunOutcome) {
    let m = &outcome.manifest;
    println!();
    println!("deep-research: {outcome:?}");
    println!("terminal state: {}", outcome.terminal_state.as_str());
    println!("report: {}", outcome.report_path.display());
    println!(
        "rounds: {} | gaps after last round: {} | searches: {} | fetched sources: {}",
        m.rounds.len(),
        m.rounds.last().map(|r| r.gaps_after).unwrap_or(0),
        m.rounds.iter().map(|r| r.search_calls).sum::<u32>(),
        m.sources.fetched.len()
    );
    if !m.not_covered.is_empty() {
        println!("open questions (could-not-judge):");
        for g in &m.not_covered {
            println!("  - {g}");
        }
    }
    println!("artifacts (flight recorder):");
    for a in &outcome.artifacts {
        println!("  {a}");
    }
}

#[cfg(test)]
mod tests {
    use sovereign_core::deep_research::estate::estate_snippet;

    /// Measured fixture (demo re-ask dr-1786727099): the Smithsonian
    /// timeline chunk's 240-char prefix is nav + donate blurb; the
    /// answer content starts ~1.6k chars in. The snippet must center
    /// on the query terms, not the prefix.
    #[test]
    fn estate_snippet_centers_on_query_terms_not_nav_chrome() {
        let content = "Apollo 11 Timeline | National Air and Space Museum Skip to main content \
            Visit tips around Freedom 250 Grand Prix in Washington, DC. \
            Give Show additional content Give Donate Become a Member Wall of Honor Ways to Give \
            Host an Event Be the spark Your support will help fund exhibitions, educational \
            programming, and preservation efforts. Apollo 11 Timeline \
            Breadcrumb Home Explore Stories The Apollo Missions Apollo 11 Timeline \
            On July 20, 1969, a human walked on the Moon for the first time. \
            From launch to landing, Armstrong, Aldrin, and Collins were on a three day journey \
            to the Moon.";
        let query =
            "When did the Apollo 11 mission land on the Moon and who were its crew members?";
        let snippet = estate_snippet(content, query, 600);
        assert!(
            snippet.contains("July 20, 1969"),
            "snippet must carry the answer content, not the donate blurb: {snippet}"
        );
        assert!(
            snippet.contains("Armstrong, Aldrin, and Collins"),
            "snippet must carry the crew content: {snippet}"
        );
    }

    /// No query term in the chunk — fall back to the prefix (short
    /// chunks, non-lexical matches).
    #[test]
    fn estate_snippet_falls_back_to_prefix_without_query_terms() {
        let content = "short chunk with no matching terms here";
        let snippet = estate_snippet(content, "zzzqqq wwww", 50);
        assert_eq!(snippet, content);
    }
}
