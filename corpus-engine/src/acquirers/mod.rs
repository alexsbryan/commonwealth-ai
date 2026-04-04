pub mod bulk_download;
pub mod local_file;

use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::progress::ProgressCallback;

/// Trait for acquiring source data (downloading, crawling, etc.).
pub trait Acquirer: Send + Sync {
    /// Acquire the source data and return the path to the local file/directory.
    fn acquire(
        &self,
        download_dir: &Path,
        progress: &Option<ProgressCallback>,
    ) -> impl std::future::Future<Output = Result<PathBuf>> + Send;
}
