// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn mesh plan`: dry-run a model's tensor split across a mesh, and say
//! what the placement measurements know about its speed (moved from
//! sovereign-cli-mesh `mesh_cmd.rs` by pb-serve-placement).

/// One machine on the live mesh, as `mesh plan --from-mesh` sees it.
///
/// Carries identity alongside capacity because a measured throughput number is
/// only meaningful for the machines it was measured on: `name` distinguishes
/// one peer's share from another's in the placement digest, and
/// `hw_fingerprint` pins the hardware. Both are absent for a peer on an older
/// daemon, which `mesh plan` reports as "not measured" rather than guessing.
pub(crate) struct MeshDevice {
    pub(crate) name: String,
    /// What this machine's GPU could hold if nothing else were resident — the
    /// gossiped device TOTAL. A durable hardware fact, and the basis for "is
    /// this configuration even viable".
    pub(crate) vram_gb: f64,
    /// What is FREE on it right now, as the loader's own oracle reports it
    /// (`/v1/mesh/status.device_memory`, ultimately `ggml_backend_dev_memory`).
    ///
    /// `None` when no live reading exists for this peer — an older daemon, or a
    /// member with no discovered RPC worker. It is deliberately a separate field
    /// rather than a correction to `vram_gb`: the difference between them is
    /// "held by other work right now", which is the entire distinction between
    /// "this node is too small" and "this node is busy". Those have opposite
    /// repairs, so the plan reports both and names the gap instead of picking.
    pub(crate) free_vram_gb: Option<f64>,
    pub(crate) hw_fingerprint: Option<u64>,
    pub(crate) backend: Option<String>,
    /// How this machine's rpc-server would be reached, when discovery has
    /// found one for it.
    ///
    /// `None` means no worker is currently discovered for this peer — the plan
    /// then cannot say how the tensor stream would travel, which is a reason to
    /// report "not measured" rather than to assume the good case. See
    /// [`crate::mesh_measurements::LinkClass`].
    pub(crate) link: Option<crate::mesh_measurements::LinkClass>,
}

/// Read the live mesh from the running daemon's `/v1/mesh/status` and build the
/// per-device vector for `mesh plan --from-mesh`: online anchor workers first,
/// this host (`is_self`) last so the output head lands on it. Returns
/// `(devices, host index)`. Prints the resolved mesh to stderr (so `--json`
/// stays clean on stdout).
async fn devices_from_live_mesh() -> Result<(Vec<MeshDevice>, usize, Option<String>), String> {
    let port = sovereign_contracts::setup_config::SetupConfig::load()
        .map(|c| c.daemon.client_port)
        .unwrap_or(9741);
    let url = format!("http://127.0.0.1:{port}/v1/mesh/status");
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| format!("http client: {e}"))?;
    let resp = client.get(&url).send().await.map_err(|e| {
        format!("daemon at {url} not reachable: {e}\n  hint: start it (`svrn daemon start`) or pass --devices manually")
    })?;
    if !resp.status().is_success() {
        return Err(format!("daemon returned HTTP {} from {url}", resp.status()));
    }
    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("bad status JSON: {e}"))?;
    let members = body
        .get("members")
        .and_then(|m| m.as_array())
        .cloned()
        .unwrap_or_default();

    // node_id → the endpoint ggml would dial for that peer's rpc-server.
    //
    // This is the consumer half of the link agreement: `mesh bench` classifies
    // the endpoints in the daemon's live *placement*, and this side classifies
    // the endpoints in the daemon's live *discovery*. They are the same strings
    // from the same daemon, run through the same `link_class_of_endpoint`, so
    // the plan asks about the link the bench would measure. A peer absent from
    // this list has no discovered worker; it stays `None` and the plan reports
    // "not measured" rather than assuming a direct link it has not seen.
    let worker_endpoints: std::collections::HashMap<String, String> = body
        .get("rpc_workers")
        .and_then(|w| w.as_array())
        .map(|ws| {
            ws.iter()
                .filter_map(|w| {
                    let id = w.get("node_id")?.as_str()?;
                    let ep = w.get("endpoint")?.as_str()?;
                    (!id.is_empty() && !ep.is_empty()).then(|| (id.to_string(), ep.to_string()))
                })
                .collect()
        })
        .unwrap_or_default();

    // The LOADER's own per-device memory view, as the daemon publishes it. This
    // is the second half of the two-capacity answer: `members[].vram_gb` above is
    // the gossiped device TOTAL, and this is what is actually free right now —
    // the number the live fit gate judges against. Only the daemon can read it
    // (it holds the registered ggml devices), which is why it travels on the
    // status payload rather than being sampled here.
    //
    // Rows are keyed by RPC endpoint; the entries with NO endpoint are this
    // host's own local GPU device(s), summed to one figure because the plan
    // models the host as a single device (matching `local_gpu_total_vram_gb`).
    let dev_mem = body
        .get("device_memory")
        .and_then(|d| d.as_array())
        .cloned()
        .unwrap_or_default();
    const MIB_PER_GB: f64 = 1024.0;
    // Lendable, not raw free: the owning node's reserve is already off the
    // capacity its loader plans against, so a preview that used raw `free_mb`
    // would describe a cut the loader would refuse to make.
    let lendable_mb = |d: &serde_json::Value| -> Option<f64> {
        let free = d.get("free_mb")?.as_f64()?;
        let reserve = d.get("reserve_mb").and_then(|v| v.as_f64()).unwrap_or(0.0);
        Some((free - reserve).max(0.0))
    };
    let free_by_endpoint: std::collections::HashMap<String, f64> = dev_mem
        .iter()
        .filter_map(|d| {
            let ep = d.get("endpoint")?.as_str()?;
            Some((ep.to_string(), lendable_mb(d)? / MIB_PER_GB))
        })
        .collect();
    // Age of the reading. It is an observation taken when the loader last planned
    // a cut, not a live sample (sampling would block on a busy worker — see
    // `last_device_memory`), so an operator has to be able to see how old it is.
    if let Some(obs) = body
        .get("device_memory_observed_unix")
        .and_then(|v| v.as_u64())
    {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        eprintln!(
            "  free-memory reading observed {}s ago, when the loader last planned a cut",
            now.saturating_sub(obs)
        );
    }
    let host_free_gb: Option<f64> = {
        let local: Vec<f64> = dev_mem
            .iter()
            .filter(|d| d.get("endpoint").and_then(|e| e.as_str()).is_none())
            .filter_map(lendable_mb)
            .collect();
        (!local.is_empty()).then(|| local.iter().sum::<f64>() / MIB_PER_GB)
    };

    let mut workers: Vec<MeshDevice> = Vec::new();
    let mut host: Option<MeshDevice> = None;
    for m in &members {
        let is_self = m.get("is_self").and_then(|b| b.as_bool()).unwrap_or(false);
        let endpoint = m
            .get("node_id")
            .and_then(|v| v.as_str())
            .and_then(|id| worker_endpoints.get(id));
        let dev = MeshDevice {
            name: m
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or("?")
                .to_string(),
            vram_gb: m.get("vram_gb").and_then(|v| v.as_f64()).unwrap_or(0.0),
            free_vram_gb: if is_self {
                host_free_gb
            } else {
                endpoint.and_then(|ep| free_by_endpoint.get(ep).copied())
            },
            hw_fingerprint: m.get("hw_fingerprint").and_then(|v| v.as_u64()),
            backend: m
                .get("backend")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            link: endpoint.map(|ep| crate::mesh_measurements::link_class_of_endpoint(ep)),
        };
        let online = m.get("status").and_then(|s| s.as_str()) == Some("online");
        let can_anchor = m
            .get("can_anchor")
            .and_then(|b| b.as_bool())
            .unwrap_or(false);
        if is_self {
            host = Some(dev);
        } else if online && can_anchor {
            workers.push(dev);
        }
    }
    let host =
        host.ok_or_else(|| "could not find this node (is_self) in the mesh status".to_string())?;

    // Glassbox both capacities per device, so the operator can see WHERE the two
    // bases disagree before reading a verdict built on either.
    eprintln!(
        "Resolved live mesh: {} online anchor worker(s) + this host",
        workers.len()
    );
    let show = |role: &str, d: &MeshDevice, suffix: &str| {
        match d.free_vram_gb {
        Some(free) => eprintln!(
            "  {role}  {}: {:.0} GB total · {:.1} GB free now ({:.1} GB held by other work){suffix}",
            d.name,
            d.vram_gb,
            free,
            (d.vram_gb - free).max(0.0)
        ),
        None => eprintln!(
            "  {role}  {}: {:.0} GB total · free now UNKNOWN (no live reading){suffix}",
            d.name, d.vram_gb
        ),
    }
    };
    for w in &workers {
        show("worker", w, "");
    }
    show("host  ", &host, "  (holds the output head)");
    if workers.is_empty() {
        eprintln!(
            "  note: no online anchor workers — the plan will show a single-node (local) load."
        );
    }
    eprintln!();

    let mut devices = workers;
    devices.push(host);
    let host_idx = devices.len() - 1;
    // The operator's block-split pin, if the daemon reports one. Not a capacity —
    // it overrides capacity entirely — so it travels beside the devices, not in
    // them.
    let pin = body
        .get("rpc_block_split_pin")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    Ok((devices, host_idx, pin))
}

/// `svrn mesh plan` — dry-run a model's tensor split across a mesh, offline. Reuses
/// the daemon's own `plan_shards` + `quantize_vram` and the SAME `shard_fits`
/// decider its per-device gate runs (see `first_worker_overflow`), so the preview
/// and the load it previews reach the same verdict — with operator-set
/// `--headroom` for what-if planning instead of the configured factor.
///
/// Under `--from-mesh` it reports TWO capacity bases, never one:
/// [`Possible`](CapacityBasis::Possible) from device totals and
/// [`SafeNow`](CapacityBasis::SafeNow) from the loader's live free-memory reading.
/// They routinely disagree, and which one you need depends on the question —
/// "could this mesh ever run this model" versus "would a load right now succeed".
/// Picking one on the operator's behalf hid a real cut mismatch for weeks.
pub(crate) async fn cmd_plan(args: &[String]) -> i32 {
    use sovereign_inference::embedded as inf;
    // Kept as the raw spec, not a PathBuf: it may be `hf:<owner>/<repo>/<variant>`,
    // which `remote_gguf::resolve` turns into header-only stand-ins below.
    let mut model_spec: Option<String> = None;
    let mut devices_gb: Vec<f64> = Vec::new();
    let mut host_idx: Option<usize> = None;
    // Default headroom mirrors the daemon's OWN resolution order exactly, so a
    // previewed plan uses the SAME factor the load executes with: an explicit
    // `SOVEREIGN_RPC_HEADROOM` env wins (the daemon reads it directly), else the
    // `[shared_model] headroom` config (bootstrap bridges config→env), else 1.2.
    // `--headroom` overrides this for what-if planning.
    // ONE parser, ONE default (§10.6). The env read, the `>= 1.0` filter and
    // the literal `1.2` used to exist here AND in
    // `sovereign_inference::embedded::rpc_headroom_factor` — the function that
    // actually gates the load. They agreed, which is the weakest way for a
    // promise to hold: this command's whole contract is that "a previewed plan
    // uses the headroom the load executes with", and it was kept by two copies
    // of a number matching.
    //
    // The config fallback stays here, and is why the shared helper returns an
    // `Option`: this CLI can run BEFORE bootstrap has bridged
    // `[shared_model] headroom` into the environment, so it has a second
    // source the daemon does not. The filter applies to that source too — a
    // headroom below 1.0 gates on less memory than the model needs.
    let mut headroom: f64 = sovereign_inference::embedded::rpc_headroom_from_env()
        .or_else(|| {
            sovereign_contracts::setup_config::SetupConfig::load()
                .ok()
                .and_then(|c| c.shared_model.headroom)
                .filter(|&h| h >= 1.0)
        })
        .unwrap_or(sovereign_inference::embedded::RPC_HEADROOM_DEFAULT);
    let mut headroom_from_flag = false;
    let mut json = false;
    let mut from_mesh = false;
    // `Some` only under `--from-mesh`. A `--devices` plan describes hardware
    // that is not here, so it has no identity and can never match a
    // measurement — see `SpeedSection::NotMeasurable`.
    let mut mesh_devices: Option<Vec<MeshDevice>> = None;
    // `Some` only under `--from-mesh`, and only when the daemon reported a live
    // reading for EVERY device. `--devices` describes hardware that is not here,
    // so nothing can be free on it.
    let mut devices_free_gb: Option<Vec<f64>> = None;
    // The daemon's `SOVEREIGN_RPC_BLOCK_SPLIT`, when it reports one. Only a live
    // daemon can have a pin; `--devices` previews nothing that would honour it.
    let mut block_split_pin: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--devices" => {
                i += 1;
                let Some(v) = args.get(i) else {
                    eprintln!(
                        "--devices needs a value (per-node usable VRAM in GB, e.g. 64,32,32)"
                    );
                    return 2;
                };
                match v
                    .split(',')
                    .map(|s| s.trim().parse::<f64>())
                    .collect::<Result<Vec<_>, _>>()
                {
                    Ok(d) => devices_gb = d,
                    Err(_) => {
                        eprintln!("--devices: comma-separated GB numbers, e.g. 64,32,32");
                        return 2;
                    }
                }
            }
            "--host" => {
                i += 1;
                match args.get(i).and_then(|s| s.parse::<usize>().ok()) {
                    Some(h) => host_idx = Some(h),
                    None => {
                        eprintln!("--host: a 0-based device index");
                        return 2;
                    }
                }
            }
            "--headroom" => {
                i += 1;
                match args.get(i).and_then(|s| s.parse::<f64>().ok()) {
                    Some(h) if h >= 1.0 => {
                        headroom = h;
                        headroom_from_flag = true;
                    }
                    _ => {
                        eprintln!("--headroom: a number >= 1.0 (1.15 aggressive · 1.2 default · 1.4 safe)");
                        return 2;
                    }
                }
            }
            "--json" => json = true,
            "--from-mesh" => from_mesh = true,
            "--help" | "-h" => {
                sovereign_cli_base::help::print(&HELP_MESH_PLAN);
                return 0;
            }
            s if model_spec.is_none() && !s.starts_with('-') => model_spec = Some(s.to_string()),
            other => {
                eprintln!("Unknown arg: {other}");
                return 2;
            }
        }
        i += 1;
    }
    let Some(model_spec) = model_spec else {
        sovereign_cli_base::help::print(&HELP_MESH_PLAN);
        return 2;
    };
    if from_mesh {
        if !devices_gb.is_empty() {
            eprintln!("--from-mesh and --devices are mutually exclusive");
            return 2;
        }
        match devices_from_live_mesh().await {
            Ok((devs, h, pin)) => {
                devices_gb = devs.iter().map(|d| d.vram_gb).collect();
                host_idx = Some(h);
                block_split_pin = pin;
                // All-or-nothing: one device missing a live reading means there
                // is no coherent "safe now" basis to plan on, and the report says
                // so rather than mixing bases. See `PlanInput::devices_free_gb`.
                devices_free_gb = devs.iter().map(|d| d.free_vram_gb).collect();
                if devices_free_gb.is_none() {
                    let blind: Vec<&str> = devs
                        .iter()
                        .filter(|d| d.free_vram_gb.is_none())
                        .map(|d| d.name.as_str())
                        .collect();
                    eprintln!(
                        "  note: no live free-memory reading for {} — the plan can show what is \n        POSSIBLE (device totals) but not what is SAFE RIGHT NOW. An older peer \n        daemon, or a member with no discovered RPC worker.\n",
                        blind.join(", ")
                    );
                }
                // Retained for the speed lookup: only a real, present mesh can
                // identify the machines a measurement would belong to.
                mesh_devices = Some(devs);
            }
            Err(e) => {
                eprintln!("--from-mesh: {e}");
                return 1;
            }
        }
    }
    if devices_gb.is_empty() {
        eprintln!("provide the mesh: --devices <gb,gb,..> (per-node VRAM) or --from-mesh (read the live mesh)");
        return 2;
    }
    if devices_gb.iter().any(|&g| g <= 0.0) {
        eprintln!(
            "device VRAM must be > 0 (a member may advertise 0 GB — pass --devices manually)"
        );
        return 2;
    }

    // Resolve the spec to something with a readable header. A filesystem path
    // passes through untouched; `hf:…` is fetched header-only so an operator
    // can size a 155 GB model before spending the download on it.
    let resolved = match crate::remote_gguf::resolve(&model_spec).await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    let model = resolved.path.clone();

    // Say what was actually read. A split model whose siblings are missing
    // resolves to shard 1 alone and would otherwise plan a ~Nx-too-small
    // model in confident silence — the one way this command can lie.
    if !resolved.headers_only {
        let named_split = model
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(sovereign_inference::embedded::split_shard_names)
            .map(|v| v.len());
        match named_split {
            Some(want) if resolved.shards < want => {
                eprintln!(
                    "  WARNING: {} names a {want}-shard split but only {} shard(s) are on disk.\n           Planning the shard(s) present — this UNDERSTATES the model by ~{:.1}x.\n           Fetch the missing siblings before trusting this verdict.\n",
                    model.display(),
                    resolved.shards,
                    want as f64 / resolved.shards.max(1) as f64
                );
            }
            Some(want) => eprintln!(
                "  model: {want} shards, {:.1} GB\n",
                resolved.total_bytes as f64 / 1e9
            ),
            None => {}
        }
    } else {
        // Name the basis. A header-only plan has exact tensor mass — the byte
        // counts come from dims + ggml type, not from any file length — but it
        // has not fetched a single weight, so it can say the model WOULD fit
        // and cannot say the download will succeed.
        eprintln!(
            "  basis: GGUF headers only, {} — tensor mass exact, no weights fetched\n",
            resolved.label
        );
    }

    let n_layer = match inf::gguf_block_count(&model) {
        Ok(Some(n)) if n > 0 => n,
        Ok(_) => {
            eprintln!(
                "could not read a positive block_count from {} (not a GGUF, or missing <arch>.block_count)",
                model.display()
            );
            return 1;
        }
        Err(e) => {
            eprintln!("reading {}: {e}", model.display());
            return 1;
        }
    };
    let sizes = match inf::tensor_sizes(&model) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("reading tensor table from {}: {e}", model.display());
            return 1;
        }
    };

    let host = host_idx.unwrap_or(devices_gb.len() - 1);
    if host >= devices_gb.len() {
        eprintln!(
            "--host {host} out of range (valid 0..{})",
            devices_gb.len() - 1
        );
        return 2;
    }

    // The context length the plan assumes. Same accessor the cold-start and
    // reload paths use, so a plan and the load it previews cannot disagree
    // about KV size — which is part of the measurement key.
    let n_ctx = sovereign_contracts::setup_config::SetupConfig::load()
        .map(|c| c.effective_context_size())
        .unwrap_or(16384);

    // llama.cpp's three-term projection (KV + compute per device) — the SAME
    // numbers the live fit gate judges with, so the preview's fit column and a
    // load-time refusal can never disagree about the non-weight terms. The
    // backend guard must stay alive past the projection call (its Drop frees
    // the backend); a failed init or projection degrades the preview to
    // weights-only fit, the same fallback the live gate takes. Measured
    // ~278 ms warm on a 155 GB set (tests/device_memory_probe.rs).
    let _backend = sovereign_inference::llama::cpp::llama_backend::LlamaBackend::init();
    let overheads = sovereign_inference::embedded::projected_overheads(&model, n_ctx, false);

    // What peers have measured. A key pins the exact silicon *and* the exact
    // split, so an operator asking about a configuration they have never run will
    // essentially never get a local hit — which makes the peer half the part most
    // likely to answer the question they actually asked. Degrades to empty with
    // no daemon; never fatal.
    let peers = crate::mesh_travel::peer_history().await;
    if let Some(note) = &peers.note {
        eprintln!("mesh plan: peer measurements unavailable — {note}");
    }
    // On stderr rather than in the plan: it is a fact about the mesh, not about
    // this model. But it does have to be said — otherwise a peer running an
    // incompatible schema is indistinguishable from a peer who has measured
    // nothing, and the operator would go looking for the wrong problem.
    if peers.unreadable > 0 {
        eprintln!(
            "mesh plan: {} peer measurement(s) could not be read — a peer is probably on an \
             incompatible schema; upgrade it or ignore this",
            peers.unreadable
        );
    }

    let report = build_report(
        PlanInput {
            model_name: model
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("?")
                .to_string(),
            n_layer,
            sizes,
            devices_gb,
            devices_free_gb,
            block_split_pin,
            host,
            headroom,
            headroom_from_flag,
            mesh: mesh_devices,
            n_ctx,
            overheads,
        },
        &crate::mesh_measurements::load(),
        &peers.records,
        env!("CARGO_PKG_VERSION"),
    );

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&render_json(&report)).unwrap_or_default()
        );
    } else {
        print!("{}", render_human(&report));
    }
    report.exit_code()
}

// ---------------------------------------------------------------------------
// `mesh plan`, as a pure function of its inputs
//
// Everything below is deliberately free of I/O, so the report can be built and
// rendered in a test without a GGUF on disk, a daemon, or a GPU. `cmd_plan`
// above is the shell: it parses args, reads the header table, talks to the
// mesh, and hands the result here.
//
// The split exists because this command shipped for months with no tests at
// all — there was no seam to test at. Keep the seam: computation belongs in
// `build_report`, wording belongs in the renderers, and neither should acquire
// a file read or a network call.
// ---------------------------------------------------------------------------

const HELP_MESH_PLAN: sovereign_cli_base::help::Help = sovereign_cli_base::help::Help {
    command: "svrn mesh plan",
    summary: "Dry-run a model's tensor split across a mesh — per-device fit, offline, no load.",
    sections: &[
        sovereign_cli_base::help::HelpSection::Usage(
            "svrn mesh plan <model.gguf | hf:owner/repo[/variant]> (--from-mesh | --devices <gb,..>) [--host <idx>] [--headroom <f>] [--json]",
        ),
        sovereign_cli_base::help::HelpSection::Flags(&[
            (
                "--from-mesh",
                "Read the live mesh from the running daemon (online anchor workers + this host). Exclusive with --devices.",
            ),
            (
                "--devices <gb,...>",
                "Per-node usable VRAM in GB, in mesh order (e.g. 64,32,32). Plan a hypothetical mesh instead of --from-mesh.",
            ),
            (
                "--host <idx>",
                "0-based index of the host node (holds the output head). Default: last.",
            ),
            (
                "--headroom <f>",
                "Override the headroom factor (weight × f must fit each device). Defaults to `[shared_model] headroom` (else 1.2) — the value the load itself uses.",
            ),
            ("--json", "Emit the plan as a machine-readable JSON split manifest."),
        ]),
        sovereign_cli_base::help::HelpSection::Notes(
            "Reuses the daemon's own plan_shards + quantize_vram, then overlays the real per-block\n\
             byte mass to show the BYTES each node holds and whether they fit — via the SAME\n\
             shard_fits decider the live load's per-device gate runs, so a plan that says OVERFLOW\n\
             names a cut the load would actually refuse. Reads only the GGUF header table: no model\n\
             load, no GPU, instant even on a 400 GB split. Also reports whether the model's per-block\n\
             mass is uniform (heterogeneous VRAM safe) or skewed (OOM risk).\n\
             \n\
             --from-mesh reports TWO capacities per device and does not choose between them:\n\
             POSSIBLE is the device total — what the silicon could hold if nothing else were\n\
             resident, i.e. whether this mesh can run the model at all. SAFE NOW is live free\n\
             memory as the loader reads it — whether a load started this second would succeed.\n\
             They differ whenever anything is loaded, and a large gap means 'busy', not 'too\n\
             small'. The exit code follows POSSIBLE: a device being momentarily busy does not\n\
             make the plan wrong. Read the SAFE NOW line for what would happen right now.\n\
             \n\
             The model can be a path or `hf:<owner>/<repo>[/<variant>]`, which plans a model you\n\
             have NOT downloaded. Only the GGUF headers are fetched — a few MB by HTTP range —\n\
             because the planner needs the block count and tensor table, never the weights: a\n\
             155 GB five-shard model sizes for ~17 MB. Name the quant directory as <variant>;\n\
             omit it and the command lists what the repo publishes rather than guessing, since\n\
             choosing a quant is choosing how much of your memory to spend. A header-only plan\n\
             is honest about tensor mass but has fetched no weights, so it cannot tell you the\n\
             download will succeed — only whether it would fit if it did.",
        ),
        sovereign_cli_base::help::HelpSection::Examples(&[
            (
                "svrn mesh plan GLM-5.2.gguf --from-mesh",
                "Plan across your actual running mesh (reads each node's advertised VRAM)",
            ),
            (
                "svrn mesh plan hf:unsloth/DeepSeek-V4-Flash-0731-GGUF/UD-Q4_K_XL --from-mesh",
                "Will this 155 GB model fit my mesh? Answered before downloading it",
            ),
            (
                "svrn mesh plan hf:unsloth/DeepSeek-V4-Flash-0731-GGUF --devices 51,124",
                "No variant named: lists the quants the repo publishes, then pick one",
            ),
            (
                "svrn mesh plan Qwen3.5-122B.gguf --devices 64,32,32",
                "A hypothetical 64 GB host + two 32 GB workers",
            ),
            (
                "svrn mesh plan model.gguf --devices 128,128,128,128 --headroom 1.35",
                "Four nodes, conservative headroom",
            ),
        ]),
    ],
};

mod render;
mod report;

pub(crate) use render::*;
pub(crate) use report::*;

#[cfg(test)]
mod tests;
