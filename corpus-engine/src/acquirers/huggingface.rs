//! HuggingFace multi-shard dataset acquirer.
//!
//! Calls the HuggingFace dataset API to enumerate parquet shards for a public
//! dataset repo, then downloads each shard with resume support. All shards are
//! written into a local directory; aggregate progress is reported via
//! `IngestProgress::Downloading` across the full download session.

use std::io::Write;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::progress::{IngestProgress, ProgressCallback};

const HF_USER_AGENT: &str = "CorpusEngine/0.1 (+https://sovereign.dev/corpus-engine)";

pub struct HuggingFaceDatasetAcquirer {
    pub repo: String,
    pub subset: Option<String>,
}

impl HuggingFaceDatasetAcquirer {
    pub fn new(repo: &str, subset: Option<&str>) -> Self {
        Self {
            repo: repo.to_string(),
            subset: subset.map(|s| s.to_string()),
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

        // ── 1. Enumerate shards via HuggingFace API ───────────────────
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
        // Running estimate of the full corpus download size. Updated each time
        // we learn a new shard's content-length so progress converges quickly.
        let mut known_total: Option<u64> = None;
        let mut last_report: u64 = 0;

        for (shard_idx, rfilename) in shards.iter().enumerate() {
            // Strip leading directory component for the local filename.
            let local_name = rfilename.rsplit('/').next().unwrap_or(rfilename.as_str());
            let final_path = dest_dir.join(local_name);
            let part_path = dest_dir.join(format!("{local_name}.part"));

            // Resume: fully-downloaded shards count toward aggregate progress.
            if final_path.exists() {
                let existing = std::fs::metadata(&final_path)
                    .map(|m| m.len())
                    .unwrap_or(0);
                total_bytes_downloaded += existing;
                continue;
            }

            let existing_len = if part_path.exists() {
                std::fs::metadata(&part_path).map(|m| m.len()).unwrap_or(0)
            } else {
                0
            };

            let download_url = format!(
                "https://huggingface.co/datasets/{}/resolve/main/{}",
                self.repo, rfilename
            );

            let mut request = client.get(&download_url);
            if existing_len > 0 {
                request = request.header("Range", format!("bytes={existing_len}-"));
            }

            let response = request.send().await?;
            if !response.status().is_success() && response.status().as_u16() != 206 {
                return Err(Error::Http(response.error_for_status().unwrap_err()));
            }

            let is_partial = response.status().as_u16() == 206;
            let shard_total = response.content_length().map(|cl| {
                if is_partial { cl + existing_len } else { cl }
            });

            // Extrapolate total download size from first known shard size.
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

                // Report every 1 MiB of aggregate progress.
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

        // Final 100% report.
        if let Some(ref cb) = progress {
            cb(IngestProgress::Downloading {
                percent: 100.0,
                bytes_downloaded: total_bytes_downloaded,
                bytes_total: Some(total_bytes_downloaded),
            });
        }

        Ok(dest_dir)
    }

    /// Query the HuggingFace dataset API and return all rfilenames that are
    /// parquet files, optionally filtered by the configured subset prefix.
    async fn list_shards(&self, client: &reqwest::Client) -> Result<Vec<String>> {
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
        let siblings = body["siblings"].as_array().ok_or_else(|| {
            Error::Recipe(format!(
                "HuggingFace API response for '{}' has no 'siblings' array. \
                 The API format may have changed.",
                self.repo
            ))
        })?;

        let prefix = self.subset.as_ref().map(|s| format!("data/{}-", s));

        let shards: Vec<String> = siblings
            .iter()
            .filter_map(|s| s["rfilename"].as_str())
            .filter(|f| f.ends_with(".parquet"))
            .filter(|f| match &prefix {
                Some(p) => f.starts_with(p.as_str()),
                None => true,
            })
            .map(|f| f.to_string())
            .collect();

        Ok(shards)
    }
}
