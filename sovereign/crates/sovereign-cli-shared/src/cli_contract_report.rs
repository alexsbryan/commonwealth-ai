// SPDX-License-Identifier: AGPL-3.0-or-later
//! Rendering for the CLI contract's quality surface — the answer to "what does
//! this CLI promise, how much of it can actually fail, and when was that last
//! checked against reality?"
//!
//! WHY THIS IS A MODULE AND NOT `eprintln!` IN A TEST. The map used to be
//! printed by `cli_contract_journeys::print_the_experience_map`, reachable only
//! by knowing to run
//!
//! ```text
//! cargo test -p sovereign-cli --test cli_contract_journeys --features dev-tools \
//!     print_the_experience_map -- --nocapture
//! ```
//!
//! Nobody guesses that. A quality surface nobody can find is the same failure as
//! a quality surface nobody runs, and this repo has a graveyard of both — its
//! predecessor harness (`cli-contract-live-verify.sh`) was written, documented
//! as "safe to call unconditionally in CI", and then never called by anything.
//!
//! So the rendering lives here, next to the manifest type, and has exactly two
//! callers: the `svrn contract` verb (for humans) and the cargo test (so the
//! numbers a developer reads and the numbers the gate enforces cannot drift —
//! there is one census, in [`crate::cli_contract::Contract::assertion_census`],
//! and one renderer for it).

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::cli_contract::{Contract, Evidence, Journey};

/// The evidence census, rendered. The numbers are computed by
/// [`Contract::assertion_census`] — this only lays them out.
pub fn render_census(contract: &Contract) -> String {
    let c = contract.assertion_census();
    let mut s = String::new();
    let live = &c.live;
    let never = &c.never_run;
    let total = live.total() + never.total();
    s.push_str("── what can actually fail ──\n");
    // The two halves are printed on adjacent lines on purpose: the whole point
    // of the split is that the second number cannot be added to the first and
    // called coverage.
    s.push_str(&format!(
        "  a lane RUNS   {:>3} steps   {:>3} assert output   {:>3} exit-code only   {:>3} assert NOTHING\n",
        live.total(),
        live.output,
        live.exit_only,
        live.none
    ));
    s.push_str(&format!(
        "  nothing runs  {:>3} steps   {:>3} assert output   {:>3} exit-code only   {:>3} assert NOTHING\n",
        never.total(),
        never.output,
        never.exit_only,
        never.none
    ));
    let pct = if total > 0 { live.output * 100 / total } else { 0 };
    s.push_str(&format!(
        "  → of {total} declared steps, {} are executed by some lane AND assert an \
         answer ({pct}%)\n",
        live.output
    ));
    if !c.live_unfalsifiable.is_empty() {
        s.push_str(&format!(
            "\n  {} LIVE step(s) assert nothing — invoked, never checked:\n",
            c.live_unfalsifiable.len()
        ));
        for w in &c.live_unfalsifiable {
            s.push_str(&format!("    · {w}\n"));
        }
    }
    if !c.live_journeys_without_output.is_empty() {
        s.push_str(&format!(
            "\n  live journey(s) with no output assertion anywhere: {}\n",
            c.live_journeys_without_output.join(", ")
        ));
    }
    if !c.never_run_journeys.is_empty() {
        s.push_str(&format!(
            "\n  {} journey(s) no lane runs ({} steps) — declared, dispatch-replayed, \
             never executed:\n",
            c.never_run_journeys.len(),
            never.total()
        ));
        for (id, why) in &c.never_run_journeys {
            s.push_str(&format!("    ∅ {id:<20} {}\n", truncate(why, 88)));
        }
    }
    s
}

/// The experience map: every promise, the journeys serving it, and how much of
/// each is proven. The `asserts` column is the honest one — a journey may
/// legally carry zero output assertions if it neither mutates nor claims a
/// capability, so printing the ratio is the difference between a known weakness
/// and a hidden one.
pub fn render_experience_map(contract: &Contract) -> String {
    let mut s = String::new();
    s.push_str("── what this CLI promises ──\n");
    let mut steps_total = 0usize;
    let mut asserts_total = 0usize;
    for e in &contract.experiences {
        let serving = contract.journeys_for(&e.id);
        let steps: usize = serving.iter().map(|j| j.steps.len()).sum();
        let asserts: usize = serving
            .iter()
            .flat_map(|j| j.steps.iter())
            .filter(|st| st.evidence() == Evidence::Output)
            .count();
        steps_total += steps;
        asserts_total += asserts;
        let live = serving.iter().filter(|j| j.runs_live()).count();
        s.push_str(&format!(
            "{:<24} {} journeys ({live} live)  {asserts}/{steps} steps assert output  {} capabilities\n",
            e.id,
            serving.len(),
            e.capabilities.len()
        ));
        if let Some(why) = &e.gap {
            s.push_str(&format!("    ∅ NO JOURNEY — {}\n", truncate(&squash(why), 88)));
        }
        for j in &serving {
            s.push_str(&format!("    {}\n", journey_line(j)));
        }
    }
    if !contract.dependencies.is_empty() {
        s.push_str("\n── what journeys stand on ──\n");
        for d in &contract.dependencies {
            s.push_str(&format!(
                "{:<18} {:<40} verify: {:<16} doc: {}\n",
                d.id, d.title, d.verify, d.doc
            ));
        }
    }
    s.push_str(&format!(
        "\n{} experiences, {} journeys, {asserts_total}/{steps_total} steps assert output\n",
        contract.experiences.len(),
        contract.journeys.len()
    ));
    s
}

fn journey_line(j: &Journey) -> String {
    let needs = if j.needs.is_empty() {
        String::new()
    } else {
        format!(
            "  needs {}",
            j.needs.iter().map(|n| n.as_str()).collect::<Vec<_>>().join(",")
        )
    };
    format!(
        "{} {:<22} tier {}  {} steps{needs}",
        if j.runs_live() { "▸" } else { " " },
        j.id,
        j.tier,
        j.steps.len()
    )
}

/// The last verdict the nightly journey lane recorded, read from the
/// `latest.json` that `scripts/cli-journey-nightly.sh` writes.
///
/// This is the only part of the report that is about REALITY rather than about
/// the manifest. Everything above it describes what the repo claims; this line
/// says whether anything ever tried it, and how long ago — which is the
/// question that kills tools in this codebase. A harness that has not run in
/// three weeks is indistinguishable from a harness that was never wired up,
/// unless something says so out loud.
#[derive(Debug, Clone)]
pub struct NightlyPosture {
    /// Path of the `latest.json` that was read.
    pub path: PathBuf,
    /// `pass` / `fail` / `no-daemon` / … as recorded by the lane.
    pub verdict: String,
    /// The lane's own one-line summary.
    pub summary: String,
    /// The lane's coverage line.
    pub coverage: String,
    /// Commit the lane ran against, and whether the tree was dirty.
    pub commit: String,
    /// Verdict of the read-only capability lane, when the field is present.
    pub capability_lane: Option<String>,
    /// Age of the report, from the file's mtime — deliberately not parsed from
    /// the `stamp` field, which is a local-time string with no offset.
    pub age: Option<Duration>,
}

impl NightlyPosture {
    /// Human age, e.g. `"11h ago"`. `"unknown"` when the mtime was unreadable.
    pub fn age_human(&self) -> String {
        match self.age {
            None => "unknown".to_string(),
            Some(d) => {
                let h = d.as_secs() / 3600;
                if h < 1 {
                    format!("{}m ago", d.as_secs() / 60)
                } else if h < 48 {
                    format!("{h}h ago")
                } else {
                    format!("{}d ago", h / 24)
                }
            }
        }
    }

    /// Is this report old enough that it should not be quoted as current?
    /// The lane fires daily, so anything past 48h means it has been failing to
    /// run — not that the code is fine.
    pub fn is_stale(&self) -> bool {
        self.age.map(|d| d > Duration::from_secs(48 * 3600)).unwrap_or(true)
    }
}

/// Candidate paths for the nightly's `latest.json`, in order.
///
/// `$HOME/.sovereign/journey-nightly` is FIRST and hardcoded because that is
/// what `cli-journey-nightly.sh` writes (`${JOURNEY_NIGHTLY_DIR:-$HOME/.sovereign/journey-nightly}`).
/// Resolving via [`crate::dirs::sovereign_root`] alone would silently look in
/// `~/.svrnmesh` on a host where the rename migration has run, find nothing, and
/// report "never run" for a lane that runs every night — the exact false
/// negative this module exists to avoid.
pub fn nightly_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(dir) = std::env::var("JOURNEY_NIGHTLY_DIR") {
        out.push(PathBuf::from(dir).join("latest.json"));
    }
    // Deliberate LEGACY-dir probe (candidate list, not a derivation): the
    // branded-root candidate below covers the post-migration layout.
    #[allow(clippy::disallowed_methods)]
    if let Some(home) = dirs::home_dir() {
        out.push(home.join(".sovereign").join("journey-nightly").join("latest.json"));
    }
    out.push(crate::dirs::sovereign_root().join("journey-nightly").join("latest.json"));
    out
}

/// Read the nightly posture from the first candidate path that exists.
pub fn nightly_posture() -> Option<NightlyPosture> {
    nightly_candidates().into_iter().find_map(|p| read_nightly(&p).ok())
}

fn read_nightly(path: &Path) -> Result<NightlyPosture, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let v: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("parse {}: {e}", path.display()))?;
    let field = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("-").to_string();
    let age = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| SystemTime::now().duration_since(t).ok());
    let dirty = v.get("dirty").and_then(|x| x.as_bool()).unwrap_or(false);
    Ok(NightlyPosture {
        path: path.to_path_buf(),
        verdict: field("verdict"),
        summary: field("summary"),
        coverage: field("coverage"),
        commit: format!("{}{}", field("commit"), if dirty { " (dirty tree)" } else { "" }),
        capability_lane: v
            .get("capability_lane")
            .and_then(|x| x.as_str())
            .map(str::to_string),
        age,
    })
}

/// The whole developer-facing report: promises, evidence, debt, last contact
/// with reality, and how to run each lane yourself.
pub fn render_report(contract: &Contract, nightly: Option<&NightlyPosture>) -> String {
    let mut s = String::new();
    s.push_str(&render_experience_map(contract));
    s.push('\n');
    s.push_str(&render_census(contract));
    s.push('\n');
    s.push_str(&render_nightly(nightly));
    s.push('\n');
    s.push_str(HOW_TO_RUN);
    s
}

/// The reality line, rendered — including the case that matters most, where
/// there is no report at all.
pub fn render_nightly(nightly: Option<&NightlyPosture>) -> String {
    let mut s = String::from("── last contact with reality ──\n");
    match nightly {
        None => {
            s.push_str(
                "  NO NIGHTLY REPORT ON THIS HOST. Every number above describes what\n\
                 \x20 the manifest CLAIMS; none of it has been run here. Install the\n\
                 \x20 timer with scripts/install-journey-nightly.sh, or run a lane by\n\
                 \x20 hand (below).\n",
            );
            s.push_str("  looked in:\n");
            for p in nightly_candidates() {
                s.push_str(&format!("    {}\n", p.display()));
            }
        }
        Some(n) => {
            s.push_str(&format!(
                "  nightly lane   {} · {} · {}\n",
                n.verdict,
                n.age_human(),
                n.commit
            ));
            s.push_str(&format!("  {}\n", n.summary));
            if n.coverage != "-" {
                s.push_str(&format!("  {}\n", n.coverage));
            }
            if let Some(cap) = &n.capability_lane {
                s.push_str(&format!("  capability lane  {cap}\n"));
            }
            s.push_str(&format!("  report  {}\n", n.path.display()));
            if n.is_stale() {
                s.push_str(
                    "  STALE — the lane fires daily, so a report older than two days\n\
                     \x20 means it has stopped running, not that nothing broke.\n",
                );
            }
        }
    }
    s
}

/// Kept as one block so the commands a developer needs are in one place, in the
/// order of rising cost. Every one of them is copy-pasteable from a checkout.
const HOW_TO_RUN: &str = "\
── run it yourself ──
  static (no daemon, ~1s, runs in CI on every push):
    cargo test -p sovereign-cli --features dev-tools --test cli_contract_journeys
    cargo test -p sovereign-cli --features dev-tools --test cli_contract_code
  the runner's own negative controls (stub binary + stub daemon, ~10s, CI):
    sovereign/scripts/tests/cli-journey-selftest.sh
  read-only against the daemon you already have (safe anywhere, ~1m):
    SOVEREIGN_LIVE_JOURNEYS=1 sovereign/scripts/cli-journey-verify.sh --tier 2
  mutating, in a sandbox HOME it boots and owns (~10m, needs models):
    sovereign/scripts/cli-journey-sandbox.sh
  both lanes plus the controls, as the nightly timer runs them:
    sovereign/scripts/cli-journey-nightly.sh
";

fn squash(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    format!("{}…", s.chars().take(max.saturating_sub(1)).collect::<String>())
}
