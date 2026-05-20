//! `sovereign-cli static-embed distill` — distill an embedding teacher
//! gguf into a `vocab × dim` matrix the routing/scoping layer mean-pools
//! per-query without paying a GPU forward.
//!
//! Mirrors Model2Vec at the algorithmic level:
//! 1. Load the teacher (e.g. `embeddinggemma-300M-BF16.gguf`) with
//!    `LlamaContextType` defaulted to the embedding pooled-mean path
//!    (`with_embeddings(true)`).
//! 2. For each token id in the gguf vocab, render the piece, embed
//!    it as a single-input batch via the same context, and read the
//!    pooled embedding via `embeddings_seq_ith`.
//! 3. MRL-truncate to the operator's target `--dim` (default 256).
//! 4. Write a `.s2v` artifact directory containing `header.json`,
//!    `tokenizer.json` (copy of the operator-supplied teacher
//!    tokenizer), and `matrix.safetensors`.
//!
//! Why per-token batches instead of bulk-encode-then-pool: the teacher
//! is an *attention*-based encoder. A single-token input is what
//! Model2Vec needs — the static-embed quality comes from each
//! token's context-free pretraining signal, not from inter-token
//! attention. Batching multiple tokens into one sequence would
//! produce mean-pooled embeddings dominated by whatever heuristic
//! co-occurrence the teacher's chat-templated prompts taught it.
//!
//! Cost on Strix Halo / Mac M2 Max: ~256K vocab × ~5ms per batch of
//! 16 single-token sequences = ~80 seconds to ~5 minutes wall-clock,
//! depending on GPU availability. Acceptable as an offline one-shot.

use std::num::NonZeroU32;
use std::path::PathBuf;
use std::sync::Arc;

use safetensors::tensor::TensorView;
use sovereign_inference::llama::cpp as llama;
use sovereign_static_embed::{artifact, ArtifactHeader};

/// Distill subcommand entrypoint. Argv shape:
///
/// ```text
/// sovereign static-embed distill \
///     --teacher        ~/models/embeddinggemma-300M-BF16.gguf \
///     --tokenizer-json ~/models/embeddinggemma/tokenizer.json \
///     --out            ~/.sovereign/static-embed/active \
///     [--dim 256] [--batch 16] [--n-gpu-layers 999]
/// ```
pub async fn run_static_embed(args: &[String]) -> i32 {
    let Some(sub) = args.first().map(|s| s.as_str()) else {
        print_help();
        return 0;
    };
    match sub {
        "distill" => match parse_distill_args(&args[1..]) {
            Ok(opts) => match run_distill(opts) {
                Ok(()) => 0,
                Err(e) => {
                    eprintln!("static-embed: distill failed: {e}");
                    1
                }
            },
            Err(e) => {
                eprintln!("static-embed distill: {e}");
                print_distill_help();
                2
            }
        },
        "--help" | "-h" | "help" => {
            print_help();
            0
        }
        other => {
            eprintln!("static-embed: unknown subcommand `{other}`");
            print_help();
            2
        }
    }
}

fn print_help() {
    println!(
        "Usage: sovereign static-embed <subcommand> [flags]\n\n\
         Subcommands:\n\
           distill    Distill a teacher gguf into a static-embed artifact.\n"
    );
}

fn print_distill_help() {
    println!(
        "Usage: sovereign static-embed distill --teacher <path> \\\n\
         \t                                    --tokenizer-json <path> \\\n\
         \t                                    --out <dir> \\\n\
         \t                                    [--dim 256] [--batch 16] \\\n\
         \t                                    [--n-gpu-layers 999]\n\n\
         The teacher is loaded with embedding context; the tokenizer.json\n\
         is copied into the artifact unchanged. Default `--out` is\n\
         ~/.sovereign/static-embed/active/.\n"
    );
}

struct DistillOpts {
    teacher: PathBuf,
    tokenizer_json: PathBuf,
    out: PathBuf,
    dim: usize,
    batch: usize,
    n_gpu_layers: u32,
}

fn parse_distill_args(argv: &[String]) -> Result<DistillOpts, String> {
    let mut teacher: Option<PathBuf> = None;
    let mut tokenizer_json: Option<PathBuf> = None;
    let mut out: Option<PathBuf> = None;
    let mut dim: usize = 256;
    let mut batch: usize = 16;
    let mut n_gpu_layers: u32 = 999;
    let mut i = 0;
    while i < argv.len() {
        match argv[i].as_str() {
            "--teacher" => {
                teacher = argv.get(i + 1).map(PathBuf::from);
                i += 2;
            }
            "--tokenizer-json" => {
                tokenizer_json = argv.get(i + 1).map(PathBuf::from);
                i += 2;
            }
            "--out" => {
                out = argv.get(i + 1).map(PathBuf::from);
                i += 2;
            }
            "--dim" => {
                dim = argv
                    .get(i + 1)
                    .and_then(|s| s.parse().ok())
                    .ok_or_else(|| "--dim requires a positive integer".to_string())?;
                i += 2;
            }
            "--batch" => {
                batch = argv
                    .get(i + 1)
                    .and_then(|s| s.parse().ok())
                    .ok_or_else(|| "--batch requires a positive integer".to_string())?;
                i += 2;
            }
            "--n-gpu-layers" => {
                n_gpu_layers = argv
                    .get(i + 1)
                    .and_then(|s| s.parse().ok())
                    .ok_or_else(|| "--n-gpu-layers requires u32".to_string())?;
                i += 2;
            }
            "--help" | "-h" => return Err("help".into()),
            other => return Err(format!("unknown flag `{other}`")),
        }
    }
    let teacher = teacher.ok_or_else(|| "missing --teacher".to_string())?;
    let tokenizer_json =
        tokenizer_json.ok_or_else(|| "missing --tokenizer-json".to_string())?;
    let out = out.unwrap_or_else(|| {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/tmp"));
        home.join(".sovereign/static-embed/active")
    });
    if !teacher.is_file() {
        return Err(format!("teacher gguf not found: {}", teacher.display()));
    }
    if !tokenizer_json.is_file() {
        return Err(format!(
            "tokenizer.json not found: {}",
            tokenizer_json.display()
        ));
    }
    if dim == 0 {
        return Err("--dim must be > 0".into());
    }
    if batch == 0 {
        return Err("--batch must be > 0".into());
    }
    Ok(DistillOpts {
        teacher,
        tokenizer_json,
        out,
        dim,
        batch,
        n_gpu_layers,
    })
}

fn run_distill(opts: DistillOpts) -> Result<(), String> {
    use llama::context::params::LlamaContextParams;
    use llama::llama_backend::LlamaBackend;
    use llama::llama_batch::LlamaBatch;
    use llama::model::params::LlamaModelParams;
    use llama::model::{LlamaModel, Special};
    use llama::token::LlamaToken;

    eprintln!(
        "static-embed distill: teacher={} dim={} batch={} out={}",
        opts.teacher.display(),
        opts.dim,
        opts.batch,
        opts.out.display()
    );

    let backend =
        Arc::new(LlamaBackend::init().map_err(|e| format!("llama backend init: {e}"))?);
    let model_params = LlamaModelParams::default().with_n_gpu_layers(opts.n_gpu_layers);
    let model = LlamaModel::load_from_file(&backend, &opts.teacher, &model_params)
        .map_err(|e| format!("load teacher gguf: {e}"))?;
    let model = Arc::new(model);

    let n_vocab = model.n_vocab();
    if n_vocab <= 0 {
        return Err(format!("teacher reports n_vocab={n_vocab}; aborting"));
    }
    let n_vocab = n_vocab as usize;
    let native_dim = model.n_embd() as usize;
    if opts.dim > native_dim {
        return Err(format!(
            "--dim {} > teacher native dim {native_dim}; MRL truncation only \
             reduces dimensions",
            opts.dim
        ));
    }

    // Context sized for one batch of `opts.batch` single-token
    // sequences. n_seq_max must allow our batch fan-out; n_batch must
    // hold `batch` tokens; n_ctx can be small — we only ever feed one
    // token per sequence.
    let n_ctx: u32 = 64;
    let ctx_params = LlamaContextParams::default()
        .with_n_ctx(NonZeroU32::new(n_ctx))
        .with_n_batch(opts.batch as u32 * 2)
        .with_n_ubatch(opts.batch as u32 * 2)
        .with_n_seq_max(opts.batch as u32)
        .with_n_threads(llama_threads_for_host() as i32)
        .with_n_threads_batch(llama_threads_for_host() as i32)
        .with_embeddings(true)
        .with_offload_kqv(opts.n_gpu_layers > 0);

    // SAFETY: the static lifetime is anchored by holding `model`
    // alive for the entire distill loop below; see EmbedSlot in
    // sovereign-inference/src/embedded.rs for the same pattern.
    let mut ctx = unsafe {
        let model_ref: &'static LlamaModel =
            &*(Arc::as_ptr(&model) as *const LlamaModel);
        model_ref
            .new_context(&backend, ctx_params)
            .map_err(|e| format!("create embedding context: {e}"))?
    };

    let total_floats = n_vocab * opts.dim;
    let mut matrix: Vec<f32> = vec![0.0; total_floats];
    let mut covered: usize = 0;
    let mut empty_pieces: usize = 0;

    let started = std::time::Instant::now();

    let mut id_cursor: usize = 0;
    while id_cursor < n_vocab {
        let batch_end = (id_cursor + opts.batch).min(n_vocab);
        // Build the batch: one token per seq_id.
        let mut batch = LlamaBatch::new(opts.batch * 2, opts.batch as i32);
        let mut local_seqs: Vec<(i32, usize)> = Vec::with_capacity(opts.batch);
        for (seq_offset, vocab_id) in (id_cursor..batch_end).enumerate() {
            let tok = LlamaToken(vocab_id as i32);
            // Skip non-renderable tokens — empty pieces produce
            // garbage embeddings (the runtime mean-pool would fold
            // them into the output otherwise).
            let piece = match model.token_to_str(tok, Special::Plaintext) {
                Ok(s) if !s.is_empty() => s,
                _ => {
                    empty_pieces += 1;
                    continue;
                }
            };
            // Re-tokenize the piece — the gguf vocab includes byte
            // pieces and special markers whose direct add_id doesn't
            // round-trip cleanly. Going through str_to_token is the
            // reliable path; the result is usually a 1-2 token
            // sequence.
            let toks = match model.str_to_token(&piece, llama::model::AddBos::Never) {
                Ok(t) if !t.is_empty() => t,
                _ => {
                    empty_pieces += 1;
                    continue;
                }
            };
            let seq_id = seq_offset as i32;
            for (pos, tk) in toks.iter().enumerate() {
                let last = pos == toks.len() - 1;
                batch
                    .add(*tk, pos as i32, &[seq_id], last)
                    .map_err(|e| format!("batch add (seq {seq_id}): {e}"))?;
            }
            local_seqs.push((seq_id, vocab_id));
        }
        if !local_seqs.is_empty() {
            ctx.decode(&mut batch).map_err(|e| {
                format!("decode at vocab cursor {id_cursor}: {e}")
            })?;
            for (seq_id, vocab_id) in &local_seqs {
                let emb = ctx
                    .embeddings_seq_ith(*seq_id)
                    .map_err(|e| format!("read embedding seq {seq_id}: {e}"))?;
                if emb.len() < opts.dim {
                    return Err(format!(
                        "teacher returned {} floats for seq {seq_id}; need {}",
                        emb.len(),
                        opts.dim
                    ));
                }
                let row_start = vocab_id * opts.dim;
                let row = &mut matrix[row_start..row_start + opts.dim];
                row.copy_from_slice(&emb[..opts.dim]);
                covered += 1;
            }
            ctx.clear_kv_cache();
        }

        if id_cursor % (opts.batch * 50) == 0 && id_cursor > 0 {
            let pct = (id_cursor as f32 / n_vocab as f32) * 100.0;
            eprintln!(
                "[distill] {id_cursor}/{n_vocab} ({pct:.1}%) — covered={covered} empty={empty_pieces}"
            );
        }
        id_cursor = batch_end;
    }

    let elapsed = started.elapsed();
    eprintln!(
        "[distill] complete: {covered}/{n_vocab} tokens embedded ({empty_pieces} empty pieces skipped) in {:.1}s",
        elapsed.as_secs_f32()
    );

    write_artifact(&opts, n_vocab, &matrix)?;
    Ok(())
}

fn write_artifact(
    opts: &DistillOpts,
    vocab_size: usize,
    matrix: &[f32],
) -> Result<(), String> {
    let out = &opts.out;
    std::fs::create_dir_all(out)
        .map_err(|e| format!("mkdir {}: {e}", out.display()))?;

    // Header.
    let teacher_id = opts
        .teacher
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();
    let now = chrono::Utc::now().to_rfc3339();
    let header = ArtifactHeader {
        teacher_id,
        dim: opts.dim,
        vocab_size,
        zipf_alpha: 0.0,
        pca_reduced: false,
        created_at: now,
    };
    let header_path = out.join(artifact::HEADER);
    std::fs::write(
        &header_path,
        serde_json::to_string_pretty(&header)
            .map_err(|e| format!("serialize header: {e}"))?,
    )
    .map_err(|e| format!("write header: {e}"))?;

    // Tokenizer (copy verbatim).
    let tok_dst = out.join(artifact::TOKENIZER);
    std::fs::copy(&opts.tokenizer_json, &tok_dst)
        .map_err(|e| format!("copy tokenizer.json: {e}"))?;

    // Matrix.
    let bytes: Vec<u8> = matrix.iter().flat_map(|f| f.to_le_bytes()).collect();
    let view = TensorView::new(
        safetensors::tensor::Dtype::F32,
        vec![vocab_size, opts.dim],
        &bytes,
    )
    .map_err(|e| format!("matrix view: {e}"))?;
    let mut tensors = std::collections::HashMap::new();
    tensors.insert("matrix".to_string(), view);
    let serialized = safetensors::serialize(tensors, &None)
        .map_err(|e| format!("serialize matrix: {e}"))?;
    std::fs::write(out.join(artifact::MATRIX), serialized)
        .map_err(|e| format!("write matrix: {e}"))?;

    eprintln!(
        "[distill] artifact written: {}\n         header: {}\n         tokenizer: {}\n         matrix:    {} ({} bytes)",
        out.display(),
        header_path.display(),
        tok_dst.display(),
        out.join(artifact::MATRIX).display(),
        bytes.len()
    );
    Ok(())
}

/// Match the embed slot's thread heuristic: half the cores rounded
/// down (BLAS-friendly), min 1. Embedding decode is compute-bound,
/// not memory-bound, so over-subscribing cores actively slows it.
fn llama_threads_for_host() -> usize {
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    (cores / 2).max(1)
}
