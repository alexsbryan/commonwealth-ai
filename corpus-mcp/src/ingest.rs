// SPDX-License-Identifier: AGPL-3.0-or-later
//! `corpus ingest <recipe.toml>` — the whole of `EPISTEMIC_INDEX.md` §4's
//! middle command, against two bare OpenAI-compatible endpoints.
//!
//! ```sh
//! # one model per process (llama-server)
//! corpus ingest my-coins.toml --chat-url http://localhost:8090/v1 \
//!                             --embed-url http://localhost:8089/v1
//! # one URL for both (Ollama)
//! corpus ingest my-coins.toml --base-url http://localhost:11434/v1
//! ```
//!
//! acquire → extract → chunk → embed → index (`corpus-engine`'s own recipe
//! pipeline), then seed → extract → cluster → name → resolve → tensions →
//! gaps → configure → report → backfill (`sovereign-enrichment-build`'s
//! orchestrator). The output is a v2 atlas store with a seed table and an
//! `ontology.json`, which is exactly what [`crate::ask`] walks.
//!
//! ## What this file is NOT
//!
//! It is not a pipeline. Both halves already exist and are driven from three
//! other hosts; this is the fourth, and its whole job is to resolve two
//! endpoints, write one `config.json`, and call them in order. If something
//! here starts deciding HOW a phase runs, it is in the wrong crate.
//!
//! ## Two endpoints, because llama-server serves one model per process
//!
//! `--base-url` sets both (Ollama, vLLM, our own daemon). `--chat-url` +
//! `--embed-url` name them apart. Each is probed before any disk is touched
//! and each probe's finding is printed — the OICP capability answer (404 is
//! the normal case and the case this binary exists for), the model id, and
//! the embedding width. A combination that names neither, or names a base
//! AND a half, is REFUSED rather than guessed at (ARCH §18.3).
//!
//! ## Structured output, and the two capabilities a bare endpoint lacks
//!
//! Phase schemas ride `response_format: {type: "json_schema"}`, which
//! llama-server and Ollama both honour; the orchestrator's provider registry
//! already defaults an OpenAI-compatible host to that mode and refines it
//! from `/oicp/v1/capabilities` when a host advertises. GLiNER is absent —
//! `CorpusEngine::with_chunk_entity_extractor` is never called here, because
//! the extractor lives behind `ort`, which this package's boundary forbids —
//! so the entity pass is the model's. Both facts are PRINTED at the start of
//! the run, not left to be inferred from a slower phase 1.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use corpus_engine::enrichment::pipeline::{EnrichProgress, EnrichProgressFn, PipelineRegistry};
use corpus_engine::{CorpusEngine, CorpusSpec, Recipe};
use sovereign_enrichment_build::{
    build_with_progress_with_embedder, config::EnrichConfig, corpus_io, paths, ParsedBuild,
};

use crate::host;

/// The parsed `corpus ingest` invocation. Flags only; every value here is
/// either something the person typed or something a probe learned.
#[derive(Debug, clap::Args)]
pub struct IngestArgs {
    /// The recipe to ingest. Registered under `<data-dir>/recipes/<id>/`
    /// (where the registry looks) before anything is built.
    pub recipe: PathBuf,

    /// One OpenAI-compatible endpoint serving BOTH chat and embeddings —
    /// Ollama, vLLM, or our own daemon. Mutually exclusive with the pair
    /// below.
    #[arg(long)]
    pub base_url: Option<String>,

    /// `POST <url>/chat/completions` — the enrichment phases' endpoint.
    #[arg(long)]
    pub chat_url: Option<String>,

    /// `POST <url>/embeddings` — the ingest embedder and the atlas seed
    /// table's.
    #[arg(long)]
    pub embed_url: Option<String>,

    /// Chat model id. Default: what `GET <chat-url>/models` lists first that
    /// does not look like an embedding model; refused, not defaulted, if
    /// that is empty.
    #[arg(long)]
    pub chat_model: Option<String>,

    /// Embedding model id. Default: the first id `GET <embed-url>/models`
    /// returns.
    #[arg(long)]
    pub embed_model: Option<String>,

    /// Data root holding `recipes/` and `indexes/`. Default: the same
    /// derivation every sovereign binary uses. Note that the ENRICHMENT store
    /// (`<root>/enrichment/`) derives its own root from `SOVEREIGN_DATA_DIR`
    /// and cannot be redirected per-invocation, so a value that disagrees is
    /// refused unless `--no-enrich` is given.
    #[arg(long)]
    pub data_dir: Option<PathBuf>,

    /// Skip an enrichment step (repeatable): seed, extract, cluster, name,
    /// resolve, tensions, gaps, configure, report, backfill.
    #[arg(long = "skip")]
    pub skip: Vec<String>,

    /// Enrich only these chapter ids (comma-separated, e.g.
    /// `sec_00001,sec_00002`). The whole manifest otherwise. Exists so the
    /// wiring of a long run can be proven on one chapter in a minute rather
    /// than discovered wrong after an hour (ARCH §18.4).
    #[arg(long, value_delimiter = ',')]
    pub chapters: Vec<String>,

    /// Cap on tokens a phase's chat call may emit. Thinking models spend
    /// 2-3k on chain-of-thought before the JSON.
    #[arg(long, default_value_t = 16_384)]
    pub max_output_tokens: u32,

    /// Drop sections whose body has fewer than this many words. 0 disables.
    #[arg(long, default_value_t = 40)]
    pub min_section_body_words: usize,

    /// Index the corpus and stop: no enrichment, no atlas. What `--skip`ing
    /// every step would mean, said once.
    #[arg(long)]
    pub no_enrich: bool,
}

/// The two endpoints, each in both the shapes a caller needs.
///
/// BOTH forms are load-bearing and they are not interchangeable. `*_v1` is
/// what a request path hangs off (`{v1}/embeddings`, `{v1}/chat/completions`)
/// and what the OICP probe and this host's own embed probe take. `*_root` is
/// what goes into `config.json`, because every reader downstream —
/// `probe_daemon`'s `{base}/v1/models`, `embed_one`'s `{base}/v1/embeddings`,
/// `providers::local_daemon_base`'s `{base}/v1` — appends the version segment
/// itself. Writing the `/v1` form there yields `…/v1/v1/models`, which 404s as
/// "daemon is not responding" before phase 1 runs.
#[derive(Debug)]
struct Endpoints {
    chat_v1: String,
    chat_root: String,
    embed_v1: String,
    embed_root: String,
}

/// `--base-url` XOR (`--chat-url` AND `--embed-url`) — or NEITHER, which runs
/// the discovery ladder. Anything else is a refusal naming what is missing: a
/// half-specified pair would otherwise silently send phase 1 to the embedding
/// process (ARCH §18.3).
///
/// Discovery finds ONE host serving both halves, which is Ollama's shape and
/// the reason `EPISTEMIC_INDEX.md` §4 writes the command as a bare `corpus
/// ingest my-coins.toml`. It cannot find llama-server's two-process shape —
/// nothing on the wire says which of `:8089` and `:8090` is the embedder — so
/// a discovered llama-server that serves only one of the two fails its OTHER
/// probe by name, with the pair of flags in the message.
async fn resolve_endpoints(args: &IngestArgs) -> Result<Endpoints> {
    let (chat, embed) = match (&args.base_url, &args.chat_url, &args.embed_url) {
        (Some(b), None, None) => (b.clone(), b.clone()),
        (None, Some(c), Some(e)) => (c.clone(), e.clone()),
        (None, None, None) => {
            // The SAME ladder `corpus serve` walks (ARCH §10.6) — one
            // implementation of "which endpoint", so the verb that builds a
            // corpus and the verb that serves it cannot find different hosts.
            let (url, _attempts) = host::discover(None).await?;
            eprintln!(
                "corpus-mcp: no endpoint given — using the discovered host {url} for BOTH \
                 chat and embeddings. Two llama-server processes need --chat-url and \
                 --embed-url."
            );
            (url.clone(), url)
        }
        (None, Some(_), None) => bail!("--chat-url given without --embed-url"),
        (None, None, Some(_)) => bail!("--embed-url given without --chat-url"),
        (Some(_), _, _) => bail!(
            "--base-url means one host serves both; pass it alone, or pass --chat-url and \
             --embed-url instead"
        ),
    };
    let (chat_v1, chat_root) = host::split_base(&chat);
    let (embed_v1, embed_root) = host::split_base(&embed);
    Ok(Endpoints {
        chat_v1,
        chat_root,
        embed_v1,
        embed_root,
    })
}

pub async fn run(args: IngestArgs) -> Result<()> {
    let started = std::time::Instant::now();
    let endpoints = resolve_endpoints(&args).await?;
    let data_dir = args
        .data_dir
        .clone()
        .unwrap_or_else(sovereign_contracts::rebrand::data_dir);
    let recipes_dir = data_dir.join("recipes");
    let indexes_dir = data_dir.join("indexes");
    eprintln!("corpus-mcp: data root {}", data_dir.display());

    // ── 1. Both endpoints, before any disk is touched ───────────────────────
    //
    // The embed probe is the SAME one `corpus-mcp --base-url` runs before it
    // serves: it reports capability, resolves the model id, and learns the
    // width from a real embedding. Doing it first means a wrong endpoint
    // costs a second rather than an hour of phase 1.
    let embed_profile = host::probe(&endpoints.embed_v1, args.embed_model.clone())
        .await
        .with_context(|| format!("embedding endpoint {}", endpoints.embed_v1))?;
    // `host::client()` and not a second client built here: ONE constructor
    // for this crate, so there is one answer to "how long do we wait on an
    // endpoint" — and one row in the F26 egress census instead of two (order
    // ei-6-distribution).
    //
    // The naming above is deliberate. That census is a TEXT scan — a line
    // carrying the `reqwest` constructor token counts as a construction site
    // whether it is code or a comment — so spelling the thing this line no
    // longer does would re-register the site it just removed. Watched
    // failing: it did exactly that, as `UNREGISTERED: corpus-mcp/src/
    // ingest.rs (1 site(s))`, with the only match in the file being the
    // comment.
    let chat_kind =
        host::probe_capability(&host::client(), &endpoints.chat_root, "chat endpoint").await;
    let chat_model = match args.chat_model.clone() {
        Some(m) => m,
        None => {
            // The orchestrator's own `/v1/models` reader, so this host and
            // `svrn enrich init` cannot disagree about which listed id is
            // the chat one (ARCH §10.6). Absence is refused, not defaulted.
            let (chat, _) = sovereign_enrichment_build::inference_client::resolve_default_models(
                &endpoints.chat_root,
            )
            .await;
            chat.with_context(|| {
                format!(
                    "GET {}/models listed no chat-capable model id; pass --chat-model <id>",
                    endpoints.chat_v1
                )
            })?
        }
    };
    eprintln!(
        "corpus-mcp: chat via {}/chat/completions, model `{chat_model}` ({})",
        endpoints.chat_v1,
        chat_kind.label()
    );
    // Two degradations relative to a daemon-hosted build, named here rather
    // than inferred from a slow phase later (ARCH §18.3).
    eprintln!(
        "corpus-mcp: structured output = response_format json_schema (refined from \
         /oicp/v1/capabilities when the host advertises)"
    );
    eprintln!(
        "corpus-mcp: GLiNER is NOT linked in this binary (ort is outside the package \
         boundary) — the entity pass is the chat model's, and slower"
    );

    // ── 2. Register the recipe where the registry reads it ──────────────────
    let registered = corpus_engine::recipe_install::register(&args.recipe, &recipes_dir)
        .map_err(|e| anyhow::anyhow!(e))?;
    println!(
        "registered {} as corpus `{}` → {}",
        args.recipe.display(),
        registered.id,
        registered.registered_at.display()
    );
    if let Some((before, after)) = &registered.acquire_rewrite {
        println!("  acquire path `{before}` resolved against the recipe → {after}");
    }
    let corpus_id = registered.id.clone();
    let recipe = Recipe::from_file(&registered.registered_at)
        .with_context(|| format!("loading {}", registered.registered_at.display()))?;

    // ── 3. What the enrichment will be — decided BEFORE anything is built ───
    //
    // A recipe this verb cannot enrich should fail in a second, not after the
    // index. It also settles what the corpus-engine warning further down
    // means: the recipe pipeline has its own in-ingest enrichment hook, which
    // needs an `InferenceFn` this binary deliberately does not give it, so it
    // logs "skipping". The atlas build below is what runs the enrichment here.
    let plan = enrichment_plan(&recipe, args.no_enrich)?;
    match &plan {
        Some(p) => println!(
            "enrichment: {} ({})",
            p.pipeline_id,
            match &p.ontology {
                Some(o) => format!("declared ontology `{}`", o.name),
                None => "registry pipeline".to_string(),
            }
        ),
        None => println!("enrichment: none — this run indexes and stops"),
    }
    // The enrichment store has ONE root and it is not this flag's: every
    // `paths::` accessor derives it from `SOVEREIGN_DATA_DIR` (via
    // `rebrand::data_dir`). Honouring `--data-dir` for the index and silently
    // not for the atlas would put the two halves of one corpus in two roots
    // (ARCH §18.3), so a disagreement is refused and named.
    if plan.is_some() {
        let store_root = sovereign_contracts::rebrand::data_dir();
        if data_dir != store_root {
            bail!(
                "--data-dir {} disagrees with the enrichment store's root {}. The atlas half \
                 of an ingest derives its root from SOVEREIGN_DATA_DIR and cannot be pointed \
                 elsewhere per-invocation. Set SOVEREIGN_DATA_DIR={} instead, or pass \
                 --no-enrich to index only.",
                data_dir.display(),
                store_root.display(),
                data_dir.display(),
            );
        }
    }

    // ── 4. acquire → extract → chunk → embed → index ────────────────────────
    // Document side: `ingest`'s whole job is writing chunk vectors, and they
    // must land in the space this model's family defines or nothing built here
    // is searchable by a daemon (or by `serve`) later.
    let embed = embed_profile.embed_document_fn();
    let engine = CorpusEngine::new(recipes_dir.clone(), indexes_dir.clone(), embed.clone())
        .with_embedding_model(&embed_profile.embed_model);
    println!("\n=== corpus ingest — {corpus_id} ===");
    let t_index = std::time::Instant::now();
    let result = engine
        .ingest(
            &CorpusSpec::Builtin(corpus_id.clone()),
            Some(Box::new(render_ingest_progress)),
        )
        .await
        .with_context(|| format!("ingesting `{corpus_id}`"))?;
    let index_secs = t_index.elapsed().as_secs();
    println!(
        "indexed: {} chunks in {index_secs}s ({} doc(s) skipped), {} MB",
        result.chunks_created,
        result.docs_skipped,
        result.index_size_bytes / 1_048_576,
    );

    // ── 5. Stop here when there is no enrichment to run ─────────────────────
    let Some(plan) = plan else {
        println!(
            "\ncorpus `{corpus_id}` is indexed. `corpus_search` will serve it; `ask` will \
             report it has no atlas."
        );
        return Ok(());
    };

    // ── 6. The enrichment config — the ONE thing this verb writes ──────────
    let rows = corpus_io::fetch_all_corpus_chunks(&corpus_id)
        .with_context(|| format!("reading `{corpus_id}` chunks back for the chapter manifest"))?;
    let manifest = corpus_io::build_manifest_from_corpus_rows(&corpus_id, rows, None, None, 1)
        .map_err(|e| anyhow::anyhow!("building the chapter manifest: {e}"))?;
    if manifest.is_empty() {
        bail!(
            "the chapter manifest for `{corpus_id}` is empty — the index has chunks but no \
             section metadata for the enrichment to work over"
        );
    }
    let manifest_path = paths::chapters_manifest_path(&corpus_id);
    manifest
        .save(&manifest_path)
        .with_context(|| format!("saving {}", manifest_path.display()))?;
    println!(
        "wrote {} ({} chapters)",
        manifest_path.display(),
        manifest.len()
    );
    paths::scaffold_dirs(&corpus_id).context("creating the enrichment directories")?;

    let cfg = EnrichConfig {
        schema_version: sovereign_enrichment_build::config::CONFIG_SCHEMA_VERSION,
        corpus_id: corpus_id.clone(),
        pipeline_id: plan.pipeline_id.clone(),
        // The sentinel `enrich init --from-corpus` writes: chapter inputs come
        // from the manifest's chunk_ids, not from a source file this process
        // could re-read.
        source_path: PathBuf::from(format!("corpus:{corpus_id}")),
        chapter_regex: String::new(),
        chat_model: chat_model.clone(),
        chat_models: None,
        embed_model: embed_profile.embed_model.clone(),
        // THE SEAM, and it is two fields because llama-server is two
        // processes. Every phase's chat call, the provider registry's
        // synthesized `local` entry and the egress gate that compares a
        // resolved provider against it read `base_url`; every phase's
        // resolution embedding reads `embed_base_url` through
        // `EnrichConfig::embed_base`. Roots, not `/v1` — see `Endpoints`.
        // A bare endpoint here is the whole of "runs without our stack".
        base_url: endpoints.chat_root.clone(),
        embed_base_url: Some(endpoints.embed_root.clone()),
        min_section_body_words: args.min_section_body_words,
        toc_markers: None,
        max_output_tokens: args.max_output_tokens,
        phase1b_max_output_tokens: None,
        phase_overrides: None,
        ontology: plan.ontology.clone(),
        // The same RFC3339 stamp every other writer of this field uses;
        // `chrono` is already in this binary's closure through the
        // orchestrator, so there is nothing to re-derive.
        created_at: chrono::Utc::now().to_rfc3339(),
    };
    cfg.save()
        .map_err(|e| anyhow::anyhow!("saving config.json: {e}"))?;
    println!("wrote {}", cfg.path().display());
    println!("  pipeline    = {}", plan.pipeline_id);
    if let Some(spec) = cfg.ontology.as_ref() {
        let policies = spec.policies();
        println!(
            "  ontology    = {} (version {}, {} declared types)",
            spec.name,
            spec.ontology_version,
            policies.shape.types.len()
        );
    }
    println!("  chat_model  = {chat_model} @ {}", endpoints.chat_v1);
    println!(
        "  embed_model = {} @ {} ({}-d)",
        embed_profile.embed_model, embed_profile.embeddings_url, embed_profile.embed_dims
    );

    // ── 7. The build ───────────────────────────────────────────────────────
    let chapters = (!args.chapters.is_empty()).then(|| args.chapters.clone());
    if let Some(only) = chapters.as_ref() {
        println!(
            "  chapters    = {} of the manifest (--chapters)",
            only.len()
        );
    }
    let parsed = ParsedBuild::from_inputs(corpus_id.clone(), chapters, &args.skip, false)
        .map_err(|e| anyhow::anyhow!(e))?;
    let progress: EnrichProgressFn = Arc::new(render_build_event);
    let t_build = std::time::Instant::now();
    // The Backfill step's embedder is the QUERY-side one, and that is not a
    // slip: `build_with_progress_with_embedder`'s own contract says so (the
    // daemon adapts with `inference_to_embed_query_fn`), because the seed
    // table it writes is searched by `atlas_navigate_ann` with a query-side
    // vector. `chunks.lance` is the document space; the seed table is the
    // query space; they are DIFFERENT spaces on an instruction-aware embedder
    // and pairing them is the substitution §18.3 forbids.
    //
    // Until 2026-09-07 this passed the document-side closure, with a comment
    // asserting the opposite invariant. It went unnoticed because neither side
    // applied any instruction at all, so the two wrong halves matched each
    // other — and matched no daemon-built atlas. Measured cost of the query
    // instruction on a daemon-built seed table: +0.128 mean cosine (note
    // 500f1229).
    let code = build_with_progress_with_embedder(
        &parsed,
        Some(progress),
        Some(embed_profile.embed_query_fn()),
        None,
    )
    .await;
    let build_secs = t_build.elapsed().as_secs();
    if code != 0 {
        bail!("enrichment build for `{corpus_id}` failed with exit code {code}");
    }

    println!(
        "\ncorpus `{corpus_id}` ingested and enriched in {}s ({index_secs}s index, \
         {build_secs}s enrich).",
        started.elapsed().as_secs(),
    );
    println!(
        "Serve it:  corpus-mcp --base-url {} --corpus {corpus_id}",
        endpoints.embed_v1
    );
    Ok(())
}

/// What the recipe says the enrichment is, once — resolved before anything is
/// written so a recipe this verb cannot build fails before the config exists.
#[derive(Debug)]
struct EnrichmentPlan {
    pipeline_id: String,
    ontology: Option<corpus_engine::enrichment::pipeline::CustomAtlasSpec>,
}

/// Resolve the pipeline this recipe's `[enrichment]` block asks for.
///
/// Three sources, in the order the recipe's own documentation gives them: a
/// declared `[enrichment.ontology]` (which IS the pipeline — `custom_atlas`
/// is built from the declaration, not looked up), an explicit `pipeline`, and
/// otherwise `<domain>_atlas` checked against the registry. An unresolvable
/// one is refused by name with the ids that exist; nothing is defaulted to a
/// pipeline the author did not ask for (ARCH §18.3).
///
/// `Ok(None)` means "this recipe declares no enrichment", which is a normal
/// outcome, not a failure — the corpus is indexed and searchable.
fn enrichment_plan(recipe: &Recipe, no_enrich: bool) -> Result<Option<EnrichmentPlan>> {
    if no_enrich {
        println!("--no-enrich: indexing only");
        return Ok(None);
    }
    let Some(enr) = recipe.enrichment.as_ref().filter(|e| e.enabled) else {
        return Ok(None);
    };
    if enr.enrichment_type != "atlas" {
        bail!(
            "`[enrichment] type = \"{}\"` cannot run here: this verb drives the ATLAS build \
             (the one that produces atoms, edges and ontology.json). Re-run with --no-enrich \
             to index the corpus without it.",
            enr.enrichment_type
        );
    }
    if let Some(ontology) = recipe.custom_atlas_spec() {
        return Ok(Some(EnrichmentPlan {
            pipeline_id:
                corpus_engine::enrichment::pipeline::pipelines::configurable_atlas::PIPELINE_ID
                    .to_string(),
            ontology: Some(ontology),
        }));
    }
    let registry = PipelineRegistry::builtin();
    let known = || registry.pipeline_ids().join(", ");
    let id = match (&enr.pipeline, &enr.domain) {
        (Some(p), _) => p.clone(),
        (None, Some(d)) => format!("{d}_atlas"),
        (None, None) => bail!(
            "`[enrichment] type = \"atlas\"` declares neither `pipeline` nor `domain`, and no \
             `[enrichment.ontology]` to build one from. Known pipelines: {}",
            known()
        ),
    };
    if registry.get(&id).is_none() {
        bail!("unknown atlas pipeline `{id}`. Known: {}", known());
    }
    Ok(Some(EnrichmentPlan {
        pipeline_id: id,
        ontology: None,
    }))
}

/// One line per ingest milestone. Deliberately terse: the recipe pipeline
/// emits an event per batch and this verb's interesting half is the build.
fn render_ingest_progress(p: corpus_engine::progress::IngestProgress) {
    use corpus_engine::progress::IngestProgress as P;
    match p {
        P::Extracting {
            documents_processed,
        } => eprintln!("  extract: {documents_processed} doc(s)"),
        P::Chunking { chunks_created } => eprintln!("  chunk: {chunks_created} chunk(s)"),
        P::Embedding {
            chunks_embedded,
            total,
            chunks_per_sec,
            ..
        } => {
            if chunks_embedded % 100 == 0 {
                eprintln!("  embed: {chunks_embedded}/{total} ({chunks_per_sec:.1}/s)");
            }
        }
        _ => {}
    }
}

/// One line per build transition. The CLI renders these as banners and the
/// desktop reads the wire encoding; this host wants neither — an ingest is
/// watched in a terminal, and what a watcher needs is which step is running
/// and what each one found.
fn render_build_event(evt: EnrichProgress) {
    match evt {
        EnrichProgress::BuildStart {
            pipeline_id, steps, ..
        } => println!(
            "\n=== enrich — pipeline {pipeline_id}, {} step(s): {} ===",
            steps.len(),
            steps
                .iter()
                .map(|s| s.id().to_string())
                .collect::<Vec<_>>()
                .join(" → ")
        ),
        EnrichProgress::StepStart {
            step,
            ordinal,
            total,
            ..
        } => println!("[{ordinal}/{total}] {} …", step.id()),
        EnrichProgress::StepDone { step, summary, .. } => {
            println!("[done] {}: {summary}", step.id())
        }
        EnrichProgress::StepFailed { step, message, .. } => {
            eprintln!("[FAILED] {}: {message}", step.id())
        }
        EnrichProgress::Complete {
            steps_completed, ..
        } => {
            println!("=== enrich complete: {steps_completed} step(s) ===")
        }
        other => eprintln!("  {other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(base: Option<&str>, chat: Option<&str>, embed: Option<&str>) -> IngestArgs {
        IngestArgs {
            recipe: PathBuf::from("r.toml"),
            base_url: base.map(str::to_string),
            chat_url: chat.map(str::to_string),
            embed_url: embed.map(str::to_string),
            chat_model: None,
            embed_model: None,
            data_dir: None,
            skip: Vec::new(),
            chapters: Vec::new(),
            max_output_tokens: 16_384,
            min_section_body_words: 40,
            no_enrich: false,
        }
    }

    #[tokio::test]
    async fn one_url_serves_both_halves() {
        let e = resolve_endpoints(&args(Some("http://h:11434/v1"), None, None))
            .await
            .unwrap();
        assert_eq!(e.chat_v1, "http://h:11434/v1");
        assert_eq!(e.embed_v1, "http://h:11434/v1");
        assert_eq!(e.chat_root, "http://h:11434");
        assert_eq!(e.embed_root, "http://h:11434");
    }

    #[tokio::test]
    async fn a_bare_root_is_accepted_and_normalised() {
        let e = resolve_endpoints(&args(Some("http://h:11434"), None, None))
            .await
            .unwrap();
        assert_eq!(e.chat_v1, "http://h:11434/v1");
        assert_eq!(e.chat_root, "http://h:11434");
    }

    #[tokio::test]
    async fn two_processes_are_named_apart() {
        let e = resolve_endpoints(&args(
            None,
            Some("http://h:8090/v1"),
            Some("http://h:8089/v1"),
        ))
        .await
        .unwrap();
        assert_eq!(e.chat_v1, "http://h:8090/v1");
        assert_eq!(e.embed_v1, "http://h:8089/v1");
        // The ROOTS are what reach config.json, and they must stay apart:
        // one of them is where every phase's resolution embedding goes.
        assert_eq!(e.chat_root, "http://h:8090");
        assert_eq!(e.embed_root, "http://h:8089");
    }

    /// The failure this refusal exists for: with only `--chat-url`, a guessed
    /// `--embed-url` would send every embedding to the chat process, which
    /// answers 200 with the wrong width and is only noticed at retrieval.
    #[tokio::test]
    async fn a_half_specified_pair_is_refused_by_name() {
        let err = resolve_endpoints(&args(None, Some("http://h:8090/v1"), None))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("--embed-url"), "{err}");
        let err = resolve_endpoints(&args(None, None, Some("http://h:8089/v1")))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("--chat-url"), "{err}");
    }

    // WHERE `no_endpoint_at_all_names_both_forms` WENT (order ei-6-distribution).
    //
    // It asserted that naming no endpoint is a REFUSAL listing both flag
    // forms. That stopped being true: `EPISTEMIC_INDEX.md` §4 writes the
    // command as a bare `corpus ingest my-coins.toml`, so no endpoint now runs
    // the discovery ladder, and the message a person gets is the ladder's
    // report. The test went red on exactly that change, which is what it was
    // for.
    //
    // It is not rewritten HERE because the honest version cannot live in a
    // unit test: `resolve_endpoints(None, None, None)` performs live probes,
    // and on a developer's box the third rung is their own running daemon —
    // the test would pass or fail on what happens to be up. The behaviour is
    // proven in `tests/verbs.rs::ingest_with_no_endpoint_walks_the_same_ladder`,
    // which runs the binary as a subprocess with the daemon knob pointed at a
    // dead port, and the ladder's ORDER is pinned by `host::tests` next door.

    #[tokio::test]
    async fn a_base_url_alongside_a_half_is_refused_rather_than_ranked() {
        let err = resolve_endpoints(&args(Some("http://h:1/v1"), Some("http://h:2/v1"), None))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("--base-url"), "{err}");
    }

    /// A recipe with no `[enrichment]` block indexes and stops — that is a
    /// normal outcome, and the caller must be able to tell it from a refusal.
    #[test]
    fn a_recipe_without_enrichment_plans_nothing() {
        let recipe = Recipe::from_toml(
            "[corpus]\nid = \"x\"\nname = \"x\"\n\n[acquire]\ntype = \"local_file\"\npath = \"/x.md\"\n\n[extract]\ntype = \"markdown\"\n\n[chunk]\ntype = \"paragraph\"\n",
        )
        .unwrap();
        assert!(enrichment_plan(&recipe, false).unwrap().is_none());
    }

    /// A declared ontology IS the pipeline: `custom_atlas` is built from the
    /// declaration rather than looked up, so it must win over any `domain`.
    #[test]
    fn a_declared_ontology_selects_the_custom_pipeline() {
        let recipe = Recipe::from_toml(
            "[corpus]\nid = \"x\"\nname = \"x\"\n\n[acquire]\ntype = \"local_file\"\npath = \"/x.md\"\n\n[extract]\ntype = \"markdown\"\n\n[chunk]\ntype = \"paragraph\"\n\n[enrichment]\nenabled = true\ntype = \"atlas\"\ndomain = \"literary\"\n\n[enrichment.ontology]\nversion = 1\nguidance = \"coins\"\n\n[[enrichment.ontology.types]]\nname = \"coin\"\nkind = \"entity\"\n",
        )
        .unwrap();
        let plan = enrichment_plan(&recipe, false).unwrap().unwrap();
        assert_eq!(plan.pipeline_id, "custom_atlas");
        assert!(plan.ontology.is_some());
    }

    /// Without a declaration, `domain` names a registry pipeline — and an
    /// unknown one is refused with the list, never defaulted to `literary`.
    #[test]
    fn an_unknown_domain_is_refused_with_the_registry_listing() {
        let recipe = Recipe::from_toml(
            "[corpus]\nid = \"x\"\nname = \"x\"\n\n[acquire]\ntype = \"local_file\"\npath = \"/x.md\"\n\n[extract]\ntype = \"markdown\"\n\n[chunk]\ntype = \"paragraph\"\n\n[enrichment]\nenabled = true\ntype = \"atlas\"\ndomain = \"numismatics\"\n",
        )
        .unwrap();
        let err = enrichment_plan(&recipe, false).unwrap_err().to_string();
        assert!(err.contains("numismatics_atlas"), "{err}");
        assert!(err.contains("literary_atlas"), "{err}");
    }

    #[test]
    fn a_domain_that_names_a_registry_pipeline_resolves() {
        let recipe = Recipe::from_toml(
            "[corpus]\nid = \"x\"\nname = \"x\"\n\n[acquire]\ntype = \"local_file\"\npath = \"/x.md\"\n\n[extract]\ntype = \"markdown\"\n\n[chunk]\ntype = \"paragraph\"\n\n[enrichment]\nenabled = true\ntype = \"atlas\"\ndomain = \"literary\"\n",
        )
        .unwrap();
        let plan = enrichment_plan(&recipe, false).unwrap().unwrap();
        assert_eq!(plan.pipeline_id, "literary_atlas");
        assert!(plan.ontology.is_none());
    }

    /// A non-atlas enrichment is REFUSED, not silently indexed: the person
    /// asked for enrichment and would otherwise get a corpus with no atlas
    /// and no message saying why (ARCH §18.3).
    #[test]
    fn a_non_atlas_enrichment_is_refused_by_name() {
        let recipe = Recipe::from_toml(
            "[corpus]\nid = \"x\"\nname = \"x\"\n\n[acquire]\ntype = \"local_file\"\npath = \"/x.md\"\n\n[extract]\ntype = \"markdown\"\n\n[chunk]\ntype = \"paragraph\"\n\n[enrichment]\nenabled = true\ntype = \"field_model\"\ndomain = \"philosophy\"\n",
        )
        .unwrap();
        let err = enrichment_plan(&recipe, false).unwrap_err().to_string();
        assert!(err.contains("field_model"), "{err}");
        assert!(err.contains("--no-enrich"), "{err}");
    }
}
