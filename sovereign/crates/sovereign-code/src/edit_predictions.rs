// SPDX-License-Identifier: AGPL-3.0-or-later
//! `POST /v1/edit_predictions` — next-edit prediction, both lanes
//! (`sovereign/docs/NEXT_EDIT.md` §3). Deliberately thin over the pure
//! pipelines: parse + validate the wire shape, convert the client's
//! UTF-16 offsets to bytes, predict, convert back. The rule lane
//! ([`code_next_edit::next_edit`]) always runs first and needs no inference;
//! when it declines AND the request opts in (`model_lane: true`), the
//! model lane ([`code_next_edit::next_edit_model`]) may consult the resident
//! FIM slot for a region rewrite — behind the same response shape,
//! with `engine: "model"` and the drop-invalid posture: no suggestion
//! beats a wrong one.
//!
//! Silence is a 200 with an empty `edits` array, never an error: the
//! client polls this on every edit-settle, and "nothing to suggest"
//! is the common, healthy case. With `debug: true` (always on from
//! the first-party extension) the response says which gate held, on
//! both lanes.
//!
//! Code's door since pb-meshapp-rest (FIVE_PROGRAMS §2; phase-b-23: "the
//! door is not the model"): `svrn code mcp` serves [`router`] alone, and the
//! stock binary mounts it on svrn's client surfaces through code's face. The
//! model call dials serve on this host ([`serve_dial`]); the grammar lookup
//! is the host's ([`EditDoor::grammar`]).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::Json;

use code_next_edit::grammar::GrammarLookup;
use code_next_edit::next_edit::{self, HistoryUnit};
use code_next_edit::next_edit_model::{self, Consult};
use code_next_edit::next_edit_symbols;
use code_next_edit::next_edit_syntax;
use sovereign_contracts::oicp::openai_types::ChatCompletionRequest;
use sovereign_contracts::oicp::FimCompletionRequest;

pub mod outcome;

mod serve_dial;
mod wire;

use wire::bad_request;
pub use wire::{validate_wire, EditPredictionsRequestWire, HistoryUnitWire, MAX_BODY_BYTES};
#[cfg(test)]
pub(crate) use wire::{MAX_TEXT_BYTES, MAX_UNIT_BYTES};

/// The door's placement, handed in by whoever composes code.
pub struct EditDoor {
    /// The host's extension → grammar registry. The stock binary supplies
    /// corpus-engine's; standalone `svrn code` has none, so the rule lane
    /// runs without its syntax filter and the symbol lane declines, each
    /// named in the debug block (never a guessed site set).
    pub grammar: Option<GrammarLookup>,
    /// Where `<corpus>/scip_graph.db` lives: code's indexes root.
    pub indexes_dir: PathBuf,
    /// serve's base on this host, which the model lane dials.
    pub serve_base: String,
    /// One-in-flight budget for the model lane (`sovereign/docs/NEXT_EDIT.md`
    /// §4): a consult that finds the slot busy is dropped immediately
    /// (`dropped: "busy"`), never queued — ghost text and chat always win
    /// the slot. `Arc` so the permit moves into the task that runs the
    /// inference, which is what keeps it bounding the generation rather than
    /// the handler (see `edit_predictions`).
    model_slot: Arc<tokio::sync::Semaphore>,
    http: reqwest::Client,
}

impl EditDoor {
    /// A door over `indexes_dir`, dialing serve at `serve_base`.
    pub fn new(grammar: Option<GrammarLookup>, indexes_dir: PathBuf, serve_base: String) -> Self {
        Self {
            grammar,
            indexes_dir,
            serve_base,
            model_slot: Arc::new(tokio::sync::Semaphore::new(1)),
            http: reqwest::Client::new(),
        }
    }
}

/// `POST /v1/edit_predictions` and `POST /v1/edit_predictions/outcome`.
/// The tighter body limit overrides a host's wider front door: the
/// handler's documented caps (512 KiB text, 32 units) are a contract check,
/// and the transport should refuse a body that could never satisfy them
/// before serde allocates it. The host adds its own admission gate.
pub fn router(door: EditDoor) -> axum::Router {
    tracing::info!(
        target: "next_edit",
        grammar = door.grammar.is_some(),
        serve = %door.serve_base,
        indexes = %door.indexes_dir.display(),
        "editor door: composed"
    );
    axum::Router::new()
        .route(
            "/v1/edit_predictions",
            axum::routing::post(edit_predictions)
                .layer(axum::extract::DefaultBodyLimit::max(MAX_BODY_BYTES)),
        )
        .route(
            "/v1/edit_predictions/outcome",
            axum::routing::post(outcome::edit_prediction_outcome),
        )
        .with_state(Arc::new(door))
}

/// Why a lane that needs a grammar did not judge, on a composition that
/// supplies no grammar lookup.
const GRAMMAR_ABSENT: &str = "unjudged:grammar_absent";

/// Has the actionable symbol-lane warning already been said?
///
/// Once per process. This fires on the typing path, so an un-throttled
/// warn would emit on every coalesced edit unit and bury the log it is
/// trying to make useful — and the condition it reports (no index) is
/// static until somebody runs a command.
static SYMBOL_LANE_WARNED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Model-lane inference budget; a slower response is dropped as
/// `timeout` (the GM5 latency gate lives in the §6 bank, not here).
const MODEL_TIMEOUT_MS: u64 = 15_000;

/// Symbol-lane budget. The measured lookup is 0.03 ms, and 9.4 ms for
/// the worst-cased symbol on this repo's graph — but the reindexer can
/// hold SQLite's write lock, and this runs on the interactive typing
/// path. Bounded rather than trusted: a jump list is never worth a
/// stalled keystroke, and the decline says `timed_out` rather than
/// disappearing.
const SYMBOL_TIMEOUT_MS: u64 = 250;

/// The resident slot the model lane may consult. Read once per
/// request so the debug block can name the model even when the consult
/// is later refused.
#[derive(Debug, Clone)]
pub struct ModelSlot {
    pub model_id: String,
    pub slot: String,
    pub format: String,
    /// Carried from [`EditSlotInfo::degraded`]: this slot is the
    /// automatic fallback, not an operator-chosen edit model. Not used
    /// in any decision here — it exists so the journal can tell a
    /// fallback episode from a specialist one, which is the distinction
    /// the `SOVEREIGN_NEXT_EDIT_FALLBACK` ledger row turns on.
    pub degraded: bool,
}

/// One inference the lane wants run. Owned rather than borrowed so the
/// caller can move it into a spawned task without lifetime gymnastics;
/// the clone costs nothing beside a decode.
#[derive(Debug, Clone)]
pub struct InferenceCall {
    pub prompt: next_edit_model::Prompt,
    pub max_tokens: u32,
    pub stop: Vec<String>,
    pub temperature: f32,
    pub model_id: String,
    /// Carried verbatim from `ConsultPlan::suppress_thinking` — see
    /// that field for why an unsuppressed thinking phase scores 0/30
    /// on this lane rather than merely scoring worse.
    pub suppress_thinking: bool,
}

/// A drop the caller's inference produced, named for the debug block.
/// The set is closed: `busy`, `timeout`, `error`, `unavailable`.
#[derive(Debug, Clone, Copy)]
pub struct InferError(pub &'static str);

/// Everything the handler learned, so the caller can log it without
/// re-deriving any of it from the JSON body.
pub struct PredictOutcome {
    pub body: serde_json::Value,
    pub engine: &'static str,
    pub proposed: usize,
    pub support: usize,
    pub sites: usize,
    pub reason_silent: &'static str,
    pub model_state: String,
    /// The metadata-only journal record for this episode, already built
    /// but NOT written. Constructing it here (where every fact is in
    /// scope) and appending it at the route is what keeps the offline
    /// scorer's runs out of a developer's acceptance numbers: the
    /// scorer shares this pipeline and simply drops the record.
    pub episode: sovereign_contracts::types::NextEditEpisode,
}

/// The pipeline over one already-validated request, with inference
/// supplied by the caller.
///
/// The door passes its slot-bounded, timeout-bounded inference; the
/// offline scorer (`examples/next_edit_score.rs`) passes a plain HTTP
/// call to any OpenAI-compatible endpoint. Everything else — both
/// lanes, the UTF-16 conversion, and the exact `sovereign_debug` shape
/// — is shared, so a checkpoint scored offline gets *the door's*
/// answer rather than a second implementation's opinion of it. That is
/// the whole reason this function exists as a seam instead of the
/// scorer re-walking the same ordering (NEXT_EDIT.md §9a, "Two rulers
/// on one contract"). `grammar` is the host's registry; `None` runs the
/// rule lane without its syntax filter and says so.
pub async fn predict_response<F, Fut>(
    wire: &EditPredictionsRequestWire,
    model: Option<ModelSlot>,
    started: std::time::Instant,
    force: bool,
    grammar: Option<GrammarLookup>,
    infer: F,
) -> PredictOutcome
where
    F: FnOnce(InferenceCall) -> Fut,
    Fut: std::future::Future<Output = Result<(String, Option<String>), InferError>>,
{
    let history: Vec<HistoryUnit> = wire
        .history
        .iter()
        .map(|u| HistoryUnit {
            before: u.before.clone(),
            after: u.after.clone(),
            left: u.left.clone(),
            right: u.right.clone(),
        })
        .collect();
    let cursor = next_edit::utf16_to_byte(&wire.text, wire.cursor);
    // Syntax narrows the site set; the lane itself stays pure. The
    // oracle is built HERE because this is where the buffer, the path
    // and the language live — `next_edit` has no editor knowledge by
    // contract, and a parser is editor knowledge. `None` (no grammar,
    // buffer too large, parse failed) means "cannot judge", and the
    // closure then returns sites untouched rather than guessing. No lookup
    // at all (standalone `svrn code`) is the same "cannot judge", named.
    let syntax_unjudged = wire.path.is_some() && grammar.is_none();
    if syntax_unjudged {
        tracing::debug!(target: "next_edit", "syntax filter unjudged: this composition supplies no grammar lookup");
    }
    let oracle = wire
        .path
        .as_deref()
        .zip(grammar)
        .and_then(|(p, g)| next_edit_syntax::SyntaxOracle::parse(p, &wire.text, g));
    let p =
        next_edit::predict_filtered(&history, &wire.text, cursor, &|rule, sites| match &oracle {
            Some(o) => o.keep(&wire.text, rule, sites),
            None => sites,
        });

    let mut engine = "rule";
    let mut final_edits: Vec<next_edit::Edit> = p.edits.clone();
    let mut model_debug: Option<serde_json::Value> = None;
    // The slot's identity, kept behind while `model` itself moves into
    // the lane. Four scalars, not the slot, because this is the only
    // thing downstream still wants from it.
    let slot_facts = model.as_ref().map(|m| {
        (
            m.model_id.clone(),
            m.slot.clone(),
            m.format.clone(),
            m.degraded,
        )
    });
    if wire.model_lane {
        let (m_edits, dbg) = model_lane(wire, model, &history, &p, cursor, force, infer).await;
        if let Some(me) = m_edits {
            engine = "model";
            final_edits = me;
        }
        model_debug = Some(dbg);
    }

    // Byte → UTF-16 for the wire, one pass for all edit boundaries.
    let boundaries: Vec<usize> = final_edits.iter().flat_map(|e| [e.start, e.end]).collect();
    let utf16 = next_edit::bytes_to_utf16(&wire.text, &boundaries);
    let edits: Vec<serde_json::Value> = final_edits
        .iter()
        .zip(utf16.chunks_exact(2))
        .map(|(e, se)| serde_json::json!({ "start": se[0], "end": se[1], "new_text": e.new_text }))
        .collect();

    let model_state = match &model_debug {
        None => "off".to_string(),
        Some(_) if engine == "model" => "fired".to_string(),
        Some(d) => d["dropped"]
            .as_str()
            .map(|r| format!("dropped:{r}"))
            .or_else(|| d["skipped"].as_str().map(|r| format!("skipped:{r}")))
            .unwrap_or_else(|| "silent".to_string()),
    };

    // Built here, where every fact is still in scope, and written by the
    // route rather than by this function — see `PredictOutcome::episode`.
    let episode = code_next_edit::next_edit_journal::episode_from(
        engine,
        edits.len(),
        p.support,
        p.sites,
        p.reason_silent,
        wire.language.as_deref(),
        wire.path.as_deref(),
        slot_facts
            .as_ref()
            .map(|(m, s, f, d)| (m.as_str(), s.as_str(), f.as_str(), *d)),
        model_debug.as_ref(),
        started.elapsed().as_millis() as u64,
    );

    // `episode_id` is NOT debug-gated: it is how the editor's outcome
    // report joins back to this episode, and outcome reporting has to
    // work on an ordinary production request. It is a random opaque id
    // and says nothing about the document.
    let mut body = serde_json::json!({
        "object": "edit_prediction",
        "engine": engine,
        "edits": edits,
        "episode_id": episode.episode_id,
    });
    if wire.debug {
        body["sovereign_debug"] = serde_json::json!({
            "rule_find": p.rule.as_ref().map(|r| r.find.clone()),
            "rule_replace": p.rule.as_ref().map(|r| r.replace.clone()),
            "rule_key": p.rule.as_ref().map(next_edit::GuardedRule::key),
            "support": p.support,
            "sites": p.sites,
            "edits_capped": p.edits_capped,
            "reason_silent": p.reason_silent,
            "timings_ms": { "total": started.elapsed().as_millis() as u64 },
        });
        // The lane verdict the daemon already traces, put on the wire
        // under the same name. A scoring harness has to attribute every
        // outcome to a lane — "did the model even get asked?" is the
        // first question of any model measurement — and deriving this
        // from the raw skipped/dropped fields harness-side would be a
        // second implementation of one formula (ARCH §10.6).
        body["sovereign_debug"]["model_state"] = model_state.clone().into();
        if syntax_unjudged {
            body["sovereign_debug"]["syntax_filter"] = GRAMMAR_ABSENT.into();
        }
        if let Some(m) = model_debug {
            body["sovereign_debug"]["model"] = m;
        }
    }

    PredictOutcome {
        proposed: edits.len(),
        engine,
        support: p.support,
        sites: p.sites,
        reason_silent: p.reason_silent.unwrap_or("no"),
        model_state,
        body,
        episode,
    }
}

/// POST /v1/edit_predictions.
pub async fn edit_predictions(
    State(door): State<Arc<EditDoor>>,
    Json(wire): Json<EditPredictionsRequestWire>,
) -> Response {
    let started = std::time::Instant::now();

    if let Err(msg) = validate_wire(&wire) {
        return bad_request(msg);
    }

    // The resident slot, read before the gate so the debug block can
    // name the model even when the consult is later refused.
    let model = if wire.model_lane {
        // Gate on the NEXT-EDIT lane, not on the slot's existence. An
        // editing slot may serve FIM only (or, transitionally, neither)
        // — consulting it for a region rewrite would feed the model a
        // dialect it has no contract for. No lane, no model, and the
        // debug block below reports `unavailable` rather than guessing.
        // No serve at the base is the same `unavailable`, traced.
        serve_dial::edit_slot(&door.serve_base)
            .await
            .and_then(|edit| {
                let degraded = edit.degraded;
                edit.next_edit.map(|lane| ModelSlot {
                    model_id: edit.model_id,
                    slot: edit.slot,
                    format: lane.format.as_str().to_string(),
                    degraded,
                })
            })
    } else {
        None
    };
    let sem = door.model_slot.clone();
    let (http, serve_base) = (door.http.clone(), door.serve_base.clone());
    let req_path = wire.path.clone();
    let req_language = wire.language.clone();

    // The door never forces: the consult gate IS production routing.
    let out = predict_response(&wire, model, started, false, door.grammar, move |call| async move {
        // The permit rides INTO the task, not just this scope.
        // Abandoning a completion future does not stop the generation
        // behind it: the engine dispatches through `spawn_blocking`,
        // and dropping a `JoinHandle` detaches rather than cancels, so
        // llama.cpp keeps decoding and keeps the slot's context lock.
        // If the permit were released when we time out, every
        // timed-out consult would leave a live generation behind while
        // the next request sailed through `try_acquire` — the
        // one-in-flight budget would stop bounding anything precisely
        // when the slot is most contended. Holding it until the
        // inference genuinely returns makes the next consult report an
        // honest `busy` instead.
        let Ok(permit) = sem.try_acquire_owned() else {
            return Err(InferError("busy"));
        };
        // Both branches resolve to `Result<(content, finish_reason), _>`
        // so the outcome handling below is format-agnostic.
        let task = match call.prompt {
            // Completion-style edit models: the lane built the model's
            // own raw prompt and rides the FIM slot's verbatim path — a
            // chat template would wrap the special tokens in a user
            // turn and the fine-tune would never see its trained shape.
            next_edit_model::Prompt::Raw(raw) => {
                let freq = FimCompletionRequest {
                    prefix: String::new(),
                    suffix: String::new(),
                    path: req_path,
                    language: req_language,
                    max_tokens: Some(call.max_tokens as usize),
                    temperature: Some(call.temperature),
                    stop: call.stop,
                    debug: false,
                    raw_prompt: Some(raw),
                };
                // serve's `/v1/completions` runs the FIM adapter over the
                // edit slot (phase-b-23); the permit is held until serve
                // answers, which is when its generation has ended.
                tokio::spawn(async move {
                    let out = serve_dial::raw_completion(&http, &serve_base, &freq).await;
                    drop(permit);
                    out
                })
            }
            chat @ (next_edit_model::Prompt::Chat(_)
            | next_edit_model::Prompt::ChatSystem { .. }) => {
                // `chat_messages` owns the layout so this and the
                // offline scorer cannot drift apart (ARCH §10.6).
                let messages = chat.chat_messages().unwrap_or_default();
                // Thinking suppression rides BOTH transports because
                // they are read by different layers and only the pair
                // is sufficient: `chat_template_kwargs.enable_thinking`
                // is what the Jinja template branches on, while
                // `think_budget: 0` drives the daemon's `/no_think`
                // injection for families whose template ignores the
                // kwarg. A budget of 0 on its own was measured inert on
                // this chat template (2026-08-07) — 811 chars of
                // reasoning still emitted, content empty, finish=length.
                let (kwargs, think_budget) = if call.suppress_thinking {
                    (serde_json::json!({ "enable_thinking": false }), Some(0))
                } else {
                    (serde_json::Value::Null, None)
                };
                let req: ChatCompletionRequest = match serde_json::from_value(serde_json::json!({
                    "model": call.model_id,
                    "messages": messages,
                    "temperature": call.temperature,
                    "max_tokens": call.max_tokens,
                    "chat_template_kwargs": kwargs,
                    "think_budget": think_budget,
                })) {
                    Ok(r) => r,
                    Err(e) => {
                        tracing::warn!(target: "next_edit", error = %e, "model lane request build failed");
                        return Err(InferError("error"));
                    }
                };
                tokio::spawn(async move {
                    // The next-edit lane has its own timeout + fallback and does
                    // not branch on shed structure, so a refusal is the `String`
                    // error the sibling FIM arm above produces — both arms spawn
                    // into one `JoinHandle` type.
                    let out = serve_dial::chat_completion(&http, &serve_base, &req).await;
                    drop(permit);
                    out
                })
            }
        };
        match tokio::time::timeout(Duration::from_millis(MODEL_TIMEOUT_MS), task).await {
            Err(_) => Err(InferError("timeout")),
            Ok(Err(e)) => {
                tracing::warn!(target: "next_edit", error = %e, "model lane task failed");
                Err(InferError("error"))
            }
            Ok(Ok(Err(e))) => {
                tracing::warn!(target: "next_edit", error = %e, "model lane inference error");
                Err(InferError("error"))
            }
            Ok(Ok(Ok(v))) => Ok(v),
        }
    })
    .await;

    tracing::info!(
        target: "next_edit",
        path = wire.path.as_deref().unwrap_or("<unset>"),
        history = wire.history.len(),
        support = out.support,
        sites = out.sites,
        proposed = out.proposed,
        silent = out.reason_silent,
        engine = out.engine,
        model = %out.model_state,
        elapsed_ms = started.elapsed().as_millis() as u64,
        "edit prediction"
    );

    // The symbol lane runs HERE rather than inside `predict_response`
    // because it is independent of both lanes that function owns: it
    // reads no rule, consults no model, and proposes no edit. Keeping
    // it out also keeps `predict_response` the pure two-lane seam the
    // offline scorer shares (NEXT_EDIT.md §9a) — that harness scores
    // edits, and a jump list is not one.
    let mut body = out.body;
    if wire.symbol_lane && door.grammar.is_none() {
        // The lane parses the buffer to find the edited signature; with no
        // grammar lookup it cannot judge, and says so rather than guessing.
        tracing::debug!(target: "next_edit", "symbol lane unjudged: this composition supplies no grammar lookup");
        body["navigation"] = serde_json::json!({ "declined": "grammar_absent" });
        if wire.debug {
            body["sovereign_debug"]["symbol_lane"] = GRAMMAR_ABSENT.into();
        }
    }
    if let (true, Some(grammar)) = (wire.symbol_lane, door.grammar) {
        let nav = match tokio::time::timeout(
            Duration::from_millis(SYMBOL_TIMEOUT_MS),
            symbol_lane(&door, &wire, grammar),
        )
        .await
        {
            Ok(r) => r,
            Err(_) => Err(next_edit_symbols::Decline::TimedOut),
        };
        body["navigation"] = match &nav {
            Ok(n) => serde_json::json!({
                "symbol": n.symbol,
                "sites": n.sites.iter().map(|s| serde_json::json!({
                    "path": s.path,
                    "line": s.line,
                    "col": s.col,
                    "preview": s.preview,
                })).collect::<Vec<_>>(),
                "truncated": n.truncated,
                "dropped": n.dropped,
            }),
            // A decline is a NAMED state on the wire, not an absent
            // key: the client renders nothing either way, but a
            // developer asking why gets an answer (ARCH §9).
            Err(reason) => serde_json::json!({ "declined": reason.as_str() }),
        };
        match &nav {
            // A decline the developer can ACT on is worth saying out
            // loud — once. The lane degrades gracefully either way (the
            // response is a normal 200 and every other lane is
            // untouched), and that is exactly what makes a missing
            // index invisible: no error, no failed request, just a
            // status-bar item that never appears.
            Err(r) if r.is_actionable() => {
                if !SYMBOL_LANE_WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
                    tracing::warn!(
                        target: "next_edit",
                        reason = r.as_str(),
                        corpus_id = wire.corpus_id.as_deref().unwrap_or("<unset>"),
                        "{}",
                        r.remedy().unwrap_or("symbol lane unavailable")
                    );
                }
            }
            _ => tracing::debug!(
                target: "next_edit",
                symbol_lane = match &nav {
                    Ok(n) => format!("{} site(s), {} dropped", n.sites.len(), n.dropped),
                    Err(r) => format!("declined:{}", r.as_str()),
                },
                "symbol lane"
            ),
        }
    }

    // The developer's own local record of what this lane did. Off the
    // request path and unable to fail it: `record` drops the join handle
    // and turns any error into a `warn` (see that function).
    code_next_edit::next_edit_journal::record_next_edit(
        sovereign_contracts::types::JournalLine::Episode(out.episode),
    );

    Json(body).into_response()
}

/// The symbol lane's impure half: locate the corpus graph, then hand
/// the pure lane a reader for the last-saved text.
///
/// Every failure is a named [`Decline`]. The graph is opened per
/// request — a SQLite open on a warm page cache, against a lookup the
/// lane only reaches after the trigger has already fired, which is
/// rare. Caching a handle here would need invalidation against the
/// reindexer's rewrite of the same file and is not worth that until a
/// measurement says the open is the cost.
async fn symbol_lane(
    door: &EditDoor,
    wire: &EditPredictionsRequestWire,
    grammar: GrammarLookup,
) -> Result<next_edit_symbols::Navigation, next_edit_symbols::Decline> {
    use next_edit_symbols::Decline;
    let corpus_id = wire.corpus_id.as_deref().ok_or(Decline::GraphUnavailable)?;
    let root = std::path::Path::new(wire.workspace_root.as_deref().ok_or(Decline::NoPath)?);
    let db = door.indexes_dir.join(corpus_id).join("scip_graph.db");
    if !db.exists() {
        return Err(Decline::GraphUnavailable);
    }

    // The editor sends an absolute path; the graph is keyed on the
    // repo-relative one. Relativise here, and REFUSE when the file is
    // outside the declared root rather than falling back to the
    // absolute form — a lookup that cannot match would report
    // `symbol_not_indexed`, which reads like "new function" and is a
    // different diagnosis (ARCH §18.3).
    let abs = wire.path.as_deref().ok_or(Decline::NoPath)?;
    let rel = std::path::Path::new(abs)
        .strip_prefix(root)
        .map_err(|_| Decline::NoPath)?
        .to_string_lossy()
        .into_owned();

    let graph = corpus_engine_scip::scip_graph::ScipGraph::open(&db, corpus_id)
        .map_err(|_| Decline::GraphUnavailable)?;
    let cursor = next_edit::utf16_to_byte(&wire.text, wire.cursor);
    // Reads are confined to the declared root and to paths the INDEX
    // named — never to a path the request supplied.
    next_edit_symbols::navigate(&graph, Some(&rel), &wire.text, cursor, grammar, |p| {
        let candidate = root.join(p);
        if !candidate.starts_with(root) {
            return None;
        }
        std::fs::read_to_string(&candidate).ok()
    })
    .await
}

/// The model lane, end to end: consult gate → region guards → prompt
/// → (the caller's inference) → parse → diff → verify. Returns the
/// absolute-byte edits on success; the debug value explains every
/// other outcome (`skipped` when the gate refused, `dropped` when the
/// model was consulted but its output didn't survive — NEXT_EDIT.md
/// §9).
///
/// Every *decision* lives in `next_edit_model::{plan, finish}`. This
/// function only sequences them, hands the one impure step to the
/// caller, and renders the glassbox block — which is what lets the
/// offline scorer reach the same verdicts as the daemon.
async fn model_lane<F, Fut>(
    wire: &EditPredictionsRequestWire,
    model: Option<ModelSlot>,
    history: &[HistoryUnit],
    p: &next_edit::Prediction,
    cursor: usize,
    force: bool,
    infer: F,
) -> (Option<Vec<next_edit::Edit>>, serde_json::Value)
where
    F: FnOnce(InferenceCall) -> Fut,
    Fut: std::future::Future<Output = Result<(String, Option<String>), InferError>>,
{
    // No resident slot. The gate's own answer is still reported, so
    // silence caused by policy never reads as silence caused by an
    // absent model — the two have different fixes.
    let Some(slot) = model else {
        return match next_edit_model::should_consult(history, &wire.text, p) {
            Consult::No { skipped } => (
                None,
                serde_json::json!({ "consulted": false, "skipped": skipped }),
            ),
            Consult::Yes { reason, needle } => (
                None,
                serde_json::json!({
                    "consulted": true,
                    "reason": reason,
                    "needle": needle,
                    "dropped": "unavailable",
                }),
            ),
        };
    };

    let plan = match next_edit_model::plan(
        history,
        &wire.text,
        cursor,
        p,
        wire.path.as_deref(),
        wire.language.as_deref(),
        &slot.format,
        force,
    ) {
        next_edit_model::Plan::Skip { skipped } => {
            return (
                None,
                serde_json::json!({ "consulted": false, "skipped": skipped }),
            );
        }
        next_edit_model::Plan::Decline {
            reason,
            needle,
            dropped,
            region_bytes,
        } => {
            let mut dbg = serde_json::json!({
                "consulted": true,
                "reason": reason,
                "needle": needle,
                "model_id": slot.model_id,
                "slot": slot.slot,
                "format": slot.format,
                "dropped": dropped,
            });
            if let Some(bytes) = region_bytes {
                dbg["region_bytes"] = bytes.into();
            }
            return (None, dbg);
        }
        next_edit_model::Plan::Send(plan) => plan,
    };

    let bounds = next_edit::bytes_to_utf16(&wire.text, &[plan.region_start, plan.region_end]);
    let mut dbg = serde_json::json!({
        "consulted": true,
        "reason": plan.reason,
        "needle": plan.needle,
        "model_id": slot.model_id,
        "slot": slot.slot,
        "format": slot.format,
        "region": { "start": bounds[0], "end": bounds[1] },
        "needle_hit": plan.needle_hit,
        // Visible because it is the difference between 21/30 and 0/30
        // on a chat model, and a truncated-empty result looks identical
        // to a model that had nothing to say (ARCH §9.1).
        "suppress_thinking": plan.suppress_thinking,
    });

    let t0 = std::time::Instant::now();
    let outcome = infer(InferenceCall {
        prompt: plan.prompt.clone(),
        max_tokens: plan.max_tokens,
        stop: plan.stop.clone(),
        temperature: plan.temperature,
        model_id: slot.model_id.clone(),
        suppress_thinking: plan.suppress_thinking,
    })
    .await;
    dbg["timings_ms"] = serde_json::json!({ "inference": t0.elapsed().as_millis() as u64 });

    let (content, finish) = match outcome {
        Ok(v) => v,
        Err(InferError(why)) => {
            dbg["dropped"] = why.into();
            return (None, dbg);
        }
    };

    let region = &wire.text[plan.region_start..plan.region_end];
    match next_edit_model::finish(&plan, history, region, &content, finish.as_deref()) {
        Err(next_edit_model::FinishDrop { dropped, hunk }) => {
            dbg["dropped"] = dropped.into();
            if let Some(e) = hunk {
                dbg["verify_hunk"] = serde_json::json!({
                    "start": e.start,
                    "end": e.end,
                    "old": region[e.start..e.end].chars().take(120).collect::<String>(),
                    "new": e.new_text.chars().take(120).collect::<String>(),
                });
            }
            (None, dbg)
        }
        Ok(region_edits) => {
            let edits = region_edits
                .into_iter()
                .map(|e| next_edit::Edit {
                    start: plan.region_start + e.start,
                    end: plan.region_start + e.end,
                    new_text: e.new_text,
                })
                .collect();
            (Some(edits), dbg)
        }
    }
}

#[cfg(test)]
mod tests;
