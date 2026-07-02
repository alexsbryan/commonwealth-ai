// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn code capability-graph <corpus-id> [--out <path>]`
//!
//! Emits a SELF-CONTAINED interactive `graph.html` for a capability graph
//! that the `capability-map` (+ optional `capability-findings`) pipeline has
//! already produced under `~/.sovereign/capabilities/<corpus>/`.
//!
//! The graph:
//!   * nodes      — capabilities (size ∝ core function count)
//!   * edges      — first-party call edges between capabilities, derived by
//!                  mapping every SCIP ref's caller/callee through the
//!                  capability membership map (cross-capability edges only)
//!   * node color — the code-vs-docs reconciliation verdict (the differentiator):
//!                  corroborated (green) / undocumented (amber) / drifted (red),
//!                  neutral grey when there is no finding for that capability.
//!
//! Glassbox by construction: the entire graph is one human-readable HTML file
//! with the node/link data inlined as JSON. No server, no live queries — open
//! it in any browser and the structure of the codebase's capabilities (and how
//! well the docs describe them) is right there.
//!
//! TODO(offline): the render/layout libraries (`force-graph` + `d3-force`) are
//! pulled from a CDN (`https://unpkg.com/...`). Inlining the minified libraries
//! into the emitted HTML would make the artifact fully offline / air-gap friendly.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

// ─── on-disk shapes (subset; serde ignores the fields we don't name) ───────

/// `capability_map.json` — produced by `svrn code capability-map`.
#[derive(Deserialize)]
struct CapMapFile {
    #[serde(default)]
    capabilities: Vec<Cap>,
}

/// One capability cluster. `entries`/`core` hold full SCIP qualified names.
#[derive(Deserialize)]
struct Cap {
    label: String,
    /// Part of the finding join key — see [`finding_key`]. Capability `label`s
    /// are NOT unique (e.g. two `<pkg>/(root)` clusters), so we disambiguate
    /// on `(label, n_entries, n_core)`, both of which the findings file carries.
    #[serde(default)]
    n_entries: usize,
    #[serde(default)]
    n_core: usize,
    #[serde(default)]
    entries: Vec<String>,
    #[serde(default)]
    core: Vec<String>,
}

/// `capability_findings.json` — produced by the reconciliation tool. Optional.
#[derive(Deserialize)]
struct FindingsFile {
    #[serde(default)]
    findings: Vec<Finding>,
}

/// One code-vs-docs verdict. Joined to a capability on the composite key
/// `(label, n_entries, n_core)` — see [`finding_key`] — because `label` alone
/// collides across distinct clusters.
#[derive(Deserialize)]
struct Finding {
    /// "corroborated" | "undocumented" | "drifted".
    kind: String,
    label: String,
    #[serde(default)]
    n_entries: usize,
    #[serde(default)]
    n_core: usize,
    #[serde(default)]
    evidence: Option<String>,
}

// ─── output shapes (inlined into the HTML as JSON) ─────────────────────────

#[derive(Serialize)]
struct Member {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    loc: Option<String>,
    is_entry: bool,
}

#[derive(Serialize)]
struct Node {
    id: usize,
    label: String,
    /// Crate/package the capability belongs to (label prefix before the first
    /// `/`). Drives positional clustering in the `force` layout — see `cluster_of`.
    cluster: String,
    val: usize,
    color: String,
    kind: String,
    evidence: String,
    members: Vec<Member>,
    /// L2-normalized mean of this capability's member embeddings (dim = the
    /// corpus embedding dim), rounded to 4 dp. Only emitted in `meaning` layout;
    /// `None` (omitted) in `force` layout or when no member matched an embedding.
    /// The browser projects these to 2D with UMAP.
    #[serde(skip_serializing_if = "Option::is_none")]
    centroid: Option<Vec<f64>>,
    /// Meaning-layout semantic region id (a k-means cluster over centroids).
    /// `None` (omitted) in force layout or for an unplaced capability.
    #[serde(skip_serializing_if = "Option::is_none")]
    region: Option<usize>,
}

/// A named semantic region — a k-means cluster of capability centroids, named
/// by the daemon LLM (or a crate-prefix fallback when it is unreachable).
#[derive(Serialize)]
struct RegionMeta {
    id: usize,
    name: String,
}

/// Which layout the emitted HTML uses.
#[derive(Clone, Copy, PartialEq)]
enum Layout {
    /// Force-directed by call structure, nodes clustered by crate. (default)
    Force,
    /// Fixed positions from a 2D UMAP projection of capability embeddings.
    Meaning,
}

// ─── function-embedding cache (read-only consumer of spec-reconcile's output) ─

/// Sidecar for `<data_dir>/specs/_fn_vecs/<corpus>.json`. We read the shape,
/// per-row meta, AND the per-row `summary` (the exact text spec-reconcile
/// embedded — identical to `code_intel_cache.json[i].summary` but guaranteed
/// row-aligned with the matrix, so no cross-file index is needed). Row `i` of
/// the `.bin` matrix (raw little-endian f32, `count × dim`, row-major) ↔ `fns[i]`.
#[derive(Deserialize)]
struct FnVecSidecar {
    dim: usize,
    count: usize,
    #[serde(default)]
    fns: Vec<FnMeta>,
}

#[derive(Deserialize)]
struct FnMeta {
    name: String,
    file: String,
    /// 1-based line. NOTE: the SCIP symbols table's `line_start` is 0-based, so
    /// the join key is `file:(line_start + 1)` — see `cmd_capability_graph`.
    line: i64,
    /// The function's natural-language summary — the exact text embedded, so it
    /// is perfectly row-aligned. Used to name semantic regions (capability
    /// labels like `*_cmd` bias the LLM toward generic "command execution").
    #[serde(default)]
    summary: String,
}

/// The loaded embedding matrix plus two lookup indices for resolving a
/// capability member to its row.
struct FnVecs {
    dim: usize,
    /// Raw `count × dim` little-endian f32 matrix; rows sliced on demand.
    bytes: Vec<u8>,
    /// `"file:line"` (line 1-based, matching the sidecar) → row index.
    by_fileline: HashMap<String, usize>,
    /// `(file, bare-name)` → row index. Fallback when the precise line misses.
    by_fileleaf: HashMap<(String, String), usize>,
    /// Per-row function summary (indexed by row). Region-naming signal.
    summaries: Vec<String>,
}

impl FnVecs {
    /// Add row `i`'s f32 values element-wise into `acc` (a `dim`-length f64 accumulator).
    fn add_row_into(&self, i: usize, acc: &mut [f64]) {
        let start = i * self.dim * 4;
        let row = &self.bytes[start..start + self.dim * 4];
        for (j, b) in row.chunks_exact(4).enumerate() {
            acc[j] += f32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64;
        }
    }
}

/// Load the cached function embeddings for a corpus. `Err` carries a
/// user-facing message (missing files → the "run spec-reconcile" hint).
fn load_fn_vecs(data_dir: &Path, corpus_id: &str) -> Result<FnVecs, String> {
    let dir = data_dir.join("specs").join("_fn_vecs");
    let bin_path = dir.join(format!("{corpus_id}.bin"));
    let side_path = dir.join(format!("{corpus_id}.json"));
    let missing = || {
        format!(
            "no embeddings for {corpus_id} — run `svrn enrich spec-reconcile {corpus_id} \
             --spec <any>` once to build them, or use --layout force."
        )
    };
    let side_raw = std::fs::read_to_string(&side_path).map_err(|_| missing())?;
    let side: FnVecSidecar =
        serde_json::from_str(&side_raw).map_err(|e| format!("parsing {}: {e}", side_path.display()))?;
    if side.dim == 0 {
        return Err(format!("embedding dim is 0 for {corpus_id} — re-run spec-reconcile"));
    }
    let bytes = std::fs::read(&bin_path).map_err(|_| missing())?;
    let expected = side.count.saturating_mul(side.dim).saturating_mul(4);
    if bytes.len() != expected {
        return Err(format!(
            "embedding matrix {} is {} bytes, expected {}×{}×4 = {} — re-run spec-reconcile",
            bin_path.display(),
            bytes.len(),
            side.count,
            side.dim,
            expected
        ));
    }
    let mut by_fileline = HashMap::new();
    let mut by_fileleaf = HashMap::new();
    let mut summaries = Vec::with_capacity(side.fns.len());
    for (i, m) in side.fns.iter().enumerate() {
        by_fileline.entry(format!("{}:{}", m.file, m.line)).or_insert(i);
        by_fileleaf
            .entry((m.file.clone(), m.name.clone()))
            .or_insert(i);
        summaries.push(m.summary.clone());
    }
    Ok(FnVecs {
        dim: side.dim,
        bytes,
        by_fileline,
        by_fileleaf,
        summaries,
    })
}

#[derive(Serialize)]
struct Link {
    source: usize,
    target: usize,
    weight: u32,
}

#[derive(Serialize)]
struct GraphData {
    corpus: String,
    nodes: Vec<Node>,
    links: Vec<Link>,
    /// Named semantic regions (meaning layout only). Empty (omitted) otherwise.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    regions: Vec<RegionMeta>,
}

// ─── command ───────────────────────────────────────────────────────────────

pub async fn cmd_capability_graph(args: &[String]) -> i32 {
    const USAGE: &str = "usage: sovereign code capability-graph <corpus-id> \
         [--layout force|meaning] [--regions N] [--out <path>] [--open]";
    let mut corpus_id: Option<String> = None;
    let mut out_path: Option<PathBuf> = None;
    let mut layout = Layout::Force;
    let mut regions_arg: Option<usize> = None;
    let mut open_flag = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--help" | "-h" | "help" => {
                println!("{USAGE}");
                return 0;
            }
            "--out" => {
                i += 1;
                out_path = args.get(i).map(PathBuf::from);
                if out_path.is_none() {
                    eprintln!("error: --out requires a value");
                    return 1;
                }
            }
            "--layout" => {
                i += 1;
                match args.get(i).map(String::as_str) {
                    Some("force") => layout = Layout::Force,
                    Some("meaning") => layout = Layout::Meaning,
                    _ => {
                        eprintln!("error: --layout must be force|meaning");
                        return 1;
                    }
                }
            }
            "--regions" => {
                i += 1;
                match args.get(i).and_then(|s| s.parse::<usize>().ok()) {
                    Some(n) if n >= 1 => regions_arg = Some(n),
                    _ => {
                        eprintln!("error: --regions requires a positive integer");
                        return 1;
                    }
                }
            }
            "--open" => open_flag = true,
            flag if flag.starts_with('-') => {
                eprintln!("error: unknown flag {flag}");
                return 1;
            }
            positional => {
                if corpus_id.is_none() {
                    corpus_id = Some(positional.to_string());
                }
            }
        }
        i += 1;
    }

    let corpus_id = match corpus_id {
        Some(c) => c,
        None => {
            eprintln!("{USAGE}");
            return 1;
        }
    };

    // ── load the SCIP graph (mirrors `cmd_capability_map`) ──
    let db_path = home_dir()
        .join(".sovereign")
        .join("indexes")
        .join(&corpus_id)
        .join("scip_graph.db");
    if !db_path.exists() {
        eprintln!(
            "error: no SCIP graph at {} — run `svrn code capability-map {corpus_id}` first",
            db_path.display()
        );
        return 1;
    }
    let graph = match corpus_engine_scip::ScipGraph::open(&db_path, &corpus_id) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("error: cannot open SCIP graph: {e}");
            return 1;
        }
    };
    let symbols = match graph.iter_all_symbols().await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: reading symbols: {e}");
            return 1;
        }
    };
    let refs = match graph.iter_all_refs().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: reading refs: {e}");
            return 1;
        }
    };

    // ── load the capability artifacts ──
    let caps_dir = home_dir()
        .join(".sovereign")
        .join("capabilities")
        .join(&corpus_id);

    let map_path = caps_dir.join("capability_map.json");
    let cap_map: CapMapFile = match std::fs::read_to_string(&map_path) {
        Ok(raw) => match serde_json::from_str(&raw) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("error: parsing {}: {e}", map_path.display());
                return 1;
            }
        },
        Err(e) => {
            eprintln!(
                "error: cannot read {} ({e}) — run `svrn code capability-map {corpus_id}` first",
                map_path.display()
            );
            return 1;
        }
    };

    // Findings are optional: no file → every node gets the neutral colour.
    // Keyed by (label, n_entries, n_core) because labels collide — see the
    // note on `Cap`.
    let findings_path = caps_dir.join("capability_findings.json");
    let findings_by_key: HashMap<(String, usize, usize), (String, String)> =
        match std::fs::read_to_string(&findings_path) {
            Ok(raw) => match serde_json::from_str::<FindingsFile>(&raw) {
                Ok(f) => f
                    .findings
                    .into_iter()
                    .map(|x| {
                        (
                            finding_key(&x.label, x.n_entries, x.n_core),
                            (x.kind, x.evidence.unwrap_or_default()),
                        )
                    })
                    .collect(),
                Err(e) => {
                    eprintln!(
                        "warning: ignoring malformed {}: {e}",
                        findings_path.display()
                    );
                    HashMap::new()
                }
            },
            Err(_) => HashMap::new(),
        };

    // ── qualified name → (file, line_start) — for the side-panel "file:line"
    //    AND the embedding join key. `line_start` is 0-based here. ──
    let mut pos_by_qn: HashMap<&str, (&str, i32)> = HashMap::new();
    for s in &symbols {
        if !s.qualified_name.is_empty() {
            pos_by_qn
                .entry(s.qualified_name.as_str())
                .or_insert((s.file_path.as_str(), s.line_start));
        }
    }

    // ── meaning layout: load the cached function embeddings up front so a
    //    missing/stale cache fails fast with a clear hint. ──
    let fn_vecs = if layout == Layout::Meaning {
        let data_dir = home_dir().join(".sovereign");
        match load_fn_vecs(&data_dir, &corpus_id) {
            Ok(fv) => Some(fv),
            Err(msg) => {
                eprintln!("error: {msg}");
                return 1;
            }
        }
    } else {
        None
    };

    // Resolve a capability member to its embedding row: precise `file:(line+1)`
    // first (SCIP line_start is 0-based, the sidecar line is 1-based), then a
    // `(file, leaf-name)` fallback. `leaf` is the member's display name.
    let match_row = |qn: &str, leaf: &str| -> Option<usize> {
        let fv = fn_vecs.as_ref()?;
        let &(file, line) = pos_by_qn.get(qn)?;
        let key = format!("{file}:{}", line + 1);
        fv.by_fileline
            .get(&key)
            .copied()
            .or_else(|| fv.by_fileleaf.get(&(file.to_string(), leaf.to_string())).copied())
    };

    // ── fn_to_cap: qualified name → capability index (first cap wins) ──
    let mut fn_to_cap: HashMap<&str, usize> = HashMap::new();
    for (idx, cap) in cap_map.capabilities.iter().enumerate() {
        for qn in cap.core.iter().chain(cap.entries.iter()) {
            fn_to_cap.entry(qn.as_str()).or_insert(idx);
        }
    }

    // ── capability edges: map each ref's endpoints through fn_to_cap ──
    // Refs that touch a non-capability symbol drop out naturally (the lookup
    // misses); self-loops are skipped.
    let mut weights: HashMap<(usize, usize), u32> = HashMap::new();
    for r in &refs {
        let (Some(&src), Some(&dst)) = (
            fn_to_cap.get(r.caller_qualified.as_str()),
            fn_to_cap.get(r.callee_qualified.as_str()),
        ) else {
            continue;
        };
        if src == dst {
            continue;
        }
        *weights.entry((src, dst)).or_insert(0) += 1;
    }

    // ── build nodes ──
    let dim = fn_vecs.as_ref().map(|f| f.dim).unwrap_or(0);
    let mut total_members = 0usize; // for the meaning match-rate report
    let mut total_matched = 0usize;
    let mut caps_placed = 0usize;
    // Per-cap matched embedding rows (aligned with node index) — region naming
    // pulls summaries from these. Empty in force layout.
    let mut cap_rows: Vec<Vec<usize>> = Vec::with_capacity(cap_map.capabilities.len());
    let mut nodes: Vec<Node> = Vec::with_capacity(cap_map.capabilities.len());
    for (idx, cap) in cap_map.capabilities.iter().enumerate() {
        let (kind, evidence) = findings_by_key
            .get(&finding_key(&cap.label, cap.n_entries, cap.n_core))
            .cloned()
            .unwrap_or_default();
        let color = color_for(&kind).to_string();

        // members = entries (is_entry true) first, then any core not already
        // listed (is_entry false). Dedupe so a function that is both an entry
        // and on the core spine appears once, marked as an entry. In meaning
        // mode we accumulate each member's embedding into the centroid here.
        let mut members: Vec<Member> = Vec::new();
        let mut seen: HashSet<&str> = HashSet::new();
        let mut acc = vec![0.0f64; dim];
        let mut matched = 0usize;
        let mut rows: Vec<usize> = Vec::new();
        let mut add_member = |qn: &str, is_entry: bool| {
            let name = display_name(qn);
            if let Some(fv) = fn_vecs.as_ref() {
                total_members += 1;
                if let Some(r) = match_row(qn, &name) {
                    matched += 1;
                    rows.push(r);
                    fv.add_row_into(r, &mut acc);
                }
            }
            members.push(Member {
                name,
                loc: pos_by_qn.get(qn).map(|(f, l)| format!("{f}:{l}")),
                is_entry,
            });
        };
        for qn in &cap.entries {
            if seen.insert(qn.as_str()) {
                add_member(qn, true);
            }
        }
        for qn in &cap.core {
            if seen.insert(qn.as_str()) {
                add_member(qn, false);
            }
        }

        // Finalize the centroid: mean of matched rows, then L2-normalize,
        // rounded to 4 dp to keep the inlined JSON small.
        let centroid = if matched > 0 {
            for a in acc.iter_mut() {
                *a /= matched as f64;
            }
            let norm = acc.iter().map(|x| x * x).sum::<f64>().sqrt();
            if norm > 1e-12 {
                for a in acc.iter_mut() {
                    *a /= norm;
                }
            }
            caps_placed += 1;
            Some(acc.iter().map(|&x| (x * 1e4).round() / 1e4).collect())
        } else {
            None
        };
        total_matched += matched;
        cap_rows.push(rows);

        nodes.push(Node {
            id: idx,
            cluster: cluster_of(&cap.label).to_string(),
            label: cap.label.clone(),
            val: cap.n_core,
            color,
            kind,
            evidence,
            members,
            centroid,
            region: None,
        });
    }

    // ── build links (sorted for stable, diff-friendly output) ──
    let mut links: Vec<Link> = weights
        .into_iter()
        .map(|((source, target), weight)| Link {
            source,
            target,
            weight,
        })
        .collect();
    links.sort_by_key(|l| (l.source, l.target));

    let n_nodes = nodes.len();
    let n_links = links.len();

    // ── meaning layout: cluster the placed centroids into semantic regions
    //    (k-means), then name each from its members' SUMMARIES via one batched
    //    daemon call (→ per-region daemon → crate-prefix fallback). ──
    let mut regions: Vec<RegionMeta> = Vec::new();
    if layout == Layout::Meaning {
        let placed: Vec<usize> = nodes
            .iter()
            .enumerate()
            .filter(|(_, n)| n.centroid.is_some())
            .map(|(i, _)| i)
            .collect();
        if !placed.is_empty() {
            let points: Vec<&[f64]> = placed
                .iter()
                .map(|&i| nodes[i].centroid.as_deref().unwrap())
                .collect();
            // K: explicit --regions, else 16 (was 12 — split the megacluster);
            // clamped to #placed capabilities.
            let k = regions_arg.unwrap_or(16).clamp(1, placed.len());
            let assign = kmeans(&points, k, 20, 0x5EED_C0DE);
            for (pi, &ni) in placed.iter().enumerate() {
                nodes[ni].region = Some(assign[pi]);
            }

            // Naming signal: ~8 member-function SUMMARIES per region, sampled
            // evenly across the region. Summaries (not `*_cmd` labels) describe
            // behavior, so the model names the actual shared concern. They come
            // from the embedding sidecar — row-aligned with the centroids.
            let fv = fn_vecs.as_ref().expect("meaning layout loads fn_vecs");
            let mut region_rows: Vec<Vec<usize>> = vec![Vec::new(); k];
            for &ni in &placed {
                let r = nodes[ni].region.unwrap();
                region_rows[r].extend_from_slice(&cap_rows[ni]);
            }
            let region_samples: Vec<Vec<String>> = region_rows
                .iter_mut()
                .map(|rows| {
                    rows.sort_unstable();
                    rows.dedup();
                    sample_evenly(rows, 8)
                        .into_iter()
                        .filter_map(|row| {
                            let s = fv.summaries.get(row)?.trim();
                            (!s.is_empty()).then(|| s.to_string())
                        })
                        .collect()
                })
                .collect();

            // Naming fallback chain: batched (one call → distinct names) →
            // per-region daemon → crate-prefix. Log which path was used.
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(120))
                .build()
                .ok();
            let daemon_url = "http://localhost:9741";
            let mut names: Option<Vec<String>> = None;
            let mut path = "crate-fallback";
            if let Some(c) = client.as_ref() {
                if let Some(n) = name_regions_batched(c, daemon_url, &region_samples).await {
                    names = Some(n);
                    path = "batched-daemon";
                } else {
                    let mut per = Vec::with_capacity(k);
                    let mut ok = true;
                    for samples in &region_samples {
                        match name_one_region(c, daemon_url, samples).await {
                            Some(nm) => per.push(nm),
                            None => {
                                ok = false;
                                break;
                            }
                        }
                    }
                    if ok && per.len() == k {
                        names = Some(per);
                        path = "per-region-daemon";
                    }
                }
            }
            let names = names
                .unwrap_or_else(|| (0..k).map(|r| region_crate_name(r, &placed, &nodes)).collect());
            regions = names
                .into_iter()
                .enumerate()
                .map(|(id, name)| RegionMeta { id, name })
                .collect();
            println!("  {k} semantic regions named via {path}");
        }
    }

    // ── inline the data into the self-contained HTML ──
    let data = GraphData {
        corpus: corpus_id.clone(),
        nodes,
        links,
        regions,
    };
    let data_json = match serde_json::to_string(&data) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: serializing graph data: {e}");
            return 1;
        }
    };
    // Guard the inline `<script>`: never let a `</` inside a string close it.
    let data_json = data_json.replace("</", "<\\/");

    // Per-mode wiring: which layout the JS runs, which extra CDN libs it needs,
    // and the default output filename (so meaning mode doesn't clobber the force
    // graph). force-graph is always loaded. BOTH layouts now need FULL d3 (the
    // standalone d3-force omits its d3-quadtree peer dep → forceManyBody/
    // forceCollide throw); meaning additionally needs umap-js for the one-time
    // projection. Only the active layout's branch runs in the JS.
    let (layout_tag, scripts, default_name) = match layout {
        Layout::Force => (
            "force",
            "  <script src=\"https://unpkg.com/d3@7/dist/d3.min.js\"></script>",
            "graph.html",
        ),
        Layout::Meaning => (
            "meaning",
            "  <script src=\"https://unpkg.com/umap-js\"></script>\n  \
             <script src=\"https://unpkg.com/d3@7/dist/d3.min.js\"></script>",
            "meaning_map.html",
        ),
    };
    let html = HTML_TEMPLATE
        .replace("__DATA__", &data_json)
        .replace("__LAYOUT__", layout_tag)
        .replace("__SCRIPTS__", scripts);

    let out_path = out_path.unwrap_or_else(|| caps_dir.join(default_name));
    if let Some(parent) = out_path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            eprintln!("error: cannot create {}: {e}", parent.display());
            return 1;
        }
    }
    if let Err(e) = std::fs::write(&out_path, html) {
        eprintln!("error: writing {}: {e}", out_path.display());
        return 1;
    }

    let abs = std::fs::canonicalize(&out_path).unwrap_or(out_path);
    match layout {
        Layout::Force => println!(
            "wrote {} — {n_nodes} capabilities, {n_links} call-derived edges (force layout)",
            abs.display()
        ),
        Layout::Meaning => {
            let pct = if total_members > 0 {
                100.0 * total_matched as f64 / total_members as f64
            } else {
                0.0
            };
            let avg = if n_nodes > 0 {
                total_matched as f64 / n_nodes as f64
            } else {
                0.0
            };
            println!(
                "wrote {} — meaning layout (UMAP of capability embeddings)",
                abs.display()
            );
            println!(
                "  {caps_placed}/{n_nodes} capabilities placed; \
                 {total_matched}/{total_members} members matched to embeddings ({pct:.1}%); \
                 avg {avg:.1} matched/cap"
            );
            if caps_placed < n_nodes {
                println!(
                    "  note: {} capability(ies) had no matched member — pinned at the origin",
                    n_nodes - caps_placed
                );
            }
            if total_members > 0 && pct < 60.0 {
                eprintln!(
                    "  warning: low embedding match rate ({pct:.1}%) — the file:line key may be \
                     stale; positions will be coarse"
                );
            }
        }
    }
    if open_flag {
        open_in_browser(&abs);
    }
    0
}

// ─── helpers ─────────────────────────────────────────────────────────────

fn home_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"))
}

/// Open an emitted artifact in the OS default application (the browser), so a CLI
/// user lands on the rendered graph without copy-pasting the path. Best-effort:
/// spawns the platform opener and does not wait; the path was already printed, so
/// on failure we just note it and let the user open it by hand.
fn open_in_browser(path: &Path) {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(target_os = "windows") {
        "explorer"
    } else {
        "xdg-open"
    };
    match std::process::Command::new(opener).arg(path).spawn() {
        Ok(_) => println!("  opening in your browser…"),
        Err(e) => eprintln!("  note: couldn't launch {opener} ({e}); open the path above manually"),
    }
}

/// Composite join key for matching a capability to its finding. A capability
/// `label` is NOT unique across clusters (e.g. two `<pkg>/(root)` capabilities),
/// so the cluster shape `(n_entries, n_core)` disambiguates. Both the map and
/// the findings file carry these counts; when a findings file predates them
/// they default to `0` and this degrades exactly to a label-only join.
fn finding_key(label: &str, n_entries: usize, n_core: usize) -> (String, usize, usize) {
    (label.to_string(), n_entries, n_core)
}

/// The crate/package a capability belongs to: its `label` prefix before the
/// first `/` (e.g. `corpus-engine/raptor` → `corpus-engine`). Used as the
/// positional cluster key in the layout. Labels with no `/` cluster on
/// themselves.
fn cluster_of(label: &str) -> &str {
    label.split('/').next().unwrap_or(label)
}

/// Node colour from the reconciliation verdict.
fn color_for(kind: &str) -> &'static str {
    match kind {
        "corroborated" => "#4ade80",
        "undocumented" => "#fbbf24",
        "drifted" => "#f87171",
        _ => "#94a3b8",
    }
}

/// Readable leaf name for a SCIP qualified name. Uses the public spec helper
/// [`corpus_engine_scip::capability_map::pkg_and_desc`] to strip the
/// `scheme manager package version` prefix, then takes the final descriptor
/// token (the method/type identifier). Falls back to the raw input for the
/// odd `local …` / non-global symbol.
fn display_name(qn: &str) -> String {
    let desc = corpus_engine_scip::capability_map::pkg_and_desc(qn)
        .map(|(_pkg, desc)| desc)
        .unwrap_or(qn);
    let leaf = desc.rsplit('/').next().unwrap_or(desc);
    let trimmed = leaf.trim_end_matches(|c| matches!(c, '.' | '(' | ')' | '#'));
    let token = trimmed
        .rsplit(|c: char| matches!(c, ']' | '#' | '.' | '/') || c.is_whitespace())
        .find(|s| !s.is_empty())
        .unwrap_or(trimmed);
    if token.is_empty() {
        leaf.to_string()
    } else {
        token.to_string()
    }
}

// ─── semantic regions: k-means + LLM naming ────────────────────────────────

/// Deterministic SplitMix64 PRNG — keeps k-means++ seeding reproducible across
/// runs (same data → same regions) without pulling in `rand`.
struct Lcg(u64);
impl Lcg {
    fn new(seed: u64) -> Self {
        Lcg(seed ^ 0x9E37_79B9_7F4A_7C15)
    }
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

fn sqdist(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b).map(|(x, y)| (x - y) * (x - y)).sum()
}

/// k-means (k-means++ init + Lloyd iterations) over the L2-normalized capability
/// centroids. Euclidean distance on unit vectors is a monotone function of
/// cosine, so this clusters by meaning. Returns the cluster id per input point.
fn kmeans(points: &[&[f64]], k: usize, iters: usize, seed: u64) -> Vec<usize> {
    let n = points.len();
    if n == 0 {
        return Vec::new();
    }
    let k = k.clamp(1, n);
    let dim = points[0].len();
    let mut rng = Lcg::new(seed);

    // k-means++ seeding: first center random, each next ∝ squared distance.
    let mut centers: Vec<Vec<f64>> = Vec::with_capacity(k);
    centers.push(points[rng.below(n)].to_vec());
    let mut d2 = vec![f64::INFINITY; n];
    while centers.len() < k {
        let last = centers.last().unwrap();
        let mut sum = 0.0;
        for (i, p) in points.iter().enumerate() {
            let d = sqdist(p, last);
            if d < d2[i] {
                d2[i] = d;
            }
            sum += d2[i];
        }
        let mut target = rng.unit() * sum;
        let mut chosen = n - 1;
        for (i, &di) in d2.iter().enumerate() {
            target -= di;
            if target <= 0.0 {
                chosen = i;
                break;
            }
        }
        centers.push(points[chosen].to_vec());
    }

    // Lloyd iterations.
    let mut assign = vec![0usize; n];
    for _ in 0..iters {
        let mut changed = false;
        for (i, p) in points.iter().enumerate() {
            let mut best = 0usize;
            let mut best_d = f64::INFINITY;
            for (ci, c) in centers.iter().enumerate() {
                let d = sqdist(p, c);
                if d < best_d {
                    best_d = d;
                    best = ci;
                }
            }
            if assign[i] != best {
                assign[i] = best;
                changed = true;
            }
        }
        let mut sums = vec![vec![0.0f64; dim]; k];
        let mut counts = vec![0usize; k];
        for (i, p) in points.iter().enumerate() {
            let a = assign[i];
            counts[a] += 1;
            for j in 0..dim {
                sums[a][j] += p[j];
            }
        }
        for ci in 0..k {
            if counts[ci] > 0 {
                for j in 0..dim {
                    sums[ci][j] /= counts[ci] as f64;
                }
                centers[ci] = std::mem::take(&mut sums[ci]);
            }
            // Empty cluster: keep its previous center.
        }
        if !changed {
            break;
        }
    }
    assign
}

/// Sample up to `k` items spread evenly across `xs` (so a large region's signal
/// covers it, rather than taking the first `k`).
fn sample_evenly(xs: &[usize], k: usize) -> Vec<usize> {
    let n = xs.len();
    if n <= k {
        return xs.to_vec();
    }
    (0..k).map(|i| xs[i * n / k]).collect()
}

/// Normalize a model-produced name: strip any chain-of-thought, take the first
/// non-empty line, strip surrounding quote/punctuation noise, cap at 40 chars.
fn clean_name(s: &str) -> String {
    let line = s
        .rsplit("</think>")
        .next()
        .unwrap_or(s)
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    line.trim_matches(|c: char| matches!(c, '"' | '\'' | '`' | '.' | ':'))
        .trim()
        .chars()
        .take(40)
        .collect()
}

/// Parse a JSON array of strings, tolerating leading/trailing prose (slice from
/// the first `[` to the last `]`).
fn parse_json_string_array(s: &str) -> Option<Vec<String>> {
    if let Ok(v) = serde_json::from_str::<Vec<String>>(s) {
        return Some(v);
    }
    let start = s.find('[')?;
    let end = s.rfind(']')?;
    (end > start)
        .then(|| serde_json::from_str::<Vec<String>>(&s[start..=end]).ok())
        .flatten()
}

/// Name ALL regions in one daemon call — a json_schema-constrained array of
/// exactly `k` strings, so the model is forced to make them distinct. Returns
/// `None` on any failure (unreachable / non-2xx / unparseable / wrong count).
async fn name_regions_batched(
    client: &reqwest::Client,
    daemon_url: &str,
    region_samples: &[Vec<String>],
) -> Option<Vec<String>> {
    let k = region_samples.len();
    if k == 0 || region_samples.iter().all(Vec::is_empty) {
        return None;
    }
    let mut user = String::new();
    for (i, samples) in region_samples.iter().enumerate() {
        user.push_str(&format!("Group {i} behaviors:\n"));
        if samples.is_empty() {
            user.push_str("- (no summaries available)\n");
        } else {
            for s in samples {
                user.push_str("- ");
                user.push_str(s);
                user.push('\n');
            }
        }
        user.push('\n');
    }
    let url = format!("{}/v1/chat/completions", daemon_url.trim_end_matches('/'));
    let body = serde_json::json!({
        "model": "primary",
        "temperature": 0.2,
        "max_tokens": 64 + 16 * k as u32,
        "response_format": {
            "type": "json_schema",
            "json_schema": {
                "name": "region_names",
                "strict": true,
                "schema": {
                    "type": "array",
                    "items": {"type": "string"},
                    "minItems": k,
                    "maxItems": k
                }
            }
        },
        "messages": [
            {"role": "system", "content": "You name groups of software capabilities by their shared concern. Each group is given as sample behaviors. Give each group a DISTINCT, specific 2-4 word noun phrase — the names MUST differ from one another. Output a JSON array of names, in group order."},
            {"role": "user", "content": user},
        ],
    });
    let resp = client.post(&url).json(&body).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let v: serde_json::Value = resp.json().await.ok()?;
    let content = v
        .pointer("/choices/0/message/content")
        .and_then(serde_json::Value::as_str)?;
    let content = content.rsplit("</think>").next().unwrap_or(content).trim();
    let arr = parse_json_string_array(content)?;
    if arr.len() != k {
        return None;
    }
    Some(arr.iter().map(|s| clean_name(s)).collect())
}

/// Per-region fallback: name one region from its sampled summaries.
async fn name_one_region(
    client: &reqwest::Client,
    daemon_url: &str,
    samples: &[String],
) -> Option<String> {
    if samples.is_empty() {
        return None;
    }
    let mut user = String::from("Behaviors:\n");
    for s in samples {
        user.push_str("- ");
        user.push_str(s);
        user.push('\n');
    }
    user.push_str("\nName:");
    let url = format!("{}/v1/chat/completions", daemon_url.trim_end_matches('/'));
    let body = serde_json::json!({
        "model": "primary",
        "temperature": 0.2,
        "max_tokens": 24,
        "messages": [
            {"role": "system", "content": "You name a group of software capabilities by their single shared concern. Reply with ONLY a 2-4 word noun phrase (e.g. 'version parsing', 'mesh inference routing'). No punctuation, no explanation."},
            {"role": "user", "content": user},
        ],
    });
    let resp = client.post(&url).json(&body).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let v: serde_json::Value = resp.json().await.ok()?;
    let content = v
        .pointer("/choices/0/message/content")
        .and_then(serde_json::Value::as_str)?;
    let name = clean_name(content);
    (!name.is_empty()).then_some(name)
}

/// Offline fallback: name a region by its most common crate prefix.
fn region_crate_name(region: usize, placed: &[usize], nodes: &[Node]) -> String {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for &ni in placed {
        if nodes[ni].region == Some(region) {
            *counts.entry(nodes[ni].cluster.as_str()).or_insert(0) += 1;
        }
    }
    counts
        .into_iter()
        .max_by_key(|(_, c)| *c)
        .map(|(name, _)| name.to_string())
        .unwrap_or_else(|| "misc".to_string())
}

// ─── the self-contained HTML (force-graph from CDN; data inlined) ──────────

const HTML_TEMPLATE: &str = r##"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Capability graph</title>
<style>
  :root { color-scheme: dark; }
  * { box-sizing: border-box; }
  html, body { margin: 0; height: 100%; }
  body {
    display: flex; flex-direction: column; height: 100vh;
    background: #0f172a; color: #e2e8f0;
    font: 14px/1.5 ui-sans-serif, system-ui, -apple-system, "Segoe UI", Roboto, sans-serif;
  }
  #header {
    padding: 10px 16px; border-bottom: 1px solid #1e293b;
    display: flex; flex-wrap: wrap; align-items: center; gap: 16px;
  }
  #title { font-weight: 600; font-size: 15px; white-space: nowrap; }
  #title .dim { color: #94a3b8; font-weight: 400; }
  #search {
    flex: 0 1 280px; padding: 6px 10px; border-radius: 6px;
    border: 1px solid #334155; background: #1e293b; color: #e2e8f0; font: inherit;
  }
  #search::placeholder { color: #64748b; }
  #legend { display: flex; flex-wrap: wrap; gap: 14px; margin-left: auto; }
  #legend span { display: inline-flex; align-items: center; gap: 6px; color: #cbd5e1; font-size: 12px; }
  .swatch { width: 11px; height: 11px; border-radius: 50%; display: inline-block; }
  #main { display: flex; flex: 1; min-height: 0; }
  #graph { flex: 1; position: relative; min-width: 0; }
  #panel {
    width: 360px; flex: none; overflow: auto; padding: 16px 18px;
    border-left: 1px solid #1e293b; background: #0b1220;
  }
  #panel h2 { margin: 0 0 6px; font-size: 16px; word-break: break-word; }
  #panel .tag {
    display: inline-block; padding: 2px 9px; border-radius: 999px;
    font-size: 11px; font-weight: 600; color: #0b1220; margin-bottom: 10px;
  }
  #panel .evidence { color: #cbd5e1; margin: 0 0 14px; font-size: 13px; }
  #panel h3 { margin: 16px 0 8px; font-size: 12px; text-transform: uppercase; letter-spacing: .04em; color: #94a3b8; }
  #panel ul { list-style: none; margin: 0; padding: 0; }
  #panel li { padding: 4px 0; border-bottom: 1px solid #131c2e; font-size: 13px; word-break: break-word; }
  #panel li .nm { color: #e2e8f0; }
  #panel li.entry .nm { color: #93c5fd; font-weight: 600; }
  #panel li .entry-mark { color: #93c5fd; margin-right: 5px; }
  #panel li .loc { display: block; color: #64748b; font-size: 11px; font-family: ui-monospace, SFMono-Regular, Menlo, monospace; }
  #panel .empty { color: #64748b; }
  #panel .hint { color: #475569; font-style: italic; }
  .toggle { display: inline-flex; align-items: center; gap: 6px; color: #cbd5e1; font-size: 12px; cursor: pointer; white-space: nowrap; }
  .toggle input { accent-color: #93c5fd; margin: 0; }
  #dispWrap input[type=range] { width: 110px; accent-color: #93c5fd; }
  #dispVal { color: #93c5fd; min-width: 2.6em; display: inline-block; text-align: right; font-variant-numeric: tabular-nums; }
</style>
</head>
<body>
  <div id="header">
    <div id="title"></div>
    <input id="search" type="search" placeholder="filter capabilities…" autocomplete="off">
    <label class="toggle" id="toggleHide"><input id="hideUnconnected" type="checkbox"> hide unconnected</label>
    <label class="toggle" id="toggleEdges"><input id="showEdges" type="checkbox" checked> show call edges</label>
    <label class="toggle" id="dispWrap" title="spread nodes apart (higher = more spread)">dispersion <input type="range" id="dispersion" min="0.4" max="3" step="0.05" value="1"><span id="dispVal">1.00</span></label>
    <div id="legend">
      <span><i class="swatch" style="background:#4ade80"></i>corroborated</span>
      <span><i class="swatch" style="background:#fbbf24"></i>undocumented</span>
      <span><i class="swatch" style="background:#f87171"></i>drifted</span>
      <span><i class="swatch" style="background:#94a3b8"></i>undocumented-or-unknown</span>
    </div>
  </div>
  <div id="main">
    <div id="graph"></div>
    <div id="panel"><p class="hint">Click a capability to inspect its verdict and functions.</p></div>
  </div>

  <!-- Rendering (force-graph, always) + the per-layout helper, injected by the
       emitting Rust (__SCRIPTS__): force layout = FULL d3 (d3-force alone omits
       its d3-quadtree peer dep, so forceManyBody/forceCollide throw "quadtree is
       not a function" and blank the canvas); meaning layout = umap-js. All from
       CDN; see TODO(offline) in the emitting Rust. -->
  <script src="https://unpkg.com/force-graph"></script>
__SCRIPTS__
  <script>
    const DATA = __DATA__;
    const LAYOUT = '__LAYOUT__';   // 'force' | 'meaning'

    // ── header ──
    const count = (k) => DATA.nodes.filter(n => n.kind === k).length;
    document.getElementById('title').innerHTML =
      '<b>' + esc(DATA.corpus) + '</b>'
      + ' <span class="dim">· ' + DATA.nodes.length + ' capabilities'
      + ' · ' + count('corroborated') + ' corroborated'
      + ' · ' + count('undocumented') + ' undocumented'
      + ' · ' + count('drifted') + ' drifted'
      + (LAYOUT === 'meaning' ? ' · meaning layout (UMAP of capability embeddings)' : '')
      + '</span>';

    // ── filter state ──
    let query = '';
    let hideUnconnected = false;   // force-layout toggle
    let showEdges = true;          // meaning-layout toggle (edges are faint context)
    let fitted = false;
    let regionGeom = [];           // meaning-layout: per-region {cx,cy,hull,name,tint}
    let dispersion = 1.0;          // meaning-layout spread dial (live; 1.0 ≈ raw UMAP)
    const NODE_REL_SIZE = 5;       // node radius factor — shared by render + collide
    const matches = (n) => !query || (n.label || '').toLowerCase().includes(query);
    const DIM = 'rgba(100,116,139,0.12)';

    // ── node degree (computed BEFORE force-graph mutates link endpoints from
    //    ids to node objects) — drives the "hide unconnected" toggle. ──
    const degree = {};
    for (const n of DATA.nodes) degree[n.id] = 0;
    for (const l of DATA.links) { degree[l.source]++; degree[l.target]++; }

    // ── package clustering: one centroid per crate, placed evenly on a ring.
    //    The forceX/forceY pull (below) seats every node — including edgeless
    //    ones — in its crate region, so there is no floating halo. ──
    const clusters = [...new Set(DATA.nodes.map(n => n.cluster))].sort();
    const K = clusters.length;
    // Size-proportional ring: each crate is allocated arc ∝ its blob radius, so a
    // big crate (sovereign-cli-llm, ~77 nodes) gets far more room than a 2-node one
    // and the clusters stop overlapping. blob radius ≈ the packing radius of `size`
    // collide-disks. Bump SPREAD if crates still touch; lower it to compact.
    const SPREAD = 20;
    const sizeOf = {};
    for (const n of DATA.nodes) sizeOf[n.cluster] = (sizeOf[n.cluster] || 0) + 1;
    const radiusOf = (c) => SPREAD * Math.sqrt(sizeOf[c]) + 40;
    const totalArc = clusters.reduce((s, c) => s + 2 * radiusOf(c), 0);
    const RING = Math.max(totalArc / (2 * Math.PI), 220);
    const O = { x: 0, y: 0 };
    const centroids = {};
    let acc = 0;
    clusters.forEach((c) => {
      const arc = 2 * radiusOf(c);
      const a = (acc + arc / 2) / RING;            // centroid at this crate's arc midpoint
      centroids[c] = { x: RING * Math.cos(a), y: RING * Math.sin(a) };
      acc += arc;
    });
    const centroid = (n) => centroids[n.cluster] || O;

    const el = document.getElementById('graph');
    const Graph = ForceGraph()(el)
      .graphData(DATA)
      .backgroundColor('#0f172a')
      .nodeId('id')
      .nodeLabel(n => n.cluster + '  ·  ' + n.label + (n.kind ? '  ·  ' + n.kind : ''))
      .nodeVal(n => Math.max(1, n.val))
      .nodeRelSize(NODE_REL_SIZE)
      .nodeColor(n => matches(n) ? n.color : DIM)
      .nodeVisibility(n => !hideUnconnected || degree[n.id] > 0)
      .linkColor(l => LAYOUT === 'meaning'
        ? 'rgba(148,163,184,0.12)'   // meaning: edges are faint context
        : (matches(node(l.source)) && matches(node(l.target)) ? 'rgba(148,163,184,0.35)' : DIM))
      .linkVisibility(l => LAYOUT !== 'meaning' || showEdges)
      .linkWidth(l => 1 + Math.sqrt(l.weight || 1))
      .linkDirectionalArrowLength(3.5)
      .linkDirectionalArrowRelPos(1)
      .onNodeClick(showPanel)
      // meaning: recompute region geometry from live positions, then draw the
      // faint blobs behind the nodes; force: nothing pre-draws.
      .onRenderFramePre((ctx, scale) => {
        if (LAYOUT === 'meaning') { rebuildRegionGeom(); drawRegionBlobs(ctx, scale); }
      })
      // meaning: semantic region headers on top; force: crate-centroid labels.
      .onRenderFramePost((ctx, scale) => {
        if (LAYOUT === 'meaning') drawRegionLabels(ctx, scale); else drawClusterLabels(ctx, scale);
      })
      .onEngineStop(() => { if (!fitted) { fitted = true; Graph.zoomToFit(500, 60); } });

    // force-graph may pass either the raw id or the resolved node object.
    function node(ref) { return (ref && typeof ref === 'object') ? ref : DATA.nodes[ref]; }

    // ── layout ──
    if (LAYOUT === 'meaning') {
      layoutByMeaning();
    } else {
      // Force clustering (full d3 from CDN). Tuned on commonwealth-ai (226
      // nodes, ~20 crates): centroid pull (0.35) is strong enough to seat
      // edgeless nodes in their crate; charge spreads nodes within a cluster;
      // collide stops overlap; the link force gets a short fixed distance.
      // Wrapped so any force failure degrades to the base layout, never blank.
      try {
        Graph.d3Force('x', d3.forceX(n => centroid(n).x).strength(0.35));
        Graph.d3Force('y', d3.forceY(n => centroid(n).y).strength(0.35));
        Graph.d3Force('charge', d3.forceManyBody().strength(-90));
        Graph.d3Force('collide', d3.forceCollide(n => Math.sqrt(Math.max(1, n.val)) + 6));
        Graph.d3Force('center', null);   // centroids define position; drop global centering
        const linkForce = Graph.d3Force('link');
        if (linkForce && linkForce.distance) linkForce.distance(40);
        Graph.d3ReheatSimulation();
      } catch (e) {
        console.warn('[capability-graph] clustering forces unavailable; base layout used:', e);
      }
    }

    // ── meaning layout: a 2D UMAP projection of the per-capability embeddings
    //    (inlined as `centroid`) becomes each node's ANCHOR (ux,uy). Nodes are
    //    not pinned — a forceX/forceY pulls them to anchor×dispersion (live
    //    slider) and a collide de-stacks dense spots. UMAP runs once; the dial
    //    only rescales the anchors, so re-spreading is instant (no re-fit).
    //    Wrapped in try/catch with a circle fallback (umap needs neighbors). ──
    function layoutByMeaning() {
      const proj = DATA.nodes.filter(n => Array.isArray(n.centroid));
      let coords = [];
      try {
        if (proj.length <= 2) {
          coords = proj.map((_, i) => [i * 50, 0]);
        } else {
          // umap-js's UMD global is the module namespace ({UMAP: class}), not
          // the class itself — so the constructor is `UMAP.UMAP`. Tolerate both
          // shapes; a miss throws → the circle fallback below catches it.
          const UMAPCtor = (typeof UMAP === 'function') ? UMAP : (UMAP && UMAP.UMAP);
          if (typeof UMAPCtor !== 'function') throw new Error('umap-js constructor not found');
          const umap = new UMAPCtor({
            nComponents: 2,
            nNeighbors: Math.min(15, proj.length - 1),
            minDist: 0.1,
          });
          coords = umap.fit(proj.map(n => n.centroid));
        }
      } catch (err) {
        console.warn('[capability-graph] UMAP failed; circle fallback used:', err);
        coords = proj.map((_, i) => {
          const a = 2 * Math.PI * i / Math.max(1, proj.length);
          return [Math.cos(a), Math.sin(a)];
        });
      }
      // Scale + center the projection, then store it as each node's ANCHOR
      // (ux,uy) — not fx/fy, so the dispersion force can move nodes off it.
      if (coords.length) {
        let minx = Infinity, maxx = -Infinity, miny = Infinity, maxy = -Infinity;
        for (const [x, y] of coords) {
          if (x < minx) minx = x; if (x > maxx) maxx = x;
          if (y < miny) miny = y; if (y > maxy) maxy = y;
        }
        const SPAN = 60 * Math.sqrt(DATA.nodes.length);
        const sx = maxx > minx ? SPAN / (maxx - minx) : 1;
        const sy = maxy > miny ? SPAN / (maxy - miny) : 1;
        const s = Math.min(sx, sy);
        const cx = (minx + maxx) / 2, cy = (miny + maxy) / 2;
        proj.forEach((n, i) => { n.ux = (coords[i][0] - cx) * s; n.uy = (coords[i][1] - cy) * s; });
      }
      // Capabilities with no embedding match → anchor at origin (reported by CLI).
      DATA.nodes.forEach(n => { if (!Array.isArray(n.centroid)) { n.ux = 0; n.uy = 0; } });

      applyDispersionForces();   // drive the anchored, tunable force layout
    }

    // ── meaning-layout forces: pull each node toward anchor×dispersion, collide
    //    to de-stack, no charge/center. Re-called on every slider change (the
    //    forceX/forceY x-accessor is cached at init, so the new dispersion only
    //    takes effect when the force is re-set). Wrapped: a failure leaves the
    //    last good layout rather than blanking. ──
    function applyDispersionForces() {
      try {
        Graph.d3Force('x', d3.forceX(n => (n.ux || 0) * dispersion).strength(0.7));
        Graph.d3Force('y', d3.forceY(n => (n.uy || 0) * dispersion).strength(0.7));
        Graph.d3Force('collide', d3.forceCollide(n => Math.sqrt(Math.max(1, n.val)) * NODE_REL_SIZE + 3));
        Graph.d3Force('charge', null);
        Graph.d3Force('center', null);
        Graph.d3ReheatSimulation();
      } catch (e) {
        console.warn('[capability-graph] dispersion forces unavailable:', e);
      }
    }

    // ── region geometry from the CURRENT node positions (n.x,n.y) — rebuilt each
    //    frame so the hull blobs + name headers follow the live layout. ──
    function rebuildRegionGeom() {
      const nRegions = (DATA.regions || []).length;
      regionGeom = (DATA.regions || []).map(r => {
        const pts = DATA.nodes
          .filter(n => n.region === r.id && Number.isFinite(n.x) && Number.isFinite(n.y))
          .map(n => [n.x, n.y]);
        if (!pts.length) return null;
        let cx = 0, cy = 0;
        for (const [x, y] of pts) { cx += x; cy += y; }
        cx /= pts.length; cy /= pts.length;
        const hull = convexHull(pts).map(([x, y]) => [cx + (x - cx) * 1.18, cy + (y - cy) * 1.18]);
        const hue = Math.round(r.id * 360 / Math.max(1, nRegions));
        return { id: r.id, name: r.name, cx, cy, hull, tint: 'hsla(' + hue + ',55%,60%,0.10)' };
      }).filter(Boolean);
    }

    // ── faint crate label at each centroid, constant on-screen size. Force
    //    layout only — in meaning layout nodes sit at UMAP coords, not the ring. ──
    function drawClusterLabels(ctx, globalScale) {
      if (LAYOUT !== 'force') return;
      ctx.save();
      ctx.font = (13 / globalScale) + 'px ui-sans-serif, system-ui, sans-serif';
      ctx.fillStyle = 'rgba(148,163,184,0.45)';
      ctx.textAlign = 'center';
      ctx.textBaseline = 'middle';
      for (const c of clusters) ctx.fillText(c, centroids[c].x, centroids[c].y);
      ctx.restore();
    }

    // ── meaning-layout region overlays: faint blob behind, header on top. ──
    function convexHull(points) {
      if (points.length < 3) return points.slice();
      const pts = points.slice().sort((a, b) => a[0] - b[0] || a[1] - b[1]);
      const cross = (o, a, b) => (a[0] - o[0]) * (b[1] - o[1]) - (a[1] - o[1]) * (b[0] - o[0]);
      const lower = [];
      for (const p of pts) {
        while (lower.length >= 2 && cross(lower[lower.length - 2], lower[lower.length - 1], p) <= 0) lower.pop();
        lower.push(p);
      }
      const upper = [];
      for (let i = pts.length - 1; i >= 0; i--) {
        const p = pts[i];
        while (upper.length >= 2 && cross(upper[upper.length - 2], upper[upper.length - 1], p) <= 0) upper.pop();
        upper.push(p);
      }
      lower.pop(); upper.pop();
      return lower.concat(upper);
    }

    function drawRegionBlobs(ctx) {
      if (!regionGeom.length) return;
      for (const g of regionGeom) {
        if (g.hull.length < 3) continue;
        ctx.beginPath();
        ctx.moveTo(g.hull[0][0], g.hull[0][1]);
        for (let i = 1; i < g.hull.length; i++) ctx.lineTo(g.hull[i][0], g.hull[i][1]);
        ctx.closePath();
        ctx.fillStyle = g.tint;
        ctx.fill();
      }
    }

    function drawRegionLabels(ctx, globalScale) {
      if (!regionGeom.length) return;
      ctx.save();
      ctx.font = '600 ' + (19 / globalScale) + 'px ui-sans-serif, system-ui, sans-serif';
      ctx.textAlign = 'center';
      ctx.textBaseline = 'middle';
      ctx.lineWidth = 3.5 / globalScale;
      ctx.strokeStyle = 'rgba(15,23,42,0.75)';   // dark halo so the name reads over nodes
      ctx.fillStyle = 'rgba(226,232,240,0.62)';
      for (const g of regionGeom) {
        ctx.strokeText(g.name, g.cx, g.cy);
        ctx.fillText(g.name, g.cx, g.cy);
      }
      ctx.restore();
    }

    function sizeGraph() { Graph.width(el.clientWidth).height(el.clientHeight); }
    window.addEventListener('resize', sizeGraph);
    sizeGraph();

    // ── search: dim non-matching nodes/links; re-applying the accessors
    //    forces a repaint after the simulation has settled. ──
    document.getElementById('search').addEventListener('input', (e) => {
      query = e.target.value.trim().toLowerCase();
      Graph.nodeColor(Graph.nodeColor()).linkColor(Graph.linkColor());
    });

    // ── show only the controls that apply to this layout. ──
    document.getElementById(LAYOUT === 'meaning' ? 'toggleHide' : 'toggleEdges').style.display = 'none';
    if (LAYOUT !== 'meaning') document.getElementById('dispWrap').style.display = 'none';

    // ── hide-unconnected toggle (force layout): drop degree-0 nodes (the halo). ──
    document.getElementById('hideUnconnected').addEventListener('change', (e) => {
      hideUnconnected = e.target.checked;
      Graph.nodeVisibility(Graph.nodeVisibility());
    });

    // ── show-call-edges toggle (meaning layout): edges are faint context. ──
    document.getElementById('showEdges').addEventListener('change', (e) => {
      showEdges = e.target.checked;
      Graph.linkVisibility(Graph.linkVisibility());
    });

    // ── dispersion dial (meaning layout): rescale the UMAP anchors live. The
    //    forceX/forceY x-accessor caches its targets at init, so we re-set the
    //    forces (applyDispersionForces) to recompute them with the new value. ──
    {
      const dispEl = document.getElementById('dispersion');
      const dispVal = document.getElementById('dispVal');
      dispVal.textContent = dispersion.toFixed(2);
      dispEl.addEventListener('input', (e) => {
        dispersion = parseFloat(e.target.value) || 1;
        dispVal.textContent = dispersion.toFixed(2);
        applyDispersionForces();
      });
    }

    // ── side panel ──
    function showPanel(n) {
      const kindLabel = n.kind || 'no finding';
      const tagStyle = 'background:' + n.color + ';';
      let html = '<h2>' + esc(n.label) + '</h2>';
      html += '<span class="tag" style="' + tagStyle + '">' + esc(kindLabel) + '</span>';
      html += n.evidence
        ? '<p class="evidence">' + esc(n.evidence) + '</p>'
        : '<p class="evidence empty">No reconciliation evidence recorded.</p>';
      html += '<h3>Functions (' + n.members.length + ')</h3>';
      if (!n.members.length) {
        html += '<p class="empty">No members.</p>';
      } else {
        html += '<ul>';
        for (const m of n.members) {
          html += '<li class="' + (m.is_entry ? 'entry' : '') + '">'
            + (m.is_entry ? '<span class="entry-mark">▶ entry</span> ' : '')
            + '<span class="nm">' + esc(m.name) + '</span>'
            + (m.loc ? '<span class="loc">' + esc(m.loc) + '</span>' : '')
            + '</li>';
        }
        html += '</ul>';
      }
      const p = document.getElementById('panel');
      p.innerHTML = html;
      p.scrollTop = 0;
      Graph.centerAt(n.x, n.y, 600);
    }

    function esc(s) {
      return String(s == null ? '' : s)
        .replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;')
        .replace(/"/g, '&quot;');
    }
  </script>
</body>
</html>
"##;
