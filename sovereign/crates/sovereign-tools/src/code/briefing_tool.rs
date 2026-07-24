// SPDX-License-Identifier: AGPL-3.0-or-later
//! `briefing` — the session-boot brief as a daemon MCP tool.
//!
//! Wraps [`assemble_brief`] so any MCP client (Claude Code sessions,
//! peer agents, `svrn tools call briefing`) can pull the same
//! token-budgeted orientation brief the SessionStart hook injects,
//! without shelling out to `svrn code brief` (which requires the
//! release `sovereign-cli-dev` sibling on PATH). Same renderer, same
//! sections; one source of truth for "what should an agent know
//! before its first edit here."
//!
//! The daemon variant additionally threads its live
//! [`WorkAtlasStore`] into the "Work in flight" section, so the brief
//! carries peer claims/edit-observations overlapping the working set
//! — the Tier 1 coordination signal the CLI path can only get
//! best-effort (it opens the shared mesh.db read-only).

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use corpus_engine_notes::NoteStore;
use sovereign_core::error::{Error, Result};
use sovereign_core::traits::Tool;
use sovereign_core::types::{
    Effect, Idempotency, Latency, Permission, Scope, StepOutput, ToolContext, ToolDescriptor,
    ToolExample,
};
use sovereign_work_atlas::store::ScopeMatch;
use sovereign_work_atlas::WorkAtlasStore;

use super::brief::{assemble_brief, BriefInputs, WorkInFlightEntry};
use super::working_set::{detect_working_set, Strategy};

/// Daemon-side MCP wrapper around the brief assembler.
pub struct BriefingTool {
    notes: Arc<NoteStore>,
    workspace_root: Option<PathBuf>,
    atlas: Option<Arc<WorkAtlasStore>>,
}

impl BriefingTool {
    pub fn new(notes: Arc<NoteStore>) -> Self {
        Self {
            notes,
            workspace_root: None,
            atlas: None,
        }
    }

    /// The repo the brief describes. Without it the tool rejects at
    /// execute time with an actionable message — same posture as the
    /// lint/test watcher tools on an unconfigured daemon.
    pub fn with_workspace_root(mut self, root: PathBuf) -> Self {
        self.workspace_root = Some(root);
        self
    }

    /// Live work-atlas handle for the "Work in flight" section.
    pub fn with_atlas(mut self, atlas: Arc<WorkAtlasStore>) -> Self {
        self.atlas = Some(atlas);
        self
    }
}

/// Reduce live atlas signals overlapping `working_set` to brief
/// entries. Shared by this tool and `svrn code brief` so both
/// surfaces render identical "Work in flight" sections.
///
/// Each file is queried under BOTH its repo-relative and absolute
/// form. Since 2026-07-23 both atlas writers normalize to
/// repo-relative at write time, so the relative query is the one
/// that matches; the absolute variant is transition tolerance for
/// records gossiped from peers running older binaries (which stored
/// CodeWatcher paths absolute — a class of silent zero-result bugs).
///
/// Best-effort by design: any store error yields an empty list — a
/// brief must never fail because coordination signals were
/// unavailable. Claims are deduped by `claim_id` (one claim can
/// overlap several working-set files); observations by
/// `(session_id, file_path)`.
pub fn overlaps_for_working_set(
    store: &WorkAtlasStore,
    repo_root: &std::path::Path,
    working_set: &[PathBuf],
    caller_token: Option<&str>,
) -> Vec<WorkInFlightEntry> {
    let mut acc = OverlapAccumulator::new(repo_root);

    // Query the FULL working set, not the 20-file render cap: a peer
    // collision on file #150 of a big recent-commits set is exactly
    // the signal this section exists to surface (verified live
    // 2026-07-23 — a 236-file set had all its active edits past the
    // alphabetical first 20 and a capped query reported zero). The
    // renderer caps displayed entries at 8 separately.
    for file in working_set.iter() {
        let rel = file.to_string_lossy().into_owned();
        let abs = repo_root.join(file).to_string_lossy().into_owned();
        for scope in [rel.as_str(), abs.as_str()] {
            let Ok(in_flight) = sovereign_work_atlas::tools::collect_in_flight(
                store,
                scope,
                ScopeMatch::File,
                caller_token,
            ) else {
                continue;
            };
            for c in &in_flight.claims {
                acc.add_claim(c);
            }
            for o in &in_flight.observations {
                acc.add_observation(o);
            }
        }
    }
    acc.finish()
}

/// Dedup + display mapping from raw `work_in_flight` claim/observation
/// JSON to brief entries. Shared by the in-process path
/// ([`overlaps_for_working_set`]) and the CLI's daemon-HTTP path
/// (`svrn code brief` — the daemon's atlas store is in-memory, so a
/// separate CLI process can only reach live signals over `/mcp`).
pub struct OverlapAccumulator {
    repo_root: PathBuf,
    now: u64,
    seen_claims: std::collections::HashSet<String>,
    seen_observations: std::collections::HashSet<(String, String)>,
    entries: Vec<WorkInFlightEntry>,
}

impl OverlapAccumulator {
    pub fn new(repo_root: &std::path::Path) -> Self {
        Self {
            repo_root: repo_root.to_path_buf(),
            now: sovereign_core::time::unix_now_u64(),
            seen_claims: Default::default(),
            seen_observations: Default::default(),
            entries: Vec::new(),
        }
    }

    /// Display rule: the brief shows repo-relative paths, matching
    /// the "Working set" section above it.
    fn rel_display(&self, p: &str) -> String {
        std::path::Path::new(p)
            .strip_prefix(&self.repo_root)
            .map(|r| r.to_string_lossy().into_owned())
            .unwrap_or_else(|_| p.to_string())
    }

    /// Deduped by `claim_id` — one claim can overlap several files.
    pub fn add_claim(&mut self, c: &serde_json::Value) {
        let claim_id = c["claim_id"].as_str().unwrap_or_default().to_string();
        if !self.seen_claims.insert(claim_id) {
            return;
        }
        let intent = c["intent"].as_str().unwrap_or("(no intent)");
        // Show the claim's own first declared scope; the `scopes`
        // field carries them since 2026-07-23.
        let scope = c["scopes"]
            .as_array()
            .and_then(|a| a.first())
            .and_then(|v| v.as_str())
            .map(|s| self.rel_display(s))
            .unwrap_or_else(|| "repo".to_string());
        self.entries.push(WorkInFlightEntry {
            scope,
            grade: c["confidence"].as_str().unwrap_or("declared").to_string(),
            detail: format!("claim: {}", truncate(intent, 100)),
            node: c["node_id"].as_str().map(str::to_string),
        });
    }

    /// Deduped by `(session_id, file_path)`.
    pub fn add_observation(&mut self, o: &serde_json::Value) {
        let file_path = o["file_path"].as_str().unwrap_or_default().to_string();
        let session = o["session_id"].as_str().unwrap_or_default().to_string();
        if !self.seen_observations.insert((session, file_path.clone())) {
            return;
        }
        let age_min = o["last_observed_at"]
            .as_u64()
            .map(|t| self.now.saturating_sub(t) / 60)
            .unwrap_or(0);
        let events = o["event_count"].as_u64().unwrap_or(0);
        self.entries.push(WorkInFlightEntry {
            scope: self.rel_display(&file_path),
            grade: o["confidence"].as_str().unwrap_or("recent").to_string(),
            detail: format!("edited {age_min}m ago · {events} event(s)"),
            node: o["node_id"].as_str().map(str::to_string),
        });
    }

    /// Active edits first, then recent, then standing claims — the
    /// reader should hit the hottest collision risk in line one.
    pub fn finish(mut self) -> Vec<WorkInFlightEntry> {
        let rank = |g: &str| match g {
            "active" => 0u8,
            "recent" => 1,
            _ => 2,
        };
        self.entries.sort_by_key(|e| rank(&e.grade));
        self.entries
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max).collect();
        format!("{cut}…")
    }
}

fn current_branch(repo_root: &std::path::Path) -> Option<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!name.is_empty()).then_some(name)
}

#[async_trait]
impl Tool for BriefingTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "briefing".to_string(),
            name: "Session Briefing".to_string(),
            description: "Assemble the session-orientation brief for this repo: \
                          working set (from git), live peer work-in-flight, drift \
                          posture, area principles, active decision/invariant \
                          notes, structural atoms, and recent activity — one \
                          token-budgeted markdown document. Call at session start \
                          (or after a long gap) instead of reading files to \
                          orient. Same renderer as `svrn code brief`, so hook-\
                          injected briefs and this tool never disagree."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "strategy": {
                        "type": "string",
                        "enum": ["recent", "branch", "explicit"],
                        "default": "recent",
                        "description": "Working-set detection: `recent` = files touched by commits in the last `hours` (best for orientation on a clean tree); `branch` = diff vs the default branch; `explicit` = use `files`."
                    },
                    "hours": {
                        "type": "integer",
                        "default": 48,
                        "description": "Window for the `recent` strategy."
                    },
                    "files": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Repo-relative working-set files (for `explicit`)."
                    },
                    "budget_tokens": {
                        "type": "integer",
                        "default": 1500,
                        "description": "Token budget for the rendered brief."
                    },
                    "feature_id": {
                        "type": "string",
                        "description": "Optional ATOS feature id to scope notes."
                    }
                },
                "required": []
            }),
            examples: vec![ToolExample {
                situation:
                    "Session start — orient on what changed recently and who else is active.".into(),
                call: json!({ "strategy": "recent", "hours": 48 }),
            }],
            effect: Effect::Read,
            idempotency: Idempotency::Idempotent,
            latency: Latency::Fast,
            scope: Scope::Session,
            output_schema: Some(json!({
                "type": "object",
                "properties": {
                    "markdown": { "type": "string", "description": "The rendered brief." },
                    "working_set_size": { "type": "integer" },
                    "work_in_flight_count": { "type": "integer" },
                    "strategy": { "type": "string" },
                    "budget_tokens": { "type": "integer" }
                }
            })),
        }
    }

    fn required_permissions(&self) -> Vec<Permission> {
        vec![]
    }

    async fn execute(&self, params: &serde_json::Value, ctx: &ToolContext) -> Result<StepOutput> {
        let Some(repo_root) = self.workspace_root.clone() else {
            return Err(Error::InvalidInput(
                "briefing: daemon has no workspace configured — set \
                 SOVEREIGN_WORKSPACE_DIR or write the repo path to \
                 ~/.sovereign/workspace, then restart the daemon"
                    .into(),
            ));
        };

        let strategy_kind = params
            .get("strategy")
            .and_then(|v| v.as_str())
            .unwrap_or("recent");
        let hours = params.get("hours").and_then(|v| v.as_u64()).unwrap_or(48);
        let budget_tokens = params
            .get("budget_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(1500) as usize;
        let feature_id = params
            .get("feature_id")
            .and_then(|v| v.as_str())
            .map(str::to_string);

        let strategy = match strategy_kind {
            "recent" => Strategy::RecentCommits { hours },
            "branch" => Strategy::default_branch_diff(),
            "explicit" => {
                let files: Vec<PathBuf> = params
                    .get("files")
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str())
                            .map(PathBuf::from)
                            .collect()
                    })
                    .unwrap_or_default();
                if files.is_empty() {
                    return Err(Error::InvalidInput(
                        "briefing: strategy `explicit` requires a non-empty `files` array".into(),
                    ));
                }
                Strategy::Explicit(files)
            }
            other => {
                return Err(Error::InvalidInput(format!(
                    "briefing: unknown strategy `{other}` — use recent | branch | explicit"
                )));
            }
        };

        let working_set = detect_working_set(&repo_root, strategy).map_err(|e| Error::Tool {
            tool_id: "briefing".into(),
            message: format!("working-set detection failed: {e}"),
        })?;

        let work_in_flight = self
            .atlas
            .as_deref()
            .map(|store| {
                overlaps_for_working_set(
                    store,
                    &repo_root,
                    &working_set,
                    ctx.agent_session_token.as_deref(),
                )
            })
            .unwrap_or_default();

        let repo_name = repo_root
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("repo")
            .to_string();
        let branch_name = current_branch(&repo_root).unwrap_or_else(|| "HEAD".into());

        // Drift dir mirrors the CLI default (~/.sovereign/drift);
        // atlas/inquiries sections resolve the same conventions as
        // `cmd_brief` where the daemon can know them.
        let drift_dir = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("/"))
            .join(".sovereign")
            .join("drift");
        let drift_dir_opt = drift_dir.exists().then_some(drift_dir);
        let inquiries_dir = repo_root.join("inquiries");
        let inquiries_dir_opt = inquiries_dir.is_dir().then_some(inquiries_dir);

        let inputs = BriefInputs {
            working_set: &working_set,
            repo_root: Some(&repo_root),
            atlas_dir: None,
            inquiries_dir: inquiries_dir_opt.as_deref(),
            repo_name: &repo_name,
            branch_name: &branch_name,
            budget_tokens,
            feature_id: feature_id.as_deref(),
            drift_dir: drift_dir_opt.as_deref(),
            work_in_flight: &work_in_flight,
        };
        let markdown = assemble_brief(inputs, &self.notes)
            .await
            .map_err(|e| Error::Tool {
                tool_id: "briefing".into(),
                message: format!("brief assembly failed: {e}"),
            })?;

        Ok(StepOutput::Json(json!({
            "markdown": markdown,
            "working_set_size": working_set.len(),
            "work_in_flight_count": work_in_flight.len(),
            "strategy": strategy_kind,
            "budget_tokens": budget_tokens,
        })))
    }
}
