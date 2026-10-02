// SPDX-License-Identifier: AGPL-3.0-or-later
//! The fieldglass page's serialized data model — everything that lands in
//! `__DATA__`. Split from `mod.rs` per ARCH §3.1 when the module crossed
//! the 1200-line ceiling — flagged by fieldglass's own delta panel on its
//! first P2 render (2026-08-06), which is exactly the job it was built for.

use super::derive::{CrateNode, FlowEdge, TraitMatrix};

#[derive(serde::Serialize)]
pub(super) struct FileLeaf {
    pub(super) path: String,
    #[serde(rename = "crate")]
    pub(super) crate_name: String,
    pub(super) x: f64,
    pub(super) y: f64,
    pub(super) w: f64,
    pub(super) h: f64,
    pub(super) lines: usize,
    pub(super) fan_in: usize,
    /// Co-change community id; -1 = not in any recent community.
    pub(super) community: i32,
    /// 0.0–0.5+: how evenly this file's callers split across communities.
    pub(super) bridge: f32,
    /// Over the 1200-line ceiling (ARCH §3.1).
    pub(super) offender: bool,
    /// Agent activity from session transcripts (`cache-audit --by-file`).
    pub(super) reads: u64,
    pub(super) read_tokens: u64,
    pub(super) edits: u64,
    pub(super) agent_sessions: u64,
    /// Commits touching this file inside the churn window. Window-neutral by
    /// name on purpose: it was `commits_90d` until `--window` made that name
    /// a lie on every non-default render. `Honesty::churn_window_label` is
    /// the one place the period is stated.
    pub(super) commits_window: u32,
}

/// One UTC day's slice of a file's agent activity, mirroring `cache-audit
/// --by-file --json`'s `days` array. Held so any window can be EXTRACTED
/// from one transcript scan instead of re-scanning per window.
#[derive(Default, Clone)]
pub(super) struct AgentDay {
    /// Days since the Unix epoch, UTC.
    pub(super) day: i64,
    pub(super) reads: u64,
    pub(super) read_tokens: u64,
    pub(super) edits: u64,
    /// Session indices (into the scan's session-id table), so a window's
    /// distinct-session count is an exact union rather than a sum.
    pub(super) sessions: Vec<u32>,
}

/// Per-file agent activity, parsed from `cache-audit --by-file --json`. The
/// flat fields are the totals for whatever span this value describes; `days`
/// is the same activity decomposed per UTC day.
#[derive(Default, Clone)]
pub(super) struct AgentStat {
    pub(super) reads: u64,
    pub(super) read_tokens: u64,
    pub(super) edits: u64,
    pub(super) sessions: u64,
    /// Ascending by day. Empty when the scan could not date the events.
    pub(super) days: Vec<AgentDay>,
}

/// The whole agent-heat scan: the full-history per-file table plus what the
/// honesty footer needs to describe it. One scan serves every window.
#[derive(Default)]
pub(super) struct AgentScan {
    pub(super) files: std::collections::BTreeMap<String, AgentStat>,
    pub(super) sessions: u64,
    pub(super) first_mtime: i64,
    pub(super) last_mtime: i64,
    /// Paths inside the repo but outside the git source set (ignored or
    /// generated) that were dropped — real spend, architecture noise.
    pub(super) non_source_dropped: usize,
    /// Events with no parseable timestamp: counted in totals, in no day.
    pub(super) days_unattributed: u64,
    /// The sibling `cache-audit` emitted day slices at all (it declares
    /// `bucket_unit`). False against a binary predating them — `--window`
    /// then REFUSES rather than presenting full-history heat as windowed.
    pub(super) buckets_supported: bool,
}

/// One comprehension-tax row: read-hot and edit-cold — load-bearing but
/// confusing. THE dragon signal agent telemetry adds over git's.
#[derive(serde::Serialize)]
pub(super) struct TaxEntry {
    pub(super) path: String,
    pub(super) reads: u64,
    pub(super) read_tokens: u64,
    pub(super) edits: u64,
    pub(super) sessions: u64,
}

/// "Since last render" — computed against the previous JSON sidecar.
#[derive(serde::Serialize)]
pub(super) struct Delta {
    pub(super) prev_unix: i64,
    /// (path, line delta) — biggest absolute change first; new files carry
    /// their full line count.
    pub(super) grown: Vec<(String, i64)>,
    pub(super) new_offenders: Vec<String>,
    pub(super) new_files: usize,
    pub(super) removed_files: usize,
}

#[derive(serde::Serialize)]
pub(super) struct CrateRect {
    pub(super) name: String,
    pub(super) layer: i32,
    pub(super) x: f64,
    pub(super) y: f64,
    pub(super) w: f64,
    pub(super) h: f64,
}

#[derive(serde::Serialize)]
pub(super) struct GhostEdge {
    pub(super) a: String,
    pub(super) b: String,
    pub(super) joint: u32,
    pub(super) corr: f32,
    /// true = structural edge exists but crosses crates ("crate boundary
    /// fiction"); false = pure hidden coupling (no structural edge at all).
    pub(super) fiction: bool,
}

#[derive(serde::Serialize)]
pub(super) struct DupArc {
    pub(super) a: String,
    pub(super) a_line: u32,
    pub(super) b: String,
    pub(super) b_line: u32,
    pub(super) sim: f32,
    pub(super) exact: bool,
    pub(super) lines: usize,
}

/// One duplication cluster, summarized for the attention queue.
#[derive(serde::Serialize)]
pub(super) struct DupClusterSummary {
    pub(super) label: String,
    pub(super) files: Vec<String>,
    pub(super) members: usize,
    pub(super) lines: usize,
    /// (members − 1) × lines — the mass a factoring-out would remove.
    pub(super) redundant: usize,
    pub(super) exact: bool,
}

/// "Start here" — the page's evidence, ordered by MAGNITUDE. This is
/// curation, not judgment: nothing here is scored healthy/unhealthy, it is
/// sorted by size so a human reads the biggest evidence first.
#[derive(serde::Serialize)]
pub(super) struct Attention {
    pub(super) dup_clusters: Vec<DupClusterSummary>,
    /// (path, bridge score) — most evenly split callers first.
    pub(super) bridges: Vec<(String, f32)>,
    /// (path, lines) — largest over-ceiling files first.
    pub(super) offenders: Vec<(String, usize)>,
    /// Read-hot, edit-cold files — highest read_tokens/(edits+1) first.
    pub(super) comprehension_tax: Vec<TaxEntry>,
    /// (path, commits in window, share of window commits) — the files every
    /// change flows through.
    pub(super) tollbooths: Vec<(String, u32, f32)>,
}

#[derive(serde::Serialize)]
pub(super) struct Honesty {
    /// Commit the SCIP index was built at — the STRUCTURE panels describe
    /// this commit, not necessarily HEAD.
    pub(super) scip_head: String,
    /// How many commits HEAD is ahead of the indexed commit. `None` when
    /// either side is unknown. >0 means the structural panels lag reality.
    pub(super) scip_commits_behind: Option<u64>,
    /// Age of the chunk-embedding index the duplication NEAR tier reads.
    /// The exact tier reads SCIP+source and is as fresh as `scip_head`;
    /// the two tiers are on different cadences, and this is the skew.
    pub(super) chunk_index_age_days: Option<f64>,
    pub(super) refs_total: usize,
    pub(super) refs_cross_crate: usize,
    pub(super) refs_dropped_unattributed: usize,
    pub(super) refs_dropped_test: usize,
    pub(super) refs_dropped_external: usize,
    pub(super) temporal_window_days: i64,
    pub(super) srp_correlation: f32,
    pub(super) srp_min_joint: u32,
    pub(super) dry_threshold: f32,
    pub(super) dry_min_lines: usize,
    pub(super) files_walked: usize,
    pub(super) files_outside_crates: usize,
    pub(super) communities: usize,
    pub(super) dup_arcs_dropped: usize,
    /// Transcript sessions the agent-heat pass scanned, and its time range.
    pub(super) agent_sessions: u64,
    pub(super) agent_first_mtime: i64,
    pub(super) agent_last_mtime: i64,
    /// The churn/activity window in seconds, and the label the page states
    /// it by ("90d" by default, or whatever `--window` asked for). Seconds
    /// because `--window 36h` is not a whole number of days.
    pub(super) churn_window_secs: i64,
    pub(super) churn_window_label: String,
    /// Distinct commits touching .rs files inside the churn window — the
    /// tollbooth percentages' denominator.
    pub(super) churn_commits: u32,
    /// True when `--window` was given: activity panels describe the window,
    /// structure panels still describe all of history. False on the default
    /// render, where activity is the 90d churn window and full-history heat.
    pub(super) windowed: bool,
    /// First UTC day the windowed agent heat includes. Agent slices are
    /// per-day, so the effective agent window is the whole days containing
    /// the request and can reach up to 24h further back than the label.
    /// `None` on a default (unwindowed) render.
    pub(super) agent_window_from_day: Option<i64>,
    /// Agent events the scan could not date, so they are in the full-history
    /// totals but in no day slice — and therefore in no window.
    pub(super) agent_days_unattributed: u64,
    /// Non-obvious render decisions, stated on the page (glassbox §9 applied
    /// to the artifact itself).
    pub(super) notes: Vec<String>,
}

#[derive(serde::Serialize)]
pub(super) struct FieldglassData {
    pub(super) corpus: String,
    pub(super) head: String,
    pub(super) generated_unix: i64,
    pub(super) canvas_w: f64,
    pub(super) canvas_h: f64,
    pub(super) layers: Vec<String>,
    pub(super) crates: Vec<CrateNode>,
    pub(super) flow_edges: Vec<FlowEdge>,
    pub(super) crate_rects: Vec<CrateRect>,
    pub(super) files: Vec<FileLeaf>,
    pub(super) ghosts: Vec<GhostEdge>,
    pub(super) dup_arcs: Vec<DupArc>,
    pub(super) isp: Vec<TraitMatrix>,
    pub(super) attention: Attention,
    pub(super) delta: Option<Delta>,
    pub(super) honesty: Honesty,
}
