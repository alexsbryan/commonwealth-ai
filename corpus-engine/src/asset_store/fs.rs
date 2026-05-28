//! Filesystem-backed [`AssetStore`] impl (AD-1).

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use sha2::{Digest, Sha256};

use super::ledger::{self, LedgerEntry, LedgerReader};
use super::{AssetReceipt, AssetStore};
use crate::error::{Error, Result};

/// Filesystem-backed content-addressed asset store.
///
/// Layout (see [`super`] docs for the spec):
/// ```text
/// <root>/
///   ledger.jsonl
///   <hh>/<sha256>           # raw bytes, sharded by leading two hex
///   parsed/<sha256>.<ext>   # optional typed cache
/// ```
///
/// The two-character shard prefix keeps any single directory under
/// ~30k entries on a million-asset corpus — both `ls` and the
/// filesystem's own directory-block storage stay healthy.
pub struct FilesystemAssetStore {
    root: PathBuf,
    /// In-memory de-dup index built lazily from the ledger on first
    /// observation. `Mutex` over `Option<HashSet>` so the first writer
    /// pays the rebuild cost; subsequent writers see the cached set
    /// and avoid the disk round-trip.
    seen: Mutex<Option<std::collections::HashMap<String, LedgerEntry>>>,
}

impl FilesystemAssetStore {
    /// Create or open an asset store rooted at `dir`. The directory
    /// (and `parsed/` subdirectory) is created on demand.
    pub fn new(dir: impl AsRef<Path>) -> Result<Self> {
        let root = dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&root).map_err(Error::Io)?;
        std::fs::create_dir_all(root.join("parsed")).map_err(Error::Io)?;
        Ok(Self {
            root,
            seen: Mutex::new(None),
        })
    }

    fn ledger_path(&self) -> PathBuf {
        self.root.join("ledger.jsonl")
    }

    fn ensure_index_loaded(&self) -> Result<()> {
        let mut guard = self
            .seen
            .lock()
            .expect("FilesystemAssetStore.seen poisoned");
        if guard.is_some() {
            return Ok(());
        }
        let entries = LedgerReader::new(self.ledger_path()).read_all()?;
        let mut map = std::collections::HashMap::with_capacity(entries.len());
        for e in entries {
            map.insert(e.sha256.clone(), e);
        }
        *guard = Some(map);
        Ok(())
    }

    fn shard_dir(&self, sha256: &str) -> PathBuf {
        let prefix = sha256.get(0..2).unwrap_or("__");
        self.root.join(prefix)
    }
}

impl AssetStore for FilesystemAssetStore {
    fn put_raw(
        &self,
        bytes: &[u8],
        original_filename: Option<&str>,
        mime: Option<&str>,
        source_doc_id: &str,
    ) -> Result<AssetReceipt> {
        let sha256 = hex_sha256(bytes);
        let raw_path = self.raw_path(&sha256);

        self.ensure_index_loaded()?;
        let already_seen = {
            let guard = self
                .seen
                .lock()
                .expect("FilesystemAssetStore.seen poisoned");
            guard
                .as_ref()
                .expect("seen index loaded")
                .contains_key(&sha256)
        };

        if !already_seen {
            std::fs::create_dir_all(self.shard_dir(&sha256)).map_err(Error::Io)?;
            // Write to a `.partial` first so a torn write does not
            // leave half a payload at the canonical path. atomic rename.
            let partial = raw_path.with_extension("partial");
            std::fs::write(&partial, bytes).map_err(Error::Io)?;
            std::fs::rename(&partial, &raw_path).map_err(Error::Io)?;

            let entry = LedgerEntry {
                sha256: sha256.clone(),
                original_filename: original_filename.map(|s| s.to_string()),
                mime: mime.map(|s| s.to_string()),
                size: bytes.len() as u64,
                first_seen_source_doc_id: source_doc_id.to_string(),
                first_seen_ts: now_secs(),
                parsed_form: None,
            };
            ledger::append(&self.ledger_path(), &entry)?;
            let mut guard = self
                .seen
                .lock()
                .expect("FilesystemAssetStore.seen poisoned");
            guard
                .as_mut()
                .expect("seen index loaded")
                .insert(sha256.clone(), entry);
        }

        Ok(AssetReceipt {
            sha256,
            raw_path,
            size: bytes.len() as u64,
            newly_stored: !already_seen,
        })
    }

    fn put_parsed(&self, sha256: &str, ext: &str, bytes: &[u8]) -> Result<PathBuf> {
        let parsed_dir = self.root.join("parsed");
        std::fs::create_dir_all(&parsed_dir).map_err(Error::Io)?;
        let path = parsed_dir.join(format!("{sha256}.{ext}"));
        let partial = path.with_extension(format!("{ext}.partial"));
        std::fs::write(&partial, bytes).map_err(Error::Io)?;
        std::fs::rename(&partial, &path).map_err(Error::Io)?;
        Ok(path)
    }

    fn record_parsed_form(&self, sha256: &str, parsed_path: &Path) -> Result<()> {
        // Rewrite-the-ledger-with-an-additional-line strategy: append
        // a new entry that supersedes the older one on read (a
        // ledger reader that reduces duplicates picks the latest).
        // This keeps the ledger append-only and crash-safe.
        self.ensure_index_loaded()?;
        let mut existing = {
            let guard = self
                .seen
                .lock()
                .expect("FilesystemAssetStore.seen poisoned");
            guard
                .as_ref()
                .expect("seen index loaded")
                .get(sha256)
                .cloned()
        };
        let entry = match existing.take() {
            Some(mut e) => {
                e.parsed_form = Some(parsed_path.to_path_buf());
                e
            }
            None => {
                return Err(Error::Extraction(format!(
                    "asset_store: cannot record parsed form — {sha256} not in ledger"
                )));
            }
        };
        ledger::append(&self.ledger_path(), &entry)?;
        let mut guard = self
            .seen
            .lock()
            .expect("FilesystemAssetStore.seen poisoned");
        guard
            .as_mut()
            .expect("seen index loaded")
            .insert(sha256.to_string(), entry);
        Ok(())
    }

    fn lookup(&self, sha256: &str) -> Result<Option<LedgerEntry>> {
        self.ensure_index_loaded()?;
        let guard = self
            .seen
            .lock()
            .expect("FilesystemAssetStore.seen poisoned");
        Ok(guard
            .as_ref()
            .expect("seen index loaded")
            .get(sha256)
            .cloned())
    }

    fn entries(&self) -> Result<Vec<LedgerEntry>> {
        self.ensure_index_loaded()?;
        let guard = self
            .seen
            .lock()
            .expect("FilesystemAssetStore.seen poisoned");
        let mut out: Vec<LedgerEntry> = guard
            .as_ref()
            .expect("seen index loaded")
            .values()
            .cloned()
            .collect();
        out.sort_by(|a, b| a.sha256.cmp(&b.sha256));
        Ok(out)
    }

    fn raw_path(&self, sha256: &str) -> PathBuf {
        self.shard_dir(sha256).join(sha256)
    }

    fn root(&self) -> &Path {
        &self.root
    }
}

fn hex_sha256(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    let digest = h.finalize();
    let mut s = String::with_capacity(64);
    for b in digest {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_path_is_sharded_by_sha_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let store = FilesystemAssetStore::new(dir.path()).unwrap();
        let receipt = store.put_raw(b"hello", None, None, "doc-1").unwrap();
        let prefix = &receipt.sha256[..2];
        assert_eq!(receipt.raw_path.parent().unwrap(), dir.path().join(prefix));
        assert!(receipt.raw_path.exists());
    }

    #[test]
    fn record_parsed_form_appears_in_lookup() {
        let dir = tempfile::tempdir().unwrap();
        let store = FilesystemAssetStore::new(dir.path()).unwrap();
        let r = store.put_raw(b"x", None, None, "d").unwrap();
        let p = store.put_parsed(&r.sha256, "parquet", b"PARQ").unwrap();
        store.record_parsed_form(&r.sha256, &p).unwrap();
        let entry = store.lookup(&r.sha256).unwrap().unwrap();
        assert_eq!(entry.parsed_form, Some(p));
    }

    #[test]
    fn record_parsed_form_unknown_sha_errors() {
        let dir = tempfile::tempdir().unwrap();
        let store = FilesystemAssetStore::new(dir.path()).unwrap();
        let err = store
            .record_parsed_form("nope", &dir.path().join("p.parquet"))
            .unwrap_err();
        assert!(err.to_string().contains("not in ledger"), "{err}");
    }
}
