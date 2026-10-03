// SPDX-License-Identifier: AGPL-3.0-or-later
//! HuggingFace multi-shard dataset acquirer.
//!
//! Supports two kinds of HuggingFace dataset layouts:
//!
//! 1. **Flat repos** (e.g. `manu/project_gutenberg`): parquet files listed in
//!    the `siblings` array of `/api/datasets/{repo}`, filtered by a subset
//!    prefix (`data/{subset}-*` or `{subset}/*`).
//!
//! 2. **Config-based repos** (e.g. `wikimedia/wikipedia`): parquet files are
//!    not in `siblings` but accessible via the parquet conversion API at
//!    `/api/datasets/{repo}/parquet/{subset}/train`, which returns a JSON
//!    array of direct CDN URLs.
//!
//! The acquirer tries the siblings approach first; if it finds no matching
//! shards and `subset` is set, it falls back to the parquet API.
//! All shards are downloaded with resume support into a local directory,
//! and the directory `PathBuf` is returned for the extractor to consume.

use std::io::Write;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::progress::{IngestProgress, ProgressCallback};

pub(crate) const HF_USER_AGENT: &str = "CorpusEngine/0.1 (+https://sovereign.dev/corpus-engine)";

pub struct HuggingFaceDatasetAcquirer {
    pub repo: String,
    pub subset: Option<String>,
    /// Optional subset of shard indices to download.
    ///
    /// Indices are 0-based positions in the **sorted** full shard manifest
    /// (ascending by local filename). Both the coordinator and the peer must
    /// sort the same full manifest before indexing, so they agree on which
    /// file each index refers to.
    ///
    /// `None` = download all shards (default).
    pub file_indices: Option<Vec<usize>>,
}

impl HuggingFaceDatasetAcquirer {
    pub fn new(repo: &str, subset: Option<&str>) -> Self {
        Self {
            repo: repo.to_string(),
            subset: subset.map(|s| s.to_string()),
            file_indices: None,
        }
    }

    pub async fn download(
        &self,
        download_dir: &Path,
        corpus_id: &str,
        progress: &Option<ProgressCallback>,
    ) -> Result<PathBuf> {
        let dest_dir = download_dir.join(corpus_id);
        std::fs::create_dir_all(&dest_dir)?;

        let client = reqwest::Client::builder()
            .user_agent(HF_USER_AGENT)
            .build()?;

        // Each shard is (local_filename, download_url).
        let shards = self.list_shards(&client).await?;
        if shards.is_empty() {
            return Err(Error::Recipe(format!(
                "HuggingFace dataset '{}' returned no parquet shards \
                 matching subset {:?}. Check the repo name and subset.",
                self.repo, self.subset
            )));
        }

        let n = shards.len() as u64;
        let mut total_bytes_downloaded: u64 = 0;
        let mut known_total: Option<u64> = None;
        let mut last_report: u64 = 0;

        for (shard_idx, (local_name, download_url)) in shards.iter().enumerate() {
            let final_path = dest_dir.join(local_name);
            let part_path = dest_dir.join(format!("{local_name}.part"));

            // Resume: skip fully-downloaded shards.
            if final_path.exists() {
                let existing = std::fs::metadata(&final_path).map(|m| m.len()).unwrap_or(0);
                total_bytes_downloaded += existing;
                continue;
            }

            let existing_len = if part_path.exists() {
                std::fs::metadata(&part_path).map(|m| m.len()).unwrap_or(0)
            } else {
                0
            };

            let mut request = client.get(download_url.as_str());
            if existing_len > 0 {
                request = request.header("Range", format!("bytes={existing_len}-"));
            }

            let response = request.send().await?;
            if !response.status().is_success() && response.status().as_u16() != 206 {
                return Err(Error::Http(response.error_for_status().unwrap_err()));
            }

            let is_partial = response.status().as_u16() == 206;
            let shard_total =
                response
                    .content_length()
                    .map(|cl| if is_partial { cl + existing_len } else { cl });

            // Extrapolate total size from the first shard's content-length.
            if let Some(sz) = shard_total {
                let remaining = n - shard_idx as u64;
                known_total = Some(total_bytes_downloaded + sz * remaining);
            }

            let mut file = if is_partial {
                std::fs::OpenOptions::new().append(true).open(&part_path)?
            } else {
                std::fs::File::create(&part_path)?
            };

            if is_partial {
                total_bytes_downloaded += existing_len;
            }

            let mut response = response;
            while let Some(chunk) = response.chunk().await? {
                file.write_all(&chunk)?;
                total_bytes_downloaded += chunk.len() as u64;

                if total_bytes_downloaded - last_report >= 1_048_576 {
                    last_report = total_bytes_downloaded;
                    if let Some(ref cb) = progress {
                        let pct = known_total
                            .map(|t| (total_bytes_downloaded as f32 / t as f32) * 100.0)
                            .unwrap_or(0.0);
                        cb(IngestProgress::Downloading {
                            percent: pct.min(99.9),
                            bytes_downloaded: total_bytes_downloaded,
                            bytes_total: known_total,
                        });
                    }
                }
            }

            std::fs::rename(&part_path, &final_path)?;
        }

        if let Some(ref cb) = progress {
            cb(IngestProgress::Downloading {
                percent: 100.0,
                bytes_downloaded: total_bytes_downloaded,
                bytes_total: Some(total_bytes_downloaded),
            });
        }

        Ok(dest_dir)
    }

    /// Return `(local_filename, download_url)` pairs for all parquet shards.
    ///
    /// Two-pass strategy:
    /// 1. Try the siblings API (works for flat repos like `manu/project_gutenberg`).
    /// 2. If no shards found, try the parquet conversion API (works for
    ///    config-based repos like `wikimedia/wikipedia`).
    ///
    /// When `self.file_indices` is set, only those positions in the **sorted**
    /// full shard list are returned.  The sort order is ascending by local
    /// filename, matching what the parquet extractor uses when opening files
    /// from a directory — so both acquirer and extractor agree on which
    /// `file_index` corresponds to which physical file.
    pub(crate) async fn list_shards(
        &self,
        client: &reqwest::Client,
    ) -> Result<Vec<(String, String)>> {
        // ── Pass 1: siblings API ──────────────────────────────────────
        let siblings_shards = self.list_from_siblings(client).await?;
        if !siblings_shards.is_empty() {
            return Ok(self.apply_file_indices(siblings_shards));
        }

        // ── Pass 2: parquet conversion API ───────────────────────────
        if let Some(ref subset) = self.subset {
            match self.list_from_parquet_api(client, subset).await {
                Ok(shards) if !shards.is_empty() => {
                    return Ok(self.apply_file_indices(shards));
                }
                Ok(_) => {}
                Err(e) => tracing::debug!(
                    "Parquet API fallback for '{}' config '{}' failed: {e}",
                    self.repo,
                    subset
                ),
            }
        }

        Ok(Vec::new())
    }

    /// Filter a full sorted shard list to only the positions in `file_indices`.
    ///
    /// The input must already be in the canonical sorted order (ascending by
    /// local filename) so that `file_index` in the manifest matches the same
    /// physical file on both coordinator and peer.
    fn apply_file_indices(&self, mut shards: Vec<(String, String)>) -> Vec<(String, String)> {
        // Sort by local filename first — both acquirer and extractor must use
        // the same canonical ordering so file_index values stay consistent.
        shards.sort_by(|(a, _), (b, _)| a.cmp(b));

        match &self.file_indices {
            None => shards,
            Some(indices) => {
                use std::collections::HashSet;
                let index_set: HashSet<usize> = indices.iter().copied().collect();
                shards
                    .into_iter()
                    .enumerate()
                    .filter(|(i, _)| index_set.contains(i))
                    .map(|(_, s)| s)
                    .collect()
            }
        }
    }

    /// Query `GET /api/datasets/{repo}` and filter `siblings[].rfilename` to
    /// parquet files matching the subset prefix.
    ///
    /// Subset matching (either pattern may apply depending on the repo layout):
    /// - `data/{subset}-*`  e.g. `data/en-00000-of-00052-hash.parquet`
    /// - `{subset}/*`       e.g. `20231101.en/train-00000-of-00041.parquet`
    async fn list_from_siblings(&self, client: &reqwest::Client) -> Result<Vec<(String, String)>> {
        let api_url = format!("https://huggingface.co/api/datasets/{}", self.repo);
        let resp = client.get(&api_url).send().await?;
        if !resp.status().is_success() {
            return Err(Error::Recipe(format!(
                "HuggingFace API returned {} for dataset '{}'. \
                 Verify the repo name is correct and the dataset is public.",
                resp.status(),
                self.repo,
            )));
        }

        let body: serde_json::Value = resp.json().await?;
        let siblings = match body["siblings"].as_array() {
            Some(s) => s,
            None => return Ok(Vec::new()), // config-based dataset — no siblings
        };

        let flat_prefix = self.subset.as_ref().map(|s| format!("data/{}-", s));
        let dir_prefix = self.subset.as_ref().map(|s| format!("{}/", s));

        let shards: Vec<(String, String)> = siblings
            .iter()
            .filter_map(|s| s["rfilename"].as_str())
            .filter(|f| f.ends_with(".parquet"))
            .filter(|f| match (&flat_prefix, &dir_prefix) {
                (Some(fp), Some(dp)) => f.starts_with(fp.as_str()) || f.starts_with(dp.as_str()),
                (None, None) => true,
                _ => false,
            })
            .map(|rfilename| {
                let local = rfilename
                    .rsplit('/')
                    .next()
                    .unwrap_or(rfilename)
                    .to_string();
                let url = format!(
                    "https://huggingface.co/datasets/{}/resolve/main/{}",
                    self.repo, rfilename
                );
                (local, url)
            })
            .collect();

        Ok(shards)
    }

    /// Query `GET /api/datasets/{repo}/parquet/{config}/train` which returns a
    /// JSON array of direct CDN download URLs for config-based datasets.
    async fn list_from_parquet_api(
        &self,
        client: &reqwest::Client,
        config: &str,
    ) -> Result<Vec<(String, String)>> {
        let api_url = format!(
            "https://huggingface.co/api/datasets/{}/parquet/{}/train",
            self.repo, config
        );
        let resp = client.get(&api_url).send().await?;
        if !resp.status().is_success() {
            return Err(Error::Recipe(format!(
                "HuggingFace parquet API returned {} for '{}/{}'. \
                 The config name may be wrong.",
                resp.status(),
                self.repo,
                config,
            )));
        }

        let urls: Vec<String> = resp.json().await.map_err(|e| {
            Error::Recipe(format!(
                "HuggingFace parquet API returned unexpected JSON: {e}"
            ))
        })?;

        let shards: Vec<(String, String)> = urls
            .into_iter()
            .enumerate()
            .map(|(i, url)| {
                // Use the last path component as the local filename, falling
                // back to a zero-padded index if the URL has no clean basename.
                let local = url
                    .split('/')
                    .next_back()
                    .and_then(|s| {
                        // Strip query strings and decode %2F etc.
                        let s = s.split('?').next().unwrap_or(s);
                        if s.ends_with(".parquet") {
                            Some(s.to_string())
                        } else {
                            None
                        }
                    })
                    .unwrap_or_else(|| format!("{i:05}.parquet"));
                (local, url)
            })
            .collect();

        Ok(shards)
    }
}
