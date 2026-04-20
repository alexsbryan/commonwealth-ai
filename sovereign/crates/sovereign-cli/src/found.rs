//! `sovereign project found` — the four-stage founding conversation.
//!
//! M6.3 delivers **Stage 1 (Understanding)**. Later stages land in
//! M6.4 (fault lines), M6.5 (CHARTER.md + PHASES.md), M6.6 (doc
//! URL question + fetch).
//!
//! ## Stage 1 — what we're doing
//!
//! The user runs `sovereign project found [--design <path>]` once,
//! after `project init`, before the agent starts writing code. This
//! stage's job is to turn ambient knowledge into explicit,
//! durable decisions — things that are expensive to change later.
//!
//! The discipline (from requirements):
//! - **Max five questions.** Not a conversation, not an interview —
//!   the smallest set of questions that materially changes the
//!   output.
//! - **Each question states why.** The user should always know why
//!   they're being asked; "because the tool asks" is an anti-goal.
//! - **Derived answers don't get asked.** If the design document
//!   or the observation already supplies the answer, we skip.
//! - **Persistence.** Every answered question becomes a
//!   `decision`-kind note in the project scope. A session six
//!   weeks later reads those notes and knows why the choice was
//!   made.
//!
//! ## Not yet implemented in M6.3
//!
//! - CHARTER.md / PHASES.md authoring (Stages 3/4 — M6.5).
//! - Fault-line surfacing from the knowledge corpus (Stage 2 — M6.4).
//! - The single documentation-URLs question + corpus fetch (M6.6).
//! - LLM-driven material-question generation from a free-text
//!   design doc. Until that lands, question selection is governed
//!   by the curated `catalog()` below plus observation-based
//!   filtering. The catalog is explicit about what's expensive to
//!   change; the LLM path in a later milestone will generate
//!   project-specific follow-ups.
//!
//! ## Seams
//!
//! [`FoundInterlocutor`] is the ask/answer trait, separate from
//! `honesty::Interlocutor` because the question shapes differ
//! (decisions vs information gaps). Production uses stdin; tests
//! use a scripted stub.
//!
//! The catalog of questions is static data. Tests assert which
//! questions fire given which observations.

// Same #![allow(dead_code)] precedent as honesty.rs — the
// integration wire-up (cmd_found) only uses the high-level runner;
// sub-items get pinned as M6.3's consumer graph widens.
#![allow(dead_code)]

use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::observation::{DepKind, DetectedDependency, ProjectObservation};

// ─── Stage 1 data ────────────────────────────────────────────────────────────

/// A single Stage-1 question. `why` MUST be non-empty — the
/// requirements are explicit that we never ask without saying why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stage1Question {
    /// Stable id, used in the note and in tests. Dash-separated,
    /// namespaced under `found.stage1.`.
    pub id: String,
    pub prompt: String,
    pub why: String,
}

/// The user's answer. Empty `text` means "skipped" — the user saw
/// the question but declined to commit. We still record the
/// skip, with content that makes the defer explicit, so the next
/// session sees "we asked; they punted" instead of "we never asked."
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stage1Answer {
    pub question_id: String,
    pub text: String,
    pub skipped: bool,
}

// ─── Catalog ─────────────────────────────────────────────────────────────────

/// The v1 curated catalog. Each entry carries a predicate that
/// decides whether the question is material GIVEN the observation.
///
/// ## How to edit
///
/// Adding a question: append an entry. Keep the prompt under 140
/// chars; put anything longer in `why`.
///
/// Removing a question: delete the entry. The curated set is
/// meant to shrink as LLM-driven generation takes over — this
/// catalog is the floor, not the ceiling.
///
/// Changing `id`: don't. Notes written under the old id stay
/// under the old id. Pick a new id and add a new entry if the
/// question reshapes.
struct CatalogEntry {
    id: &'static str,
    prompt: &'static str,
    why: &'static str,
    /// Predicate: should this question fire for the given
    /// observation + design-doc presence?
    applicable: fn(&ProjectObservation, bool) -> bool,
}

fn catalog() -> &'static [CatalogEntry] {
    &[
        // Q1 — only fires when there's no design document; when
        // there IS one we read it instead.
        CatalogEntry {
            id: "found.stage1.project-purpose",
            prompt: "In one paragraph: what is this project building, and who is it for?",
            why: "We need the problem statement in the user's own words so the charter doesn't invent one.",
            applicable: |_obs, has_design| !has_design,
        },
        // Q2 — data/persistence contract. Always fires: every project
        // has SOMETHING that must survive across deploys, and getting
        // its shape wrong is expensive.
        CatalogEntry {
            id: "found.stage1.persistence-contract",
            prompt: "What state must survive across restarts / deploys, and what downstream or external consumer relies on its shape?",
            why: "Persistence contracts are expensive to change later — anyone reading the field you rename has to change too.",
            applicable: |_obs, _has_design| true,
        },
        // Q3 — interface boundary. Always fires when there's at
        // least one detected external dependency; otherwise skip.
        CatalogEntry {
            id: "found.stage1.external-interface",
            prompt: "For each external service or library the project depends on, what assumption about its behavior are you trusting most? (Rate limits, ordering guarantees, schema stability, …)",
            why: "The assumptions you trust become the invariants the charter must name. When they break, you want the decision note explaining what you were counting on.",
            applicable: |obs, _has_design| obs.deps.iter().any(|d| d.kind == DepKind::Direct),
        },
        // Q4 — evolution vs stability. Always fires: forces an
        // explicit answer to "what's the stable spine vs the
        // rapidly-iterating edge?"
        CatalogEntry {
            id: "found.stage1.evolution-spine",
            prompt: "Which parts of the system do you expect to evolve rapidly, and which parts should stay stable for at least the first few months?",
            why: "ATOS milestones + the charter's invariants should protect the stable spine and leave the rapid parts unconstrained.",
            applicable: |_obs, _has_design| true,
        },
        // Q5 — naming/convention decision. Only fires when the
        // project has enough scope (any external dep OR a workspace
        // setup) to make conventions material. Trivial scripts
        // skip this.
        CatalogEntry {
            id: "found.stage1.convention-risk",
            prompt: "Is there a domain convention you've already chosen over an alternative (e.g., dealer gamma vs market-maker gamma, UTC vs local time, dollars vs cents, singular vs plural resource names)?",
            why: "These are cheap to decide once and expensive to change later — someone will assume the other convention if it's not documented.",
            applicable: |obs, _has_design| {
                !obs.deps.is_empty() || obs.languages.len() > 1
            },
        },
    ]
}

/// Hard cap enforcing the requirements' "max five questions" rule.
/// If the catalog ever grows past this, we still cap at selection
/// time rather than relying on editorial discipline.
const MAX_QUESTIONS: usize = 5;

/// Pick the questions that fire for this `(observation, has_design)`
/// combo, honoring [`MAX_QUESTIONS`].
fn select_questions(obs: &ProjectObservation, has_design: bool) -> Vec<Stage1Question> {
    catalog()
        .iter()
        .filter(|c| (c.applicable)(obs, has_design))
        .take(MAX_QUESTIONS)
        .map(|c| Stage1Question {
            id: c.id.into(),
            prompt: c.prompt.into(),
            why: c.why.into(),
        })
        .collect()
}

// ─── Interlocutor seam ───────────────────────────────────────────────────────

/// Stage-1-specific ask/answer. Kept separate from
/// `honesty::Interlocutor` because the shape is different: we
/// don't have a "best guess" to accept, just a free-text prompt
/// the user answers in their own words.
pub trait FoundInterlocutor {
    fn ask_stage1(&mut self, q: &Stage1Question) -> Stage1Answer;
}

/// Stdin-backed implementation. Reads a single line per ask;
/// blank input records a skipped answer.
///
/// ## Why no persistent `BufReader`
///
/// An earlier draft held `Box<dyn BufRead + Send>` so both stages
/// could share a reader. That's a trap: `BufReader::read_line`
/// pre-fetches bytes from the underlying stdin into its private
/// buffer; when the reader is dropped those bytes go with it.
/// Splitting stdin consumers across Stage-1 and Stage-2
/// interlocutors meant Stage-1 could swallow a Stage-2 answer.
/// The fix is trivial: grab `io::stdin()` per ask — it's the same
/// shared handle — and read one line off it. Zero cross-stage
/// bleed, at the cost of one system call per prompt (negligible at
/// CLI speeds).
pub struct StdinFoundInterlocutor;

impl StdinFoundInterlocutor {
    pub fn new() -> Self {
        Self
    }
}

impl Default for StdinFoundInterlocutor {
    fn default() -> Self {
        Self::new()
    }
}

pub(crate) fn stdin_read_line() -> String {
    let mut line = String::new();
    if io::stdin().read_line(&mut line).is_err() {
        return String::new();
    }
    line.trim_end_matches(&['\r', '\n'][..]).to_string()
}

impl FoundInterlocutor for StdinFoundInterlocutor {
    fn ask_stage1(&mut self, q: &Stage1Question) -> Stage1Answer {
        let mut stderr = io::stderr();
        let _ = writeln!(stderr);
        let _ = writeln!(stderr, "? {}", q.prompt);
        let _ = writeln!(stderr, "  Why I'm asking: {}", q.why);
        let _ = write!(stderr, "  (Enter to skip) > ");
        let _ = stderr.flush();
        let text = stdin_read_line();
        let skipped = text.trim().is_empty();
        Stage1Answer {
            question_id: q.id.clone(),
            text: text.trim().to_string(),
            skipped,
        }
    }
}

// ─── Note writer seam ────────────────────────────────────────────────────────

/// Persistence side of Stage 1. Separate trait (not a concrete
/// NoteStore call) so tests can assert the note shape without
/// spinning up SQLite. Production uses
/// [`NoteStoreDecisionWriter`].
pub trait DecisionRecorder {
    fn record(&mut self, question: &Stage1Question, answer: &Stage1Answer);
}

/// Format a Stage-1 Q&A into the canonical note body. Extracted so
/// both production and tests use the identical rendering.
pub fn render_decision_body(q: &Stage1Question, a: &Stage1Answer) -> String {
    if a.skipped {
        format!(
            "Stage 1 · {}\n\nQuestion: {}\nWhy asked: {}\n\nAnswer: _(skipped by user)_\n",
            q.id, q.prompt, q.why,
        )
    } else {
        format!(
            "Stage 1 · {}\n\nQuestion: {}\nWhy asked: {}\n\nAnswer:\n{}\n",
            q.id, q.prompt, q.why, a.text,
        )
    }
}

/// Production recorder: writes into a `NoteStore` in Global scope
/// with kind=`decision`.
pub struct NoteStoreDecisionWriter<'a> {
    pub store: &'a corpus_engine::NoteStore,
    pub session_id: &'a str,
    /// Collected ids of written notes, so the caller can surface
    /// them in the Stage-1 summary.
    pub written: Vec<String>,
    /// Runtime for blocking writes inside a sync trait method.
    /// Reused across calls so we don't spin up a runtime per Q&A.
    pub rt: tokio::runtime::Handle,
}

impl<'a> DecisionRecorder for NoteStoreDecisionWriter<'a> {
    fn record(&mut self, question: &Stage1Question, answer: &Stage1Answer) {
        let body = render_decision_body(question, answer);
        let id_res = tokio::task::block_in_place(|| {
            self.rt.block_on(self.store.write_note_scoped(
                "decision",
                &body,
                Vec::new(),
                Vec::new(),
                self.session_id,
                corpus_engine::NoteScope::Global,
                None,
            ))
        });
        match id_res {
            Ok(id) => self.written.push(id),
            Err(e) => {
                eprintln!("    \u{2717} Stage 1 note write failed: {e}");
            }
        }
    }
}

// ─── Runner ──────────────────────────────────────────────────────────────────

/// Orchestrates Stage 1 end-to-end: selection → ask loop →
/// recording. Returns the answers so the caller can render a
/// summary line.
pub fn run_stage1<I: FoundInterlocutor, R: DecisionRecorder>(
    obs: &ProjectObservation,
    design: Option<&str>,
    interlocutor: &mut I,
    recorder: &mut R,
) -> Vec<Stage1Answer> {
    let has_design = design.is_some();
    let questions = select_questions(obs, has_design);
    let mut answers = Vec::with_capacity(questions.len());
    for q in &questions {
        let a = interlocutor.ask_stage1(q);
        recorder.record(q, &a);
        answers.push(a);
    }
    answers
}

/// Read a design document into a string. Returns `None` on
/// read-error OR when the path is missing — the caller then
/// falls through to elicitation mode. Errors aren't propagated
/// because `found` treats "missing design doc" the same as
/// "no design doc."
pub fn load_design(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

/// Convenience for the CLI: extracts the first non-empty paragraph
/// of a design document for an at-a-glance confirmation line.
pub fn design_preview(doc: &str) -> String {
    doc.split("\n\n")
        .find(|p| !p.trim().is_empty())
        .map(|p| p.trim().replace('\n', " "))
        .unwrap_or_default()
}

/// Absolute path `<repo_root>/.sovereign/design.md`, for
/// commands that want to offer "we saved a copy" later.
#[allow(dead_code)]
pub fn canonical_design_path(repo_root: &Path) -> PathBuf {
    repo_root.join(".sovereign").join("design.md")
}

// ═════════════════════════════════════════════════════════════════════════════
// Stage 2 — Fault lines (M6.4)
// ═════════════════════════════════════════════════════════════════════════════
//
// Stage 2 names genuine technical disagreements in the project's
// domain — places where reasonable people pick different sides.
// The system presents each fault line as an honest uncertainty,
// not a recommendation; the user decides.
//
// Per requirements:
// - Draw from local knowledge corpus first, web second.
// - Resolved fault lines → `decision`-kind notes.
// - Open fault lines → `uncertainty`-kind notes.
// - Nothing is written that the user didn't explicitly consent to.
//
// M6.4 ships the curated catalog path. The honesty-protocol hook
// for fetching domain-specific fault lines from web is a clearly
// documented seam — the runner exposes `surface_domain_gap` so the
// caller (cmd_found) can invoke the honesty protocol between the
// curated pass and the summary.

// ─── Data ────────────────────────────────────────────────────────────────────

/// A named technical disagreement with 2+ defensible sides. The
/// system presents all sides neutrally; the user picks one, leaves
/// it open, or skips.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FaultLine {
    pub id: String,
    pub title: String,
    /// One-sentence framing of the disagreement.
    pub summary: String,
    /// The defensible positions. 2 is the minimum meaningful; 3
    /// is common (sync/async/actor); we don't enforce a max but
    /// keep catalog entries to <= 4.
    pub sides: Vec<FaultLineSide>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FaultLineSide {
    pub label: String,
    /// Prose. Should name BOTH what makes this side attractive AND
    /// what it costs — the point is to help the user see the real
    /// trade-off, not to advocate.
    pub tradeoffs: String,
}

/// What the user decided for a fault line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FaultLineOutcome {
    /// User picked a side. `choice` is free-text (doesn't have to
    /// match a `FaultLineSide.label` verbatim — the user may
    /// synthesize their own position). `reasoning` is why they
    /// picked it, for future sessions to read.
    Resolved { choice: String, reasoning: String },
    /// Deferred. `note` may be empty — the mere acknowledgment
    /// that this fault line exists is the value.
    Open { note: String },
    /// User moved on without deciding OR noting. No record written.
    Skipped,
}

// ─── Catalog ─────────────────────────────────────────────────────────────────

struct FaultLineEntry {
    id: &'static str,
    title: &'static str,
    summary: &'static str,
    sides: &'static [(&'static str, &'static str)],
    /// Predicate: should this fault line surface given the
    /// observation + the prior stage-1 answers?
    applicable: fn(&ProjectObservation, &[String]) -> bool,
}

fn fault_line_catalog() -> &'static [FaultLineEntry] {
    &[
        FaultLineEntry {
            id: "fault.time-representation",
            title: "Time representation",
            summary: "Do timestamps live in UTC everywhere, or preserve local offsets at boundaries?",
            sides: &[
                (
                    "UTC-everywhere",
                    "Every timestamp is stored + compared in UTC. Conversions happen only at the UI seam. Easy to reason about; painful when the domain cares about calendar-local concepts (trading hours, business-day rollovers, user-facing schedules).",
                ),
                (
                    "Local-at-boundaries",
                    "Ingested timestamps retain their original offset; conversions happen at specific seams. Matches human intuition for domain-local events; easy to mis-convert at the wrong seam. Requires a discipline about `chrono::DateTime<Tz>` vs naive types throughout.",
                ),
            ],
            // Time is universal; fire always.
            applicable: |_obs, _ans| true,
        },
        FaultLineEntry {
            id: "fault.id-scheme",
            title: "Identifier scheme",
            summary: "UUID v4, sortable (v7 / ULID), snowflake, or DB-auto-increment — the ID choice ripples through logs, URLs, joins, and replication.",
            sides: &[
                (
                    "UUID v4",
                    "Universally unique, no coordination. Unsortable; hurts B-tree locality on large tables; opaque for humans reading logs.",
                ),
                (
                    "Sortable (UUID v7 / ULID)",
                    "Time-prefixed, globally unique, index-friendly. Leaks creation time (sometimes fine, sometimes a leak). Newer libs; less mature tooling on some runtimes.",
                ),
                (
                    "DB auto-increment",
                    "Smallest, fastest joins, human-readable. Couples you to the DB; replication + multi-region gets hairy; guessable.",
                ),
                (
                    "Snowflake / 64-bit structured",
                    "Time-sortable + shard-encoded. Requires a coordinator or clever epoch handling; tooling cost.",
                ),
            ],
            applicable: has_persistence_signal,
        },
        FaultLineEntry {
            id: "fault.persistence-shape",
            title: "Persistence shape",
            summary: "Mutable tables (last-write-wins) vs append-only event log (replay from events) vs a hybrid with projections.",
            sides: &[
                (
                    "Mutable tables",
                    "Simple model, fast reads, every dev already knows how. No free audit log; time-travel queries need application-level bookkeeping; bugs that corrupt state are hard to recover from.",
                ),
                (
                    "Append-only event log",
                    "Perfect audit by construction; replay fixes corruption; analytics over event stream is natural. Reads require projections; bugs in projections are a full category of new problem; schema migrations are event-migrations, which are harder.",
                ),
                (
                    "Hybrid (events + snapshot tables)",
                    "Audit where you need it, speed where you don't. Two stores to keep consistent; overhead if the audit is never used.",
                ),
            ],
            applicable: has_persistence_signal,
        },
        FaultLineEntry {
            id: "fault.error-surface",
            title: "Error surface in user-facing APIs",
            summary: "Typed errors-as-values (Result / Either) vs exceptions vs HTTP-status-only. Determines what downstream consumers can do.",
            sides: &[
                (
                    "Typed errors-as-values",
                    "Every failure is visible in the return type; caller handles them explicitly. More verbose; some teams hate the ceremony.",
                ),
                (
                    "Exceptions",
                    "Happy path stays readable; failures propagate for free. Easy to miss a failure mode; harder to document completely.",
                ),
                (
                    "HTTP status + opaque body",
                    "Minimal coupling; downstreams interpret status codes. Loses structured detail; error recovery across services depends on convention.",
                ),
            ],
            applicable: has_web_framework_signal,
        },
        FaultLineEntry {
            id: "fault.schema-evolution",
            title: "Schema evolution",
            summary: "Breaking bumps via API/schema versioning vs additive-only with deprecation windows. Drives what the charter's invariants must protect.",
            sides: &[
                (
                    "Versioned endpoints / schemas",
                    "Clean breaks; clear lifecycle; consumers migrate when they're ready. N versions in flight means N codepaths; versioning gets gamed if bumps are free.",
                ),
                (
                    "Additive-only + deprecation window",
                    "No breaking changes; downstream safety is structural. Accretion of unused fields; deletions require long-held commitments; some changes genuinely need a break.",
                ),
            ],
            applicable: has_external_consumer_signal,
        },
        FaultLineEntry {
            id: "fault.concurrency-model",
            title: "Concurrency model",
            summary: "async/await vs explicit threads/processes vs actor/CSP. Shapes the codebase for years.",
            sides: &[
                (
                    "async/await",
                    "Dense IO-concurrency in a single process; cheap tasks. Coloring function problem; debugging stacks is harder; CPU-bound work still needs threads.",
                ),
                (
                    "Threads / processes",
                    "Simple mental model, debuggable stacks, unlocks CPU cores. Context-switch cost; shared state requires discipline; fewer concurrent tasks before you feel it.",
                ),
                (
                    "Actors / CSP",
                    "Message-passing forces explicit state boundaries; scales well horizontally. Library / runtime buy-in; debugging distributed actors is a specialty.",
                ),
            ],
            applicable: has_concurrency_signal,
        },
        FaultLineEntry {
            id: "fault.secrets",
            title: "Secrets",
            summary: "Env vars, file-based vault, or KMS-backed at request time. Affects dev ergonomics, audit, and rotation cost.",
            sides: &[
                (
                    "Environment variables",
                    "Zero setup; every platform supports it. Leaks to process listings + crash dumps; rotation means restart; audit is whatever your deploy tool does.",
                ),
                (
                    "File-based vault (1Password, age-encrypted, etc.)",
                    "Works offline; per-env config is just a file swap. Requires a fetch/unseal gesture; drift between environments if discipline slips.",
                ),
                (
                    "KMS / vault at request time",
                    "Rotation is live; audit is centralized; credentials never sit on disk decrypted. Adds a runtime dependency and a failure mode on the hot path.",
                ),
            ],
            applicable: has_external_api_signal,
        },
        FaultLineEntry {
            id: "fault.delivery-semantics",
            title: "Delivery semantics",
            summary: "At-most-once vs at-least-once vs effectively-exactly-once for messages / events / jobs.",
            sides: &[
                (
                    "At-most-once",
                    "Simple; never duplicates. Can drop on failure; unacceptable for payments, counting, anything that must not under-deliver.",
                ),
                (
                    "At-least-once + idempotent consumers",
                    "Safe; standard in the industry. Every consumer must be idempotent — that discipline is a design constraint, not an afterthought.",
                ),
                (
                    "Effectively-exactly-once (transactional dedup)",
                    "Strongest guarantee. Requires coordinated state (dedup window + ack log); operational burden on the whole pipeline.",
                ),
            ],
            applicable: has_queue_signal,
        },
    ]
}

// ─── Predicates ──────────────────────────────────────────────────────────────
//
// These scan `(observation, stage1_answers)` for signals that make a
// fault line material. They're conservative: we'd rather surface a
// relevant fault line than skip one, but we also won't fire
// fault.delivery-semantics on a static-site project.

fn has_persistence_signal(obs: &ProjectObservation, answers: &[String]) -> bool {
    let persistence_deps = [
        "sqlite", "rusqlite", "sqlx", "diesel", "sea-orm", "postgres", "pg",
        "tokio-postgres", "mysql", "mongodb", "redis", "rocksdb", "sled", "lmdb",
        "pymongo", "psycopg", "psycopg2", "sqlalchemy", "prisma", "typeorm",
        "gorm", "pgx", "mongoose", "sequelize",
    ];
    has_any_dep_matching(obs, &persistence_deps)
        || answers_mention_any(answers, &["database", "persist", "store", "migration", "schema"])
}

fn has_web_framework_signal(obs: &ProjectObservation, answers: &[String]) -> bool {
    let web_deps = [
        "axum", "actix-web", "rocket", "warp", "hyper", "tower",
        "express", "fastify", "koa", "hono", "next",
        "fastapi", "flask", "django", "starlette",
        "gin", "echo", "fiber", "chi",
    ];
    has_any_dep_matching(obs, &web_deps)
        || answers_mention_any(answers, &["api", "endpoint", "http", "rest", "grpc"])
}

fn has_external_consumer_signal(obs: &ProjectObservation, answers: &[String]) -> bool {
    has_web_framework_signal(obs, answers)
        || answers_mention_any(
            answers,
            &["consumer", "downstream", "client", "integrator", "partner"],
        )
}

fn has_concurrency_signal(obs: &ProjectObservation, answers: &[String]) -> bool {
    let async_deps = ["tokio", "async-std", "smol", "asyncio", "trio", "anyio"];
    // Languages with async-as-ambient also count.
    let lang_signal = obs
        .languages
        .iter()
        .any(|l| matches!(l.id.as_str(), "javascript" | "typescript" | "go"));
    has_any_dep_matching(obs, &async_deps)
        || lang_signal
        || answers_mention_any(answers, &["concurrent", "parallel", "throughput", "latency"])
}

fn has_external_api_signal(obs: &ProjectObservation, answers: &[String]) -> bool {
    let api_client_deps = [
        "reqwest", "ureq", "hyper", "http", "requests", "httpx", "aiohttp",
        "axios", "fetch", "got", "undici", "polygon", "alpaca", "stripe",
        "twilio", "aws-sdk", "google-cloud", "firebase",
    ];
    has_any_dep_matching(obs, &api_client_deps)
        || answers_mention_any(answers, &["api key", "oauth", "token", "secret", "credential"])
}

fn has_queue_signal(obs: &ProjectObservation, answers: &[String]) -> bool {
    let queue_deps = [
        "kafka", "rdkafka", "nats", "rabbitmq", "amqp", "lapin", "sqs",
        "pubsub", "redpanda", "pulsar", "nsq", "celery", "sidekiq", "bull",
    ];
    has_any_dep_matching(obs, &queue_deps)
        || answers_mention_any(answers, &["queue", "stream", "event bus", "pipeline"])
}

fn has_any_dep_matching(obs: &ProjectObservation, needles: &[&str]) -> bool {
    obs.deps.iter().any(|d| {
        let lower = d.name.to_lowercase();
        needles.iter().any(|n| lower.contains(n))
    })
}

fn answers_mention_any(answers: &[String], keywords: &[&str]) -> bool {
    answers.iter().any(|a| {
        let lower = a.to_lowercase();
        keywords.iter().any(|k| lower.contains(k))
    })
}

// ─── Selection ───────────────────────────────────────────────────────────────

/// Pick the fault lines that apply. Returns them in catalog order
/// — deterministic, so the UX is stable across runs on the same
/// project.
pub fn select_fault_lines(obs: &ProjectObservation, stage1_answers: &[Stage1Answer]) -> Vec<FaultLine> {
    let answer_texts: Vec<String> = stage1_answers
        .iter()
        .filter(|a| !a.skipped)
        .map(|a| a.text.clone())
        .collect();
    fault_line_catalog()
        .iter()
        .filter(|e| (e.applicable)(obs, &answer_texts))
        .map(|e| FaultLine {
            id: e.id.into(),
            title: e.title.into(),
            summary: e.summary.into(),
            sides: e
                .sides
                .iter()
                .map(|(label, tradeoffs)| FaultLineSide {
                    label: (*label).into(),
                    tradeoffs: (*tradeoffs).into(),
                })
                .collect(),
        })
        .collect()
}

// ─── Interlocutor ────────────────────────────────────────────────────────────

pub trait FaultLineInterlocutor {
    fn present(&mut self, fault: &FaultLine, index: usize, total: usize) -> FaultLineOutcome;
}

/// Stdin-backed presenter. Reads one line per prompt via
/// [`stdin_read_line`] — see StdinFoundInterlocutor's note about
/// why we don't hold a persistent BufReader.
pub struct StdinFaultLineInterlocutor;

impl StdinFaultLineInterlocutor {
    pub fn new() -> Self {
        Self
    }
}

impl Default for StdinFaultLineInterlocutor {
    fn default() -> Self {
        Self::new()
    }
}

fn prompt_field(label: &str) -> String {
    let mut stderr = io::stderr();
    let _ = write!(stderr, "    {label}: ");
    let _ = stderr.flush();
    stdin_read_line()
}

impl FaultLineInterlocutor for StdinFaultLineInterlocutor {
    fn present(
        &mut self,
        fault: &FaultLine,
        index: usize,
        total: usize,
    ) -> FaultLineOutcome {
        let mut stderr = io::stderr();
        let _ = writeln!(stderr);
        let _ = writeln!(
            stderr,
            "Fault line {}/{}: {}",
            index + 1,
            total,
            fault.title
        );
        let _ = writeln!(stderr, "  {}", fault.summary);
        for side in &fault.sides {
            let _ = writeln!(stderr);
            let _ = writeln!(stderr, "  • {}", side.label);
            let _ = writeln!(stderr, "      {}", side.tradeoffs);
        }
        let _ = writeln!(stderr);
        let _ = write!(stderr, "  [R]esolve now, leave [O]pen, or [S]kip? ");
        let _ = stderr.flush();
        let answer = stdin_read_line().to_lowercase();
        match answer.chars().next() {
            Some('r') => {
                let choice = prompt_field("Your choice");
                let reasoning = prompt_field("Why");
                FaultLineOutcome::Resolved { choice, reasoning }
            }
            Some('o') => {
                let note = prompt_field("Optional note (enter to skip)");
                FaultLineOutcome::Open { note }
            }
            // Default + unrecognized + explicit 's' all skip.
            _ => FaultLineOutcome::Skipped,
        }
    }
}

// ─── Persistence ─────────────────────────────────────────────────────────────

pub trait FaultLineRecorder {
    fn record(&mut self, fault: &FaultLine, outcome: &FaultLineOutcome);
}

pub fn render_fault_line_decision_body(fault: &FaultLine, choice: &str, reasoning: &str) -> String {
    format!(
        "Stage 2 · {}\n\n{}\n{}\n\nChoice: {}\nReasoning: {}\n",
        fault.id,
        fault.title,
        fault.summary,
        choice,
        if reasoning.is_empty() {
            "_(none given)_"
        } else {
            reasoning
        },
    )
}

pub fn render_fault_line_open_body(fault: &FaultLine, note: &str) -> String {
    let sides: String = fault
        .sides
        .iter()
        .map(|s| format!("  • {}: {}", s.label, s.tradeoffs))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "Stage 2 · {} (left open)\n\n{}\n{}\n\nSides:\n{}\n\nNote: {}\n",
        fault.id,
        fault.title,
        fault.summary,
        sides,
        if note.is_empty() {
            "_(left open without additional note)_"
        } else {
            note
        },
    )
}

/// Production recorder that writes Stage-2 outcomes into the
/// NoteStore. Resolved → decision; Open → uncertainty; Skipped →
/// (no note, deliberately).
pub struct NoteStoreFaultLineWriter<'a> {
    pub store: &'a corpus_engine::NoteStore,
    pub session_id: &'a str,
    pub written: Vec<(String, String)>, // (kind, note_id)
    /// Retained (fault, outcome) pairs in presentation order. Stage
    /// 3/4 composition needs these to render the charter; pulling
    /// them from here avoids a SQLite round-trip to re-derive
    /// in-memory data we already had.
    pub outcomes: Vec<(FaultLine, FaultLineOutcome)>,
    pub rt: tokio::runtime::Handle,
}

impl<'a> FaultLineRecorder for NoteStoreFaultLineWriter<'a> {
    fn record(&mut self, fault: &FaultLine, outcome: &FaultLineOutcome) {
        self.outcomes.push((fault.clone(), outcome.clone()));
        let (kind, body) = match outcome {
            FaultLineOutcome::Resolved { choice, reasoning } => (
                "decision",
                render_fault_line_decision_body(fault, choice, reasoning),
            ),
            FaultLineOutcome::Open { note } => {
                ("uncertainty", render_fault_line_open_body(fault, note))
            }
            // Skipped writes nothing — the user explicitly moved on.
            FaultLineOutcome::Skipped => return,
        };
        let session_id = self.session_id.to_string();
        let kind_owned = kind.to_string();
        let body_owned = body.clone();
        let store = self.store;
        let id_res = tokio::task::block_in_place(|| {
            self.rt.block_on(async move {
                store
                    .write_note_scoped(
                        &kind_owned,
                        &body_owned,
                        Vec::new(),
                        Vec::new(),
                        &session_id,
                        corpus_engine::NoteScope::Global,
                        None,
                    )
                    .await
            })
        });
        match id_res {
            Ok(id) => self.written.push((kind.to_string(), id)),
            Err(e) => eprintln!("    \u{2717} Stage 2 note write failed: {e}"),
        }
    }
}

// ─── Runner ──────────────────────────────────────────────────────────────────

/// Aggregate outcome tallies — what the caller renders as the
/// stage-2 summary line.
#[derive(Debug, Default)]
pub struct Stage2Summary {
    pub resolved: usize,
    pub open: usize,
    pub skipped: usize,
}

pub fn run_stage2<I: FaultLineInterlocutor, R: FaultLineRecorder>(
    obs: &ProjectObservation,
    stage1_answers: &[Stage1Answer],
    interlocutor: &mut I,
    recorder: &mut R,
) -> Stage2Summary {
    let faults = select_fault_lines(obs, stage1_answers);
    let total = faults.len();
    let mut summary = Stage2Summary::default();
    for (i, fault) in faults.iter().enumerate() {
        let outcome = interlocutor.present(fault, i, total);
        recorder.record(fault, &outcome);
        match outcome {
            FaultLineOutcome::Resolved { .. } => summary.resolved += 1,
            FaultLineOutcome::Open { .. } => summary.open += 1,
            FaultLineOutcome::Skipped => summary.skipped += 1,
        }
    }
    summary
}

// ═════════════════════════════════════════════════════════════════════════════
// Stage 2.5 — Documentation URLs (M6.6)
// ═════════════════════════════════════════════════════════════════════════════
//
// One question, asked once during founding, with the detected
// direct dependencies listed so the user has a concrete prompt to
// answer against. URLs they paste get fetched + indexed into
// `ProjectDocsStore` before the session ends; a blank answer is a
// valid answer — we record it and fall through to the runtime
// ask-before-fetch path for gaps that emerge later.
//
// The fetch + index work itself lives in `crate::doc_fetcher`.
// This module owns only the question and the pretty-print of the
// dep list.

/// Render the prompt body — the list of direct deps the user is
/// working with. Called by the CLI before reading a URL paste.
pub fn render_docs_prompt(obs: &ProjectObservation) -> String {
    let mut out = String::new();
    let direct: Vec<&DetectedDependency> = obs
        .deps
        .iter()
        .filter(|d| d.kind == DepKind::Direct)
        .collect();
    if direct.is_empty() {
        out.push_str(
            "I'll be working with whatever external services/libraries you name later — \
             no direct deps have been observed yet.\n\n",
        );
    } else {
        out.push_str("I'll be working with these external services and libraries:\n\n");
        // Cap at 15 entries to keep the prompt readable; deduplicate
        // by name so the user doesn't see reqwest 3x.
        let mut seen = std::collections::BTreeSet::new();
        let mut printed = 0usize;
        for d in &direct {
            if !seen.insert(&d.name) {
                continue;
            }
            out.push_str(&format!("  - {}\n", d.name));
            printed += 1;
            if printed >= 15 {
                let remaining = direct.len().saturating_sub(printed);
                if remaining > 0 {
                    out.push_str(&format!("  - …and {remaining} more\n"));
                }
                break;
            }
        }
        out.push('\n');
    }
    out.push_str(
        "Where should I look for documentation when I'm stuck?\n\
         Drop any URLs — official docs, internal wikis, anything you'd open yourself.\n",
    );
    out
}

/// Interlocutor for the docs-URL question. Kept separate from
/// `ApprovalInterlocutor` so test harnesses can script them
/// independently, and so a future non-stdin host (IDE plugin, TUI)
/// can substitute its own input source without touching the
/// approval flow.
pub trait DocsInterlocutor {
    /// Show the prompt and collect a single paste (may span lines).
    /// Returns the raw string exactly as entered; URL parsing is
    /// the caller's responsibility (see
    /// `doc_fetcher::parse_urls`).
    fn ask_docs_urls(&mut self, prompt: &str) -> String;
}

pub struct StdinDocsInterlocutor;

impl StdinDocsInterlocutor {
    pub fn new() -> Self {
        Self
    }
}

impl Default for StdinDocsInterlocutor {
    fn default() -> Self {
        Self::new()
    }
}

impl DocsInterlocutor for StdinDocsInterlocutor {
    fn ask_docs_urls(&mut self, prompt: &str) -> String {
        let mut stderr = io::stderr();
        let _ = writeln!(stderr);
        let _ = writeln!(stderr, "  Documentation sources");
        let _ = writeln!(stderr, "  {}", "─".repeat(54));
        let _ = writeln!(stderr);
        // Render the prompt indented.
        for line in prompt.lines() {
            let _ = writeln!(stderr, "  {line}");
        }
        let _ = writeln!(stderr);
        let _ = writeln!(
            stderr,
            "  Paste one or more URLs (space-separated or on one line).",
        );
        let _ = write!(stderr, "  (Enter to skip; the runtime fallback covers gaps later.) > ");
        let _ = stderr.flush();
        stdin_read_line()
    }
}

/// Render the body of the decision-kind note that records what the
/// user answered to the docs-URL question. Pure so tests can
/// assert the exact form.
pub fn render_docs_decision_body(prompt: &str, urls: &[String]) -> String {
    let mut out = String::from("Stage 2.5 · found.docs-urls\n\nPrompt:\n");
    for line in prompt.lines() {
        out.push_str("  ");
        out.push_str(line);
        out.push('\n');
    }
    out.push_str("\nAnswer:\n");
    if urls.is_empty() {
        out.push_str(
            "  _(No URLs provided. Documentation gaps will surface at runtime via \
             the honest-uncertainty prompt; the operator decides then.)_\n",
        );
    } else {
        for u in urls {
            out.push_str(&format!("  - {u}\n"));
        }
    }
    out
}

// ═════════════════════════════════════════════════════════════════════════════
// Stages 3 + 4 — Charter + Phases (M6.5)
// ═════════════════════════════════════════════════════════════════════════════
//
// Composition turns the prior stages' output into two markdown
// documents. Nothing is written until the user approves both;
// approval flips `lifecycle.founded = true` + records the charter
// hash in project.toml.
//
// The composition functions are pure — no I/O. Tests assert the
// exact structure we produce. Approval is a separate seam
// (trait-based) so interactive and automated paths share the same
// rendering.

// ─── Composition inputs ──────────────────────────────────────────────────────

/// Aggregates what Stages 1 + 2 produced + the elicited
/// Phase-1 stop condition. Built by the caller; passed to the
/// compose functions.
#[derive(Debug, Clone)]
pub struct FoundingInputs<'a> {
    pub project_id: &'a str,
    pub founded_date: &'a str, // ISO 8601 calendar date (YYYY-MM-DD).
    pub observation: &'a ProjectObservation,
    pub design: Option<&'a str>,
    pub stage1_answers: &'a [Stage1Answer],
    pub stage2_outcomes: &'a [(FaultLine, FaultLineOutcome)],
    /// Concrete, verifiable, system-specific — elicited from the
    /// user in Stage 4. MUST be non-empty to compose PHASES.md.
    pub phase1_stop_condition: &'a str,
}

// ─── Stage 3: Charter ────────────────────────────────────────────────────────

/// Compose CHARTER.md from the gathered inputs. Deterministic —
/// same inputs produce byte-identical output, which is what lets
/// [`hash_charter`] give us a stable drift sensor.
pub fn compose_charter(inputs: &FoundingInputs) -> String {
    let mut out = String::new();
    out.push_str(&format!("# {} — Charter\n\n", inputs.project_id));
    out.push_str(&format!(
        "_Founded: {}, charter version 1._\n\n",
        inputs.founded_date
    ));

    // 1. System design.
    out.push_str("## System design\n\n");
    match inputs.design {
        Some(doc) if !doc.trim().is_empty() => {
            // Verbatim per the requirements: "verbatim from the
            // design document, or structured from the elicitation
            // dialogue." Trim trailing whitespace to keep the
            // rendered doc tidy.
            out.push_str(doc.trim_end());
            out.push_str("\n\n");
        }
        _ => {
            out.push_str(&compose_design_from_stage1(inputs.stage1_answers));
        }
    }

    // 2. Invariants. Derived strictly — we only name invariants
    // that follow from the user's own answers, not ones we
    // speculate about. When there's nothing to say, say so;
    // fabricated invariants are worse than an honest TBD.
    out.push_str("## Invariants\n\n");
    let invariants = compose_invariants(inputs);
    if invariants.is_empty() {
        out.push_str(
            "_(No invariants named at founding. Add them via \
             `sovereign project amend` as they crystallize.)_\n\n",
        );
    } else {
        for inv in &invariants {
            out.push_str(&format!("- {inv}\n"));
        }
        out.push('\n');
    }

    // 3. Resolved decisions — Stage 1 answers + Stage 2 resolved.
    out.push_str("## Resolved decisions\n\n");
    let mut any_decision = false;
    for a in inputs.stage1_answers.iter().filter(|a| !a.skipped) {
        out.push_str(&format!("- **{}** — {}\n", a.question_id, a.text));
        any_decision = true;
    }
    for (fault, outcome) in inputs.stage2_outcomes.iter() {
        if let FaultLineOutcome::Resolved { choice, reasoning } = outcome {
            out.push_str(&format!(
                "- **{}** — {}\n    _Reasoning:_ {}\n",
                fault.id,
                choice,
                if reasoning.is_empty() {
                    "_(none given)_"
                } else {
                    reasoning.as_str()
                },
            ));
            any_decision = true;
        }
    }
    if !any_decision {
        out.push_str(
            "_(No decisions committed at founding. That's fine — \
             this section grows as questions get answered.)_\n",
        );
    }
    out.push('\n');

    // 4. Open questions — Stage 1 skipped + Stage 2 open.
    out.push_str("## Open questions\n\n");
    let mut any_open = false;
    for a in inputs.stage1_answers.iter().filter(|a| a.skipped) {
        out.push_str(&format!(
            "- **{}** — deferred at founding.\n",
            a.question_id
        ));
        any_open = true;
    }
    for (fault, outcome) in inputs.stage2_outcomes.iter() {
        if let FaultLineOutcome::Open { note } = outcome {
            out.push_str(&format!(
                "- **{}** — {}. _{}_\n",
                fault.id,
                fault.title,
                if note.is_empty() {
                    "no additional context provided"
                } else {
                    note.as_str()
                },
            ));
            any_open = true;
        }
    }
    if !any_open {
        out.push_str(
            "_(No open questions. Everything at founding was answered or skipped.)_\n",
        );
    }
    out.push('\n');

    // 5. Amendment log — explicitly empty. Amendments write here
    // via `sovereign project amend` (M6.7).
    out.push_str("## Amendment log\n\n");
    out.push_str(
        "_(Empty at founding. Amendments land here via \
         `sovereign project amend`, each carrying the adversarial \
         review + the reasoning that overrode it.)_\n",
    );

    out
}

/// When no design document was supplied, structure a minimal
/// system-design section from the Stage-1 purpose answer plus
/// the observed dep list. Keeps the section honest: we say "here's
/// what you said, here's what we saw," no fabrication.
fn compose_design_from_stage1(answers: &[Stage1Answer]) -> String {
    let mut out = String::new();
    let purpose = answers
        .iter()
        .find(|a| a.question_id == "found.stage1.project-purpose" && !a.skipped)
        .map(|a| a.text.as_str());
    match purpose {
        Some(p) => {
            out.push_str(p);
            out.push_str("\n\n");
        }
        None => {
            out.push_str(
                "_(No design document provided and the founding purpose \
                 question wasn't answered. Fill this section in before \
                 the first amendment.)_\n\n",
            );
        }
    }
    out
}

/// Derive invariants from the inputs. Rules:
/// - Stage 1 persistence answer → an invariant that the answer
///   describes a contract.
/// - Stage 1 external-interface answer → an invariant about the
///   assumptions it names.
/// - Stage 2 resolved persistence-shape / schema-evolution faults
///   → invariants about the chosen side.
///
/// Returns an empty vec when nothing qualifies — charter writers
/// should never invent an invariant.
fn compose_invariants(inputs: &FoundingInputs) -> Vec<String> {
    let mut out = Vec::new();
    for a in inputs.stage1_answers.iter().filter(|a| !a.skipped) {
        match a.question_id.as_str() {
            "found.stage1.persistence-contract" => {
                out.push(format!(
                    "The persistence contract described by the founder \
                     must not change without an amendment: {}",
                    a.text
                ));
            }
            "found.stage1.external-interface" => {
                out.push(format!(
                    "External interface assumptions (captured at \
                     founding): {}",
                    a.text
                ));
            }
            _ => {}
        }
    }
    for (fault, outcome) in inputs.stage2_outcomes.iter() {
        if let FaultLineOutcome::Resolved { choice, .. } = outcome {
            match fault.id.as_str() {
                "fault.persistence-shape"
                | "fault.schema-evolution"
                | "fault.delivery-semantics" => {
                    out.push(format!(
                        "{} is chosen: {}. Changing this is an amendment, not a refactor.",
                        fault.title, choice
                    ));
                }
                _ => {}
            }
        }
    }
    out
}

/// SHA-256 of the charter content. Used to record founding state
/// and detect drift.
pub fn hash_charter(content: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(content.as_bytes());
    format!("{:x}", h.finalize())
}

// ─── Stage 4: Phases ─────────────────────────────────────────────────────────

/// Compose PHASES.md. Phase 0-2 concrete; Phase 3+ deferred with
/// rationale inline per requirements.
pub fn compose_phases(inputs: &FoundingInputs) -> String {
    let mut out = String::new();
    out.push_str(&format!("# {} — Phases\n\n", inputs.project_id));
    out.push_str(&format!("_Founded: {}._\n\n", inputs.founded_date));

    // Phase 0 — Skeleton.
    out.push_str("## Phase 0: Skeleton\n\n");
    out.push_str(
        "Establish project structure. No feature code — scaffolding only:\n\
         \n\
         - Package layout compiles\n\
         - Trivial tests pass (placeholder is fine)\n\
         - Linter is clean\n\
         - Dependencies resolve\n\n",
    );
    out.push_str("**Stop condition:** `");
    out.push_str(phase0_stop_for_languages(inputs.observation));
    out.push_str("`\n\n");

    // Phase 1 — Foundation. User-supplied stop condition.
    out.push_str("## Phase 1: Foundation\n\n");
    out.push_str(
        "One real path, end-to-end, through the entire system. When \
         Phase 1 passes, a user can do ONE meaningful thing and see the \
         whole stack respond.\n\n",
    );
    out.push_str(&format!(
        "**Stop condition:** {}\n\n",
        inputs.phase1_stop_condition.trim()
    ));

    // Phase 2 — Hardening.
    out.push_str("## Phase 2: Hardening\n\n");
    out.push_str(
        "Tests, error handling, resilience. Phase 1's happy path is \
         already working; Phase 2 makes it survive reality.\n\
         \n\
         - Error cases for every Phase 1 path\n\
         - Invariant tests lock the charter's invariants\n\
         - Graceful degradation when external deps fail\n\n",
    );
    let degraded = phase2_degraded_condition(inputs.observation);
    out.push_str(&format!(
        "**Stop condition:** Phase 1's stop condition holds {degraded}.\n\n"
    ));

    // Phase 3+ — deferred.
    out.push_str("## Phase 3+: Feature layers\n\n");
    out.push_str(
        "Stop conditions for Phase 3+ are **intentionally deferred** \
         until Phase 2 completes.\n\n\
         **Why:** stop conditions written before Phase 2 produces real \
         observability are written without the information we'll have. \
         When Phase 2 lands, revisit this document and fill Phase 3+ \
         with conditions informed by the actual system's telemetry, \
         failure modes, and user feedback.\n\n\
         Add phases here via `sovereign project amend` as you go.\n",
    );

    out
}

fn phase0_stop_for_languages(obs: &ProjectObservation) -> &'static str {
    // Pick one based on the first/primary language. Polyglot repos
    // can amend the document — we don't try to compose a combined
    // invocation (which would be wrong more often than right).
    for lang in &obs.languages {
        match lang.id.as_str() {
            "rust" => return "cargo build && cargo test",
            "go" => return "go build ./... && go test ./...",
            "typescript" | "javascript" => return "npm run build && npm test",
            "python" => return "python -m pytest",
            "java" => return "mvn test",
            _ => {}
        }
    }
    "# TODO: define a build-and-test one-liner for your stack"
}

fn phase2_degraded_condition(obs: &ProjectObservation) -> String {
    // Pick a concrete degraded condition based on detected
    // external clients. This is a strong default the user can
    // refine; the point is that Phase 2's stop is never generic
    // "things handle errors."
    let external_client_hints = [
        "reqwest",
        "hyper",
        "requests",
        "httpx",
        "axios",
        "polygon",
        "alpaca",
        "stripe",
    ];
    let sample = obs.deps.iter().find(|d| {
        let n = d.name.to_lowercase();
        external_client_hints.iter().any(|h| n.contains(h))
    });
    if let Some(d) = sample {
        format!(
            "under `{}` returning 503 for 30 seconds (and recovers within a minute after it resumes)",
            d.name
        )
    } else {
        "under a random process restart mid-operation".into()
    }
}

// ─── Approval loop ───────────────────────────────────────────────────────────

/// What the user says when presented with the composed drafts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalAnswer {
    Approve,
    EditInEditor,
    Cancel,
}

/// Seam for the approval prompt. Production uses stdin + $EDITOR.
/// Tests script answers directly.
pub trait ApprovalInterlocutor {
    fn ask_phase1_stop(&mut self) -> String;
    fn preview(&mut self, charter: &str, phases: &str);
    fn ask_approval(&mut self) -> ApprovalAnswer;
    /// Open the user's `$EDITOR` on the two pending files. Returns
    /// `(charter_after_edit, phases_after_edit)`. On any failure
    /// (editor missing, non-zero exit, read failure) returns the
    /// inputs unchanged — we trust the user to cancel if that
    /// happened.
    fn edit_in_editor(
        &mut self,
        charter_path: &Path,
        phases_path: &Path,
    ) -> (String, String);
}

pub struct StdinApprovalInterlocutor;

impl StdinApprovalInterlocutor {
    pub fn new() -> Self {
        Self
    }
}

impl Default for StdinApprovalInterlocutor {
    fn default() -> Self {
        Self::new()
    }
}

impl ApprovalInterlocutor for StdinApprovalInterlocutor {
    fn ask_phase1_stop(&mut self) -> String {
        let mut stderr = io::stderr();
        let _ = writeln!(stderr);
        let _ = writeln!(stderr, "  Phase 1 stop condition");
        let _ = writeln!(
            stderr,
            "    Name the concrete, verifiable observation that proves the end-to-end path works."
        );
        let _ = writeln!(
            stderr,
            "    (e.g. \"POST /ingest echoes back the payload id within 200ms; GET /items returns it.\")"
        );
        let _ = write!(stderr, "  > ");
        let _ = stderr.flush();
        stdin_read_line()
    }

    fn preview(&mut self, charter: &str, phases: &str) {
        println!();
        println!("  ══════════════════════════════════════════════════════");
        println!("  CHARTER.md (draft)");
        println!("  ══════════════════════════════════════════════════════");
        println!();
        println!("{charter}");
        println!();
        println!("  ══════════════════════════════════════════════════════");
        println!("  PHASES.md (draft)");
        println!("  ══════════════════════════════════════════════════════");
        println!();
        println!("{phases}");
    }

    fn ask_approval(&mut self) -> ApprovalAnswer {
        let mut stderr = io::stderr();
        let _ = writeln!(stderr);
        let _ = write!(
            stderr,
            "  [A]pprove both, [E]dit in $EDITOR, or [C]ancel? "
        );
        let _ = stderr.flush();
        let line = stdin_read_line().to_lowercase();
        match line.chars().next() {
            Some('a') => ApprovalAnswer::Approve,
            Some('e') => ApprovalAnswer::EditInEditor,
            _ => ApprovalAnswer::Cancel,
        }
    }

    fn edit_in_editor(
        &mut self,
        charter_path: &Path,
        phases_path: &Path,
    ) -> (String, String) {
        let editor = std::env::var("EDITOR").unwrap_or_default();
        if editor.is_empty() {
            eprintln!(
                "  $EDITOR is unset. Drafts saved at:\n    {}\n    {}\n  Edit them, then re-run `sovereign project found` to continue.",
                charter_path.display(),
                phases_path.display()
            );
            // Return unchanged content so the caller's loop can
            // re-prompt; typically the user will cancel here.
            let c = std::fs::read_to_string(charter_path).unwrap_or_default();
            let p = std::fs::read_to_string(phases_path).unwrap_or_default();
            return (c, p);
        }
        // Launch $EDITOR with both files as arguments. Most
        // editors open multiple files cleanly; vi/nvim/emacs do.
        // VSCode (`code --wait`) also works when `--wait` is in
        // $EDITOR.
        let status = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!(
                "{editor} {} {}",
                shell_escape(charter_path),
                shell_escape(phases_path)
            ))
            .status();
        if let Err(e) = status {
            eprintln!("  Could not launch $EDITOR: {e}");
        } else if let Ok(s) = status {
            if !s.success() {
                eprintln!(
                    "  $EDITOR exited non-zero ({}). Changes (if any) still loaded.",
                    s.code().unwrap_or(-1)
                );
            }
        }
        let c = std::fs::read_to_string(charter_path).unwrap_or_default();
        let p = std::fs::read_to_string(phases_path).unwrap_or_default();
        (c, p)
    }
}

/// POSIX shell-escape a path for `sh -c`. Single-quotes the
/// string and replaces internal single quotes with `'\''`.
fn shell_escape(path: &Path) -> String {
    let s = path.to_string_lossy();
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Outcome of the combined Stage 3+4 approval flow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FoundingApproval {
    /// User accepted. Caller writes the two files and updates
    /// project.toml.
    Approved { charter: String, phases: String },
    /// User aborted. Caller exits without writing.
    Cancelled,
}

/// Drive the preview → edit? → approve loop. The `pending_dir`
/// is where drafts live during the interactive session; it's the
/// caller's responsibility to create/clean it.
pub fn run_stage34<I: ApprovalInterlocutor>(
    mut charter: String,
    mut phases: String,
    pending_dir: &Path,
    interlocutor: &mut I,
) -> FoundingApproval {
    let charter_path = pending_dir.join("CHARTER.md");
    let phases_path = pending_dir.join("PHASES.md");

    // Seed the pending files so the editor path has something to
    // open from turn 1.
    let _ = std::fs::write(&charter_path, &charter);
    let _ = std::fs::write(&phases_path, &phases);

    loop {
        interlocutor.preview(&charter, &phases);
        match interlocutor.ask_approval() {
            ApprovalAnswer::Approve => {
                return FoundingApproval::Approved { charter, phases };
            }
            ApprovalAnswer::Cancel => return FoundingApproval::Cancelled,
            ApprovalAnswer::EditInEditor => {
                // Persist current drafts before handing off to the
                // editor (in case the caller gave us mutated copies).
                let _ = std::fs::write(&charter_path, &charter);
                let _ = std::fs::write(&phases_path, &phases);
                let (c, p) = interlocutor.edit_in_editor(&charter_path, &phases_path);
                charter = c;
                phases = p;
            }
        }
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observation::{
        DepKind, DetectedDependency, LanguageObservation, ScipTooling,
    };
    use std::cell::RefCell;

    // ── Fixture helpers ─────────────────────────────────────────

    fn minimal_obs() -> ProjectObservation {
        ProjectObservation {
            repo_root: PathBuf::from("/tmp"),
            has_git: true,
            languages: vec![LanguageObservation {
                id: "rust".into(),
                display: "Rust".into(),
                scip_tooling: ScipTooling::NotRequired,
            }],
            deps: Vec::new(),
            embed_model_available: true,
        }
    }

    fn obs_with_deps() -> ProjectObservation {
        let mut o = minimal_obs();
        o.deps.push(DetectedDependency {
            name: "polygon-api-client".into(),
            version: Some("1.12".into()),
            source_file: "pyproject.toml".into(),
            kind: DepKind::Direct,
        });
        o
    }

    fn obs_polyglot() -> ProjectObservation {
        let mut o = minimal_obs();
        o.languages.push(LanguageObservation {
            id: "go".into(),
            display: "Go".into(),
            scip_tooling: ScipTooling::NotRequired,
        });
        o
    }

    struct ScriptedInterlocutor {
        answers: Vec<Stage1Answer>,
        asked: RefCell<Vec<String>>,
    }
    impl ScriptedInterlocutor {
        fn new(answers: Vec<Stage1Answer>) -> Self {
            Self {
                answers,
                asked: RefCell::new(Vec::new()),
            }
        }
        fn asked_ids(&self) -> Vec<String> {
            self.asked.borrow().clone()
        }
    }
    impl FoundInterlocutor for ScriptedInterlocutor {
        fn ask_stage1(&mut self, q: &Stage1Question) -> Stage1Answer {
            self.asked.borrow_mut().push(q.id.clone());
            let mut a = self.answers.remove(0);
            // Pin the id so the test harness doesn't have to care.
            a.question_id = q.id.clone();
            a
        }
    }

    #[derive(Default)]
    struct RecordingWriter {
        records: Vec<(Stage1Question, Stage1Answer)>,
    }
    impl DecisionRecorder for RecordingWriter {
        fn record(&mut self, question: &Stage1Question, answer: &Stage1Answer) {
            self.records.push((question.clone(), answer.clone()));
        }
    }

    fn free_text(text: &str) -> Stage1Answer {
        Stage1Answer {
            question_id: String::new(),
            text: text.into(),
            skipped: false,
        }
    }
    fn skipped() -> Stage1Answer {
        Stage1Answer {
            question_id: String::new(),
            text: String::new(),
            skipped: true,
        }
    }

    // ── Selection invariants ────────────────────────────────────

    #[test]
    fn selection_caps_at_five_even_if_catalog_grows() {
        // Sanity: today's catalog never exceeds 5 even in the
        // widest-applicable shape. Locks the contract.
        let obs = obs_with_deps();
        let qs = select_questions(&obs, /* has_design */ false);
        assert!(qs.len() <= MAX_QUESTIONS);
    }

    #[test]
    fn design_doc_skips_project_purpose_question() {
        let obs = minimal_obs();
        let without = select_questions(&obs, false);
        let with = select_questions(&obs, true);
        assert!(without
            .iter()
            .any(|q| q.id == "found.stage1.project-purpose"));
        assert!(
            with.iter()
                .all(|q| q.id != "found.stage1.project-purpose"),
            "with a design doc, purpose comes from the doc, not a question"
        );
    }

    #[test]
    fn interface_question_fires_only_when_direct_deps_exist() {
        let without_deps = select_questions(&minimal_obs(), false);
        assert!(
            without_deps
                .iter()
                .all(|q| q.id != "found.stage1.external-interface"),
            "no deps → no interface question"
        );
        let with_deps = select_questions(&obs_with_deps(), false);
        assert!(with_deps
            .iter()
            .any(|q| q.id == "found.stage1.external-interface"));
    }

    #[test]
    fn convention_question_fires_on_scope_signal() {
        // Trivial rust-only no-dep project: convention question is
        // noise, skip.
        let trivial = select_questions(&minimal_obs(), true);
        assert!(trivial
            .iter()
            .all(|q| q.id != "found.stage1.convention-risk"));

        // Polyglot project: convention question fires.
        let poly = select_questions(&obs_polyglot(), true);
        assert!(poly
            .iter()
            .any(|q| q.id == "found.stage1.convention-risk"));
    }

    #[test]
    fn every_catalog_entry_has_a_non_empty_why() {
        // Lock the requirement: "Each question states why it is
        // being asked." Regress if anyone adds an entry with an
        // empty `why` field.
        for entry in catalog() {
            assert!(
                !entry.why.trim().is_empty(),
                "catalog entry {} has empty `why`",
                entry.id
            );
            assert!(
                !entry.prompt.trim().is_empty(),
                "catalog entry {} has empty prompt",
                entry.id
            );
            assert!(
                entry.id.starts_with("found.stage1."),
                "catalog entry {} must use the found.stage1. namespace",
                entry.id
            );
        }
    }

    // ── Runner behavior ─────────────────────────────────────────

    #[test]
    fn runner_asks_every_selected_question_and_records_each() {
        let obs = obs_with_deps();
        let selected = select_questions(&obs, false);
        let mut interlocutor = ScriptedInterlocutor::new(
            (0..selected.len()).map(|i| free_text(&format!("answer{i}"))).collect(),
        );
        let mut recorder = RecordingWriter::default();
        let answers = run_stage1(&obs, None, &mut interlocutor, &mut recorder);
        assert_eq!(answers.len(), selected.len());
        assert_eq!(
            interlocutor.asked_ids(),
            selected.iter().map(|q| q.id.clone()).collect::<Vec<_>>(),
        );
        assert_eq!(recorder.records.len(), selected.len());
    }

    #[test]
    fn skipped_answer_still_recorded_with_skipped_marker() {
        let obs = minimal_obs();
        let selected = select_questions(&obs, true); // design-doc mode → short list
        assert!(!selected.is_empty(), "selection must produce at least one question");
        let n = selected.len();
        // Script ALL skipped.
        let mut interlocutor =
            ScriptedInterlocutor::new((0..n).map(|_| skipped()).collect());
        let mut recorder = RecordingWriter::default();
        let answers = run_stage1(&obs, Some("design"), &mut interlocutor, &mut recorder);
        assert_eq!(answers.len(), n);
        assert!(answers.iter().all(|a| a.skipped));
        assert_eq!(recorder.records.len(), n);
        // Rendered body flags the skip.
        let (q0, a0) = &recorder.records[0];
        let body = render_decision_body(q0, a0);
        assert!(
            body.contains("_(skipped by user)_"),
            "skip marker must be in the body, got:\n{body}"
        );
    }

    #[test]
    fn rendered_body_includes_why_and_preserves_answer_verbatim() {
        let q = Stage1Question {
            id: "found.stage1.t".into(),
            prompt: "What invariant matters most?".into(),
            why: "Because invariants are expensive to change".into(),
        };
        let a = Stage1Answer {
            question_id: "found.stage1.t".into(),
            text: "No field renames without a 90-day deprecation window.".into(),
            skipped: false,
        };
        let body = render_decision_body(&q, &a);
        assert!(body.contains("Stage 1 · found.stage1.t"));
        assert!(body.contains("What invariant matters most?"));
        assert!(body.contains("Because invariants are expensive to change"));
        assert!(body.contains("No field renames without a 90-day deprecation window."));
    }

    #[test]
    fn design_preview_extracts_first_paragraph() {
        let doc = "# Title\n\nFirst paragraph spans\ntwo lines.\n\nSecond paragraph.\n";
        let preview = design_preview(doc);
        // First non-empty paragraph is the H1 line itself — we
        // concatenate newlines to spaces for a one-line preview.
        assert_eq!(preview, "# Title");
    }

    #[test]
    fn design_preview_skips_leading_blank_paragraphs() {
        let doc = "\n\n\nActual first paragraph here.\n";
        let preview = design_preview(doc);
        assert_eq!(preview, "Actual first paragraph here.");
    }

    // ── Stage 2 tests ──────────────────────────────────────────

    fn obs_with_sql_dep() -> ProjectObservation {
        let mut o = minimal_obs();
        o.deps.push(DetectedDependency {
            name: "rusqlite".into(),
            version: Some("0.30".into()),
            source_file: "Cargo.toml".into(),
            kind: DepKind::Direct,
        });
        o
    }

    fn obs_with_queue_dep() -> ProjectObservation {
        let mut o = minimal_obs();
        o.deps.push(DetectedDependency {
            name: "rdkafka".into(),
            version: Some("0.35".into()),
            source_file: "Cargo.toml".into(),
            kind: DepKind::Direct,
        });
        o
    }

    fn stage1_with_answer(text: &str) -> Vec<Stage1Answer> {
        vec![Stage1Answer {
            question_id: "found.stage1.persistence-contract".into(),
            text: text.into(),
            skipped: false,
        }]
    }

    struct ScriptedFaultLineInterlocutor {
        outcomes: Vec<FaultLineOutcome>,
        presented: RefCell<Vec<String>>,
    }
    impl ScriptedFaultLineInterlocutor {
        fn new(outcomes: Vec<FaultLineOutcome>) -> Self {
            Self {
                outcomes,
                presented: RefCell::new(Vec::new()),
            }
        }
        fn presented_ids(&self) -> Vec<String> {
            self.presented.borrow().clone()
        }
    }
    impl FaultLineInterlocutor for ScriptedFaultLineInterlocutor {
        fn present(
            &mut self,
            fault: &FaultLine,
            _index: usize,
            _total: usize,
        ) -> FaultLineOutcome {
            self.presented.borrow_mut().push(fault.id.clone());
            self.outcomes.remove(0)
        }
    }

    #[derive(Default)]
    struct RecordingFaultLineWriter {
        records: Vec<(FaultLine, FaultLineOutcome)>,
    }
    impl FaultLineRecorder for RecordingFaultLineWriter {
        fn record(&mut self, fault: &FaultLine, outcome: &FaultLineOutcome) {
            self.records.push((fault.clone(), outcome.clone()));
        }
    }

    #[test]
    fn every_fault_line_entry_has_at_least_two_sides_and_non_empty_fields() {
        for entry in fault_line_catalog() {
            assert!(entry.id.starts_with("fault."), "id must be in the fault.* namespace: {}", entry.id);
            assert!(!entry.title.trim().is_empty(), "title empty: {}", entry.id);
            assert!(!entry.summary.trim().is_empty(), "summary empty: {}", entry.id);
            assert!(
                entry.sides.len() >= 2,
                "fault line {} needs at least 2 sides (otherwise it isn't a disagreement)",
                entry.id
            );
            for (label, tradeoffs) in entry.sides {
                assert!(!label.trim().is_empty(), "side label empty in {}", entry.id);
                assert!(
                    !tradeoffs.trim().is_empty(),
                    "side tradeoffs empty in {} / {}",
                    entry.id,
                    label
                );
            }
        }
    }

    #[test]
    fn time_representation_fault_fires_on_every_project() {
        let obs = minimal_obs();
        let faults = select_fault_lines(&obs, &[]);
        assert!(
            faults.iter().any(|f| f.id == "fault.time-representation"),
            "time fault must fire always"
        );
    }

    #[test]
    fn persistence_fault_requires_db_dep_or_keyword() {
        let trivial = select_fault_lines(&minimal_obs(), &[]);
        assert!(
            trivial.iter().all(|f| f.id != "fault.persistence-shape"),
            "no persistence signal → no persistence fault"
        );

        let with_sqlite = select_fault_lines(&obs_with_sql_dep(), &[]);
        assert!(with_sqlite
            .iter()
            .any(|f| f.id == "fault.persistence-shape"));

        // Keyword path: no dep, but Stage 1 answer talks about schema.
        let keyword_only = select_fault_lines(
            &minimal_obs(),
            &stage1_with_answer("we have a schema that survives across deploys"),
        );
        assert!(keyword_only
            .iter()
            .any(|f| f.id == "fault.persistence-shape"));
    }

    #[test]
    fn queue_fault_fires_on_kafka_dep() {
        let obs = obs_with_queue_dep();
        let faults = select_fault_lines(&obs, &[]);
        assert!(
            faults.iter().any(|f| f.id == "fault.delivery-semantics"),
            "kafka dep must surface delivery-semantics"
        );
    }

    #[test]
    fn skipped_stage1_answers_do_not_contribute_to_selection() {
        // A skipped answer whose (phantom) text mentions "schema"
        // must NOT trigger the persistence fault line. We only use
        // answered text.
        let skipped_answer = vec![Stage1Answer {
            question_id: "found.stage1.persistence-contract".into(),
            text: "this string mentions schema but was skipped".into(),
            skipped: true,
        }];
        let faults = select_fault_lines(&minimal_obs(), &skipped_answer);
        assert!(
            faults.iter().all(|f| f.id != "fault.persistence-shape"),
            "skipped answers must not contribute signals"
        );
    }

    fn obs_with_everything() -> ProjectObservation {
        let mut o = minimal_obs();
        for (name, src) in [
            ("rusqlite", "Cargo.toml"),
            ("axum", "Cargo.toml"),
            ("reqwest", "Cargo.toml"),
            ("rdkafka", "Cargo.toml"),
            ("tokio", "Cargo.toml"),
        ] {
            o.deps.push(DetectedDependency {
                name: name.into(),
                version: Some("1".into()),
                source_file: src.into(),
                kind: DepKind::Direct,
            });
        }
        o
    }

    #[test]
    fn runner_records_resolved_open_skipped_according_to_outcome() {
        let obs = obs_with_everything();
        let faults = select_fault_lines(&obs, &[]);
        assert!(faults.len() >= 3, "need enough faults to sample all outcomes, got {}", faults.len());
        let outcomes: Vec<FaultLineOutcome> = (0..faults.len())
            .map(|i| match i % 3 {
                0 => FaultLineOutcome::Resolved {
                    choice: format!("choice-{i}"),
                    reasoning: format!("reason-{i}"),
                },
                1 => FaultLineOutcome::Open {
                    note: format!("note-{i}"),
                },
                _ => FaultLineOutcome::Skipped,
            })
            .collect();
        let mut interloc = ScriptedFaultLineInterlocutor::new(outcomes.clone());
        let mut recorder = RecordingFaultLineWriter::default();
        let summary = run_stage2(&obs, &[], &mut interloc, &mut recorder);

        // Every fault presented, in catalog order.
        assert_eq!(
            interloc.presented_ids(),
            faults.iter().map(|f| f.id.clone()).collect::<Vec<_>>()
        );

        // Every fault recorded (Skipped included — the recorder
        // trait decides whether to persist; production impl NO-OPs
        // on Skipped but the recorder still sees it).
        assert_eq!(recorder.records.len(), faults.len());

        // Summary tallies match outcome distribution.
        let (r, o, s) =
            outcomes
                .iter()
                .fold((0usize, 0usize, 0usize), |(r, o, s), oc| match oc {
                    FaultLineOutcome::Resolved { .. } => (r + 1, o, s),
                    FaultLineOutcome::Open { .. } => (r, o + 1, s),
                    FaultLineOutcome::Skipped => (r, o, s + 1),
                });
        assert_eq!((summary.resolved, summary.open, summary.skipped), (r, o, s));
    }

    #[test]
    fn rendered_decision_body_includes_choice_and_reasoning() {
        let fault = FaultLine {
            id: "fault.id-scheme".into(),
            title: "Identifier scheme".into(),
            summary: "summary".into(),
            sides: vec![],
        };
        let body = render_fault_line_decision_body(&fault, "ULID", "sortable + global");
        assert!(body.contains("Stage 2 · fault.id-scheme"));
        assert!(body.contains("ULID"));
        assert!(body.contains("sortable + global"));
    }

    #[test]
    fn rendered_open_body_includes_all_sides_for_future_reference() {
        let fault = FaultLine {
            id: "fault.t".into(),
            title: "T".into(),
            summary: "summary".into(),
            sides: vec![
                FaultLineSide {
                    label: "A".into(),
                    tradeoffs: "alpha-tradeoffs".into(),
                },
                FaultLineSide {
                    label: "B".into(),
                    tradeoffs: "beta-tradeoffs".into(),
                },
            ],
        };
        let body = render_fault_line_open_body(&fault, "need more time");
        assert!(body.contains("(left open)"));
        assert!(body.contains("alpha-tradeoffs"));
        assert!(body.contains("beta-tradeoffs"));
        assert!(body.contains("need more time"));
    }

    #[test]
    fn empty_open_note_renders_sentinel_not_blank() {
        let fault = FaultLine {
            id: "fault.t".into(),
            title: "T".into(),
            summary: "s".into(),
            sides: vec![
                FaultLineSide {
                    label: "A".into(),
                    tradeoffs: "at".into(),
                },
                FaultLineSide {
                    label: "B".into(),
                    tradeoffs: "bt".into(),
                },
            ],
        };
        let body = render_fault_line_open_body(&fault, "");
        assert!(body.contains("_(left open without additional note)_"));
    }

    // ── Stage 3+4 tests ────────────────────────────────────────

    fn sample_stage1_answers() -> Vec<Stage1Answer> {
        vec![
            Stage1Answer {
                question_id: "found.stage1.project-purpose".into(),
                text: "Real-time options market-data ingest.".into(),
                skipped: false,
            },
            Stage1Answer {
                question_id: "found.stage1.persistence-contract".into(),
                text: "Append-only tick table; notebook consumers read it.".into(),
                skipped: false,
            },
            Stage1Answer {
                question_id: "found.stage1.external-interface".into(),
                text: "Polygon 100 req/s burst; schema NOT assumed stable.".into(),
                skipped: false,
            },
            Stage1Answer {
                question_id: "found.stage1.evolution-spine".into(),
                text: "Tick schema stable; aggregation logic volatile.".into(),
                skipped: true, // skipped on purpose for open-question test
            },
        ]
    }

    fn sample_stage2_outcomes() -> Vec<(FaultLine, FaultLineOutcome)> {
        vec![
            (
                FaultLine {
                    id: "fault.time-representation".into(),
                    title: "Time representation".into(),
                    summary: "UTC vs local at boundaries".into(),
                    sides: vec![
                        FaultLineSide {
                            label: "UTC".into(),
                            tradeoffs: "t1".into(),
                        },
                        FaultLineSide {
                            label: "Local".into(),
                            tradeoffs: "t2".into(),
                        },
                    ],
                },
                FaultLineOutcome::Resolved {
                    choice: "UTC everywhere".into(),
                    reasoning: "one timezone is simpler".into(),
                },
            ),
            (
                FaultLine {
                    id: "fault.persistence-shape".into(),
                    title: "Persistence shape".into(),
                    summary: "tables vs events".into(),
                    sides: vec![],
                },
                FaultLineOutcome::Resolved {
                    choice: "Append-only event log".into(),
                    reasoning: "audit by construction".into(),
                },
            ),
            (
                FaultLine {
                    id: "fault.id-scheme".into(),
                    title: "Identifier scheme".into(),
                    summary: "UUID v4 vs ULID vs snowflake".into(),
                    sides: vec![],
                },
                FaultLineOutcome::Open {
                    note: "still deciding between ULID and UUIDv7".into(),
                },
            ),
            (
                FaultLine {
                    id: "fault.secrets".into(),
                    title: "Secrets".into(),
                    summary: "env vs vault".into(),
                    sides: vec![],
                },
                FaultLineOutcome::Skipped,
            ),
        ]
    }

    fn sample_obs_rust() -> ProjectObservation {
        let mut o = minimal_obs();
        o.deps.push(DetectedDependency {
            name: "reqwest".into(),
            version: Some("0.11".into()),
            source_file: "Cargo.toml".into(),
            kind: DepKind::Direct,
        });
        o
    }

    fn sample_inputs<'a>(
        obs: &'a ProjectObservation,
        answers: &'a [Stage1Answer],
        outcomes: &'a [(FaultLine, FaultLineOutcome)],
        design: Option<&'a str>,
    ) -> FoundingInputs<'a> {
        FoundingInputs {
            project_id: "polygon-ingest",
            founded_date: "2026-04-20",
            observation: obs,
            design,
            stage1_answers: answers,
            stage2_outcomes: outcomes,
            phase1_stop_condition:
                "POST /ingest accepts a tick sample; GET /ticks?symbol=AAPL returns it within 200ms.",
        }
    }

    #[test]
    fn charter_structure_has_all_required_sections() {
        let obs = sample_obs_rust();
        let answers = sample_stage1_answers();
        let outcomes = sample_stage2_outcomes();
        let inputs = sample_inputs(&obs, &answers, &outcomes, None);
        let charter = compose_charter(&inputs);
        // Title + metadata
        assert!(charter.starts_with("# polygon-ingest — Charter"));
        assert!(charter.contains("_Founded: 2026-04-20"));
        // Required sections
        for heading in [
            "## System design",
            "## Invariants",
            "## Resolved decisions",
            "## Open questions",
            "## Amendment log",
        ] {
            assert!(
                charter.contains(heading),
                "charter missing section {heading}\n\n----\n{charter}\n----"
            );
        }
    }

    #[test]
    fn charter_renders_design_doc_verbatim_when_provided() {
        let obs = sample_obs_rust();
        let answers = sample_stage1_answers();
        let outcomes = sample_stage2_outcomes();
        let design = "# Architectural outline\n\nMarket-data pipeline: Polygon -> normalize -> Postgres.";
        let inputs = sample_inputs(&obs, &answers, &outcomes, Some(design));
        let charter = compose_charter(&inputs);
        assert!(charter.contains("Market-data pipeline: Polygon -> normalize -> Postgres."));
    }

    #[test]
    fn charter_falls_back_to_stage1_purpose_when_no_design() {
        let obs = sample_obs_rust();
        let answers = sample_stage1_answers();
        let outcomes = sample_stage2_outcomes();
        let inputs = sample_inputs(&obs, &answers, &outcomes, None);
        let charter = compose_charter(&inputs);
        assert!(
            charter.contains("Real-time options market-data ingest."),
            "design section should fall through to the purpose answer"
        );
    }

    #[test]
    fn charter_never_fabricates_invariants() {
        // Bare observation, no stage 1 / stage 2 inputs → we render
        // the "no invariants named at founding" sentinel, not a
        // bulleted fabrication.
        let obs = minimal_obs();
        let inputs = FoundingInputs {
            project_id: "p",
            founded_date: "2026-04-20",
            observation: &obs,
            design: None,
            stage1_answers: &[],
            stage2_outcomes: &[],
            phase1_stop_condition: "x",
        };
        let charter = compose_charter(&inputs);
        assert!(
            charter.contains("No invariants named at founding"),
            "empty input must produce the sentinel, not a bulleted list"
        );
    }

    #[test]
    fn charter_invariants_include_resolved_persistence_shape() {
        let obs = sample_obs_rust();
        let answers = sample_stage1_answers();
        let outcomes = sample_stage2_outcomes();
        let inputs = sample_inputs(&obs, &answers, &outcomes, None);
        let charter = compose_charter(&inputs);
        assert!(
            charter.contains("Persistence shape is chosen: Append-only event log"),
            "resolved persistence-shape fault must become an invariant"
        );
    }

    #[test]
    fn charter_lists_resolved_decisions_and_open_questions_separately() {
        let obs = sample_obs_rust();
        let answers = sample_stage1_answers();
        let outcomes = sample_stage2_outcomes();
        let inputs = sample_inputs(&obs, &answers, &outcomes, None);
        let charter = compose_charter(&inputs);

        // Resolved decisions section contains the stage-1 answers + fault-line resolutions.
        assert!(charter.contains("found.stage1.persistence-contract"));
        assert!(charter.contains("fault.time-representation"));
        assert!(charter.contains("UTC everywhere"));

        // Open questions section contains skipped stage 1 + open fault lines.
        assert!(charter.contains("found.stage1.evolution-spine"));
        assert!(charter.contains("still deciding between ULID and UUIDv7"));

        // Skipped fault lines get NO mention.
        assert!(
            !charter.contains("fault.secrets"),
            "skipped fault lines must not appear anywhere in the charter"
        );
    }

    #[test]
    fn phases_phase0_stop_is_language_specific() {
        let mut obs = minimal_obs();
        obs.languages = vec![LanguageObservation {
            id: "go".into(),
            display: "Go".into(),
            scip_tooling: ScipTooling::NotRequired,
        }];
        let inputs = sample_inputs(&obs, &[], &[], None);
        let phases = compose_phases(&inputs);
        assert!(phases.contains("`go build ./... && go test ./...`"));
    }

    #[test]
    fn phases_phase1_uses_user_supplied_stop_condition_verbatim() {
        let obs = sample_obs_rust();
        let inputs = sample_inputs(&obs, &[], &[], None);
        let phases = compose_phases(&inputs);
        assert!(phases.contains(
            "POST /ingest accepts a tick sample; GET /ticks?symbol=AAPL returns it within 200ms."
        ));
    }

    #[test]
    fn phases_phase2_names_a_concrete_degraded_condition() {
        let obs = sample_obs_rust(); // has reqwest
        let inputs = sample_inputs(&obs, &[], &[], None);
        let phases = compose_phases(&inputs);
        assert!(
            phases.contains("reqwest"),
            "phase 2 should reference a real external dep when one is present"
        );
        assert!(phases.contains("503"));
    }

    #[test]
    fn phases_phase2_falls_back_to_generic_chaos_without_external_deps() {
        let obs = minimal_obs(); // no external deps
        let inputs = sample_inputs(&obs, &[], &[], None);
        let phases = compose_phases(&inputs);
        assert!(
            phases.contains("random process restart"),
            "no external deps → generic chaos condition"
        );
    }

    #[test]
    fn phases_phase3_plus_is_explicitly_deferred_with_rationale() {
        let obs = sample_obs_rust();
        let inputs = sample_inputs(&obs, &[], &[], None);
        let phases = compose_phases(&inputs);
        assert!(phases.contains("intentionally deferred"));
        assert!(phases.contains("information we'll have"));
    }

    #[test]
    fn charter_hash_is_deterministic_and_differs_per_content() {
        let h1 = hash_charter("charter body\n");
        let h2 = hash_charter("charter body\n");
        let h3 = hash_charter("charter body edited\n");
        assert_eq!(h1, h2);
        assert_ne!(h1, h3);
        assert_eq!(h1.len(), 64, "SHA-256 hex is 64 chars");
    }

    // ── Approval loop runner ───────────────────────────────────

    struct ScriptedApproval {
        answers: Vec<ApprovalAnswer>,
        phase1_stop: String,
        previews: RefCell<Vec<(String, String)>>,
        editor_edits: Vec<(String, String)>,
    }

    impl ScriptedApproval {
        fn new(answers: Vec<ApprovalAnswer>, phase1: &str) -> Self {
            Self {
                answers,
                phase1_stop: phase1.into(),
                previews: RefCell::new(Vec::new()),
                editor_edits: Vec::new(),
            }
        }

        fn with_edits(mut self, edits: Vec<(String, String)>) -> Self {
            self.editor_edits = edits;
            self
        }
    }

    impl ApprovalInterlocutor for ScriptedApproval {
        fn ask_phase1_stop(&mut self) -> String {
            self.phase1_stop.clone()
        }
        fn preview(&mut self, charter: &str, phases: &str) {
            self.previews
                .borrow_mut()
                .push((charter.to_string(), phases.to_string()));
        }
        fn ask_approval(&mut self) -> ApprovalAnswer {
            self.answers.remove(0)
        }
        fn edit_in_editor(&mut self, _c: &Path, _p: &Path) -> (String, String) {
            if self.editor_edits.is_empty() {
                (String::new(), String::new())
            } else {
                self.editor_edits.remove(0)
            }
        }
    }

    #[test]
    fn approval_loop_approve_returns_current_drafts() {
        let tmp = tempfile::tempdir().unwrap();
        let mut interloc = ScriptedApproval::new(vec![ApprovalAnswer::Approve], "stop");
        let out = run_stage34(
            "charter v1".into(),
            "phases v1".into(),
            tmp.path(),
            &mut interloc,
        );
        match out {
            FoundingApproval::Approved { charter, phases } => {
                assert_eq!(charter, "charter v1");
                assert_eq!(phases, "phases v1");
            }
            FoundingApproval::Cancelled => panic!("should have approved"),
        }
        assert_eq!(
            interloc.previews.borrow().len(),
            1,
            "approve on first prompt = one preview"
        );
    }

    #[test]
    fn approval_loop_cancel_returns_without_writing() {
        let tmp = tempfile::tempdir().unwrap();
        let mut interloc = ScriptedApproval::new(vec![ApprovalAnswer::Cancel], "stop");
        let out = run_stage34(
            "c".into(),
            "p".into(),
            tmp.path(),
            &mut interloc,
        );
        assert_eq!(out, FoundingApproval::Cancelled);
    }

    #[test]
    fn approval_loop_edit_then_approve_picks_up_edited_content() {
        let tmp = tempfile::tempdir().unwrap();
        let mut interloc = ScriptedApproval::new(
            vec![ApprovalAnswer::EditInEditor, ApprovalAnswer::Approve],
            "stop",
        )
        .with_edits(vec![("charter EDITED".into(), "phases EDITED".into())]);
        let out = run_stage34(
            "charter v1".into(),
            "phases v1".into(),
            tmp.path(),
            &mut interloc,
        );
        match out {
            FoundingApproval::Approved { charter, phases } => {
                assert_eq!(charter, "charter EDITED");
                assert_eq!(phases, "phases EDITED");
            }
            _ => panic!("should have approved after the edit"),
        }
        // Two previews: one before the edit, one after.
        assert_eq!(interloc.previews.borrow().len(), 2);
    }

    // ── Stage 2.5 (docs URLs) tests ────────────────────────────

    #[test]
    fn docs_prompt_lists_direct_deps() {
        let obs = obs_with_deps();
        let prompt = render_docs_prompt(&obs);
        assert!(prompt.contains("external services and libraries"));
        assert!(prompt.contains("polygon-api-client"));
        assert!(prompt.contains("Where should I look for documentation"));
    }

    #[test]
    fn docs_prompt_handles_empty_dep_list_gracefully() {
        let obs = minimal_obs();
        let prompt = render_docs_prompt(&obs);
        assert!(
            prompt.contains("no direct deps have been observed yet"),
            "empty-dep prompt must say so, not fabricate a list"
        );
        // The ask still happens — operators know their domain
        // better than our dep detector.
        assert!(prompt.contains("Where should I look for documentation"));
    }

    #[test]
    fn docs_prompt_caps_at_15_deps_with_ellipsis() {
        let mut obs = minimal_obs();
        for i in 0..25 {
            obs.deps.push(DetectedDependency {
                name: format!("dep-{i:02}"),
                version: None,
                source_file: "Cargo.toml".into(),
                kind: DepKind::Direct,
            });
        }
        let prompt = render_docs_prompt(&obs);
        assert!(prompt.contains("dep-00"));
        assert!(prompt.contains("dep-14"));
        assert!(
            !prompt.contains("dep-15"),
            "only the first 15 deps should be listed"
        );
        assert!(prompt.contains("…and 10 more"));
    }

    #[test]
    fn docs_decision_body_records_empty_answer_explicitly() {
        let body = render_docs_decision_body("prompt here", &[]);
        assert!(body.contains("found.docs-urls"));
        assert!(body.contains("prompt here"));
        assert!(
            body.contains("No URLs provided"),
            "empty answer must be recorded as an explicit decision, not absence"
        );
        assert!(body.contains("honest-uncertainty prompt"));
    }

    #[test]
    fn docs_decision_body_lists_provided_urls_verbatim() {
        let urls = vec![
            "https://polygon.io/docs/options".to_string(),
            "https://internal.wiki/ingest".to_string(),
        ];
        let body = render_docs_decision_body("prompt", &urls);
        assert!(body.contains("https://polygon.io/docs/options"));
        assert!(body.contains("https://internal.wiki/ingest"));
        assert!(
            !body.contains("No URLs provided"),
            "non-empty answer path must not emit the empty-answer sentinel"
        );
    }

    #[test]
    fn approval_loop_seeds_pending_files_on_first_preview() {
        let tmp = tempfile::tempdir().unwrap();
        let mut interloc = ScriptedApproval::new(vec![ApprovalAnswer::Cancel], "stop");
        let _ = run_stage34(
            "charter seed".into(),
            "phases seed".into(),
            tmp.path(),
            &mut interloc,
        );
        let c_disk = std::fs::read_to_string(tmp.path().join("CHARTER.md")).unwrap();
        let p_disk = std::fs::read_to_string(tmp.path().join("PHASES.md")).unwrap();
        assert_eq!(c_disk, "charter seed");
        assert_eq!(p_disk, "phases seed");
    }
}
