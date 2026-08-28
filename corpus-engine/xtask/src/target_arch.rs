// SPDX-License-Identifier: AGPL-3.0-or-later
//! `cargo xtask target-arch` — `quality/TARGET_ARCHITECTURE.md` writes its own
//! structural half.
//!
//! The noun-convergence program's terminal test is that the destination
//! document can be written *honestly*: claiming only structure that exists,
//! rendering what is missing as a visible gap. A hand-written architecture
//! document can claim anything; a generator cannot. So the four structural
//! regions of that document are GENERATED, from four sources that already
//! exist (§19 — the inventory outranks the plan; nothing here mints a second
//! copy of a number somebody else owns):
//!
//! | block | source | kind |
//! |---|---|---|
//! | `register`       | `quality/CONCEPTS.toml`   | DECLARED — deterministic |
//! | `layer-map`      | `quality/ARCH_LAYERS.toml` via `arch-layers` | DECLARED — deterministic |
//! | `graph-evidence` | `scripts/nc-pressure.py --json` → `svrn code converge noun` | MEASURED |
//! | `boundary`       | `scripts/nc-boundary.py --json` | MEASURED |
//!
//! DECLARED vs MEASURED is the whole design, and it is the same distinction
//! `concept_gate` makes for the same reason: a measured number describes the
//! LAST INDEXED COMMIT, not the working tree this run is gating. Re-deriving
//! it on every check would make the gate flap whenever the indexer moves, and
//! a flapping gate gets switched off inside a week. So:
//!
//! - declared blocks are re-rendered and DIFFED on every run (sub-second);
//! - measured blocks carry a STAMP — when they were measured, against which
//!   graph commit, and the digest of the register they were joined to — and
//!   the check reads the stamp's age rather than re-measuring.
//!
//! Four verdicts, never two (ARCH §18.2). A missing block is `NEVER_RAN`, not
//! a pass. A measured block older than [`MEASURED_MAX_AGE_DAYS`], or joined
//! against a register that has since changed, is `COULD_NOT_JUDGE` — it is not
//! a failure and it is emphatically not a pass. Only a declared block that
//! disagrees with its source is `STALE`, because only that one is this run's
//! business.
//!
//! **Absence renders, it never blanks** (ARCH §18.3, and the order's own kill
//! clause). A register row whose canonical type has no definition in the graph
//! renders `ABSENT`; a row whose only definition sits in a different crate than
//! the register declares renders `ELSEWHERE`, naming the crate; a row the
//! instrument did not measure renders `not measured`, naming why; an
//! instrument that cannot run renders the whole block as a named absence with
//! the command that fixes it. A blank cell would make this a prettier
//! hand-maintained document, which is the one outcome the rung refuses.
//!
//! ```text
//! cargo run -p xtask -- target-arch                 # check
//! cargo run -p xtask -- target-arch --update-doc    # re-render the DECLARED blocks
//! cargo run -p xtask -- target-arch --measure       # also re-run the instruments (~2 min)
//! ```

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use crate::common;

const DOC_PATH: &str = "quality/TARGET_ARCHITECTURE.md";
const REGISTER_PATH: &str = "quality/CONCEPTS.toml";
const LAYERS_PATH: &str = "quality/ARCH_LAYERS.toml";
const PRESSURE_SCRIPT: &str = "scripts/nc-pressure.py";
const BOUNDARY_SCRIPT: &str = "scripts/nc-boundary.py";

/// A measured block older than this cannot speak for today's tree. Two weeks
/// is the register's own decay evidence rounded up: the 2026-08-16 draft was
/// materially stale ONE DAY later, so this is a ceiling on how wrong a stamp
/// may quietly be, not a claim that fourteen-day-old evidence is good.
const MEASURED_MAX_AGE_DAYS: i64 = 14;

/// Exit codes. Same four the concept gate and `svrn code converge status` use,
/// spelled once here so the two cannot drift on what a 3 means.
const PASS: i32 = 0;
const STALE: i32 = 1;
const COULD_NOT_JUDGE: i32 = 3;
const NEVER_RAN: i32 = 4;

const FIX_UPDATE: &str = "cargo run -p xtask -- target-arch --update-doc";
const FIX_MEASURE: &str = "cargo run -p xtask -- target-arch --measure";

// ─── Entry point ────────────────────────────────────────────────────

pub fn run(args: &[String]) -> i32 {
    let update_doc = args.iter().any(|a| a == "--update-doc");
    let measure = args.iter().any(|a| a == "--measure");
    let root = common::repo_root();

    let register_text = match std::fs::read_to_string(root.join(REGISTER_PATH)) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("target-arch: cannot read {REGISTER_PATH}: {e}");
            return NEVER_RAN;
        }
    };
    let register = match load_register(&register_text) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("target-arch: {REGISTER_PATH}: {e}");
            return NEVER_RAN;
        }
    };
    let layers_text = match std::fs::read_to_string(root.join(LAYERS_PATH)) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("target-arch: cannot read {LAYERS_PATH}: {e}");
            return NEVER_RAN;
        }
    };
    let layers = match arch_layers::parse(&layers_text) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("target-arch: {LAYERS_PATH}: {e}");
            return NEVER_RAN;
        }
    };

    let doc_path = root.join(DOC_PATH);
    let mut doc = match std::fs::read_to_string(&doc_path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("target-arch: cannot read {DOC_PATH}: {e}");
            return NEVER_RAN;
        }
    };

    let register_digest = digest(&register_text);
    let declared: [(&str, String); 2] = [
        ("register", render_register(&register)),
        ("layer-map", render_layer_map(&layers)),
    ];

    // ── write paths ─────────────────────────────────────────────────
    if update_doc || measure {
        for (id, body) in &declared {
            match splice(&doc, id, body) {
                Ok(next) => doc = next,
                Err(e) => {
                    eprintln!("target-arch: {e}");
                    return NEVER_RAN;
                }
            }
        }
        if measure {
            let now = unix_now();
            let (boundary, head) = measure_boundary(&root, now);
            let evidence =
                measure_graph_evidence(&root, &register, &register_digest, now, head.as_deref());
            for (id, body) in [("graph-evidence", evidence), ("boundary", boundary)] {
                match splice(&doc, id, &body) {
                    Ok(next) => doc = next,
                    Err(e) => {
                        eprintln!("target-arch: {e}");
                        return NEVER_RAN;
                    }
                }
            }
        }
        if let Err(e) = std::fs::write(&doc_path, &doc) {
            eprintln!("target-arch: write {DOC_PATH}: {e}");
            return NEVER_RAN;
        }
        eprintln!(
            "target-arch: wrote {DOC_PATH} ({} block(s))",
            if measure { 4 } else { 2 }
        );
    }

    // ── check ───────────────────────────────────────────────────────
    let mut worst = PASS;
    for (id, body) in &declared {
        match block_body(&doc, id) {
            None => {
                eprintln!(
                    "target-arch: NEVER-RAN — no `{id}` block in {DOC_PATH}. The document must \
                     carry the marker pair\n  <!-- BEGIN GENERATED {id} -->  …  <!-- END GENERATED {id} -->\n  \
                     then: {FIX_UPDATE}"
                );
                worst = worst.max(NEVER_RAN);
            }
            Some(found) if found.trim() != body.trim() => {
                eprintln!(
                    "target-arch: STALE — the `{id}` block disagrees with its source. Regenerate:\n  {FIX_UPDATE}"
                );
                worst = worst.max(STALE);
            }
            Some(_) => {}
        }
    }
    for id in ["graph-evidence", "boundary"] {
        match block_body(&doc, id) {
            None => {
                eprintln!(
                    "target-arch: NEVER-RAN — no `{id}` block in {DOC_PATH}. This is not a pass \
                     and it is not a zero.\n  {FIX_MEASURE}"
                );
                worst = worst.max(NEVER_RAN);
            }
            Some(found) => match read_stamp(found) {
                None => {
                    eprintln!(
                        "target-arch: NEVER-RAN — the `{id}` block carries no measurement stamp, \
                         so nothing can say when or against what it was taken.\n  {FIX_MEASURE}"
                    );
                    worst = worst.max(NEVER_RAN);
                }
                Some(stamp) => {
                    let age_days = (unix_now() - stamp.measured_at) / 86_400;
                    if age_days > MEASURED_MAX_AGE_DAYS {
                        eprintln!(
                            "target-arch: COULD-NOT-JUDGE — `{id}` was measured {age_days}d ago \
                             (ceiling {MEASURED_MAX_AGE_DAYS}d); it describes a tree that is gone.\n  {FIX_MEASURE}"
                        );
                        worst = worst.max(COULD_NOT_JUDGE);
                    } else if let Some(joined) = stamp.register_digest.as_deref() {
                        if joined != register_digest {
                            eprintln!(
                                "target-arch: COULD-NOT-JUDGE — `{id}` was joined against register \
                                 digest {joined}, and {REGISTER_PATH} now digests {register_digest}. \
                                 The evidence and the register are about different noun sets.\n  {FIX_MEASURE}"
                            );
                            worst = worst.max(COULD_NOT_JUDGE);
                        }
                    }
                }
            },
        }
    }

    let in_program = register.iter().filter(|c| c.in_program).count();
    eprintln!(
        "target-arch: {} register row(s) ({in_program} in program) · {} declared layer(s) · \
         {} generated block(s) · verdict {}",
        register.len(),
        layers.layers.len(),
        4,
        verdict_word(worst)
    );
    worst
}

fn verdict_word(code: i32) -> &'static str {
    match code {
        PASS => "PASSED",
        STALE => "STALE",
        COULD_NOT_JUDGE => "COULD-NOT-JUDGE",
        _ => "NEVER-RAN",
    }
}

// ─── The register (declared) ────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Concept {
    pub name: String,
    pub canonical: String,
    pub disposition: String,
    pub totality: String,
    pub status: String,
    pub in_program: bool,
    pub phase: Option<i64>,
    pub landed_at: Option<String>,
    /// One line: what the noun IS. Required — a register that cannot say what
    /// its own noun is has nothing to generate a document from.
    pub gloss: String,
    /// The type sketch, where a declaration reads better than a sentence.
    pub shape: Option<String>,
}

impl Concept {
    /// The crate the register says owns this noun. `sovereign_eval::x::Y` →
    /// `sovereign-eval`. Cargo spells crates with hyphens and Rust paths with
    /// underscores; one spelling, decided here (§10.6).
    ///
    /// `None` when the canonical is not a Rust path — two rows declare a FILE
    /// as their home (`sovereign/docs/cli-contract.toml`), and reading its
    /// first path segment as a crate name manufactures a finding that is not
    /// there. A home the graph cannot speak about is reported as such.
    pub fn owner_crate(&self) -> Option<String> {
        let head = self.canonical.split("::").next().unwrap_or("");
        if head.is_empty() || head.contains('/') || head.contains('.') {
            return None;
        }
        Some(head.replace('_', "-"))
    }

    /// What to print in an owner column — the crate where there is one, the
    /// canonical verbatim where there is not.
    pub fn owner_label(&self) -> String {
        self.owner_crate().unwrap_or_else(|| self.canonical.clone())
    }
}

pub fn load_register(text: &str) -> Result<Vec<Concept>, String> {
    let value: toml::Value = text.parse().map_err(|e| format!("parse: {e}"))?;
    let empty = Vec::new();
    let entries = value
        .get("concept")
        .and_then(|v| v.as_array())
        .unwrap_or(&empty);
    if entries.is_empty() {
        return Err(
            "no [[concept]] rows — the register is the source of truth and it is empty".into(),
        );
    }
    let mut out = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for (i, e) in entries.iter().enumerate() {
        let req = |key: &str| -> Result<String, String> {
            e.get(key)
                .and_then(|v| v.as_str())
                .map(|s| s.trim().to_string())
                .ok_or_else(|| format!("[[concept]] #{}: missing `{key}`", i + 1))
        };
        let c = Concept {
            name: req("name")?,
            canonical: req("canonical")?,
            disposition: req("disposition")?,
            totality: req("totality")?,
            status: req("status")?,
            in_program: e
                .get("in_program")
                .and_then(|v| v.as_bool())
                .ok_or_else(|| format!("[[concept]] #{}: missing `in_program`", i + 1))?,
            phase: e.get("phase").and_then(|v| v.as_integer()),
            landed_at: e
                .get("landed_at")
                .and_then(|v| v.as_str())
                .map(String::from),
            gloss: req("gloss")?,
            shape: e
                .get("shape")
                .and_then(|v| v.as_str())
                .map(|s| s.trim().to_string()),
        };
        if !matches!(c.status.as_str(), "holds" | "partial" | "target") {
            return Err(format!(
                "concept `{}`: status `{}` is not holds|partial|target",
                c.name, c.status
            ));
        }
        if !seen.insert(c.name.clone()) {
            return Err(format!("concept `{}` declared twice", c.name));
        }
        out.push(c);
    }
    Ok(out)
}

fn render_register(register: &[Concept]) -> String {
    let mut md = String::new();
    let holds = register.iter().filter(|c| c.status == "holds").count();
    let partial = register.iter().filter(|c| c.status == "partial").count();
    let target = register.iter().filter(|c| c.status == "target").count();
    let in_program = register.iter().filter(|c| c.in_program).count();

    md.push_str(&format!(
        "**{} nouns.** {holds} `holds`, {partial} `partial`, {target} `target`; {in_program} are \
         in the program, the rest are here for architectural completeness. Every row below is a \
         `[[concept]]` in [`CONCEPTS.toml`](./CONCEPTS.toml) — the count, the markers and the \
         owners are read from it, not typed here, so this section cannot claim a noun the register \
         does not carry.\n\n",
        register.len()
    ));
    md.push_str("| noun | what it is | status | owner (declared) | phase | totality |\n");
    md.push_str("|---|---|---|---|---|---|\n");
    for c in register {
        let phase = match (c.in_program, c.phase) {
            (false, _) => "—".to_string(),
            (true, Some(p)) => format!("{p}"),
            (true, None) => "**unphased**".to_string(),
        };
        let landed = c
            .landed_at
            .as_deref()
            .map(|d| format!(" · landed {}", cell(&first_sentence(d))))
            .unwrap_or_default();
        md.push_str(&format!(
            "| **`{}`** | {} | `{}`{} | `{}` | {} | {} |\n",
            c.name,
            cell(&c.gloss),
            c.status,
            landed,
            c.owner_label(),
            phase,
            cell(&first_sentence(&c.totality)),
        ));
    }

    // The sketches. Rows whose totality reads better as a declaration carry a
    // `shape`; it lives in the register with everything else, so this appendix
    // cannot drift from the row above it.
    let shaped: Vec<&Concept> = register.iter().filter(|c| c.shape.is_some()).collect();
    md.push_str(&format!(
        "\n**Declared shapes.** {} of the {} rows carry a type sketch in the register; the rest \
         are carried by their totality rule alone. A sketch is the TARGET spelling — where the \
         status marker reads `target`, no such type exists yet, and the graph evidence below says \
         so per row.\n",
        shaped.len(),
        register.len()
    ));
    for c in shaped {
        md.push_str(&format!(
            "\n**`{}`** · `{}` — {}\n\n```rust\n{}\n```\n",
            c.name,
            c.status,
            c.gloss,
            c.shape.as_deref().unwrap_or_default()
        ));
    }
    md
}

/// A table cell may not contain a pipe or a newline. Flatten rather than
/// truncate — losing a clause silently is how a generated doc starts lying.
fn cell(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace('|', "\\|")
}

fn first_sentence(s: &str) -> String {
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    match flat.find(". ") {
        Some(i) if i > 20 => flat[..=i].to_string(),
        _ => flat,
    }
}

// ─── The layer map (declared) ───────────────────────────────────────

fn render_layer_map(map: &arch_layers::LayerMap) -> String {
    let mut md = String::new();
    md.push_str(
        "Read from [`ARCH_LAYERS.toml`](./ARCH_LAYERS.toml) — the same file \
         `cargo xtask layer-gate` enforces against Cargo-declared edges, so the map below and \
         the map that fails the build are one map (§10.6).\n\n",
    );
    md.push_str("| tier | layer | crates (as declared, `*` is a pattern) |\n|---:|---|---|\n");
    for (i, layer) in map.layers.iter().enumerate() {
        md.push_str(&format!(
            "| {i} | **{}** | {} |\n",
            layer.name,
            layer
                .crates
                .iter()
                .map(|c| format!("`{c}`"))
                .collect::<Vec<_>>()
                .join(" · ")
        ));
    }
    md.push_str(&format!(
        "\n**Back of house** — outside the ordered stack, not on top of it; may observe every \
         layer, and nothing may depend on it: {}.\n",
        map.backstage
            .iter()
            .map(|c| format!("`{c}`"))
            .collect::<Vec<_>>()
            .join(" · ")
    ));
    // The exceptions are the honest part of this map, so they render as a
    // named debt rather than being left out of the picture.
    if map.exceptions.is_empty() {
        md.push_str("\n**No grandfathered violations.** The map is total and clean.\n");
    } else {
        md.push_str(&format!(
            "\n**{} grandfathered violation(s)** ride `[[exception]]` entries — each one says the \
             boundary is drawn in the wrong place and the crate split has not been paid for, not \
             that the edge is fine:\n\n",
            map.exceptions.len()
        ));
        for e in &map.exceptions {
            md.push_str(&format!(
                "- `{}` → `{}` — {}\n",
                e.from,
                e.to,
                cell(&e.reason)
            ));
        }
    }
    md
}

// ─── Graph evidence (measured) ──────────────────────────────────────

fn measure_graph_evidence(
    root: &Path,
    register: &[Concept],
    register_digest: &str,
    now: i64,
    graph_head: Option<&str>,
) -> String {
    let out = Command::new("python3")
        .arg(root.join(PRESSURE_SCRIPT))
        .arg("--json")
        .current_dir(root)
        .output();
    let json: serde_json::Value = match &out {
        Ok(o) if o.status.success() || o.status.code() == Some(3) => {
            serde_json::from_slice(&o.stdout).unwrap_or(serde_json::Value::Null)
        }
        Ok(o) => {
            return absent_block(
                "graph evidence",
                &format!(
                    "`{PRESSURE_SCRIPT}` exited {} — {}",
                    o.status.code().unwrap_or(-1),
                    String::from_utf8_lossy(&o.stderr).trim()
                ),
                FIX_MEASURE,
            );
        }
        Err(e) => {
            return absent_block(
                "graph evidence",
                &format!("`{PRESSURE_SCRIPT}` could not run: {e}"),
                FIX_MEASURE,
            );
        }
    };
    if json.is_null() {
        return absent_block(
            "graph evidence",
            &format!("`{PRESSURE_SCRIPT} --json` produced no parseable JSON"),
            FIX_MEASURE,
        );
    }

    let mut rows: BTreeMap<String, &serde_json::Value> = BTreeMap::new();
    if let Some(arr) = json.get("rows").and_then(|v| v.as_array()) {
        for r in arr {
            if let Some(n) = r.get("noun").and_then(|v| v.as_str()) {
                rows.insert(n.to_string(), r);
            }
        }
    }

    let mut md = String::new();
    // `nc-pressure.py` reports no commit of its own — it relays
    // `svrn code converge noun`, which does not emit one. The head comes from
    // the boundary instrument, which reads the provenance of the SAME
    // `scip_graph.db`. One graph, one head; where the boundary run failed there
    // is no head and the stamp says `unknown` rather than inventing one.
    md.push_str(&stamp_line(
        now,
        PRESSURE_SCRIPT,
        graph_head,
        Some(register_digest),
    ));
    md.push_str(
        "\nWhat the SCIP graph says about each register row, joined by noun name. `defs` is \
         first-party production definitions of that exact name; `kin` is names that end or start \
         with it; `sites` is reference sites. The verdict column is the one that matters — it is \
         the register's declared shape checked against the graph, and a disagreement renders as a \
         disagreement.\n\n",
    );
    md.push_str(
        "| noun | declared | disposition | defs | kin | sites | graph verdict |\n|---|---|---|---:|---:|---:|---|\n",
    );

    let mut absent = 0usize;
    let mut duplicated = 0usize;
    let mut elsewhere = 0usize;
    let mut unmeasured = 0usize;
    for c in register {
        let Some(r) = rows.get(&c.name) else {
            unmeasured += 1;
            md.push_str(&format!(
                "| **`{}`** | `{}` | {} | — | — | — | not measured — `in_program = false`, so the \
                 instrument does not visit it |\n",
                c.name,
                c.status,
                cell(&c.disposition)
            ));
            continue;
        };
        let defs = r.get("defs").and_then(|v| v.as_u64()).unwrap_or(0);
        let kin = r.get("kin").and_then(|v| v.as_u64()).unwrap_or(0);
        let sites = r.get("sites").and_then(|v| v.as_u64()).unwrap_or(0);
        let crates: Vec<String> = r
            .get("def_sites")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|d| d.get("krate").and_then(|k| k.as_str()).map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        let Some(owner) = c.owner_crate() else {
            md.push_str(&format!(
                "| **`{}`** | `{}` | {} | {defs} | {kin} | {sites} | the register's canonical \
                 `{}` is not a crate path, so the graph cannot be asked where this noun lives |\n",
                c.name,
                c.status,
                cell(&c.disposition),
                c.canonical
            ));
            continue;
        };
        let verdict = if defs == 0 {
            absent += 1;
            format!(
                "**ABSENT** — no definition anywhere; the register declares `{owner}` will own it"
            )
        } else if defs > 1 {
            duplicated += 1;
            format!(
                "**DUPLICATED** ({defs}) — defined in {}",
                crates
                    .iter()
                    .map(|k| format!("`{k}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        } else if crates.first().is_some_and(|k| *k != owner) {
            elsewhere += 1;
            format!(
                "**ELSEWHERE** — the one definition is in `{}`, the register declares `{owner}`",
                crates[0]
            )
        } else if crates.is_empty() {
            "one definition; the instrument reported no crate for it".to_string()
        } else {
            format!("converged — one definition, in `{owner}` as declared")
        };
        md.push_str(&format!(
            "| **`{}`** | `{}` | {} | {defs} | {kin} | {sites} | {verdict} |\n",
            c.name,
            c.status,
            cell(&c.disposition)
        ));
    }

    let excess = json
        .get("excess_definitions")
        .and_then(|v| v.as_u64())
        .map(|n| n.to_string())
        .unwrap_or_else(|| "unreported".into());
    let reach = json.get("reach").and_then(|v| v.as_u64());
    let of = json.get("of").and_then(|v| v.as_u64());
    md.push_str(&format!(
        "\n**{absent} absent · {elsewhere} elsewhere · {duplicated} duplicated · {unmeasured} not \
         measured.** Excess definitions (the judged number, target 0): {excess}. Nouns with a \
         canonical: {}.\n",
        match (reach, of) {
            (Some(r), Some(o)) => format!("{r}/{o}"),
            _ => "unreported".to_string(),
        }
    ));
    if let Some(un) = json.get("unmeasurable").and_then(|v| v.as_array()) {
        if !un.is_empty() {
            md.push_str(&format!(
                "\n**UNMEASURABLE, reported rather than defaulted to zero:** {}.\n",
                un.iter()
                    .filter_map(|v| v.as_str())
                    .map(|s| format!("`{s}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }
    md
}

// ─── Boundary (measured) ────────────────────────────────────────────

/// Returns the rendered block and the graph commit it describes — the latter
/// so the graph-evidence block can stamp the same commit instead of guessing.
fn measure_boundary(root: &Path, now: i64) -> (String, Option<String>) {
    let out = Command::new("python3")
        .arg(root.join(BOUNDARY_SCRIPT))
        .arg("--json")
        .current_dir(root)
        .output();
    let json: serde_json::Value = match &out {
        Ok(o) if o.status.success() => {
            serde_json::from_slice(&o.stdout).unwrap_or(serde_json::Value::Null)
        }
        Ok(o) => {
            return (
                absent_block(
                    "the boundary table",
                    &format!(
                        "`{BOUNDARY_SCRIPT}` exited {} — {}",
                        o.status.code().unwrap_or(-1),
                        String::from_utf8_lossy(&o.stderr).trim()
                    ),
                    FIX_MEASURE,
                ),
                None,
            );
        }
        Err(e) => {
            return (
                absent_block(
                    "the boundary table",
                    &format!("`{BOUNDARY_SCRIPT}` could not run: {e}"),
                    FIX_MEASURE,
                ),
                None,
            );
        }
    };
    if json.is_null() {
        return (
            absent_block(
                "the boundary table",
                &format!("`{BOUNDARY_SCRIPT} --json` produced no parseable JSON"),
                FIX_MEASURE,
            ),
            None,
        );
    }
    let head = json
        .get("indexed_head")
        .and_then(|v| v.as_str())
        .map(String::from);

    let mut md = String::new();
    md.push_str(&stamp_line(
        now,
        BOUNDARY_SCRIPT,
        json.get("indexed_head").and_then(|v| v.as_str()),
        None,
    ));
    md.push_str(
        "\nA domain boundary is only a boundary if a small, named set of types crosses it. \
         `width` is distinct types referenced across the edge; `refs` is how often. A `flag` names \
         an edge that should not exist at all.\n\n",
    );
    md.push_str("| from | to | refs | width | flag |\n|---|---|---:|---:|---|\n");
    if let Some(edges) = json.get("edges").and_then(|v| v.as_array()) {
        for e in edges {
            let refs = e.get("refs").and_then(|v| v.as_u64()).unwrap_or(0);
            if refs < 20 {
                continue;
            }
            md.push_str(&format!(
                "| `{}` | `{}` | {refs} | {} | {} |\n",
                e.get("from").and_then(|v| v.as_str()).unwrap_or("?"),
                e.get("to").and_then(|v| v.as_str()).unwrap_or("?"),
                e.get("width").and_then(|v| v.as_u64()).unwrap_or(0),
                e.get("violation")
                    .and_then(|v| v.as_str())
                    .map(|s| format!("**{s}**"))
                    .unwrap_or_else(|| "—".into()),
            ));
        }
    } else {
        md.push_str("| — | — | — | — | **no `edges` in the instrument's output** |\n");
    }

    let n = |k: &str| {
        json.get(k)
            .and_then(|v| v.as_u64())
            .map(|v| v.to_string())
            .unwrap_or_else(|| "unreported".into())
    };
    md.push_str(&format!(
        "\n- **core boundary width** (the three systems): {}\n- **types on edges that should not \
         exist**: {}\n- **shared kernel** — types all three systems speak: {}\n",
        n("core_boundary_width"),
        n("violating_types"),
        n("kernel_size"),
    ));
    if let Some(homes) = json.get("kernel_homes").and_then(|v| v.as_object()) {
        let owned: Vec<String> = homes
            .iter()
            .map(|(k, v)| format!("`{k}` {}", v.as_u64().unwrap_or(0)))
            .collect();
        md.push_str(&format!("- kernel owned by: {}\n", owned.join(" · ")));
        if homes.len() == 1 {
            md.push_str(
                "\n**Every kernel type is owned by one domain.** That is not a contract, it is a \
                 dependency on an implementation.\n",
            );
        }
    }
    (md, head)
}

// ─── Blocks, stamps, and the small helpers ──────────────────────────

fn begin_marker(id: &str) -> String {
    format!("<!-- BEGIN GENERATED {id} -->")
}
fn end_marker(id: &str) -> String {
    format!("<!-- END GENERATED {id} -->")
}

/// The body between a block's markers, or `None` when the pair is absent or
/// inverted. Never guesses: a malformed pair reads as absent, which is a
/// NEVER-RAN verdict rather than a silent pass.
pub fn block_body<'a>(doc: &'a str, id: &str) -> Option<&'a str> {
    let b = begin_marker(id);
    let e = end_marker(id);
    let start = doc.find(&b)? + b.len();
    let end = doc[start..].find(&e)? + start;
    Some(&doc[start..end])
}

pub fn splice(doc: &str, id: &str, body: &str) -> Result<String, String> {
    let b = begin_marker(id);
    let e = end_marker(id);
    let start = doc
        .find(&b)
        .ok_or_else(|| format!("{DOC_PATH} has no `{b}` marker — add the marker pair first"))?
        + b.len();
    let end = doc[start..]
        .find(&e)
        .ok_or_else(|| format!("{DOC_PATH} has `{b}` but no `{e}`"))?
        + start;
    Ok(format!(
        "{}\n{}\n{}",
        &doc[..start],
        body.trim_end(),
        &doc[end..]
    ))
}

/// The named absence. A block that cannot be measured says so, in the
/// document, with the reason and the fix — it never renders empty.
fn absent_block(what: &str, reason: &str, fix: &str) -> String {
    format!(
        "> **UNAVAILABLE — {what} could not be measured.** {reason}\n>\n> This is a reported \
         absence, not a zero and not a pass. Refresh with:\n>\n>     {fix}\n"
    )
}

pub struct Stamp {
    pub measured_at: i64,
    pub register_digest: Option<String>,
}

fn stamp_line(
    now: i64,
    instrument: &str,
    graph_head: Option<&str>,
    register_digest: Option<&str>,
) -> String {
    let head = graph_head.unwrap_or("unknown");
    let short = &head[..head.len().min(12)];
    format!(
        "<!-- measured_at={now} register_digest={} -->\n*Measured {} by `{instrument}`, against \
         graph commit `{short}` — **not** your working tree.*\n",
        register_digest.unwrap_or("none"),
        iso_utc(now),
    )
}

pub fn read_stamp(block: &str) -> Option<Stamp> {
    let line = block.lines().find(|l| l.contains("measured_at="))?;
    let at: i64 = line
        .split("measured_at=")
        .nth(1)?
        .split_whitespace()
        .next()?
        .parse()
        .ok()?;
    let digest = line
        .split("register_digest=")
        .nth(1)
        .and_then(|s| s.split_whitespace().next())
        .filter(|s| *s != "none" && !s.starts_with("--"))
        .map(String::from);
    Some(Stamp {
        measured_at: at,
        register_digest: digest,
    })
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// FNV-1a 64. Change detection only — it answers "is this the same register
/// text I joined against", never "is this text authentic". Named as what it is
/// so no later reader mistakes it for a content-addressed identity.
pub fn digest(text: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("fnv{h:016x}")
}

/// Epoch seconds → `YYYY-MM-DDTHH:MM:SSZ`, by Howard Hinnant's
/// `civil_from_days`. xtask carries no date crate on purpose, and shelling out
/// to `date(1)` would make a generated document depend on the host's coreutils.
pub fn iso_utc(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINI: &str = r#"
schema_version = 2
[[concept]]
name        = "Verdict"
canonical   = "sovereign_contracts::verdict::Verdict"
disposition = "converge"
totality    = "Four states, one definition. And a second sentence that should not reach the cell."
gloss       = "the outcome of any check."
shape       = """
pub enum Verdict { Passed }
"""
status      = "target"
in_program  = true
phase       = 2
[[concept]]
name        = "Peer"
canonical   = "commonwealth_core::mesh::Peer"
disposition = "converge"
totality    = "Identity, transport, advertised capabilities."
gloss       = "another node in the trust ring."
status      = "holds"
in_program  = false
"#;

    #[test]
    fn the_register_parses_and_the_owner_crate_is_cargo_spelled() {
        let r = load_register(MINI).expect("parses");
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].owner_crate().as_deref(), Some("sovereign-contracts"));
        assert_eq!(r[1].owner_crate().as_deref(), Some("commonwealth-core"));
    }

    #[test]
    fn a_canonical_that_is_a_file_has_no_owner_crate_rather_than_a_wrong_one() {
        let toml = MINI.replace(
            r#"canonical   = "commonwealth_core::mesh::Peer""#,
            r#"canonical   = "sovereign/docs/cli-contract.toml""#,
        );
        let r = load_register(&toml).unwrap();
        assert_eq!(r[1].owner_crate(), None);
        assert_eq!(r[1].owner_label(), "sovereign/docs/cli-contract.toml");
    }

    #[test]
    fn a_bad_status_marker_is_refused_rather_than_rendered() {
        let bad = MINI.replace(r#"status      = "target""#, r#"status      = "probably""#);
        let e = load_register(&bad).expect_err("must refuse");
        assert!(e.contains("holds|partial|target"), "{e}");
    }

    #[test]
    fn a_duplicated_row_is_refused_because_one_noun_has_one_row() {
        let dup = format!("{MINI}\n[[concept]]\nname = \"Peer\"\ncanonical = \"a::B\"\ndisposition = \"converge\"\ntotality = \"x\"\ngloss = \"y\"\nstatus = \"holds\"\nin_program = false\n");
        assert!(load_register(&dup).is_err());
    }

    #[test]
    fn a_row_with_no_gloss_is_refused_because_the_document_renders_that_field() {
        let no_gloss = MINI.replace("gloss       = \"another node in the trust ring.\"\n", "");
        let e = load_register(&no_gloss).expect_err("must refuse");
        assert!(e.contains("gloss"), "{e}");
    }

    #[test]
    fn a_declared_shape_reaches_the_rendered_appendix_verbatim() {
        let r = load_register(MINI).unwrap();
        let md = render_register(&r);
        assert!(md.contains("pub enum Verdict { Passed }"), "{md}");
        assert!(md.contains("1 of the 2 rows carry a type sketch"), "{md}");
    }

    #[test]
    fn an_empty_register_is_an_error_not_an_empty_document() {
        assert!(load_register("schema_version = 2\n").is_err());
    }

    #[test]
    fn the_rendered_table_carries_every_row_and_no_raw_pipes() {
        let r = load_register(MINI).unwrap();
        let md = render_register(&r);
        assert!(md.contains("**`Verdict`**"));
        assert!(md.contains("**`Peer`**"));
        // out-of-program rows render an em dash for phase, never a blank cell
        assert!(md.contains("| — |"));
        for line in md.lines().filter(|l| l.starts_with('|')) {
            assert_eq!(
                line.matches("| ").count(),
                line.matches(" |").count(),
                "{line}"
            );
        }
    }

    #[test]
    fn a_long_totality_is_cut_at_the_first_sentence_not_mid_word() {
        let s = first_sentence("Four states, one definition. And a second sentence.");
        assert_eq!(s, "Four states, one definition.");
    }

    #[test]
    fn splice_replaces_only_the_body_between_the_markers() {
        let doc = "before\n<!-- BEGIN GENERATED register -->\nOLD\n<!-- END GENERATED register -->\nafter\n";
        let out = splice(doc, "register", "NEW").unwrap();
        assert!(out.contains("before"));
        assert!(out.contains("after"));
        assert!(out.contains("NEW"));
        assert!(!out.contains("OLD"));
        assert_eq!(block_body(&out, "register").unwrap().trim(), "NEW");
    }

    #[test]
    fn a_missing_marker_is_an_error_not_a_silent_append() {
        assert!(splice("no markers here", "register", "NEW").is_err());
        assert!(block_body("no markers here", "register").is_none());
    }

    #[test]
    fn an_unterminated_block_reads_as_absent_rather_than_running_to_end_of_file() {
        let doc = "<!-- BEGIN GENERATED boundary -->\nbody with no end marker\n";
        assert!(block_body(doc, "boundary").is_none());
    }

    #[test]
    fn a_stamp_round_trips_through_the_block_it_is_written_into() {
        let body = stamp_line(
            1_755_000_000,
            "scripts/x.py",
            Some("abcdef1234567890"),
            Some("fnv00"),
        );
        let s = read_stamp(&body).expect("stamp is readable");
        assert_eq!(s.measured_at, 1_755_000_000);
        assert_eq!(s.register_digest.as_deref(), Some("fnv00"));
        assert!(
            body.contains("abcdef123456"),
            "graph head is short-formed: {body}"
        );
    }

    #[test]
    fn a_block_with_no_stamp_reads_as_never_ran_not_as_age_zero() {
        assert!(read_stamp("| a | b |\nno stamp here\n").is_none());
    }

    #[test]
    fn an_unavailable_instrument_renders_a_named_absence_with_its_fix() {
        let b = absent_block("the boundary table", "python3 is missing", FIX_MEASURE);
        assert!(b.contains("UNAVAILABLE"));
        assert!(b.contains("python3 is missing"));
        assert!(b.contains("--measure"));
        assert!(!b.trim().is_empty());
    }

    #[test]
    fn iso_utc_agrees_with_known_epochs() {
        assert_eq!(iso_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso_utc(1_000_000_000), "2001-09-09T01:46:40Z");
        // 1_755_000_000 / 86_400 = 20_312.5 exactly — midday, not 13:20. The
        // first two lines passed while this one caught my arithmetic, which is
        // the point of pinning three epochs rather than one.
        assert_eq!(iso_utc(1_755_000_000), "2025-08-12T12:00:00Z");
    }

    #[test]
    fn the_digest_changes_when_the_register_changes() {
        assert_ne!(digest("a"), digest("b"));
        assert_eq!(digest("a"), digest("a"));
    }
}
