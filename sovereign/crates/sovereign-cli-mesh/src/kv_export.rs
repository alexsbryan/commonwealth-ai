// SPDX-License-Identifier: AGPL-3.0-or-later
//! `sovereign-cli-mesh kv-export <file> <app-id>...` — the READ half of a
//! legacy SQLite store's one-shot migration (five-programs fp-87, §12 D4).
//!
//! It reads the file through commonwealth-state's own backend (one schema
//! decider) and prints the rows as a JSON array of `ReplicatedKvEntry`, the
//! `/v1/mesh/kv` wire form. The WRITE half is `sovereign-cli-llm`'s, through
//! the one `RailsKv` client: this package may not name it, and a second client
//! of cw-rails' doors is what fp-87 forbids.
//!
//! A missing file is a named refusal, never an empty export — `MeshStore::open`
//! would create one.

use std::path::Path;
use std::sync::Arc;

use commonwealth_state::MeshStore;
use sovereign_contracts::peer::{ReplicatedKv, ReplicatedKvEntry};
use sovereign_mesh::peer_adapter::MeshReplicatedKv;

/// Every row of `app_ids` in the legacy store at `path`.
pub fn export(path: &Path, app_ids: &[String]) -> Result<Vec<ReplicatedKvEntry>, String> {
    if !path.is_file() {
        tracing::warn!(path = %path.display(), "kv-export: no legacy store");
        return Err(format!("no legacy store at {}", path.display()));
    }
    let store =
        MeshStore::open(path).map_err(|e| format!("open legacy store {}: {e}", path.display()))?;
    let kv = MeshReplicatedKv::over(Arc::new(store));
    let mut rows = Vec::new();
    for app_id in app_ids {
        let found = kv
            .scan(app_id, "")
            .map_err(|e| format!("scan {app_id} in {}: {e}", path.display()))?;
        tracing::debug!(path = %path.display(), app_id, rows = found.len(), "kv-export: scanned");
        rows.extend(found);
    }
    Ok(rows)
}

pub async fn run(args: &[String]) -> i32 {
    let Some((path, app_ids)) = args.split_first() else {
        eprintln!("usage: sovereign-cli-mesh kv-export <file> <app-id>...");
        return 2;
    };
    match export(Path::new(path), app_ids) {
        Ok(rows) => match serde_json::to_string(&rows) {
            Ok(json) => {
                println!("{json}");
                0
            }
            Err(e) => {
                eprintln!("kv-export: encode: {e}");
                1
            }
        },
        Err(e) => {
            eprintln!("kv-export: {e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use commonwealth_core::ids::NodeId;

    #[test]
    fn a_legacy_file_exports_its_rows_for_the_named_app_ids_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("portfolio.db");
        let store = MeshStore::open(&path).unwrap();
        let me = NodeId::from_u128(7);
        store
            .set(
                "portfolio-private",
                "tech",
                Bytes::from_static(b"[\"a\"]"),
                me,
            )
            .unwrap();
        store
            .set("other", "x", Bytes::from_static(b"1"), me)
            .unwrap();
        drop(store);

        let rows = export(&path, &["portfolio-private".to_string()]).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].app_id, "portfolio-private");
        assert_eq!(rows[0].key, "tech");
        assert_eq!(rows[0].value, Bytes::from_static(b"[\"a\"]"));
        assert_eq!(rows[0].origin, me);
    }

    #[test]
    fn a_missing_file_is_refused_and_not_created() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("absent.db");
        let err = export(&path, &["portfolio-private".to_string()]).unwrap_err();
        assert!(err.contains("no legacy store"), "{err}");
        assert!(!path.exists());
    }
}
