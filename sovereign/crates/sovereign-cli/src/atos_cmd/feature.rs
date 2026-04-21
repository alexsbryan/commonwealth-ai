//! `sovereign atos feature approve <id>` — Commonwealth-native
//! approval fallback.
//!
//! Records a `FeatureApproval` row in the gossip-replicated KV store
//! so the middleware gate recognizes the feature as approved without
//! requiring a `git commit`. Use when you're prototyping on a branch
//! you don't want to commit to yet, when the approval needs to live
//! off-repo (cross-machine, mesh-replicated), or whenever else the
//! git path doesn't fit your flow.

use super::args::split_args;

pub(super) async fn cmd_feature(args: &[String]) -> i32 {
    let Some(sub) = args.first().cloned() else {
        eprintln!("feature: missing subcommand (approve)");
        return 2;
    };
    let rest = &args[1..];
    match sub.as_str() {
        "approve" => cmd_feature_approve(rest).await,
        other => {
            eprintln!("feature: unknown subcommand '{other}'");
            2
        }
    }
}

async fn cmd_feature_approve(args: &[String]) -> i32 {
    let (positional, _flags) = split_args(args);
    let Some(feature_id) = positional.first().cloned() else {
        eprintln!("feature approve: missing <id>");
        return 2;
    };

    // Repo root = CWD. We intentionally don't walk upward for
    // `.sovereign/` — the operator is expected to run the command
    // from the feature's repo.
    let repo_root = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("feature approve: cwd: {e}");
            return 1;
        }
    };

    let spec_path = sovereign_atos::approval::spec_path(&repo_root, &feature_id);
    if !spec_path.exists() {
        eprintln!("feature approve: spec not found at {}", spec_path.display());
        return 1;
    }

    // Open an in-repo MeshStore on the same path commonwealth-api
    // would use. The file lives at `.sovereign/mesh.db` — we open a
    // dedicated per-repo path so approvals travel with the repo.
    let mesh_path = repo_root.join(".sovereign").join("mesh.db");
    if let Some(parent) = mesh_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mesh = match commonwealth_state::MeshStore::open(&mesh_path) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("feature approve: mesh open: {e}");
            return 1;
        }
    };

    // Node identity. Derive deterministically from the git user
    // identity so repeated `approve` invocations from the same
    // operator produce the same witness id.
    let origin = derive_node_id_from_git(&repo_root).unwrap_or_else(|| {
        commonwealth_core::ids::NodeId::from_u128(0xA7057E07_A7057E07u128)
    });

    match sovereign_atos::approval::record_approval(&mesh, origin, &repo_root, &feature_id) {
        Ok(appr) => {
            println!(
                "approved feature '{}' (hash {}, witness {})",
                appr.feature_id,
                &appr.spec_content_hash[..8],
                &appr.witness[..appr.witness.len().min(16)]
            );
            0
        }
        Err(e) => {
            eprintln!("feature approve: {e}");
            1
        }
    }
}

/// Deterministic NodeId from the operator's git identity. Not
/// cryptographic — we're not defending against impersonation, just
/// producing a stable id without inventing new identity ceremony.
pub(super) fn derive_node_id_from_git(
    repo_root: &std::path::Path,
) -> Option<commonwealth_core::ids::NodeId> {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let name = std::process::Command::new("git")
        .args(["config", "user.name"])
        .current_dir(repo_root)
        .output()
        .ok()?;
    let email = std::process::Command::new("git")
        .args(["config", "user.email"])
        .current_dir(repo_root)
        .output()
        .ok()?;
    if !name.status.success() || !email.status.success() {
        return None;
    }
    let identity = format!(
        "{} <{}>",
        String::from_utf8_lossy(&name.stdout).trim(),
        String::from_utf8_lossy(&email.stdout).trim(),
    );
    let mut h = DefaultHasher::new();
    identity.hash(&mut h);
    let low = h.finish() as u128;
    let mut h2 = DefaultHasher::new();
    (identity.clone() + "-hi").hash(&mut h2);
    let high = h2.finish() as u128;
    Some(commonwealth_core::ids::NodeId::from_u128(
        (high << 64) | low,
    ))
}
