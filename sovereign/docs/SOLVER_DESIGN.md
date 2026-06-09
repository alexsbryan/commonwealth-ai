# Solver design — extracting search into a callable surface

**STATUS: SUPERSEDED 2026-05-24 by `TDD_MACHINE_DESIGN.md`.**

This doc framed sovereign's solver value as "expose the validated
search loop as a deep-solver backend." During review the framing
sharpened: the actual product is a red-green-refactor TDD machine,
not a one-shot solver. `tdd_green` IS this design's `solve_with_search`;
the new doc adds `tdd_red` (failing-test generation) and `tdd_refactor`
(structural improvement with tests-as-gate) and reorganizes accordingly.

**Read TDD_MACHINE_DESIGN.md instead.** Most of this doc's content
(crate layout, ARCH_PRINCIPLES compliance, `Workdir` newtype,
`ChatBackend` trait, `SolverRegistry`, dirty-git refusal, npm org)
carries forward unchanged — restated in the new doc for completeness.

The historical content below is preserved for context on how the
design evolved.

---

**Status:** Draft v2, 2026-05-24. Pre-implementation review after ARCH_PRINCIPLES.md pass.
**Owners:** your-org + claude
**Supersedes:** none
**Superseded-by:** `TDD_MACHINE_DESIGN.md`
**Related:** `sovereign/crates/sovereign-agent-bench/src/runners/search.rs`, `sovereign/ARCH_PRINCIPLES.md`

## Why

The 2026-05-24 wildcard rebuild validated the **search-not-agent** architecture: parallel candidate generation + monotonic-improvement gating + tests as the only judge. Median 20/20 on 4.2-mini-evaluator vs the role-loop's 0-3/9. The implementation lives inside `sovereign-agent-bench` and is reachable only as `--agent search` against bench problems.

We want this validated capability available to **end users**. Per the 2026-05-24 product discussion, the path is **NOT** a new sovereign CLI (atos was the lesson — don't build our own coding harness). The path is:

1. Extract the search loop from bench-specific scaffolding into a reusable crate.
2. Expose it as an HTTP endpoint on the local daemon.
3. Wrap that endpoint as a Pi extension (and later other harnesses).
4. Sovereign's positioning: **the local deep-solver backend** that existing harnesses call when their normal loop hits a wall. We don't try to be the harness.

## Non-goals

- Building a `sovereign solve` CLI. Atos v2 risk.
- Streaming progress mid-call (defer; v1 is sync request/response).
- Remote sovereign instances serving solve calls. Workdir is local-only.
- Multi-tenant authentication beyond the existing daemon auth.
- Replacing the OpenAI `/v1/chat/completions` endpoint or putting search behind a magic model id.

## Architecture

```
sovereign-agent-bench  ──┐
                         │
                         ▼
                  commonwealth-solver (NEW)
                  ─────────────────────────
                    SearchLoop (pure algorithm)
                    SolveRequest / SolveResponse types
                    Configurable knobs (rounds, candidates, temps)
                         │
                         ▼
                  sovereign-server (or wherever
                  /v1/* is hosted)
                  ─────────────────────────
                    POST /v1/solve/search
                         │
                         ▼
                  Pi extension (TypeScript, npm)
                  ─────────────────────────
                    pi.registerTool("commonwealth_solve", ...)
                         │
                         ▼
                  Pi user types prompt → pi's main model
                  decides to call commonwealth_solve
                         │
                         ▼
                  Search loop runs against user's workdir
                  → diff returned → main model continues
```

## ARCH_PRINCIPLES compliance — the rules this design has to honor

A pass against `sovereign/ARCH_PRINCIPLES.md` surfaced these specific applications. Each shapes the implementation below.

| § | Principle | Application in this design |
|---|---|---|
| 2.1 | Closed sets → enum | `EditAction`, `SolveStatus`, `SolverError` all enums (already are). |
| 3.1 | File size soft ceilings | `commonwealth-solver` ships as 5-7 focused files; no single file > 800 lines. |
| 4 | Registry pattern | `SolverRegistry` already in the plan. ✓ |
| 4.3 | Unknown-id explicit + loud | `POST /v1/solve/{unknown}` returns 400 with the registered-ids list. Not 404 (that's "no route") — 400 means "we know the route, you got the id wrong." |
| 5 | Interface segregation | `SolverProvider` trait has exactly 2 methods: `id() + solve()`. No god trait. |
| **6** | **Data vs program (SICP)** | **Prompts live as markdown assets**, not Rust string literals. `commonwealth-solver/assets/system_prompt.md`, `assets/user_prompt.md.tmpl`. Loaded via `include_str!`. Operators can tune without touching Rust. |
| **7.1** | **Structural invariants** | `Workdir` newtype wraps `PathBuf` and is only constructible via `Workdir::check_safe(path, force)` — the dirty-git case becomes unrepresentable at the type level downstream. The `solve()` signature takes `Workdir`, not `PathBuf`. |
| 7.4 | Defence in depth | Workdir safety: (1) constructor refuses dirty/system paths, (2) per-candidate snapshots prevent partial-state leakage, (3) all writes go through the existing `executor::execute` which has its own workdir guards. |
| **8.1** | **Centralise workspace deps** | `regex` gets promoted to `[workspace.dependencies]` (currently a direct dep in `sovereign-agent-bench`). New crate inherits via `workspace = true`. |
| 8.3 | Re-export boundaries | `commonwealth-solver` re-exports the executor primitives it uses; bench imports via the re-export, not direct path-dep on `commonwealth-agent-tools`. |
| 8.5 | Heavy deps stay where they're needed | `commonwealth-solver` deps: `tokio`, `reqwest`, `serde`, `regex`, `commonwealth-agent-tools`, `tracing`. No lance, no tauri, no llama. |
| **9.1** | **Glassbox tracing** | Every non-obvious decision emits a `tracing` event with `solve:` prefix: dirty-git refusal, baseline tests, each candidate dispatched, candidate result, winner selected, stall, exhausted, completed. |
| 9.3 | Redact deliberately | Workdir paths logged at full path at `debug!`, basename only at `info!`. Test failure tails capped at ~1.5 KB (already do this). |
| 10.2 | Touch one dimension at a time | Six phases shipped as separate commits: extraction, safety check, auto-detect, HTTP, MCP, Pi. |
| **12.4** | **Tests must not require GPU/network/weights** | `ChatBackend` trait abstracts the chat-completion call. `ReqwestChatBackend` is the production impl; `DeterministicChatBackend` lets tests inject canned responses. The full `solve()` loop becomes unit-testable without a daemon. |
| 12.5 | Use existing mocks | `DeterministicChatBackend` follows the `DeterministicInference` pattern from `sovereign-inference`. |

The bolded rows (§6, §7.1, §8.1, §9.1, §12.4) drive concrete changes to the design below — they're not just review notes, they change what we build.

## Phases

### Phase 6a — Crate extraction (`commonwealth-solver`)

**New crate:** `sovereign/crates/commonwealth-solver/`

**What it owns:**
- The pure search algorithm (parallel candidates, monotonic improvement, diversity ladder, stall detection)
- `SolveRequest`, `SolveResponse`, `SolveConfig`, `SolveEvent` types
- HTTP client for the daemon's chat-completions endpoint
- Workdir snapshot/restore
- Edit application (via `commonwealth-agent-tools::executor::execute`)
- Test execution + result parsing

**What it does NOT own:**
- The bench's `Problem`, `WitnessReport`, `JudgeRubric` — those stay in `sovereign-agent-bench`
- The HTTP endpoint surface — that lives in `sovereign-server`
- The Pi extension TypeScript — separate npm package

**Public surface:**

```rust
pub struct SolveConfig {
    pub candidates_per_round: usize,        // default 4
    pub rounds_per_trial: usize,            // default 6
    pub max_stall_rounds: u32,              // default 3
    pub emit_max_tokens: u32,               // default 2500
    pub candidate_test_timeout: Duration,   // default 60s
    pub temp_ladder_default: Vec<f32>,      // [0.2, 0.4, 0.7, 0.9]
    pub temp_ladder_wide: Vec<f32>,         // [0.3, 0.6, 0.9, 1.1]
}

/// ARCH §7.1 — structural invariant on workdir safety.
/// `Workdir` is the ONLY way to pass a workdir into `solve()`. It can
/// only be constructed via `check_safe`, which verifies the path is
/// not a system path and is a clean git repo. The dirty-git case is
/// unrepresentable in the `solve()` signature — code downstream of
/// construction cannot accidentally accept an unsafe path.
pub struct Workdir(PathBuf);

impl Workdir {
    pub fn check_safe(path: PathBuf, force: bool) -> Result<Self, DirtyWorkdir> { … }
    pub fn path(&self) -> &Path { &self.0 }
}

pub enum DirtyWorkdir {
    SystemPath { path: PathBuf },
    UncommittedChanges { path: PathBuf },
    NotAGitRepo { path: PathBuf },
}

impl DirtyWorkdir {
    /// Actionable hint string for the model + user.
    pub fn hint(&self) -> String { … }
}

pub struct SolveRequest {
    pub workdir: Workdir,                   // structural: can't be unsafe
    pub task: String,                       // free-form English
    pub source_file: Option<String>,        // None → auto-discover
    pub test_command: Option<String>,       // None → auto-detect
    pub language: Option<WitnessLanguage>,  // None → infer from source_file
    pub model: String,                      // e.g. "commonwealth/primary"
    pub config: SolveConfig,
}

/// ARCH §12.4 — `ChatBackend` lets tests inject canned responses
/// without a daemon. Production uses `ReqwestChatBackend`; tests use
/// `DeterministicChatBackend` (same pattern as `DeterministicInference`
/// in sovereign-inference).
#[async_trait]
pub trait ChatBackend: Send + Sync {
    async fn complete(
        &self,
        model: &str,
        messages: Vec<serde_json::Value>,
        temperature: f32,
        max_tokens: u32,
    ) -> Result<ChatResponse, BackendError>;
}

pub struct ChatResponse {
    pub content: String,
    pub tokens_in: u64,
    pub tokens_out: u64,
}

pub async fn solve(
    backend: &dyn ChatBackend,
    request: SolveRequest,
) -> SolveResponse;
```

The `force: bool` field moves to `Workdir::check_safe(path, force)` instead of `SolveRequest.force`. That keeps the request struct describing a request, not negotiating safety with itself.

pub struct SolveResponse {
    pub status: SolveStatus,
    pub diff: String,                       // unified diff of changes
    pub tests_before: TestSummary,
    pub tests_after: TestSummary,
    pub rounds_executed: u32,
    pub trajectory: Vec<RoundSummary>,      // per-round winner + score
    pub tokens: TokenCounts,
    pub wall_ms: u64,
}

pub enum SolveStatus {
    AllPassed,
    Improved,                               // strict improvement, but not all passing
    Stalled { rounds_without_improvement: u32 },
    Exhausted { rounds: u32 },              // hit round budget
    NoBaseline { reason: String },          // couldn't run baseline tests
    Errored { reason: String },
}
// Note: DirtyWorkdir is NOT a SolveStatus variant. It's a typed
// error returned from `Workdir::check_safe()` BEFORE `solve()` is
// called. The HTTP handler maps it to a 422 with hint; the solver
// itself never sees an unsafe workdir (ARCH §7.1).

pub struct RoundSummary {
    pub round: u32,
    pub winner_shape: Option<String>,
    pub winner_temp: Option<f32>,
    pub tests_passing_after: u32,
    pub candidates_attempted: u32,
}

pub async fn solve(
    http: &reqwest::Client,
    provider_url: &str,
    request: SolveRequest,
) -> SolveResponse;
```

**Migration of bench's `search.rs`:** becomes a thin `AgentRunner` impl that constructs a `SolveRequest` from `AgentRunContext` (via `Workdir::check_safe`) and calls `commonwealth_solver::solve` with a `ReqwestChatBackend`. ~60 lines instead of ~440.

**File layout** (ARCH §3.1 — each file < 800 lines, one concern):

```
commonwealth-solver/
├── Cargo.toml
├── assets/
│   ├── system_prompt.md          ← ARCH §6: prompts are data
│   ├── user_prompt.md.tmpl       ← jinja-style placeholder substitution
│   └── README.md                 ← what these are, how to tune
├── src/
│   ├── lib.rs                    ← façade re-exports + module wiring
│   ├── types.rs                  ← SolveConfig, SolveRequest, SolveResponse, SolveStatus
│   ├── workdir.rs                ← Workdir newtype + DirtyWorkdir
│   ├── backend.rs                ← ChatBackend trait + ReqwestChatBackend
│   ├── prompts.rs                ← load assets, render with substitutions
│   ├── registry.rs               ← SolverProvider trait + SolverRegistry
│   ├── search/
│   │   ├── mod.rs                ← SearchSolver (impls SolverProvider)
│   │   ├── candidate.rs          ← single-candidate try-emit-and-apply
│   │   ├── loop.rs               ← round dispatch + monotonic improvement
│   │   └── ladder.rs             ← temperature ladder + diversity widening
│   └── tests/
│       └── deterministic.rs      ← DeterministicChatBackend + scripted scenarios
```

Each module < 400 lines except possibly `search/loop.rs` (round orchestration). If `loop.rs` exceeds 400, split out the trajectory-building helper.

### Phase 6a.5 — Workdir safety check (structural, ARCH §7.1)

`commonwealth-solver/src/workdir.rs`:

```rust
pub struct Workdir(PathBuf);

impl Workdir {
    /// The ONLY constructor. `solve()` accepts `Workdir`, not
    /// `PathBuf`, so an unsafe workdir is unrepresentable
    /// downstream.
    pub fn check_safe(path: PathBuf, force: bool) -> Result<Self, DirtyWorkdir> {
        const FORBIDDEN: &[&str] = &[
            "/etc", "/usr", "/bin", "/sbin", "/boot", "/lib", "/lib64",
            // ~/.sovereign protects daemon state from autonomous mutation
        ];
        if FORBIDDEN.iter().any(|p| path.starts_with(p)) {
            return Err(DirtyWorkdir::SystemPath { path });
        }
        if force {
            return Ok(Self(path));
        }
        if !path.join(".git").exists() {
            return Err(DirtyWorkdir::NotAGitRepo { path });
        }
        let status = std::process::Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(&path)
            .output()
            .map_err(|e| DirtyWorkdir::UncommittedChanges {
                path: path.clone(),
            })?;
        if !status.stdout.is_empty() {
            return Err(DirtyWorkdir::UncommittedChanges { path });
        }
        Ok(Self(path))
    }

    pub fn path(&self) -> &Path { &self.0 }
}

pub enum DirtyWorkdir {
    SystemPath { path: PathBuf },
    UncommittedChanges { path: PathBuf },
    NotAGitRepo { path: PathBuf },
}

impl DirtyWorkdir {
    pub fn hint(&self) -> String {
        match self {
            Self::SystemPath { path } => format!(
                "workdir {} is a system path; refusing for safety.",
                path.display()
            ),
            Self::UncommittedChanges { path } => format!(
                "workdir {} has uncommitted changes. Commit or stash, then retry.",
                path.display()
            ),
            Self::NotAGitRepo { path } => format!(
                "workdir {} is not a git repo. Initialize one (git init && git commit -m initial) so the solver's changes are recoverable, then retry.",
                path.display()
            ),
        }
    }
}
```

**Tests pin the invariant (ARCH §7.2):**

```rust
#[test]
fn solve_signature_refuses_unsafe_workdir_at_compile_time() {
    // This test is a comment — the assertion is that solve()'s
    // signature takes `Workdir`, not `PathBuf`. If a future PR
    // changes the signature to accept PathBuf, this test should
    // fail to compile.
    fn assert_signature<F>(_f: F)
    where F: Fn(Workdir, ...) -> ... {}
    assert_signature(commonwealth_solver::solve);
}

#[test]
fn check_safe_refuses_uncommitted_changes() { ... }
#[test]
fn check_safe_refuses_system_paths() { ... }
#[test]
fn check_safe_accepts_clean_git_repo() { ... }
#[test]
fn check_safe_force_bypasses_dirty_check() { ... }
```

The Pi extension surfaces the `.hint()` text verbatim to its main model so the model can ask the user to fix and retry.

### Phase 6b — Auto-detection for test_command

Real user workdirs don't come with a verify_cmd specified. We need to detect:

```
if workdir/Cargo.toml exists           → "cargo test --quiet 2>&1"
if workdir/pyproject.toml or pytest.ini → "python3 -m pytest -q 2>&1"
if workdir/package.json with vitest    → "npx vitest run 2>&1"
if workdir/package.json with jest      → "npx jest 2>&1"
if workdir/go.mod                      → "go test -json ./..."
else                                   → return SolveStatus::NoBaseline
```

Lives in `commonwealth-solver` as `detect_test_command(workdir: &Path) -> Option<TestCommandSpec>`.

### Phase 6c — HTTP endpoint via SolverRegistry

**Pattern:** No hard dep from `sovereign-server` lib on `commonwealth-solver`. Instead a registry of trait objects, mirroring `AgentRunnerRegistry::builtin()`.

**Trait** (lives in a thin new crate `sovereign-solver-trait` OR in `commonwealth-solver` if we accept the dep direction). I lean toward putting it in `commonwealth-solver` since that crate is the natural owner of `SolveRequest`/`SolveResponse`:

```rust
// in commonwealth-solver
#[async_trait]
pub trait SolverProvider: Send + Sync {
    fn id(&self) -> &'static str;
    async fn solve(&self, request: SolveRequest) -> SolveResponse;
}

pub struct SolverRegistry {
    providers: HashMap<&'static str, Arc<dyn SolverProvider>>,
}

impl SolverRegistry {
    pub fn empty() -> Self { ... }
    pub fn builtin() -> Self {
        let mut r = Self::empty();
        r.register(Arc::new(SearchSolver::new()));
        // Future: r.register(Arc::new(NativeSolver::new()));
        //         r.register(Arc::new(BareMetalSolver::new()));
        r
    }
    pub fn register(&mut self, p: Arc<dyn SolverProvider>) {
        self.providers.insert(p.id(), p);
    }
    pub fn get(&self, id: &str) -> Option<Arc<dyn SolverProvider>> { ... }
    pub fn ids(&self) -> Vec<&'static str> { ... }
}
```

**`sovereign-server`'s dep**: `commonwealth-solver = { workspace = true }` (uses the trait + registry only — the trait crate could later be split out if we want to drop even this dep).

**Wiring** (in `sovereign-server/src/main.rs`):

```rust
let solvers = commonwealth_solver::SolverRegistry::builtin();
let app = router_with_solvers(solvers, ...);
```

**Route:** `POST /v1/solve/{solver_id}` — single handler dispatches via the registry. `{solver_id}` is `search` for v1; future solvers (`native`, `bare-metal`, `third-party-foo`) slot in by registering at startup. No route-handler changes when a new solver ships.

Returns 400 (not 404) with a helpful body when `solver_id` is unknown (ARCH §4.3 — explicit and loud, not silent):

```json
{ "error": "unknown solver_id 'foo'; registered: ['search']" }
```

Returns 422 with a hint when the workdir fails safety check:

```json
{
  "error": "dirty_workdir",
  "kind": "uncommitted_changes",
  "hint": "workdir /home/u/proj has uncommitted changes. Commit or stash, then retry."
}
```

**Handler flow:**

```rust
async fn solve_handler(
    Path(solver_id): Path<String>,
    Extension(registry): Extension<Arc<SolverRegistry>>,
    Json(body): Json<SolveRequestBody>,
) -> Response {
    let solver = match registry.get(&solver_id) {
        Some(s) => s,
        None => return bad_request(unknown_id_error(&solver_id, &registry)),
    };
    let workdir = match Workdir::check_safe(body.workdir, body.force) {
        Ok(w) => w,
        Err(dirty) => return unprocessable(dirty_workdir_response(&dirty)),
    };
    let req = SolveRequest::from_body(body, workdir);
    let resp = solver.solve(req).await;
    Json(resp).into_response()
}
```

**Request body** (JSON, mirrors SolveRequest minus the config which gets default + selective overrides):

```json
{
  "workdir": "/home/alex/projects/myrepo",
  "task": "Fix the failing parser tests",
  "test_command": "pytest tests/test_parser.py -q",
  "model": "commonwealth/primary",
  "max_rounds": 6,
  "max_candidates_per_round": 4
}
```

**Response body** (JSON, mirrors SolveResponse):

```json
{
  "status": "all_passed",
  "diff": "--- a/src/parser.py\n+++ b/src/parser.py\n@@ -...",
  "tests_before": { "passed": 7, "failed": 3, "total": 10 },
  "tests_after":  { "passed": 10, "failed": 0, "total": 10 },
  "rounds_executed": 2,
  "trajectory": [
    { "round": 0, "winner_shape": "patch_lines 89-92", "winner_temp": 0.4, "tests_passing_after": 8, "candidates_attempted": 4 },
    { "round": 1, "winner_shape": "patch_lines 230-365", "winner_temp": 0.9, "tests_passing_after": 10, "candidates_attempted": 4 }
  ],
  "tokens": { "input": 12450, "output": 1843 },
  "wall_ms": 88234
}
```

**Auth:** Behind the existing `/v1/*` auth middleware. Same shape as other endpoints.

**Security note:** `workdir` is an absolute path the daemon mutates. Threat model: anyone with API access can already point the model at any local file via the model's tool calls. The solve endpoint is no worse than that. **But** since it writes to disk autonomously without per-tool user approval, the design needs a default-safe stance:
- Reject `workdir` paths under `~/.sovereign/`, `/etc/`, `/usr/`, system dirs
- (Optional v1) Require workdir to be a git repo (so changes are recoverable via `git checkout`)
- (Optional v1) Soft consent — the daemon writes to a worktree copy, not the user's checkout, unless they explicitly opt in

These are real UX questions worth a follow-up section.

### Phase 6d — Pi extension

**New npm package:** `@commonwealth-ai/pi-search`

**Repo:** new directory `sovereign/integrations/pi-search/` (TypeScript). Publish to npm.

**Surface:**

```typescript
import { ExtensionContext, Type } from "@earendil-works/pi-coding-agent";

export default function activate(pi: ExtensionContext) {
  pi.registerTool({
    name: "commonwealth_solve",
    description: `Run a parallel-candidate search to repair failing tests.

Reach for this tool when:
- You've made 2+ normal edits that didn't improve test pass count
- The repair requires restructuring across multiple regions
- You want a "brute-force search" attempt at a hard test failure

The tool runs against the user's local Commonwealth daemon (sovereign).
It snapshots the workdir, tries 4 candidate fixes in parallel at varied
temperatures, runs tests on each, keeps the best, and iterates until tests
pass or it stalls. Typical wall time: 30s–3min. Returns a diff and the
trajectory.`,
    params: Type.Object({
      workdir: Type.String({ description: "Absolute path to the project root" }),
      task: Type.String({ description: "What to fix (free-form English)" }),
      test_command: Type.Optional(Type.String({ description: "Override the test command (auto-detected if omitted)" })),
    }),
    execute: async (args, ctx) => {
      const daemonUrl = process.env.COMMONWEALTH_URL ?? "http://localhost:9741";
      const resp = await fetch(`${daemonUrl}/v1/solve/search`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          workdir: args.workdir,
          task: args.task,
          test_command: args.test_command,
          model: "commonwealth/primary",
        }),
      });
      if (!resp.ok) {
        return `commonwealth_solve failed: ${resp.status} ${await resp.text()}`;
      }
      const r = await resp.json();
      return formatSolveResult(r);
    },
  });
}

function formatSolveResult(r: SolveResponse): string {
  // Compact summary the main pi model can read and act on.
  return [
    `Status: ${r.status}`,
    `Tests: ${r.tests_before.passed}/${r.tests_before.total} → ${r.tests_after.passed}/${r.tests_after.total}`,
    `Rounds: ${r.rounds_executed}, Wall: ${(r.wall_ms / 1000).toFixed(0)}s`,
    ``,
    `Trajectory:`,
    ...r.trajectory.map((rs) => `  R${rs.round}: ${rs.winner_shape ?? '(no winner)'} → ${rs.tests_passing_after}/${r.tests_after.total}`),
    ``,
    `Diff:`,
    "```diff",
    r.diff || "(no changes)",
    "```",
  ].join("\n");
}
```

**Distribution:** `npm publish @svrnmesh/pi-search` after the `@svrnmesh` org is claimed. Users install with `pi install @svrnmesh/pi-search` (per Pi's package-install pattern).

**Docs:** README explaining when to reach for it, how to configure the daemon URL, and the security model (it writes to the workdir; refuses dirty git).

### Phase 6d.5 — MCP tool variant

In parallel with the Pi extension, expose the same solver as an MCP tool on sovereign's existing MCP server (`http://localhost:9741/mcp`). Same library; different transport.

**Why:** Pi explicitly doesn't speak MCP, but Cursor, Cline, Claude Code, and growing list of other harnesses do. Shipping the MCP tool alongside the HTTP route gives us day-one reach to that ecosystem without writing per-harness plugins.

**Where:** add the tool to whatever module currently hosts the MCP server's tool list (per CLAUDE.md, the existing MCP tools include `symbols`, `callers`, `code_search`, etc. — the server is in `sovereign-tools` or `sovereign-server` depending on how it's split).

**Tool definition** (mirrors the Pi extension's surface):

```json
{
  "name": "solve_with_search",
  "description": "Drive a parallel-candidate test-driven search to repair failing tests in a local workdir. Reach for this when normal edits have failed to improve test pass count. Refuses dirty git workdirs.",
  "inputSchema": {
    "type": "object",
    "required": ["workdir", "task"],
    "properties": {
      "workdir": { "type": "string", "description": "Absolute path to project root (must be a clean git repo)" },
      "task":    { "type": "string", "description": "Free-form description of what needs fixing" },
      "test_command": { "type": "string", "description": "Override the test command (auto-detected if omitted)" }
    }
  }
}
```

Internally the MCP handler calls the same `SolverRegistry.get("search").solve(req)` path as the HTTP endpoint. ~30 LOC of plumbing.

**Naming convention check:** The Pi extension uses `commonwealth_solve` as the tool name (pi convention). The MCP tool uses `solve_with_search` (MCP convention — verb + adjective). They expose the same backend; the different names reflect different surface conventions.

### Phase 6e — Documentation

- `sovereign/docs/SOLVER.md`: end-user-facing — what is it, when to use, how to install the Pi extension, what daemon endpoints look like.
- Update `sovereign/SYSTEM_OVERVIEW.md` to mention `commonwealth-solver` as a first-class subsystem.
- Memory note recording the architectural decision (sovereign = solver backend, not harness).

## Open design questions for review

**Resolved 2026-05-24 (initial review):**

1. ✅ **HTTP endpoint location** — `sovereign-server`, but **NOT** as a hard dep on `commonwealth-solver`. Instead: registry pattern (see "SolverRegistry" below), so server's lib depends on a narrow `SolverProvider` trait + types; concrete impls (SearchSolver, future NativeSolver, third-party solvers) register at wiring time. Mirrors `AgentRunnerRegistry::builtin()`.

2. ✅ **Dirty git workdir** — **Refuse**. Fits the project ethos: the solver makes autonomous edits; the user's safety net is `git`. Hard error with actionable text: "workdir has uncommitted changes; commit or stash first, then retry." A `force: true` boolean in the request body opts out for power users (the Pi extension does NOT expose this; advanced HTTP callers can set it directly).

3. ✅ **MCP variant alongside HTTP** — Ship both in v1. ~30 min extra work; unblocks Cursor/Cline/Claude Code on day one alongside Pi. The MCP tool is a thin wrapper over the same `commonwealth-solver` library.

4. ✅ **npm org** — `@svrnmesh/*`. The Pi extension is `@svrnmesh/pi-search`. Org will be claimed before publish.

**Still open (recommendations baked in; flag if you disagree):**

5. **Auto-detect test_command in the daemon or in the Pi extension?** **Recommendation: daemon does the default detection; the request body's `test_command` field overrides.** Single impl serves all harnesses.

6. **Streaming progress?** **Recommendation: defer to v2.** Pi's "spinner during tool call" UX is fine for v1.

7. **Default temp ladder for non-primary models?** **Recommendation: ship the validated defaults; expose `temp_ladder` as an optional request-body override for advanced users.**

8. **Concurrency limit?** **Recommendation: v1 ships a single-flight mutex on the endpoint; v1.5 grows a per-workspace queue.**

9. **Persist run artifacts?** **Recommendation: persist to `~/.sovereign/solves/<timestamp>/` (workdir before/after snapshot + per-round trajectory JSON) so users can post-hoc debug.**

## Effort estimate

| Phase | Work | Days |
|---|---|---|
| 6a — `commonwealth-solver` crate extraction | Refactor `runners/search.rs` + types into new crate; `SolverProvider` trait + `SolverRegistry`; **+ assets extraction for prompts (§6); + `ChatBackend` trait + `DeterministicChatBackend` (§12.4)** | 1.5 |
| 6a.5 — Structural Workdir newtype | `Workdir::check_safe()` + `DirtyWorkdir` enum + 5 tests pinning the invariant (§7.1) | 0.5 |
| 6b — Auto-detect test_command | Adapter pattern, ~50 LOC + tests | 0.5 |
| 6c — `POST /v1/solve/{solver_id}` HTTP endpoint | Axum handler dispatching via SolverRegistry; handler-level workdir-safety mapping to 422 | 0.5 |
| 6d — Pi extension TypeScript | ~200 LOC + README + npm publish to `@svrnmesh` | 1.0 |
| 6d.5 — MCP `solve_with_search` tool | ~30 LOC plumbing on existing MCP server | 0.25 |
| 6e — Docs | SOLVER.md, SYSTEM_OVERVIEW §10 update, memory note for the architecture decision | 0.5 |
| **Total** | | **4.75 days** |

The bumps from v1's 4.0 days are the ARCH_PRINCIPLES-driven additions:
- ARCH §6: prompts-as-assets adds ~0.25 day (asset loader + tests)
- ARCH §12.4: `ChatBackend` trait + deterministic mock adds ~0.25 day
- ARCH §7.1: `Workdir` structural newtype + tests, bumped 6a.5 from 0.25 → 0.5 day

## What this design intentionally does not include

- **Opencode plugin.** Same approach (extension wrapping HTTP endpoint) once 6a-6c are in place. Separate package; can ship after Pi or in parallel.
- **MCP server exposure.** Pi explicitly doesn't speak MCP. Other harnesses do (Cursor, Cline, Claude Code). Adding `commonwealth_solve` as an MCP tool exported from sovereign's existing MCP server (`http://localhost:9741/mcp`) is the same shape as the HTTP endpoint, just a different transport. Worth shipping in parallel with the HTTP endpoint — both surface the same `commonwealth-solver` library.
- **Multi-language test runners.** v1 auto-detects pytest, cargo, vitest, jest, go test. Other runners (rspec, mocha, etc.) extend the pattern as needed.
- **Multi-step solver chaining.** The user's pi model can call `commonwealth_solve` multiple times in a session if it wants. We don't try to manage that.
- **Sandboxing.** The solver runs commands inside the user's workdir. They've already granted that access by setting up the daemon and pi. No new threat surface beyond what's already there.

## Decision checkpoint — resolved 2026-05-24 (initial review)

- [x] **Crate location**: `sovereign/crates/commonwealth-solver/`
- [x] **HTTP endpoint location**: `sovereign-server`, but routed via `SolverRegistry` trait pattern — server's lib doesn't hard-dep on any concrete solver; the binary's `main.rs` does the wiring at startup
- [x] **Default behavior on dirty git workdir**: **refuse** with actionable error. Optional `force: true` flag in request body for advanced HTTP/MCP callers (Pi extension doesn't expose it). Fits the project's "the user's safety net is git" ethos.
- [x] **Ship MCP tool variant in v1**: yes, alongside the HTTP route. Same library, two transports. Day-one reach to Cursor/Cline/Claude Code on top of Pi.
- [x] **npm org**: `@svrnmesh` (will be claimed before publish, modeled after huggingface's org pattern)

## Implementation kickoff checklist (for next session)

Listed in execution order — each item gates the next.

**Prereqs (do before any code):**

- [ ] Claim `@svrnmesh` on npm
- [ ] Promote `regex` to `[workspace.dependencies]` and switch `sovereign-agent-bench/Cargo.toml` to inherit (ARCH §8.1)

**Phase 6a — `commonwealth-solver` crate:**

- [ ] Create `sovereign/crates/commonwealth-solver/` per the file-layout table
- [ ] `assets/system_prompt.md` + `assets/user_prompt.md.tmpl` — extract from current Rust string literals (ARCH §6)
- [ ] `prompts.rs` — `include_str!` the assets + tiny placeholder substitution
- [ ] `backend.rs` — `ChatBackend` trait + `ReqwestChatBackend` + `DeterministicChatBackend` (ARCH §12.4)
- [ ] `workdir.rs` — `Workdir::check_safe()` + `DirtyWorkdir` enum + 5 invariant tests (ARCH §7.1)
- [ ] `types.rs` — `SolveConfig`, `SolveRequest`, `SolveResponse`, `SolveStatus`
- [ ] `registry.rs` — `SolverProvider` trait + `SolverRegistry`
- [ ] `search/` — port `runners::search` logic; use `DeterministicChatBackend` in unit tests
- [ ] `lib.rs` — façade re-exports

**Phase 6a.bench — migrate the bench's runner:**

- [ ] `sovereign-agent-bench/runners/search.rs` becomes ~60 LOC `AgentRunner` wrapper around `commonwealth_solver::solve` with `ReqwestChatBackend`
- [ ] `sovereign-agent-bench/runners/shared.rs` keeps the bench-specific helpers; non-bench helpers move to `commonwealth-solver`
- [ ] Bench tests still green

**Phase 6c — HTTP endpoint:**

- [ ] `sovereign-server` depends on `commonwealth-solver` (workspace dep)
- [ ] `sovereign-server/src/routes_solve.rs` — handler per the flow above
- [ ] `sovereign-server/src/main.rs` — `SolverRegistry::builtin()` wired at startup
- [ ] Smoke: `curl -X POST http://localhost:9741/v1/solve/search -d '{...}'`

**Phase 6d.5 — MCP tool:**

- [ ] Add `solve_with_search` tool to the existing MCP server's tool list
- [ ] Reuses the same `SolverRegistry` from the HTTP path

**Phase 6d — Pi extension:**

- [ ] `sovereign/integrations/pi-search/` TypeScript package
- [ ] `package.json` → `@svrnmesh/pi-search`
- [ ] README — when to install, how to configure daemon URL, security model
- [ ] `npm publish`
- [ ] End-to-end smoke: install in a real Pi session, model invokes `commonwealth_solve`, fix lands

**Phase 6e — Docs:**

- [ ] `sovereign/docs/SOLVER.md` — end-user-facing
- [ ] `sovereign/SYSTEM_OVERVIEW.md` §10 — list `commonwealth-solver` as a subsystem
- [ ] Memory note: "sovereign is the local solver backend, not a coding harness — Pi/Cline/Cursor remain the user's IDE choice"
