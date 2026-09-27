// SPDX-License-Identifier: AGPL-3.0-or-later
//! `pod pool` — the multi-pod verb, split from `mesh_pod.rs` under the
//! file ceiling.

use super::pod;

/// `svrn pipeline pod pool` — multi-pod variant of `pod up`.
///
/// Reads a JSONL manifest of work units, fans them out across `N`
/// Vast pods (round-robin), drains completions to an output file,
/// and (optionally) destroys all pods on completion. One command,
/// full lifecycle — useful for kicking off batch ingest runs from
/// the shell.
///
/// Spec: `sovereign/docs/EPHEMERAL_WORKER_PODS.md` §"Multi-pod jobs".
pub(super) async fn cmd_pod_pool(args: &[String]) -> i32 {
    let mut pod_count: usize = 0;
    let mut gpu_name: String = "L40S".into();
    let mut image: Option<String> = std::env::var("SOVEREIGN_VAST_IMAGE").ok();
    let mut disk_gb: u32 = 80;
    let mut label: Option<String> = None;
    let mut max_price: f64 = 0.80;
    let mut dry_run: bool = false;
    let mut job_id: Option<String> = None;
    let mut manifest_path: Option<std::path::PathBuf> = None;
    let mut output_path: Option<std::path::PathBuf> = None;
    let mut keep_alive: bool = false;
    let mut uploads: Vec<std::path::PathBuf> = Vec::new();
    let mut upload_urls: Vec<(String, String, String)> = Vec::new();
    let mut upload_from_base: Option<String> = None;
    // Token TTL covers the longest plausible owner-side workflow. SEP and
    // wikipedia ingest runs commonly take 30-50h; the prior 12h default
    // expired mid-run, after which every owner request to the pod 401'd
    // with `token expired` while still billing for the GPU. 48h is the
    // largest window that's still bounded for blast-radius purposes (the
    // bootstrap blob is the credential — a leaked blob is good until
    // expiry). Operators on multi-day jobs should pass `--ttl-hours 72`
    // or higher explicitly.
    let mut bootstrap_ttl_hours: u64 = 48;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--pods" => {
                i += 1;
                pod_count = args[i].parse().unwrap_or(0);
            }
            "--gpu" => {
                i += 1;
                gpu_name = args[i].clone();
            }
            "--image" => {
                i += 1;
                image = Some(args[i].clone());
            }
            "--disk" => {
                i += 1;
                disk_gb = args[i].parse().unwrap_or(disk_gb);
            }
            "--label" => {
                i += 1;
                label = Some(args[i].clone());
            }
            "--max-price" => {
                i += 1;
                max_price = args[i].parse().unwrap_or(max_price);
            }
            "--job-id" => {
                i += 1;
                job_id = Some(args[i].clone());
            }
            "--manifest" => {
                i += 1;
                manifest_path = Some(std::path::PathBuf::from(args[i].clone()));
            }
            "--output" => {
                i += 1;
                output_path = Some(std::path::PathBuf::from(args[i].clone()));
            }
            "--keep-alive" => {
                keep_alive = true;
            }
            "--upload" => {
                i += 1;
                uploads.push(std::path::PathBuf::from(args[i].clone()));
            }
            "--upload-from-base-url" => {
                i += 1;
                upload_from_base = Some(args[i].trim_end_matches('/').to_string());
            }
            "--upload-url" => {
                i += 1;
                let raw = args[i].clone();
                let mut parts = raw.splitn(3, '=');
                let name = parts.next().unwrap_or("");
                let sha = parts.next().unwrap_or("");
                let url = parts.next().unwrap_or("");
                if name.is_empty() || sha.len() != 64 || url.is_empty() {
                    eprintln!("--upload-url expects `name=sha256-hex=url`. Got: {raw}");
                    return 2;
                }
                upload_urls.push((name.to_string(), sha.to_string(), url.to_string()));
            }
            "--ttl-hours" => {
                i += 1;
                bootstrap_ttl_hours = args[i].parse().unwrap_or(bootstrap_ttl_hours);
            }
            "--dry-run" => {
                dry_run = true;
            }
            other => {
                eprintln!("unknown flag: {other}");
                eprintln!(
                    "usage: svrn pipeline pod pool \\\n\
                    \x20\x20--pods <N> --manifest <units.jsonl> [--output <results.jsonl>]\\\n\
                    \x20\x20[--gpu <name>] [--image <ref>] [--disk <gb>] [--label <s>]\\\n\
                    \x20\x20[--max-price <usd>] [--job-id <s>] [--keep-alive] \\\n\
                    \x20\x20[--upload <path>]... [--upload-from-base-url <base>]\\\n\
                    \x20\x20[--upload-url <name>=<sha>=<url>]... [--ttl-hours <h>] [--dry-run]\n\
                    \n\
                    Creates N Vast pods in parallel, partitions the manifest \n\
                    round-robin across them, drains completions, and destroys \n\
                    all pods unless --keep-alive is set."
                );
                return 2;
            }
        }
        i += 1;
    }
    if pod_count == 0 {
        eprintln!("--pods <N> is required (N ≥ 1)");
        return 2;
    }
    let Some(image) = image else {
        eprintln!("no container image. Pass `--image <ref>` or set SOVEREIGN_VAST_IMAGE.");
        return 2;
    };
    let Some(manifest_path) = manifest_path else {
        eprintln!(
            "--manifest <units.jsonl> is required (one WorkUnit JSON per line, \
             with unit_id >= 1 — see EPHEMERAL_WORKER_PODS.md)"
        );
        return 2;
    };
    let job_id = job_id.unwrap_or_else(|| {
        format!(
            "pool-{}",
            uuid::Uuid::new_v4()
                .simple()
                .to_string()
                .chars()
                .take(12)
                .collect::<String>()
        )
    });
    let label_value = label.unwrap_or_else(|| format!("{job_id}-pool"));

    // ─── Parse manifest ─────────────────────────────────────────────
    let manifest_bytes = match std::fs::read(&manifest_path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("read manifest {}: {e}", manifest_path.display());
            return 1;
        }
    };
    let manifest_text = match std::str::from_utf8(&manifest_bytes) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("manifest not utf-8: {e}");
            return 1;
        }
    };
    let mut units: Vec<sovereign_pods::worker_http::WorkUnit> = Vec::new();
    for (line_no, line) in manifest_text.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        match serde_json::from_str::<sovereign_pods::worker_http::WorkUnit>(trimmed) {
            Ok(u) => {
                if u.unit_id == 0 {
                    eprintln!(
                        "manifest line {}: unit_id must be >= 1 (cursor uses > since semantics)",
                        line_no + 1
                    );
                    return 2;
                }
                units.push(u);
            }
            Err(e) => {
                eprintln!("manifest line {}: parse error: {e}", line_no + 1);
                return 2;
            }
        }
    }
    if units.is_empty() {
        eprintln!("manifest is empty (no parseable WorkUnit lines)");
        return 2;
    }
    let total_units = units.len();
    if pod_count > total_units {
        eprintln!(
            "warning: --pods {pod_count} > manifest units {total_units}; \
             {} pods will receive no units and stay idle",
            pod_count - total_units
        );
    }

    // ─── Vast offer search — pick top N ─────────────────────────────
    // See `cmd_pod_up` for rationale on dropping `verified=true` from
    // the search query; `reliability>=0.95` is the real quality gate.
    let query = format!(
        "gpu_name={gpu_name} rentable=true cuda_max_good>=12.4 \
         direct_port_count>=2 reliability>=0.95 dph_total<={max_price}"
    );
    let offers = match pod::search_offers(&query, (pod_count as u32).saturating_mul(3).max(50)) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("vastai search failed: {e}");
            return 1;
        }
    };
    if offers.len() < pod_count {
        eprintln!(
            "only {} offers matched query (need {pod_count}): {query}",
            offers.len()
        );
        return 1;
    }
    // Sort by (verified desc, reliability desc, price asc) — same
    // criteria as pick_offer.
    let mut ranked = offers.clone();
    ranked.sort_by(|a, b| {
        b.verified
            .cmp(&a.verified)
            .then(
                b.reliability
                    .partial_cmp(&a.reliability)
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
            .then(
                a.price_per_hour
                    .partial_cmp(&b.price_per_hour)
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
    });
    let chosen: Vec<_> = ranked.into_iter().take(pod_count).collect();
    println!("selected {pod_count} offers:");
    for (i, o) in chosen.iter().enumerate() {
        println!(
            "  pod {i}: id={} gpu={} ${:.3}/hr rel={:.2} loc={}",
            o.id, o.gpu_name, o.price_per_hour, o.reliability, o.geolocation
        );
    }

    // ─── Hash uploads ───────────────────────────────────────────────
    use sha2::{Digest, Sha256};
    use std::collections::BTreeMap;
    let mut upload_specs: BTreeMap<String, sovereign_pods::worker_controller::UploadFile> =
        BTreeMap::new();
    for path in &uploads {
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_string(),
            None => {
                eprintln!(
                    "--upload path has no filename component: {}",
                    path.display()
                );
                return 2;
            }
        };
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("--upload read failed for {}: {e}", path.display());
                return 1;
            }
        };
        let mut h = Sha256::new();
        h.update(&bytes);
        let mut sha = [0u8; 32];
        sha.copy_from_slice(&h.finalize());
        let upload_file = if let Some(base) = upload_from_base.as_ref() {
            let url = format!("{base}/{name}");
            sovereign_pods::worker_controller::UploadFile::fetch_url(url, sha)
        } else {
            sovereign_pods::worker_controller::UploadFile::local(path.clone(), sha)
        };
        upload_specs.insert(name, upload_file);
    }
    for (name, sha_hex, url) in &upload_urls {
        let sha_bytes = match hex::decode(sha_hex) {
            Ok(v) if v.len() == 32 => v,
            _ => {
                eprintln!("--upload-url SHA-256 hex must decode to 32 bytes; got: {sha_hex}");
                return 2;
            }
        };
        let mut sha = [0u8; 32];
        sha.copy_from_slice(&sha_bytes);
        upload_specs.insert(
            name.clone(),
            sovereign_pods::worker_controller::UploadFile::fetch_url(url.clone(), sha),
        );
    }

    if dry_run {
        let total_cost_per_hour: f64 = chosen.iter().map(|o| o.price_per_hour).sum();
        println!(
            "DRY RUN: would create {pod_count} pods (~${:.3}/hr total), \
             upload {} file(s), dispatch {} units, drain completions to {}",
            total_cost_per_hour,
            upload_specs.len(),
            total_units,
            output_path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "<stdout>".into()),
        );
        return 0;
    }

    // ─── Owner key + coordinator ────────────────────────────────────
    let owner_key = match super::provider::load_or_create_owner_key() {
        Ok(k) => k,
        Err(e) => {
            eprintln!(
                "could not load/create owner key at {}: {e}",
                super::provider::owner_key_path().display(),
            );
            return 1;
        }
    };
    let provider = std::sync::Arc::new(super::provider::MultiOfferVastWorkerProvider::new(
        image.clone(),
        disk_gb,
        label_value.clone(),
        chosen,
    ));
    let mut ctrl_config = sovereign_pods::worker_controller::ControllerConfig::default();
    ctrl_config.bootstrap_ttl_seconds = bootstrap_ttl_hours.saturating_mul(3600);
    let coord_config = sovereign_pods::multi_pod_coordinator::CoordinatorConfig::default();
    let coordinator = sovereign_pods::multi_pod_coordinator::MultiPodCoordinator::new(
        provider,
        owner_key,
        ctrl_config,
        coord_config,
    );

    let spec = sovereign_pods::worker_controller::JobSpec {
        job_id: job_id.clone(),
        label: label_value.clone(),
        uploads: upload_specs,
        units,
        runner_config: serde_json::json!({}),
    };

    // ─── Launch ─────────────────────────────────────────────────────
    println!();
    println!("launching {pod_count} pods (this can take a few minutes per pod)…");
    let pool = match coordinator.launch(spec, pod_count).await {
        Ok(p) => p,
        Err(e) => {
            eprintln!("pool launch failed: {e}");
            return 1;
        }
    };
    println!();
    println!("pool live — {pod_count} pods running:");
    for snap in pool.snapshot().await {
        println!(
            "  pod {} vast={} gpu={} ${:.3}/hr addr={} units={}",
            snap.pod_index,
            snap.instance_id,
            snap.gpu_name,
            snap.cost_per_hour,
            snap.worker_address,
            snap.assigned_units
        );
    }

    // ─── Drain ──────────────────────────────────────────────────────
    let output_handle: std::sync::Arc<std::sync::Mutex<Option<std::fs::File>>> =
        std::sync::Arc::new(std::sync::Mutex::new(match &output_path {
            Some(p) => match std::fs::File::create(p) {
                Ok(f) => Some(f),
                Err(e) => {
                    eprintln!("could not open --output {}: {e}", p.display());
                    return 1;
                }
            },
            None => None,
        }));
    let received = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let received_h = received.clone();
    let output_h = output_handle.clone();
    println!();
    println!("draining completions…");
    let summary = match pool
        .poll_until_complete(coordinator.controller(), move |pod_idx, unit| {
            received_h.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let row = serde_json::json!({
                "pod_index": pod_idx,
                "unit": unit,
            });
            let line = serde_json::to_string(&row).unwrap_or_else(|_| "{}".into());
            if let Ok(mut guard) = output_h.lock() {
                if let Some(file) = guard.as_mut() {
                    use std::io::Write;
                    let _ = writeln!(file, "{line}");
                } else {
                    println!("{line}");
                }
            }
        })
        .await
    {
        Ok(s) => s,
        Err(e) => {
            eprintln!("poll failed: {e}");
            if !keep_alive {
                eprintln!("tearing down pool…");
                let _ = pool.destroy_all(coordinator.controller()).await;
            }
            return 1;
        }
    };

    println!();
    println!(
        "drain complete: {} units received in {:.1}s ({} poll errors){}",
        summary.total_received,
        summary.elapsed.as_secs_f64(),
        summary.total_errors,
        if summary.timed_out {
            " [TIMED OUT]"
        } else {
            ""
        },
    );

    // ─── Tear down ──────────────────────────────────────────────────
    if !keep_alive {
        let destroy_results = pool.destroy_all(coordinator.controller()).await;
        let failures = destroy_results.iter().filter(|(_, r)| r.is_err()).count();
        if failures == 0 {
            println!("all {pod_count} pods destroyed.");
        } else {
            eprintln!(
                "{}/{pod_count} pod destroys failed — check `vastai show instances` and \
                 `svrn pipeline pod down <id>` to clean up.",
                failures
            );
            for (i, r) in destroy_results.iter() {
                if let Err(e) = r {
                    eprintln!("  pod {i}: {e}");
                }
            }
        }
    } else {
        println!();
        println!("--keep-alive set: {pod_count} pods left running.");
        for snap in pool.snapshot().await {
            println!("  pod {} vast={}", snap.pod_index, snap.instance_id);
        }
        println!("destroy each with `svrn pipeline pod down <vast-id>`.");
    }
    if summary.timed_out {
        1
    } else {
        0
    }
}
