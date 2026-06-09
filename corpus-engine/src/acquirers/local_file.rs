// SPDX-License-Identifier: AGPL-3.0-or-later
use std::path::{Path, PathBuf};

use crate::error::Result;

/// Acquirer for local files — just validates the path exists.
pub struct LocalFileAcquirer {
    pub path: PathBuf,
}

impl LocalFileAcquirer {
    pub fn new(path: &str) -> Self {
        Self {
            path: PathBuf::from(path),
        }
    }

    pub fn acquire(&self) -> Result<PathBuf> {
        let expanded = expand_tilde(&self.path);
        if !expanded.exists() {
            return Err(crate::error::Error::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("Local source not found: {}", expanded.display()),
            )));
        }
        Ok(expanded)
    }
}

fn expand_tilde(path: &Path) -> PathBuf {
    let s = path.to_string_lossy();
    if s.starts_with("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(format!("{}{}", home, &s[1..]));
        }
    }
    path.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_file_existing() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        std::fs::write(&file, "hello").unwrap();

        let acq = LocalFileAcquirer::new(&file.to_string_lossy());
        let result = acq.acquire().unwrap();
        assert_eq!(result, file);
    }

    #[test]
    fn local_file_missing() {
        let acq = LocalFileAcquirer::new("/nonexistent/path/file.txt");
        assert!(acq.acquire().is_err());
    }
}
