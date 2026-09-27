// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn mesh pod …` — rent Vast.ai GPU pods as ephemeral workers.
//! Lifted out of `sovereign-cli-llm`'s `pipeline pod` with its Vast
//! shell-outs and cost ledger (phase-b-1 (10)).

pub mod ledger;
pub mod pod;
mod pool;
mod provider;

use pool::cmd_pod_pool;
use sovereign_cli_base::help::{self, Help, HelpSection};

/// `--help` anywhere in the argv prints this and exits 0, as
/// `svrn pipeline --help` did for these rows before the move.
const HELP_POD: Help = Help {
    command: "svrn mesh pod",
    summary: "Rent Vast.ai GPU pods as ephemeral workers, with a local cost ledger.",
    sections: &[
        HelpSection::Usage("svrn mesh pod <up | pool | list | down> [flags]"),
        HelpSection::Subcommands(&[
            (
                "up",
                "Launch a Vast.ai pod with the sovereign CUDA image, join the mesh, \
                 register in the cost ledger.",
            ),
            (
                "pool",
                "Fan a JSONL manifest of work units out across N pods, drain \
                 completions, destroy the pods unless --keep-alive.",
            ),
            (
                "list",
                "Show every pod the ledger knows about with accrued cost.",
            ),
            (
                "down <vast-id>",
                "Destroy a Vast pod, close its ledger entry, print final cost.",
            ),
        ]),
    ],
};

pub async fn cmd_pod(args: &[String]) -> i32 {
    if help::wants_help(args) {
        help::print(&HELP_POD);
        return 0;
    }
    if args.is_empty() {
        eprintln!("usage: svrn mesh pod <up | pool | list | down> [flags]");
        return 2;
    }
    match args[0].as_str() {
        "up" => cmd_pod_up(&args[1..]).await,
        "pool" => cmd_pod_pool(&args[1..]).await,
        "list" => cmd_pod_list(&args[1..]),
        "down" => cmd_pod_down(&args[1..]),
        other => {
            eprintln!("unknown pod subcommand: {other}");
            2
        }
    }
}

async fn cmd_pod_up(args: &[String]) -> i32 {
    // EPHEMERAL_WORKER_PODS.md path. The pod boots in worker mode,
    // owned by exactly this CLI invocation for the lifetime of the
    // job. No Tailscale, no R2, no mesh join.
    //
    // The MVP performs: search → create → wait-for-address →
    // wait-for-health → uploads (if any) → print handle. The pod is
    // left running for follow-up dispatch. `mesh pod down` (or
    // SIGINT here) tears it down.
    let mut gpu_name: String = "L40S".into();
    let mut image: Option<String> = std::env::var("SOVEREIGN_VAST_IMAGE").ok();
    let mut disk_gb: u32 = 80;
    let mut label: Option<String> = None;
    let mut max_price: f64 = 0.80;
    let mut num_gpus: Option<u32> = None;
    let mut dry_run: bool = false;
    let mut job_id: Option<String> = None;
    let mut uploads: Vec<std::path::PathBuf> = Vec::new();
    // Each entry is (name, sha256-hex, url) — the pod will fetch the
    // URL itself instead of waiting for an owner upload. Right for
    // GGUFs staged in R2/B2/S3 with multi-Gbps egress.
    let mut upload_urls: Vec<(String, String, String)> = Vec::new();
    // When set, every `--upload <path>` flag is translated into a
    // URL-backed entry at `<base>/<filename>`. The local file is
    // read only to compute SHA-256 — the pod fetches bytes from the
    // URL. This is the ergonomic primitive for the common case
    // where every model is staged in one R2/B2 bucket.
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
            "--num-gpus" => {
                i += 1;
                num_gpus = args[i].parse().ok();
            }
            "--job-id" => {
                i += 1;
                job_id = Some(args[i].clone());
            }
            "--upload" => {
                i += 1;
                uploads.push(std::path::PathBuf::from(args[i].clone()));
            }
            "--upload-from-base-url" => {
                i += 1;
                // Strip trailing slash so we can paste either with or
                // without — `<base>/<filename>` is always a clean join.
                upload_from_base = Some(args[i].trim_end_matches('/').to_string());
            }
            "--upload-url" => {
                i += 1;
                // Format: name=sha256-hex=url. Three '=' separators
                // are unambiguous because SHA-256 hex is fixed 64
                // chars and contains no '=', and presigned URLs have
                // their own '=' inside the query string — so we
                // splitn(3, '=') instead of a naive split.
                let raw = args[i].clone();
                let mut parts = raw.splitn(3, '=');
                let name = parts.next().unwrap_or("");
                let sha = parts.next().unwrap_or("");
                let url = parts.next().unwrap_or("");
                if name.is_empty() || sha.len() != 64 || url.is_empty() {
                    eprintln!(
                        "--upload-url expects `name=sha256-hex=url`. Got: {raw}\n\
                         (sha256 hex must be exactly 64 chars; name and url non-empty.)"
                    );
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
                    "usage: svrn mesh pod up \\\n\
                    \x20\x20[--gpu <name>] [--image <ref>] [--disk <gb>] [--label <s>]\\\n\
                    \x20\x20[--max-price <usd>] [--job-id <s>] \\\n\
                    \x20\x20[--upload <path>]... [--upload-url <name>=<sha256-hex>=<url>]... \\\n\
                    \x20\x20[--upload-from-base-url <base-url>] \\\n\
                    \x20\x20[--ttl-hours <h>] [--dry-run]\n\
                    \n\
                    --upload streams the file from your laptop (slow over residential\n\
                    upload). --upload-url has the pod fetch the file itself from\n\
                    R2/B2/S3 at data-center speeds — paste a presigned URL with a\n\
                    short TTL.\n\
                    \n\
                    --upload-from-base-url <base> is the ergonomic shortcut: each\n\
                    --upload <local-path> becomes a URL-backed entry at\n\
                    `<base>/<filename>` with SHA computed from the local copy. Right\n\
                    for the common case where every model is staged in one R2 bucket.\n\
                    \n\
                    SHA validation is owner-signed in every case."
                );
                return 2;
            }
        }
        i += 1;
    }

    let Some(image) = image else {
        eprintln!(
            "no container image. Pass `--image <ref>` or set SOVEREIGN_VAST_IMAGE.\n\
             Example: --image ghcr.io/<you>/sovereign-cuda:latest"
        );
        return 2;
    };
    let job_id = job_id.unwrap_or_else(|| {
        format!(
            "job-{}",
            uuid::Uuid::new_v4()
                .simple()
                .to_string()
                .chars()
                .take(12)
                .collect::<String>()
        )
    });
    // ─── Vast offer search ─────────────────────────────────────────
    // CUDA ≥ 12.4 so the image's runtime matches. `direct_port_count>=2`
    // ensures the pod will get a public host port for the worker daemon
    // on :9742 (see EPHEMERAL_WORKER_PODS.md §"Provider connectivity
    // audit"). `reliability>=0.95` is the real quality filter — Vast's
    // `verified=true` is a much narrower premium-host program (often
    // zero offers in our price band when checked 2026-05-18). We rank
    // verified higher inside `pick_offer`, so dropping the search-side
    // gate just widens the candidate pool when verified hosts are
    // unavailable.
    let num_gpus_clause = num_gpus
        .map(|n| format!(" num_gpus={n}"))
        .unwrap_or_default();
    let query = format!(
        "gpu_name={gpu_name} rentable=true cuda_max_good>=12.4 \
         direct_port_count>=2 reliability>=0.95 dph_total<={max_price}{num_gpus_clause}"
    );
    let offers = match pod::search_offers(&query, 50) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("vastai search failed: {e}");
            return 1;
        }
    };
    let pick = match pod::pick_offer(&offers).cloned() {
        Some(p) => p,
        None => {
            eprintln!("no offers matched: {query}");
            return 1;
        }
    };
    println!(
        "selected offer: id={} gpu={} ${:.3}/hr rel={:.2} verified={} loc={}",
        pick.id,
        pick.gpu_name,
        pick.price_per_hour,
        pick.reliability,
        pick.verified,
        pick.geolocation,
    );

    // ─── Hash uploads up front ─────────────────────────────────────
    // The bootstrap blob carries a `filename → SHA-256` manifest the
    // pod uses to validate streamed uploads. We compute SHAs locally
    // before paying for the pod so a missing file aborts cheaply.
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
        // If --upload-from-base-url is set, the local file is just a
        // SHA source — the pod fetches bytes from <base>/<filename>
        // instead. This is the right primitive for "all my models are
        // already staged in one R2 bucket".
        let upload_file = if let Some(base) = upload_from_base.as_ref() {
            let url = format!("{base}/{name}");
            sovereign_pods::worker_controller::UploadFile::fetch_url(url, sha)
        } else {
            sovereign_pods::worker_controller::UploadFile::local(path.clone(), sha)
        };
        upload_specs.insert(name, upload_file);
    }
    // URL-backed entries — pod fetches itself.
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

    let label_value = label.unwrap_or_else(|| format!("{job_id}-pod"));

    if dry_run {
        println!(
            "DRY RUN: would create instance via vastai create instance {} \
             --image {} --disk {} --label {} --ssh, then mint a worker bootstrap blob \
             with {} upload(s), upload to :9742 over TLS-pinned reqwest.",
            pick.id,
            image,
            disk_gb,
            label_value,
            upload_specs.len(),
        );
        return 0;
    }

    // ─── Owner key + controller ─────────────────────────────────────
    let owner_key = match provider::load_or_create_owner_key() {
        Ok(k) => k,
        Err(e) => {
            eprintln!(
                "could not load/create owner key at {}: {e}",
                provider::owner_key_path().display(),
            );
            return 1;
        }
    };
    let provider = std::sync::Arc::new(provider::VastWorkerProvider::new(
        image.clone(),
        disk_gb,
        label_value.clone(),
        pick.clone(),
    ));
    let mut ctrl_config = sovereign_pods::worker_controller::ControllerConfig::default();
    ctrl_config.bootstrap_ttl_seconds = bootstrap_ttl_hours.saturating_mul(3600);
    let controller =
        sovereign_pods::worker_controller::WorkerController::new(provider, owner_key, ctrl_config);

    // ─── JobSpec ────────────────────────────────────────────────────
    // No units list yet — `pod up` boots the pod and leaves it ready
    // for follow-up dispatch. A future `mesh pod dispatch <handle>
    // <manifest.json>` command will POST the units to the worker.
    let spec = sovereign_pods::worker_controller::JobSpec {
        job_id: job_id.clone(),
        label: label_value.clone(),
        uploads: upload_specs,
        units: Vec::new(),
        runner_config: serde_json::json!({}),
    };

    // create_and_run_with_blob mints the blob, calls vastai create,
    // polls for address, waits for health, uploads files. It does
    // NOT dispatch a job (units is empty); the pod stays in
    // "uploads ready" state for follow-up commands. The `_with_blob`
    // variant also yields the bootstrap blob so we can persist a
    // pinned-pod snapshot for the inference scheduler.
    let (handle, instance, blob, _client) = match controller.create_and_run_with_blob(&spec).await {
        Ok(t) => t,
        Err(e) => {
            eprintln!("pod up failed: {e}");
            // Best-effort cleanup: if `instance_id` was created but
            // the rest of the lifecycle failed, the operator can run
            // `vastai destroy instance <id>` manually. Surfacing the
            // error is more important than guessing the instance id
            // here.
            return 1;
        }
    };

    // Persist a pinned-pod snapshot so the daemon's inference
    // scheduler picks the pod up on next startup (or via the
    // `--extra-worker` flag). Failure to write the snapshot is
    // non-fatal — the pod still runs; the operator just won't get
    // automatic inference routing to it until they re-run pod up
    // or write the file themselves.
    // Spec: docs/PINNED_WORKER_AS_INFERENCE_PEER.md §3.6.
    // Capture token expiry before `blob` is moved into the snapshot —
    // we re-print it at the end of this command for operator visibility.
    let expires_unix = blob.expires_unix;
    if let Some(dir) = sovereign_mesh::pinned_pod_snapshot::default_snapshot_dir() {
        let capabilities = capabilities_for_gpu(&instance.gpu_name);
        let snapshot = sovereign_mesh::pinned_pod_snapshot::PinnedPodSnapshot::new(
            instance.instance_id.clone(),
            handle.host(),
            handle.port(),
            blob,
            capabilities,
        );
        match sovereign_mesh::pinned_pod_snapshot::save_snapshot(&dir, &snapshot) {
            Ok(p) => println!(
                "wrote snapshot at {} (inference routing enabled)",
                p.display()
            ),
            Err(e) => {
                eprintln!("warning: snapshot write failed ({e}) — inference routing disabled")
            }
        }
    }

    let rec = ledger::PodRecord {
        vast_id: instance.instance_id.clone(),
        label: label_value,
        recipe_id: job_id.clone(),
        gpu_name: instance.gpu_name.clone(),
        image: image.clone(),
        started_at: ledger::unix_now(),
        ended_at: None,
        cost_per_hour: instance.cost_per_hour,
        status: ledger::PodStatus::Running,
    };
    if let Err(e) = ledger::append(ledger::default_path(), rec) {
        eprintln!(
            "WARNING: pod {} launched but ledger append failed: {e}\n\
             Track manually until `pod list` resolves.",
            instance.instance_id
        );
    }

    // Surface the token expiry prominently. The 2026-05-18 SEP-on-Vast
    // run wedged silently when a 12h token expired mid-job — the operator
    // had no signal that auth was about to break beyond a buried JSON
    // field in `~/.svrnmesh/worker-pods/<id>.json`. Print the expiry
    // time + remaining hours alongside the rest of the launch summary.
    // `expires_unix` was captured above before `blob` was moved into
    // the snapshot.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let ttl_remaining_h = expires_unix.saturating_sub(now) as f64 / 3600.0;
    let expires_display = chrono::DateTime::<chrono::Utc>::from(
        std::time::UNIX_EPOCH + std::time::Duration::from_secs(expires_unix),
    )
    .format("%Y-%m-%d %H:%M:%S UTC");

    println!();
    println!("worker pod ready:");
    println!("  vast id          {}", instance.instance_id);
    println!("  job id           {job_id}");
    println!("  $/hr             {:.3}", instance.cost_per_hour);
    println!("  worker address   {}", handle.base_url());
    println!(
        "  pinned thumbprint {}",
        hex::encode(handle.pod_pubkey_thumbprint())
    );
    println!("  uploads          {}", spec.uploads.len());
    println!(
        "  token expires    {expires_display}  (in {ttl_remaining_h:.1}h — \
         re-launch with --ttl-hours <N> if your job runs longer)"
    );
    println!();
    println!("Pod is in 'uploads ready' state. Dispatch a job with the worker token:");
    println!("  (token printed once — keep it; future invocations will be `mesh pod dispatch <vast-id>`)");
    println!("  token: {}", handle.worker_token());
    println!();
    println!("Tear down with:");
    println!("  svrn mesh pod down {}", instance.instance_id);
    0
}

fn cmd_pod_list(_args: &[String]) -> i32 {
    let path = ledger::default_path();
    let pods = match ledger::read(&path) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("ledger read failed: {e}");
            return 1;
        }
    };
    if pods.is_empty() {
        println!("(no pods in {})", path.display());
        return 0;
    }
    println!(
        "{:<10} {:<6} {:<22} {:<12} {:>8} {:>10}  started_at",
        "vast_id", "state", "label", "gpu", "$/hr", "accrued"
    );
    let mut running_total = 0.0;
    for p in &pods {
        let cost = ledger::accrued_cost(p);
        let state = match p.status {
            ledger::PodStatus::Running => "live",
            ledger::PodStatus::Closed => "down",
        };
        let started = chrono::DateTime::<chrono::Local>::from(
            std::time::UNIX_EPOCH + std::time::Duration::from_secs(p.started_at as u64),
        );
        println!(
            "{:<10} {:<6} {:<22} {:<12} {:>8.3} {:>9.2}$  {}",
            p.vast_id,
            state,
            truncate(&p.label, 22),
            truncate(&p.gpu_name, 12),
            p.cost_per_hour,
            cost,
            started.format("%Y-%m-%d %H:%M")
        );
        if p.status == ledger::PodStatus::Running {
            running_total += cost;
        }
    }
    println!();
    println!("running pods accruing: ${:.2}", running_total);
    0
}

fn cmd_pod_down(args: &[String]) -> i32 {
    let Some(vast_id) = args.first().cloned() else {
        eprintln!("usage: svrn mesh pod down <vast-id>");
        return 2;
    };
    let path = ledger::default_path();
    let cost_before = ledger::read(&path)
        .ok()
        .and_then(|pods| pods.into_iter().find(|p| p.vast_id == vast_id))
        .map(|p| (p.cost_per_hour, ledger::accrued_cost(&p)));

    if let Err(e) = pod::destroy_instance(&vast_id) {
        eprintln!("vastai destroy failed: {e}");
        // Continue to ledger close — operator may have already
        // destroyed the pod manually and just wants to clean up.
    }

    // Remove the pinned-pod snapshot so the daemon's inference
    // scheduler stops considering this pod. Idempotent — a pod that
    // never wrote a snapshot (older pod-up before the inference
    // wiring shipped) just returns false here.
    // Spec: docs/PINNED_WORKER_AS_INFERENCE_PEER.md §3.6.
    if let Some(dir) = sovereign_mesh::pinned_pod_snapshot::default_snapshot_dir() {
        match sovereign_mesh::pinned_pod_snapshot::delete_snapshot(&dir, &vast_id) {
            Ok(true) => println!("removed pinned-pod snapshot for {vast_id}"),
            Ok(false) => {}
            Err(e) => eprintln!("warning: snapshot delete failed: {e}"),
        }
    }
    match ledger::close(&path, &vast_id) {
        Ok(rec) => {
            let total = ledger::accrued_cost(&rec);
            let hours = ledger::elapsed_hours(&rec);
            println!("pod {} destroyed.", vast_id);
            println!("  elapsed     {:.2} h", hours);
            println!("  $/hr        {:.3}", rec.cost_per_hour);
            println!("  total cost  ${:.2}", total);
        }
        Err(ledger::LedgerError::NotFound(_)) => {
            if let Some((rate, accrued)) = cost_before {
                println!("pod {} destroyed (was not in running set).", vast_id);
                println!("  last-seen rate {:.3} $/hr", rate);
                println!("  last accrued   ${:.2}", accrued);
            } else {
                println!("pod {} destroyed (no ledger entry).", vast_id);
            }
        }
        Err(e) => {
            eprintln!("ledger close failed: {e}");
            return 1;
        }
    }
    0
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
        t.push('…');
        t
    }
}

/// Operator-stamped capabilities for a rented GPU. Best-effort:
/// covers the GPU families we routinely rent on Vast (L40S, A6000,
/// H100, RTX 4090) with a default fallback. Tuning these tighter is
/// future work; the inference scheduler's throughput-observation
/// loop self-corrects once real traffic flows.
///
/// `system_ram_gb` is the Vast offer's *host* RAM — the pod's child
/// daemon reads this for slot sizing. A miscalibration just biases
/// routing, no correctness risk.
fn capabilities_for_gpu(gpu_name: &str) -> sovereign_mesh::pinned_worker_source::PodCapabilities {
    let upper = gpu_name.to_ascii_uppercase();
    let system_ram_gb = if upper.contains("H100") {
        192
    } else if upper.contains("L40S") || upper.contains("A6000") || upper.contains("L40") {
        128
    } else {
        64
    };
    sovereign_mesh::pinned_worker_source::PodCapabilities {
        system_ram_gb,
        benchmark: None,
        current_in_flight: None,
    }
}
