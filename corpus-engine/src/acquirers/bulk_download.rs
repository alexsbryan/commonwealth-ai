use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::progress::{IngestProgress, ProgressCallback};

pub struct BulkDownloader {
    pub url: String,
    pub resume: bool,
}

impl BulkDownloader {
    pub fn new(url: &str, resume: bool) -> Self {
        Self {
            url: url.to_string(),
            resume,
        }
    }

    pub async fn download(
        &self,
        download_dir: &Path,
        corpus_id: &str,
        progress: &Option<ProgressCallback>,
    ) -> Result<PathBuf> {
        std::fs::create_dir_all(download_dir)?;

        let ext = extract_extension(&self.url);
        let part_path = download_dir.join(format!("{corpus_id}.part"));
        let final_path = download_dir.join(format!("{corpus_id}.{ext}"));

        // If already downloaded, return early — but first verify the archive
        // is not truncated.  A crash mid-stream can leave a renamed .zip whose
        // last bytes are mid-data rather than the End-of-Central-Directory
        // record, causing the extractor to fail with "Could not find EOCD".
        // Detect this cheaply (4-byte magic at the expected EOCD offset) and
        // delete the corrupt file so we fall through to a fresh download.
        if final_path.exists() {
            if zip_looks_valid(&final_path) {
                return Ok(final_path);
            }
            tracing::warn!(
                path = %final_path.display(),
                "BulkDownloader: cached archive failed EOCD check — deleting and re-downloading"
            );
            let _ = std::fs::remove_file(&final_path);
        }

        // Check for partial download (resume support).
        let existing_len = if self.resume && part_path.exists() {
            std::fs::metadata(&part_path)
                .map(|m| m.len())
                .unwrap_or(0)
        } else {
            0
        };

        let client = reqwest::Client::builder()
            .user_agent("CorpusEngine/0.1 (+https://sovereign.dev/corpus-engine)")
            .build()?;

        let mut request = client.get(&self.url);
        if existing_len > 0 {
            request = request.header("Range", format!("bytes={existing_len}-"));
        }

        let response = request.send().await?;

        if !response.status().is_success() && response.status().as_u16() != 206 {
            return Err(Error::Http(
                response.error_for_status().unwrap_err(),
            ));
        }

        let should_append = response.status().as_u16() == 206;
        let total_size = response.content_length().map(|cl| {
            if should_append {
                cl + existing_len
            } else {
                cl
            }
        });

        use std::io::Write;
        let mut file = if should_append {
            std::fs::OpenOptions::new()
                .append(true)
                .open(&part_path)?
        } else {
            std::fs::File::create(&part_path)?
        };

        let mut downloaded = if should_append { existing_len } else { 0 };
        let mut last_report = downloaded;

        let mut response = response;
        while let Some(chunk) = response.chunk().await? {
            file.write_all(&chunk)?;
            downloaded += chunk.len() as u64;

            // Report every 1MB.
            if downloaded - last_report >= 1_048_576 {
                last_report = downloaded;
                if let Some(ref cb) = progress {
                    let pct = total_size
                        .map(|t| (downloaded as f32 / t as f32) * 100.0)
                        .unwrap_or(0.0);
                    cb(IngestProgress::Downloading {
                        percent: pct,
                        bytes_downloaded: downloaded,
                        bytes_total: total_size,
                    });
                }
            }
        }

        // Rename .part -> final.
        std::fs::rename(&part_path, &final_path)?;
        Ok(final_path)
    }
}

/// Check whether `path` looks like a structurally-valid ZIP by reading the
/// End-of-Central-Directory signature (`PK\x05\x06`) at the expected offset.
///
/// For ZIP files with no archive comment (the common case for bulk downloads),
/// the EOCD record is exactly the last 22 bytes. We read just those 4 bytes
/// rather than parsing the full directory, so this is an O(1) I/O operation
/// regardless of archive size.
///
/// Returns `false` (treat as corrupt) on any I/O error or if the file is
/// smaller than the minimum valid ZIP size (22 bytes).
fn zip_looks_valid(path: &Path) -> bool {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut f) = std::fs::File::open(path) else {
        return false;
    };
    let Ok(size) = f.seek(SeekFrom::End(0)) else {
        return false;
    };
    if size < 22 {
        return false;
    }
    if f.seek(SeekFrom::End(-22)).is_err() {
        return false;
    }
    let mut sig = [0u8; 4];
    f.read_exact(&mut sig)
        .map(|_| sig == [0x50, 0x4b, 0x05, 0x06])
        .unwrap_or(false)
}

fn extract_extension(url: &str) -> String {
    let path = url.split('?').next().unwrap_or(url);
    let filename = path.rsplit('/').next().unwrap_or("file");
    if filename.ends_with(".xml.bz2") {
        "xml.bz2".to_string()
    } else if filename.ends_with(".tar.gz") {
        "tar.gz".to_string()
    } else {
        filename
            .rsplit('.')
            .next()
            .unwrap_or("dat")
            .to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_extension_works() {
        assert_eq!(extract_extension("https://example.com/dump.xml.bz2"), "xml.bz2");
        assert_eq!(extract_extension("https://example.com/data.tar.gz"), "tar.gz");
        assert_eq!(extract_extension("https://example.com/file.jsonl"), "jsonl");
        assert_eq!(extract_extension("https://example.com/file.jsonl?v=2"), "jsonl");
    }
}
