//! Plan a model you have not downloaded yet.
//!
//! `svrn mesh plan` answers "will this model fit my mesh?" — but until now it
//! could only answer it about a GGUF already on disk. For the models the split
//! exists to serve that is backwards: DeepSeek-V4-Flash-0731 ships as 155 GB
//! across five shards, so the operator had to spend the download to learn the
//! answer, and the answer might be no.
//!
//! It never needed the weights. The planner reads exactly two things — the
//! block count and the tensor table — and both live in each shard's *header*.
//! `parse_gguf` and `gguf_block_count` are strictly sequential: magic, KV
//! block, tensor-info table, stop. Neither seeks into the data section, and
//! tensor byte-mass is computed from dims + ggml type (`ggml_row_size`), never
//! from the file length. So a file truncated just past its tensor table parses
//! identically to the whole thing.
//!
//! That is the whole trick here. We range-GET the first few MB of each shard
//! into a temp directory, under the shards' real names so `shard_files` still
//! enumerates them as one model, and hand the existing planner a path. No
//! parser changes, no second implementation of GGUF, no new dependency —
//! and the sizing for a 155 GB model costs about 17 MB and two seconds.
//!
//! Measured on `unsloth/DeepSeek-V4-Flash-0731-GGUF`: reconstructed total
//! 155.09 GB against an actual 155.10 GB.

use std::path::{Path, PathBuf};

/// How much of shard 1 to pull on the first attempt. It carries the full KV
/// block including the tokenizer, which dominates: DeepSeek-V4-Flash's vocab
/// of 129,280 tokens makes its header 5.3 MB.
const FIRST_SHARD_BYTES: u64 = 16 * 1024 * 1024;
/// Later shards carry a 3-key KV block plus their own tensor-info table
/// (~400 entries, tens of KB). 4 MB is generous.
const OTHER_SHARD_BYTES: u64 = 4 * 1024 * 1024;
/// Header bigger than the guess? Double and retry, at most this many times.
const GROWTH_ATTEMPTS: u32 = 3;

/// A model spec resolved to something the planner can read.
pub struct ResolvedModel {
    /// Path to shard 1 — the real file, or a header-only stand-in.
    pub path: PathBuf,
    /// Number of shards the model is published in.
    pub shards: usize,
    /// TRUE total size of the model on the remote, summed across shards. The
    /// header stand-ins on disk are tiny; anything reporting "the model is N
    /// GB" must use this, not `metadata().len()`.
    pub total_bytes: u64,
    /// Human label for the plan banner, e.g. `hf:unsloth/…/UD-Q4_K_XL`.
    pub label: String,
    /// True when `path` points at header stand-ins rather than real weights.
    /// The plan must say so — a fit verdict from headers is honest about
    /// tensor mass but has not verified a single byte of weight is fetchable.
    pub headers_only: bool,
    /// Keeps the temp dir alive for the caller's lifetime. Dropping this
    /// deletes the stand-ins.
    _tmp: Option<tempfile::TempDir>,
}

/// Resolve a model spec. A plain path is passed through untouched (so every
/// existing invocation behaves exactly as before); `hf:<owner>/<repo>[/<dir>]`
/// is fetched header-only.
pub async fn resolve(spec: &str) -> Result<ResolvedModel, String> {
    if let Some(rest) = spec.strip_prefix("hf:") {
        resolve_hf(rest).await
    } else {
        let path = PathBuf::from(spec);
        let shards = sovereign_inference::embedded::shard_files(&path);
        let total_bytes = shards
            .iter()
            .filter_map(|p| std::fs::metadata(p).ok().map(|m| m.len()))
            .sum();
        Ok(ResolvedModel {
            label: path.display().to_string(),
            shards: shards.len(),
            total_bytes,
            path,
            headers_only: false,
            _tmp: None,
        })
    }
}

/// One `.gguf` published in a Hugging Face repo.
#[derive(Debug)]
struct RemoteFile {
    /// Repo-relative path, e.g. `UD-Q4_K_XL/model-00002-of-00005.gguf`.
    rfilename: String,
    size: u64,
}

async fn resolve_hf(rest: &str) -> Result<ResolvedModel, String> {
    // `<owner>/<repo>` then an optional `/<subdir>` naming the quant variant.
    let parts: Vec<&str> = rest.splitn(3, '/').collect();
    if parts.len() < 2 || parts[0].is_empty() || parts[1].is_empty() {
        return Err(format!(
            "hf spec must be hf:<owner>/<repo>[/<variant>] — got `hf:{rest}`"
        ));
    }
    let repo = format!("{}/{}", parts[0], parts[1]);
    let want_variant = parts
        .get(2)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());

    let files = fetch_repo_listing(&repo).await?;
    let (variant, shards) = select_variant(&files, want_variant.as_deref(), &repo)?;

    let total_bytes: u64 = shards.iter().map(|f| f.size).sum();
    let label = match &variant {
        Some(v) => format!("hf:{repo}/{v}"),
        None => format!("hf:{repo}"),
    };

    eprintln!(
        "  {label}\n  {} shard(s), {:.1} GB on the remote — fetching headers only",
        shards.len(),
        total_bytes as f64 / 1e9
    );

    let tmp = tempfile::tempdir().map_err(|e| format!("creating temp dir: {e}"))?;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .map_err(|e| format!("building http client: {e}"))?;

    let mut first_path = None;
    let mut want_first = FIRST_SHARD_BYTES;
    let mut want_other = OTHER_SHARD_BYTES;

    for attempt in 0..GROWTH_ATTEMPTS {
        let mut fetched_bytes = 0u64;
        for (i, f) in shards.iter().enumerate() {
            let base = Path::new(&f.rfilename)
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| format!("odd remote filename: {}", f.rfilename))?;
            let want = if i == 0 { want_first } else { want_other };
            // Never ask for more than the shard actually is: a header-only
            // shard 1 can be smaller than our guess, and a 416 would be a
            // confusing way to report that.
            let want = want.min(f.size);
            let url = format!("https://huggingface.co/{repo}/resolve/main/{}", f.rfilename);
            let bytes = range_get(&client, &url, want).await?;
            fetched_bytes += bytes.len() as u64;
            let dest = tmp.path().join(base);
            std::fs::write(&dest, &bytes)
                .map_err(|e| format!("writing {}: {e}", dest.display()))?;
            if i == 0 {
                first_path = Some(dest);
            }
        }

        let path = first_path.clone().ok_or("no shards resolved")?;
        // One call validates the whole set: `tensor_sizes` walks `shard_files`,
        // which enumerates the siblings we just wrote into the temp dir.
        match sovereign_inference::embedded::tensor_sizes(&path) {
            Ok(_) => {
                eprintln!(
                    "  headers fetched: {:.1} MB ({:.5}% of the model)\n",
                    fetched_bytes as f64 / 1e6,
                    100.0 * fetched_bytes as f64 / total_bytes as f64
                );
                return Ok(ResolvedModel {
                    path,
                    shards: shards.len(),
                    total_bytes,
                    label,
                    headers_only: true,
                    _tmp: Some(tmp),
                });
            }
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                // The tensor table ran past our guess. Double and retry — this
                // is the only failure worth retrying, and it is self-limiting.
                if attempt + 1 == GROWTH_ATTEMPTS {
                    return Err(format!(
                        "GGUF header exceeds {} MB even after {GROWTH_ATTEMPTS} attempts — \
                         download the model and plan against the file",
                        want_first / 1024 / 1024
                    ));
                }
                want_first *= 2;
                want_other *= 2;
                eprintln!(
                    "  header larger than expected — refetching at {} MB",
                    want_first / 1024 / 1024
                );
            }
            Err(e) => return Err(format!("parsing fetched headers: {e}")),
        }
    }
    Err("could not fetch a parseable header".into())
}

/// Range-GET the first `want` bytes of `url`.
async fn range_get(client: &reqwest::Client, url: &str, want: u64) -> Result<Vec<u8>, String> {
    let resp = client
        .get(url)
        .header("Range", format!("bytes=0-{}", want.saturating_sub(1)))
        .send()
        .await
        .map_err(|e| format!("GET {url}: {e}"))?;
    // 206 is the good path. A 200 means the server ignored Range and is about
    // to hand us the whole shard — refuse rather than silently pull 50 GB.
    if resp.status().as_u16() == 200 {
        return Err(format!(
            "{url} ignored the Range header (HTTP 200) — refusing to download the whole shard"
        ));
    }
    if !resp.status().is_success() {
        return Err(format!("GET {url}: HTTP {}", resp.status()));
    }
    resp.bytes()
        .await
        .map(|b| b.to_vec())
        .map_err(|e| format!("reading {url}: {e}"))
}

async fn fetch_repo_listing(repo: &str) -> Result<Vec<RemoteFile>, String> {
    let url = format!("https://huggingface.co/api/models/{repo}?blobs=true");
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .map_err(|e| format!("building http client: {e}"))?;
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("GET {url}: {e}"))?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Err(format!("no such Hugging Face repo: {repo}"));
    }
    if !resp.status().is_success() {
        return Err(format!(
            "listing {repo}: HTTP {} (gated or private repos are not supported)",
            resp.status()
        ));
    }
    let json: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("parsing the repo listing: {e}"))?;
    let mut out = Vec::new();
    for s in json
        .get("siblings")
        .and_then(|v| v.as_array())
        .ok_or("repo listing had no `siblings` array")?
    {
        let Some(name) = s.get("rfilename").and_then(|v| v.as_str()) else {
            continue;
        };
        if !name.ends_with(".gguf") {
            continue;
        }
        out.push(RemoteFile {
            rfilename: name.to_string(),
            size: s.get("size").and_then(|v| v.as_u64()).unwrap_or(0),
        });
    }
    if out.is_empty() {
        return Err(format!("{repo} publishes no .gguf files"));
    }
    Ok(out)
}

/// Group the repo's GGUFs by the directory they live in — that is how quant
/// variants are published — and pick the one asked for. With no variant named
/// and more than one on offer, list them rather than guessing: picking a quant
/// for the operator is picking how much of their memory to spend.
fn select_variant<'a>(
    files: &'a [RemoteFile],
    want: Option<&str>,
    repo: &str,
) -> Result<(Option<String>, Vec<&'a RemoteFile>), String> {
    let dir_of = |f: &RemoteFile| -> Option<String> {
        Path::new(&f.rfilename)
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(|p| p.to_string_lossy().to_string())
    };
    let mut variants: Vec<String> = Vec::new();
    for f in files {
        if let Some(d) = dir_of(f) {
            if !variants.contains(&d) {
                variants.push(d);
            }
        }
    }
    variants.sort();

    let chosen: Option<String> = match want {
        Some(w) => {
            // Accept an exact directory name, or a unique case-insensitive
            // suffix so `q4_k_xl` finds `UD-Q4_K_XL`.
            if variants.iter().any(|v| v == w) {
                Some(w.to_string())
            } else {
                let lw = w.to_ascii_lowercase();
                let hits: Vec<&String> = variants
                    .iter()
                    .filter(|v| v.to_ascii_lowercase().contains(&lw))
                    .collect();
                match hits.len() {
                    1 => Some(hits[0].clone()),
                    0 => {
                        return Err(format!(
                            "{repo} has no variant matching `{w}`. Available:\n    {}",
                            variants.join("\n    ")
                        ))
                    }
                    _ => {
                        return Err(format!(
                            "`{w}` matches several variants of {repo}:\n    {}",
                            hits.iter()
                                .map(|s| s.as_str())
                                .collect::<Vec<_>>()
                                .join("\n    ")
                        ))
                    }
                }
            }
        }
        None => {
            if variants.len() > 1 {
                return Err(format!(
                    "{repo} publishes several quant variants — name one:\n    {}\n\n  e.g. hf:{repo}/{}",
                    variants.join("\n    "),
                    variants[0]
                ));
            }
            variants.first().cloned()
        }
    };

    let mut picked: Vec<&RemoteFile> = files
        .iter()
        .filter(|f| dir_of(f) == chosen)
        .collect::<Vec<_>>();
    if picked.is_empty() {
        return Err(format!("no .gguf files under {chosen:?} in {repo}"));
    }
    // Shard order, by the index in `-NNNNN-of-NNNNN`; non-split sorts alone.
    picked.sort_by_key(|f| shard_index(&f.rfilename).unwrap_or(0));
    // A published set with a hole would silently plan a too-small model, which
    // is the exact failure `shard_files` refuses to make locally.
    if let Some(expected) = shard_count(&picked[0].rfilename) {
        if picked.len() as u32 != expected {
            return Err(format!(
                "{repo} advertises a {expected}-shard split but only {} shard(s) are listed",
                picked.len()
            ));
        }
    }
    Ok((chosen, picked))
}

/// The `NNNNN` in `<stem>-NNNNN-of-MMMMM.gguf`.
fn shard_index(name: &str) -> Option<u32> {
    let base = Path::new(name).file_name()?.to_str()?;
    let of = base.rfind("-of-")?;
    let before = base.get(..of)?;
    let dash = before.rfind('-')?;
    before.get(dash + 1..)?.parse().ok()
}

/// The `MMMMM` in `<stem>-NNNNN-of-MMMMM.gguf`.
fn shard_count(name: &str) -> Option<u32> {
    let base = Path::new(name).file_name()?.to_str()?;
    let of = base.rfind("-of-")?;
    base.get(of + 4..)?.strip_suffix(".gguf")?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(name: &str, size: u64) -> RemoteFile {
        RemoteFile {
            rfilename: name.into(),
            size,
        }
    }

    #[test]
    fn shard_index_and_count_read_the_convention() {
        let n = "UD-Q4_K_XL/DeepSeek-V4-Flash-0731-UD-Q4_K_XL-00002-of-00005.gguf";
        assert_eq!(shard_index(n), Some(2));
        assert_eq!(shard_count(n), Some(5));
        assert_eq!(shard_index("plain.gguf"), None);
    }

    /// The real DeepSeek-V4-Flash repo shape: two quant dirs, five shards each.
    #[test]
    fn selects_a_named_variant_and_orders_its_shards() {
        let files: Vec<RemoteFile> = (1..=5)
            .map(|i| f(&format!("UD-Q4_K_XL/m-{i:05}-of-00005.gguf"), 1000))
            .chain((1..=5).map(|i| f(&format!("UD-Q8_K_XL/m-{i:05}-of-00005.gguf"), 2000)))
            .collect();

        let (v, picked) = select_variant(&files, Some("UD-Q4_K_XL"), "r").unwrap();
        assert_eq!(v.as_deref(), Some("UD-Q4_K_XL"));
        assert_eq!(picked.len(), 5);
        assert_eq!(shard_index(&picked[0].rfilename), Some(1));
        assert_eq!(shard_index(&picked[4].rfilename), Some(5));
        assert_eq!(picked.iter().map(|f| f.size).sum::<u64>(), 5000);

        // Case-insensitive suffix match is enough to name a variant.
        let (v, _) = select_variant(&files, Some("q8_k_xl"), "r").unwrap();
        assert_eq!(v.as_deref(), Some("UD-Q8_K_XL"));
    }

    #[test]
    fn refuses_to_guess_between_variants_and_names_them() {
        let files = vec![
            f("UD-Q4_K_XL/m-00001-of-00001.gguf", 1),
            f("UD-Q8_K_XL/m-00001-of-00001.gguf", 1),
        ];
        let err = select_variant(&files, None, "unsloth/X").unwrap_err();
        assert!(err.contains("UD-Q4_K_XL"), "must list the options: {err}");
        assert!(err.contains("UD-Q8_K_XL"), "must list the options: {err}");

        let err = select_variant(&files, Some("nope"), "unsloth/X").unwrap_err();
        assert!(err.contains("no variant matching"), "{err}");
    }

    #[test]
    fn a_single_variant_at_the_repo_root_needs_no_naming() {
        let files = vec![f("model.gguf", 42)];
        let (v, picked) = select_variant(&files, None, "r").unwrap();
        assert_eq!(v, None);
        assert_eq!(picked.len(), 1);
    }

    /// A hole in the published set must be refused, not planned around — the
    /// same rule `shard_files` enforces locally.
    #[test]
    fn refuses_an_incomplete_published_split() {
        let files: Vec<RemoteFile> = (1..=4)
            .map(|i| f(&format!("v/m-{i:05}-of-00005.gguf"), 10))
            .collect();
        let err = select_variant(&files, Some("v"), "r").unwrap_err();
        assert!(err.contains("5-shard split but only 4"), "{err}");
    }
}
