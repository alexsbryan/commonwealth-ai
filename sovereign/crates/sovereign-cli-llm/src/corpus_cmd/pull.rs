// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn corpus pull` — svrn's member act of pulling a peer's canonical
//! index (HUMAN-fp7 (a)). It stays in svrn when the rest of `corpus`
//! moves to ingest's CLI (pb-cli-llm-ingest-move); pb-mesh-dissolve
//! repoints its `sovereign_mesh` call.

use sovereign_cli_base::units::human_bytes;

/// `svrn corpus pull <id> [--from <peer-url>] [--expected-fingerprint <hex>]`
///
/// Stream a peer's canonical index over HTTP, validate the
/// content fingerprint, and atomically rename it into place at
/// `<index_dir>/<id>/`. Refuses if a canonical already exists at
/// the destination — the user must explicitly remove it first
/// (`svrn corpus remove <id> --canonical-only --yes`).
///
/// `--from <peer-url>` supplies the peer's mesh API base URL
/// (e.g. `http://100.64.0.2:9742`). Required for v1 — peer
/// auto-discovery from gossip lands in the auto_recover follow-
/// up commit. `--expected-fingerprint <hex>` adds a pre-flight
/// validation: the puller refuses if the peer's advertised
/// fingerprint doesn't match the expected value (used by the
/// auto-recover path to pin the source it chose from gossip).
///
/// On success, reports throughput + the fingerprint that's now
/// stamped on the local canonical. The on-disk meta carries the
/// original peer's fingerprint verbatim; the next daemon round
/// will pick the canonical up via `installed_indexes()` and
/// publish it onto our own gossip slot.
pub(super) async fn cmd_corpus_pull(args: &[String]) -> i32 {
    let mut corpus_id: Option<String> = None;
    let mut peer_url: Option<String> = None;
    let mut expected_fingerprint: Option<String> = None;

    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--from" => {
                let Some(val) = iter.next() else {
                    eprintln!("--from requires a peer URL (e.g. http://100.64.0.2:9742)");
                    return 1;
                };
                peer_url = Some(val.clone());
            }
            "--expected-fingerprint" => {
                let Some(val) = iter.next() else {
                    eprintln!("--expected-fingerprint requires a hex value");
                    return 1;
                };
                expected_fingerprint = Some(val.clone());
            }
            "--help" | "-h" => {
                println!(
                    "Usage: svrn corpus pull <corpus_id> --from <peer-url> \
                     [--expected-fingerprint <hex>]\n\n\
                     Stream a peer's canonical index over the mesh and atomically \
                     install it locally.\n\n\
                     Refuses when a canonical already exists at \
                     <data_dir>/indexes/<corpus_id>/. Run \
                     `svrn corpus remove <id> --canonical-only --yes` first.\n\n\
                     The peer URL is the mesh API base (port 9742). The \
                     X-Canonical-Fingerprint header on the response is \
                     validated against --expected-fingerprint (if given) AND \
                     against the recomputed fingerprint of the unpacked \
                     canonical. A mismatch wipes the temp dir and errors out \
                     — no partial canonical is left behind."
                );
                return 0;
            }
            other if !other.starts_with('-') => {
                if corpus_id.is_none() {
                    corpus_id = Some(other.to_string());
                }
            }
            other => {
                eprintln!("Unknown flag: {other}");
                return 1;
            }
        }
    }

    let Some(corpus_id) = corpus_id else {
        eprintln!("Missing corpus ID. Usage: svrn corpus pull <corpus_id> --from <peer-url>");
        return 1;
    };
    let Some(peer_url) = peer_url else {
        eprintln!(
            "Missing --from <peer-url>. Auto-discovery from gossip is a \
             follow-up commit; for now pass the peer's mesh API URL \
             explicitly (e.g. http://100.64.0.2:9742)."
        );
        return 1;
    };

    let data_dir = sovereign_contracts::setup_config::SetupConfig::load()
        .map(|cfg| cfg.data.dir)
        .unwrap_or_else(|_| sovereign_contracts::rebrand::svrnmesh_root());
    let index_dir = data_dir.join("indexes");

    println!("Pulling canonical for '{corpus_id}' from {peer_url}…");
    println!("(streaming tar.zst → unpack → fingerprint validate → atomic rename)");
    println!();

    let started = std::time::Instant::now();
    // CLI path is single-target — the operator gave us one URL.
    // Wrap in a single-element slice so the function signature
    // (which loops over candidates for the auto-pull path) sees
    // exactly the one address the user wants to try.
    let candidates = vec![peer_url.clone()];
    match sovereign_mesh::canonical_pull::pull_canonical_from_peer(
        &candidates,
        &corpus_id,
        &index_dir,
        expected_fingerprint.as_deref(),
        // Unstamped: this is a person naming a peer's URL, not a member
        // dialing one. The CLI holds no `Mesh`, so it has nothing to mint a
        // proof from — and against a peer running the default
        // `internal_auth = "member"` this pull is refused unless it is
        // loopback. Reported here rather than papered over: the fix is for the
        // CLI to ask its own daemon to pull, not for this file to forge a
        // credential it does not hold.
        None,
    )
    .await
    {
        Ok(report) => {
            let elapsed = started.elapsed();
            let mb_per_sec = if elapsed.as_secs_f64() > 0.0 {
                (report.bytes_uncompressed as f64 / elapsed.as_secs_f64()) / 1_048_576.0
            } else {
                0.0
            };
            println!("✓ pulled {corpus_id}");
            println!("  fingerprint:        {}", report.fingerprint);
            println!(
                "  uncompressed bytes: {}",
                human_bytes(report.bytes_uncompressed)
            );
            println!(
                "  elapsed:            {}m{}s ({:.1} MB/s uncompressed)",
                elapsed.as_secs() / 60,
                elapsed.as_secs() % 60,
                mb_per_sec,
            );
            println!("  canonical at:       {}", report.canonical_path.display());
            0
        }
        Err(e) => {
            eprintln!("✗ pull failed: {e}");
            1
        }
    }
}
