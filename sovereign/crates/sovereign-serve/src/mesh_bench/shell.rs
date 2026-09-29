// SPDX-License-Identifier: AGPL-3.0-or-later
//! The shell: arguments, the run, the probe's dials and the history view (split from `mesh_bench.rs` at the move to serve).

use super::*;

// ---------------------------------------------------------------------------
// The shell
// ---------------------------------------------------------------------------

/// Parsed `mesh bench` arguments.
struct BenchArgs {
    /// The optional assertion: this GGUF must be what is resident.
    assert_model: Option<PathBuf>,
    trials: u32,
    json: bool,
    history: bool,
}

/// `svrn mesh bench [<model.gguf>] [--trials <n>] [--json] [--history]`
pub async fn cmd_bench(args: &[String]) -> i32 {
    let mut parsed = BenchArgs {
        assert_model: None,
        trials: 3,
        json: false,
        history: false,
    };
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--trials" => {
                i += 1;
                match args.get(i).and_then(|s| s.parse::<u32>().ok()) {
                    Some(n) if (1..=20).contains(&n) => parsed.trials = n,
                    _ => {
                        eprintln!("--trials: a count from 1 to 20 (default 3)");
                        return 2;
                    }
                }
            }
            "--json" => parsed.json = true,
            "--history" => parsed.history = true,
            "--help" | "-h" => {
                sovereign_cli_base::help::print(&HELP_MESH_BENCH);
                return 0;
            }
            s if parsed.assert_model.is_none() && !s.starts_with('-') => {
                parsed.assert_model = Some(PathBuf::from(s));
            }
            other => {
                eprintln!("Unknown arg: {other}");
                return 2;
            }
        }
        i += 1;
    }
    run_bench(parsed).await
}

/// Exit codes, so a script can branch without parsing prose.
mod exit {
    /// A valid measurement was taken and recorded.
    pub(super) const OK: i32 = 0;
    /// The run completed but tripped a guard. The record is written for
    /// glassbox and will never be served back.
    pub(super) const INVALID: i32 = 1;
    /// The named GGUF is not what is resident, or the daemon and the config
    /// disagree about which model that is.
    pub(super) const ASSERTION: i32 = 3;
    /// No key could be constructed, so nothing could be filed. Nothing ran.
    pub(super) const NO_KEY: i32 = 4;
    /// The daemon is not reachable.
    pub(super) const NO_DAEMON: i32 = 5;
}

async fn run_bench(args: BenchArgs) -> i32 {
    let cfg = match sovereign_contracts::setup_config::SetupConfig::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("could not read the configuration: {e}");
            return exit::NO_KEY;
        }
    };
    let port = cfg.daemon.client_port;
    let n_ctx = cfg.effective_context_size();
    // The bench reads the GGUF header off disk, so it needs a real local file.
    let primary_path = match cfg.models() {
        Ok(m) => m.primary.clone(),
        Err(e) => {
            eprintln!("{e}");
            return exit::NO_KEY;
        }
    };

    // ── the model, from the config the daemon loads from ───────────────────
    let (n_layer, sizes) = match read_model_header(&primary_path) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{e}");
            return exit::NO_KEY;
        }
    };
    let fingerprint = mm::model_fingerprint(&sizes, n_layer);

    // ── the assertion, if one was made ─────────────────────────────────────
    if let Some(asserted) = &args.assert_model {
        let (a_layer, a_sizes) = match read_model_header(asserted) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("{e}");
                return exit::ASSERTION;
            }
        };
        let asserted_fp = mm::model_fingerprint(&a_sizes, a_layer);
        if asserted_fp != fingerprint {
            eprintln!(
                "ASSERTION FAILED — the model you named is not the one this daemon runs.\n\
                 \n  you named   {}\n              {asserted_fp}\n\
                 \n  configured  {}\n              {fingerprint}\n\
                 \n`mesh bench` measures the configuration you are RUNNING; it never loads one.\n\
                 To measure the model you named, point the config at it and restart the daemon:\n\
                 \n    [models]\n    primary = \"{}\"\n",
                asserted.display(),
                primary_path.display(),
                asserted.display()
            );
            return exit::ASSERTION;
        }
    }

    // ── history, if that is all that was asked for ─────────────────────────
    // Answered before any HTTP: "what has this machine measured" is a read of a
    // local file, and making it depend on a running daemon would deny the
    // operator their own recorded history exactly when the daemon is the thing
    // that is broken.
    if args.history {
        return show_history(&sizes, n_layer);
    }

    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("http client: {e}");
            return exit::NO_DAEMON;
        }
    };

    // ── the mesh, for the host's identity and the peers' names ─────────────
    let mesh_body = match get_json(&client, port, "/v1/mesh/status").await {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{e}");
            return exit::NO_DAEMON;
        }
    };
    let mesh = MeshView::parse(&mesh_body);
    let Some(host) = mm::HostIdentity::from_live_mesh(mesh.self_hw_fingerprint) else {
        eprintln!(
            "This daemon advertises no hardware fingerprint, so there is no key under which a\n\
             measurement could be filed — and a record filed under a placeholder would be served\n\
             back on somebody else's machine.\n\n\
             Nothing was measured. Restart the daemon on a build that advertises one:\n\
             \n    systemctl --user restart sovereign.service\n"
        );
        return exit::NO_KEY;
    };

    // ── the slot, before anything is fired ─────────────────────────────────
    let status_body = match get_json(&client, port, "/status").await {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{e}");
            return exit::NO_DAEMON;
        }
    };
    let uptime_before = uptime_from_status(&status_body);
    let Some((primary_model_id, resident_before, _)) = primary_from_status(&status_body) else {
        eprintln!("no `primary` slot in /status — this daemon has no primary model configured.");
        return exit::NO_KEY;
    };
    let config_stem = primary_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    if !primary_model_id.is_empty() && primary_model_id != config_stem {
        eprintln!(
            "The running daemon and the configuration disagree about the primary model.\n\
             \n  /status says  {primary_model_id}\n  config says   {config_stem}\n\
             \nThe header read for the fingerprint came from the config, so a record filed now\n\
             would describe a model that is not the one being measured. Restart the daemon to\n\
             pick up the config, or point this config line at what is actually loaded:\n\
             \n    [models]\n    primary = \"{}\"\n",
            primary_path.display()
        );
        return exit::ASSERTION;
    }

    // ── the canary (which also absorbs a cold load) ────────────────────────
    eprintln!(
        "Measuring the running configuration. This is not instant — a cold {} load alone can\n\
         take minutes, and {} timed trials follow it.\n",
        config_stem, args.trials
    );
    if !resident_before {
        eprintln!("The primary slot is idle-unloaded; the canary will pay for a cold load.");
    }
    eprintln!(
        "[1/{}] canary ({CANARY_MAX_TOKENS} tokens) …",
        args.trials + 1
    );
    let canary_start = Instant::now();
    // Retried, bounded, and only while the daemon says it is still coming up.
    //
    // This is NOT retry-until-lucky — the thing being waited out is a lazy slot
    // loading, which is the cold-load path the whole command is built around. A
    // canary that fires into "child not serving (starting)" and gives up leaves
    // the timed trials to be answered by whatever else picks them up, which is
    // precisely the false result the canary exists to prevent. Any other error,
    // and any exhausted deadline, falls through to the guards.
    let mut canary_tokens = 0u32;
    let mut canary_err = None;
    for attempt in 1..=CANARY_ATTEMPTS {
        let (tokens, err) = canary(port).await;
        canary_tokens = tokens;
        canary_err = err;

        // Tokens flowing is NOT the condition to stop on. While the primary is
        // still coming up, something else answers — that is the whole hijack —
        // so a canary that stopped at "I got tokens" would hand the timed
        // trials to the wrong slot and produce a run the guards then have to
        // throw away. Wait for the model we came to measure.
        let status = get_json(&client, port, "/status").await.ok();
        let serving = status
            .as_ref()
            .is_some_and(|b| primary_is_serving(b, &primary_model_id));
        if serving && tokens > 0 {
            break;
        }
        // The daemon already knows this will never come up. Waiting out the
        // remaining attempts would spend ten minutes calling a failure a cold
        // load; the guards downstream still record the run as invalid, they
        // just get to do it now and with the child's own reason attached.
        if let Some(reason) = status
            .as_ref()
            .and_then(|b| primary_children_failed(b, &primary_model_id))
        {
            eprintln!(
                "      the primary's compute child has FAILED — not a cold load: {reason}\n      \
                 Not waiting out the remaining {} attempt(s); nothing can serve this model \
                 until that child starts.",
                CANARY_ATTEMPTS - attempt
            );
            canary_err = Some(format!("primary compute child failed: {reason}"));
            break;
        }
        let coming_up = !serving || canary_err.as_deref().is_some_and(slot_still_starting);
        if !coming_up || attempt == CANARY_ATTEMPTS {
            break;
        }
        eprintln!(
            "      the primary is not serving yet (attempt {attempt}/{CANARY_ATTEMPTS}) — \
             waiting {}s. This is the cold load, not a failure.",
            CANARY_RETRY.as_secs()
        );
        tokio::time::sleep(CANARY_RETRY).await;
    }
    let canary_elapsed = canary_start.elapsed().as_secs_f64();
    // A cold load is attributed to the canary only when the slot was actually
    // cold. Attributing it unconditionally would report a warm run's canary
    // latency as a load time, which is a number nobody can act on.
    let cold_load_s = (!resident_before).then_some(canary_elapsed);
    if let Some(e) = &canary_err {
        eprintln!("      canary error: {e}");
    }
    eprintln!(
        "      canary produced {canary_tokens} token(s) in {canary_elapsed:.1}s{}",
        if cold_load_s.is_some() {
            " (includes the cold load)"
        } else {
            ""
        }
    );

    // ── the placement, read AFTER the load so it is real ───────────────────
    let after_canary = match get_json(&client, port, "/status").await {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{e}");
            return exit::NO_DAEMON;
        }
    };
    let placement_before = primary_from_status(&after_canary)
        .map(|(_, _, p)| p)
        .unwrap_or_default();
    let primary_serving_before = primary_is_serving(&after_canary, &primary_model_id);
    if !primary_serving_before {
        eprintln!(
            "      WARNING: /status still reports the primary NOT resident. Anything that \
             answers now is a different slot; the run will be recorded INVALID."
        );
    }

    let host_identity = NodeIdentity {
        name: mesh.self_name.clone(),
        hw: mesh.self_hw_fingerprint,
    };
    let shards = match shards_from_placement(&placement_before, &host_identity, n_layer, &|ep| {
        mesh.endpoint_nodes.get(ep).cloned()
    }) {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "cannot describe the running placement, so no record can be filed: {e}\n\
                 Nothing was measured."
            );
            return exit::NO_KEY;
        }
    };
    let human = placement_human(&shards, &mesh.self_name, &placement_before.mode);
    eprintln!("      placement: {human}\n");

    let peers_before = peer_liveness(&shards, &mesh);

    // ── the timed trials ───────────────────────────────────────────────────
    // Wall clock across the trials, for `RunConditions::run_span_s`. Not a
    // performance figure — a cross-check: two runs of the same trial count whose
    // spans differ sharply were not taken under the same load.
    let trials_started = std::time::Instant::now();
    let mut trials: Vec<Trial> = Vec::with_capacity(args.trials as usize);
    for n in 0..args.trials {
        eprintln!("[{}/{}] timed trial …", n + 2, args.trials + 1);
        let frames = match timed_trial(port).await {
            Ok(f) => f,
            Err(e) => {
                eprintln!("      trial failed: {e}");
                Vec::new()
            }
        };
        let t = parse_trial(&frames);
        eprintln!(
            "      {:.2} tok/s · {} frames · TTFT {:.2}s · finish={}",
            t.decode_tok_s,
            t.content_frames,
            t.ttft_s.unwrap_or(0.0),
            t.finish_reason.as_deref().unwrap_or("none")
        );
        for line in &t.non_sse_lines {
            eprintln!("      non-SSE line: {}", &line[..line.len().min(160)]);
        }
        trials.push(t);
    }

    // ── the after-state ────────────────────────────────────────────────────
    let after_body = get_json(&client, port, "/status").await.ok();
    let uptime_after = after_body.as_ref().and_then(uptime_from_status);
    let primary_serving_after = after_body
        .as_ref()
        .map(|b| primary_is_serving(b, &primary_model_id));
    let placement_after = after_body
        .as_ref()
        .and_then(|b| primary_from_status(b).map(|(_, _, p)| p));
    let mesh_after = get_json(&client, port, "/v1/mesh/status").await.ok();
    let peers_after = match &mesh_after {
        Some(b) => peer_liveness(&shards, &MeshView::parse(b)),
        // Unreadable mesh state is not evidence of health. Reporting every peer
        // as offline is the honest reading: we cannot say they were up.
        None => shards
            .iter()
            .filter(|s| s.node_key != mesh.self_name)
            .map(|s| (s.node_key.clone(), false))
            .collect(),
    };

    let problems = evaluate_guards(&GuardInput {
        trials: &trials,
        primary_model_id: &primary_model_id,
        primary_serving_before,
        primary_serving_after,
        canary_tokens,
        placement_before: &placement_before,
        placement_after: placement_after.as_ref(),
        peers_before: &peers_before,
        peers_after: &peers_after,
        host_alive_after: liveness(uptime_before, uptime_after),
    });
    let verdict = if problems.is_empty() {
        mm::Verdict::Valid
    } else {
        mm::Verdict::Invalid {
            problems: problems.clone(),
        }
    };

    // ── the record ─────────────────────────────────────────────────────────
    let agg = aggregate(&trials).unwrap_or(Aggregate {
        decode_tok_s: 0.0,
        decode_tok_s_min: 0.0,
        decode_tok_s_max: 0.0,
        ttft_ms: 0.0,
        itl_p50_ms: 0.0,
        itl_p95_ms: 0.0,
        prefill_tok_s: None,
        content_frames: 0,
        trials: trials.len() as u32,
    });
    let nodes = shards.len() as u32;
    // Classified from the placement this run measured, not from what discovery
    // might choose next time — the number belongs to the link it was taken on.
    let link = placement_link(&placement_before);
    // Built from the same three values the digest is, on the next line, so the
    // witness cannot drift from the key it explains.
    let witness = mesh.witness(digest_mode(&shards), n_layer, &shards);
    let key = mm::MeasurementKey::for_plan(
        host,
        fingerprint,
        mm::placement_digest(digest_mode(&shards), n_layer, &shards),
        n_ctx,
        link,
    );
    debug_assert!(
        witness.explains(&key.placement_digest),
        "the witness and the key were built from different inputs"
    );
    // Read from the two `/status` bodies already in hand — the before-poll and
    // the after-poll — so recording the conditions costs no extra round trip and
    // cannot itself perturb what it is measuring.
    let conditions = mm::RunConditions {
        co_resident_roles: co_resident_roles(&status_body, &primary_model_id),
        host_rss_mb_before: rss_mb_from_status(&status_body),
        host_rss_mb_after: after_body.as_ref().and_then(rss_mb_from_status),
        host_uptime_s: uptime_before,
        run_span_s: Some(trials_started.elapsed().as_secs_f64()),
        // Filtered by `carries_weight` — the same decider `shards_from_placement`
        // uses — so the recorded routes are exactly the machines the placement
        // names, and an idle peer cannot add a route that carried nothing.
        rpc_endpoints: placement_before
            .workers
            .iter()
            .filter(|w| carries_weight(w))
            .map(|w| w.endpoint.clone())
            .collect(),
    };
    let record = mm::MeasurementRecord {
        key: key.clone(),
        witness: Some(witness),
        conditions: Some(conditions),
        decode_tok_s: agg.decode_tok_s,
        decode_tok_s_min: agg.decode_tok_s_min,
        decode_tok_s_max: agg.decode_tok_s_max,
        ttft_ms: agg.ttft_ms,
        itl_p50_ms: agg.itl_p50_ms,
        itl_p95_ms: agg.itl_p95_ms,
        prefill_tok_s: agg.prefill_tok_s,
        cold_load_s,
        trials: agg.trials,
        content_frames: agg.content_frames,
        model_name: config_stem.to_string(),
        placement_human: human.clone(),
        nodes,
        hops: nodes.saturating_sub(1),
        measured_at: sovereign_time::unix_now_u64(),
        build: env!("CARGO_PKG_VERSION").to_string(),
        backend: mesh.self_backend.clone(),
        // Still `None`, and `None` is the honest value — not zero, which would
        // read as "no latency". Checked 2026-07-30: iroh 1.0 does not expose a
        // per-peer RTT on `remote_info` (the RTT machinery in
        // `socket/biased_rtt_path_selector.rs` is internal), so
        // `MeshIrohAccess::peer_path_on` can classify the path but not time it.
        // The two candidates are quinn connection stats, reachable only where a
        // connection is held (daemon-side, and only if one exists), or an
        // explicit timed round trip to the peer — which would measure the link
        // PLUS the peer's own request handling, and must not be filed under a
        // field named `link_rtt_ms` as though it were link latency alone.
        // Until one is built, `RunConditions` carries what can be observed
        // honestly instead.
        link_rtt_ms: None,
        verdict: verdict.clone(),
    };

    let mut file = mm::load();
    mm::record(&mut file, record.clone());
    let store_note = match mm::save(&file) {
        Ok(()) => match mm::store_path() {
            Some(p) => format!("recorded in {}", p.display()),
            None => "not recorded (SOVEREIGN_MESH_MEASUREMENTS=0)".to_string(),
        },
        Err(e) => format!("NOT recorded — writing the store failed: {e}"),
    };

    // Disk first, mesh second, and in that order for a reason: the local record
    // is the authoritative copy and the gossip buffer is a wire buffer the daemon
    // rebuilds from this file at every boot. So publishing can fail without
    // anything being lost, and `mesh bench` still works with no daemon at all.
    let travel = crate::mesh_travel::publish(&record).await;

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&render_bench_json(&record, &store_note, &travel))
                .unwrap_or_default()
        );
    } else {
        print!("{}", render_bench_human(&record, &store_note, &travel));
    }
    if verdict.is_valid() {
        exit::OK
    } else {
        exit::INVALID
    }
}

/// Read a GGUF's block count and tensor table — the header parse both the
/// fingerprint and `mesh plan` are built on.
fn read_model_header(path: &Path) -> Result<(u32, Vec<(String, Option<u32>, u64)>), String> {
    use sovereign_inference::embedded as inf;
    let n_layer = match inf::gguf_block_count(path) {
        Ok(Some(n)) if n > 0 => n,
        Ok(_) => {
            return Err(format!(
                "could not read a positive block_count from {} (not a GGUF, or missing \
                 <arch>.block_count)",
                path.display()
            ))
        }
        Err(e) => return Err(format!("reading {}: {e}", path.display())),
    };
    let sizes = inf::tensor_sizes(path)
        .map_err(|e| format!("reading tensor table from {}: {e}", path.display()))?;
    Ok((n_layer, sizes))
}

/// Which shard-holding peers were online. The host itself is excluded — its
/// liveness is the `HostLiveness` check, not this one.
pub(crate) fn peer_liveness(shards: &[mm::PlacementShard], mesh: &MeshView) -> Vec<(String, bool)> {
    shards
        .iter()
        .filter(|s| s.node_key != mesh.self_name)
        .map(|s| {
            (
                s.node_key.clone(),
                mesh.online.get(&s.node_key).copied().unwrap_or(false),
            )
        })
        .collect()
}

/// GET a JSON endpoint off the local daemon.
async fn get_json(
    client: &reqwest::Client,
    port: u16,
    path: &str,
) -> Result<serde_json::Value, String> {
    let url = format!("http://127.0.0.1:{port}{path}");
    let resp = client.get(&url).send().await.map_err(|e| {
        format!(
            "daemon at {url} not reachable: {e}\n  hint: start it with `svrn daemon start` \
             (or `systemctl --user start sovereign.service`)"
        )
    })?;
    if !resp.status().is_success() {
        return Err(format!("daemon returned HTTP {} from {url}", resp.status()));
    }
    resp.json()
        .await
        .map_err(|e| format!("bad JSON from {url}: {e}"))
}

/// Fire the canary. Returns `(completion_tokens, error)`.
///
/// Non-streaming and tiny on purpose: it proves tokens flow at all before the
/// timing window is spent, and if a bad worker is going to abort the host it
/// happens here, with a clean attribution, rather than inside a timed trial.
async fn canary(port: u16) -> (u32, Option<String>) {
    // Its own client with a long timeout: this request pays for the cold load
    // of a model that may be tens of gigabytes across a mesh, which the 20s
    // status client would abandon halfway through.
    let long = match reqwest::Client::builder()
        .timeout(Duration::from_secs(1800))
        .build()
    {
        Ok(c) => c,
        Err(e) => return (0, Some(format!("http client: {e}"))),
    };
    let body = serde_json::json!({
        "model": PRIMARY_ALIAS,
        "max_tokens": CANARY_MAX_TOKENS,
        "temperature": 0,
        "messages": [{"role": "user", "content": "Say hello."}],
    });
    let resp = match long
        .post(format!("http://127.0.0.1:{port}/v1/chat/completions"))
        .json(&body)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return (0, Some(e.to_string())),
    };
    let v: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => return (0, Some(format!("bad canary JSON: {e}"))),
    };
    let tokens = v
        .get("usage")
        .and_then(|u| u.get("completion_tokens"))
        .and_then(|c| c.as_u64())
        .unwrap_or(0) as u32;
    let err = v
        .get("error")
        .map(|e| e.to_string())
        .or_else(|| (tokens == 0).then(|| "no completion_tokens in the response".to_string()));
    (tokens, err)
}

/// Fire one timed streaming completion and stamp every line as it arrives.
///
/// The timer starts before the request is sent, so time-to-first-token includes
/// prefill and tunnel setup — which is the honest reading of "how long until it
/// starts answering" and is reported separately from the decode rate rather
/// than smeared into it.
async fn timed_trial(port: u16) -> Result<Vec<Frame>, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(600))
        .build()
        .map_err(|e| format!("http client: {e}"))?;
    let body = serde_json::json!({
        "model": PRIMARY_ALIAS,
        "max_tokens": PROBE_MAX_TOKENS,
        "temperature": 0,
        "stream": true,
        "messages": [{"role": "user", "content": PROBE_PROMPT}],
    });

    let t0 = Instant::now();
    let resp = client
        .post(format!("http://127.0.0.1:{port}/v1/chat/completions"))
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        let code = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(format!("HTTP {code}: {}", &text[..text.len().min(300)]));
    }

    let mut frames = Vec::new();
    let mut buf = String::new();
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("stream broke: {e}"))?;
        let t = t0.elapsed().as_secs_f64();
        buf.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(pos) = buf.find('\n') {
            let line: String = buf.drain(..=pos).collect();
            let line = line.trim_end();
            if line.trim().is_empty() {
                continue;
            }
            frames.push(Frame::from_line(t, line));
        }
    }
    let tail = buf.trim().to_string();
    if !tail.is_empty() {
        frames.push(Frame::from_line(t0.elapsed().as_secs_f64(), &tail));
    }
    Ok(frames)
}

/// A placement digest shortened for a table column.
///
/// Disambiguation only — never an identity. Two runs whose abbreviations differ
/// definitely measured different configurations; two whose abbreviations match
/// should be compared on the full digest before anyone calls them one spread.
pub(crate) fn abbreviated_digest(digest: &str) -> String {
    match digest.split_once(':') {
        Some((tag, hash)) => format!("{tag}:{}", &hash[..hash.len().min(8)]),
        None => digest.chars().take(12).collect(),
    }
}

/// Whether any two runs share a placement *description* while sitting under
/// different placement *keys*.
///
/// Pairwise across the whole set, not adjacent rows: the store is ordered by
/// key then by time, so two rows describing the same split under different
/// digests are usually separated by other rows.
pub(crate) fn has_ambiguous_placement(rows: &[&mm::MeasurementRecord]) -> bool {
    rows.iter().enumerate().any(|(i, a)| {
        rows[i + 1..].iter().any(|b| {
            a.placement_human == b.placement_human
                && a.key.placement_digest != b.key.placement_digest
        })
    })
}

/// `--history`: every run filed for this model, invalid ones included.
///
/// Filtered by model fingerprint rather than by the full key on purpose — an
/// operator asking "what has this machine measured" wants the other splits too,
/// which is exactly the comparison that decides whether to move the host role.
fn show_history(sizes: &[(String, Option<u32>, u64)], n_layer: u32) -> i32 {
    let file = mm::load();
    let fp = mm::model_fingerprint(sizes, n_layer);
    let rows: Vec<&mm::MeasurementRecord> = file
        .records()
        .iter()
        .filter(|r| r.key.model_fingerprint == fp)
        .collect();
    if rows.is_empty() {
        println!(
            "No runs recorded for this model on this machine yet.\n\n  \
             Take one:  svrn mesh bench\n"
        );
        return exit::OK;
    }
    println!("Runs recorded for this model ({} of them):\n", rows.len());
    for r in &rows {
        let verdict = match &r.verdict {
            mm::Verdict::Valid => "VALID  ".to_string(),
            mm::Verdict::Invalid { problems } => format!("INVALID ({} problem(s))", problems.len()),
        };
        println!(
            "  {verdict}  {:>7.2} tok/s   {:<28}  {} trial(s)  build {}  {}",
            r.decode_tok_s,
            r.placement_human,
            r.trials,
            r.build,
            abbreviated_digest(&r.key.placement_digest),
        );
        // The conditions, indented under their run. This is the line that makes
        // two rows at different rates comparable — or shows that they are not.
        if let Some(line) = r.conditions.as_ref().and_then(|c| c.describe()) {
            println!("      · {line}");
        } else {
            println!("      · conditions not recorded (run predates 2026-07-30)");
        }
        if let mm::Verdict::Invalid { problems } = &r.verdict {
            for p in problems {
                println!("      ! {p}");
            }
        }
    }
    println!(
        "\n  Only VALID runs on the exact split being planned are served back to `mesh plan`;\n  \
         the others are here so the failure is inspectable."
    );
    // Earned the hard way: two runs 69 minutes apart both rendered "36 local +
    // 12 @BeefyMac" while sitting under DIFFERENT placement digests, and a
    // reader compared them as one configuration and filed a 26% variance that
    // was never real. The human string is a description, not an identity, so the
    // key it belongs to is now on the row beside it.
    if has_ambiguous_placement(&rows) {
        println!(
            "  NOTE: two runs above share a placement description but sit under different\n  \
             keys (the trailing pd2: tag). They measured different configurations and must\n  \
             not be compared as a spread."
        );
    }
    if let Some(p) = mm::store_path() {
        println!("  store: {}", p.display());
    }
    exit::OK
}
