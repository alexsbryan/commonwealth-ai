// SPDX-License-Identifier: AGPL-3.0-or-later
//! What an engine did with one request, recorded at the code that did it, for
//! the engine conformance battery (`bench/lanes/engine-swap/`).
//!
//! Production never installs a sink, so [`observe`] costs one thread-local
//! read and builds nothing. The daemon's replay route wraps one call in
//! [`observed`], and every site the call reaches records into that call's
//! sink: the prompt tokens where the engine tokenizes, the sampler where it
//! builds one, the wire body where the remote client sends it. The battery
//! therefore observes the path that runs, never a copy of it.
//!
//! The sink lives in a thread-local that [`Observed`] installs around each
//! poll of the call's future, so it follows the call across awaits whichever
//! thread polls it. Work handed to another thread takes it along with
//! [`carry`].

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use serde::{Deserialize, Serialize};

/// One thing an engine did.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Observation {
    /// The token ids of the prompt as it was decoded.
    PromptTokens {
        /// Token ids.
        ids: Vec<i64>,
    },
    /// The sampler as built.
    Sampler {
        /// Parameters under llama-server's names.
        params: BTreeMap<String, f64>,
        /// Active stages in the order they run.
        order: Vec<String>,
        /// The grammar text the decode is constrained by, if any.
        grammar: Option<String>,
    },
    /// Prompt tokens evaluated for this call, and those reused from a cache.
    Prefill {
        /// Tokens decoded.
        evaluated: u64,
        /// Tokens restored or kept rather than decoded.
        reused: u64,
    },
    /// Generated token ids, in order.
    GeneratedTokens {
        /// Token ids.
        ids: Vec<i64>,
    },
    /// The body a remote client sent.
    WireRequest {
        /// The JSON body.
        body: serde_json::Value,
    },
    /// The raw body a remote client received.
    WireResponse {
        /// The HTTP status.
        status: u16,
        /// The body as text, an SSE stream included.
        body: String,
    },
}

/// A sampler as the conformance battery compares it: only the stages that
/// change the next token, in order, and only their parameters. `raw` uses
/// llama-server's parameter names, `order` its stage names, and the result
/// is the same whichever engine described it, which is what makes the two
/// comparable.
///
/// A stage at its neutral setting (penalties at 1/0/0, DRY at 0, top-p at 1)
/// is dropped. At temperature 0 the token is the argmax, so the truncation
/// stages, which never remove the top token, are dropped too and the last
/// stage is `greedy`. Keys that belong to no stage are kept as they are.
pub fn canonical_sampler(
    raw: &BTreeMap<String, f64>,
    order: &[&str],
) -> (BTreeMap<String, f64>, Vec<String>) {
    const STAGE_PARAMS: &[(&str, &[&str])] = &[
        (
            "penalties",
            &[
                "repeat_penalty",
                "repeat_last_n",
                "frequency_penalty",
                "presence_penalty",
            ],
        ),
        (
            "dry",
            &[
                "dry_multiplier",
                "dry_base",
                "dry_allowed_length",
                "dry_penalty_last_n",
            ],
        ),
        ("top_k", &["top_k"]),
        ("top_p", &["top_p"]),
        ("min_p", &["min_p"]),
        ("typ_p", &["typical_p"]),
        ("xtc", &["xtc_probability", "xtc_threshold"]),
        ("top_n_sigma", &["top_n_sigma"]),
        ("temperature", &["temperature"]),
    ];
    const TRUNCATION: &[&str] = &["top_k", "top_p", "min_p", "typ_p", "top_n_sigma"];
    let p = |k: &str| raw.get(k).copied();
    let greedy = order.contains(&"greedy") || p("temperature").is_some_and(|t| t <= 0.0);
    let active = |stage: &str| match stage {
        "penalties" => {
            p("repeat_last_n") != Some(0.0)
                && (p("repeat_penalty").is_some_and(|v| v != 1.0)
                    || p("frequency_penalty").is_some_and(|v| v != 0.0)
                    || p("presence_penalty").is_some_and(|v| v != 0.0))
        }
        "dry" => p("dry_multiplier").is_some_and(|v| v > 0.0),
        "top_k" => p("top_k").is_some_and(|v| v > 0.0),
        "top_p" => p("top_p").is_some_and(|v| v < 1.0),
        "min_p" => p("min_p").is_some_and(|v| v > 0.0),
        "typ_p" => p("typical_p").is_some_and(|v| v < 1.0),
        "xtc" => p("xtc_probability").is_some_and(|v| v > 0.0),
        "top_n_sigma" => p("top_n_sigma").is_some_and(|v| v > 0.0),
        "dist" => false,
        _ => true,
    };
    let mut stages: Vec<String> = order
        .iter()
        .filter(|s| {
            !(greedy && (TRUNCATION.contains(s) || **s == "temperature" || **s == "greedy"))
        })
        .filter(|s| active(s))
        .map(|s| s.to_string())
        .collect();
    if greedy {
        stages.push("greedy".into());
    }
    let owned_by = |key: &str| {
        STAGE_PARAMS
            .iter()
            .find(|(_, keys)| keys.contains(&key))
            .map(|(s, _)| *s)
    };
    let params = raw
        .iter()
        .filter(|(k, _)| match owned_by(k) {
            Some("temperature") => !greedy,
            Some(stage) => stages.iter().any(|s| s == stage),
            None => true,
        })
        .map(|(k, v)| (k.clone(), *v))
        .collect();
    (params, stages)
}

type Sink = Arc<Mutex<Vec<Observation>>>;

thread_local! {
    static SINK: RefCell<Option<Sink>> = const { RefCell::new(None) };
}

fn current() -> Option<Sink> {
    SINK.with(|s| s.borrow().clone())
}

/// Record an observation if a sink is installed. `make` runs only then.
pub fn observe(make: impl FnOnce() -> Observation) {
    if let Some(sink) = current() {
        let observation = make();
        tracing::debug!(target: "engine_observe", ?observation, "observed");
        sink.lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(observation);
    }
}

/// Whether a sink is installed, for a site whose observation is costly to
/// assemble.
pub fn is_observing() -> bool {
    current().is_some()
}

/// Run `f` and return its output with everything observed while it ran.
pub async fn observed<F: Future>(f: F) -> (F::Output, Vec<Observation>) {
    let sink: Sink = Arc::default();
    let output = Observed {
        sink: Some(sink.clone()),
        inner: Box::pin(f),
    }
    .await;
    let observations = std::mem::take(&mut *sink.lock().unwrap_or_else(|p| p.into_inner()));
    (output, observations)
}

/// Wrap a closure that will run on another thread so it records into the
/// sink installed where it was created.
pub fn carry<R>(f: impl FnOnce() -> R) -> impl FnOnce() -> R {
    let sink = current();
    move || {
        let _guard = Installed::new(sink);
        f()
    }
}

/// Wrap a future that will be spawned as its own task so it records into the
/// sink installed where it was created.
pub fn carry_future<F: Future>(f: F) -> impl Future<Output = F::Output> {
    Observed {
        sink: current(),
        inner: Box::pin(f),
    }
}

/// Polls `inner` with `sink` installed, restoring what was there after.
struct Observed<F: Future> {
    sink: Option<Sink>,
    inner: Pin<Box<F>>,
}

impl<F: Future> Future for Observed<F> {
    type Output = F::Output;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let _guard = Installed::new(self.sink.clone());
        self.inner.as_mut().poll(cx)
    }
}

/// Installs a sink for its lifetime and puts back the previous one on drop,
/// so nesting and panics leave the thread as they found it.
struct Installed {
    previous: Option<Sink>,
}

impl Installed {
    fn new(sink: Option<Sink>) -> Self {
        let previous = SINK.with(|s| std::mem::replace(&mut *s.borrow_mut(), sink));
        Self { previous }
    }
}

impl Drop for Installed {
    fn drop(&mut self) {
        let previous = self.previous.take();
        SINK.with(|s| *s.borrow_mut() = previous);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(n: i64) -> Observation {
        Observation::PromptTokens { ids: vec![n] }
    }

    fn block_on<F: Future>(f: F) -> F::Output {
        let waker = std::task::Waker::noop();
        let mut cx = Context::from_waker(waker);
        let mut f = Box::pin(f);
        loop {
            if let Poll::Ready(v) = f.as_mut().poll(&mut cx) {
                return v;
            }
        }
    }

    fn params(pairs: &[(&str, f64)]) -> BTreeMap<String, f64> {
        pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
    }

    #[test]
    fn a_greedy_sampler_keeps_only_what_moves_the_argmax() {
        // llama-server's defaults at temperature 0: nothing but the argmax.
        let server = canonical_sampler(
            &params(&[
                ("temperature", 0.0),
                ("top_k", 40.0),
                ("top_p", 0.95),
                ("min_p", 0.05),
                ("repeat_penalty", 1.0),
                ("repeat_last_n", 64.0),
                ("frequency_penalty", 0.0),
                ("presence_penalty", 0.0),
                ("dry_multiplier", 0.0),
            ]),
            &[
                "penalties",
                "dry",
                "top_n_sigma",
                "top_k",
                "typ_p",
                "top_p",
                "min_p",
                "xtc",
                "temperature",
            ],
        );
        assert_eq!(server, (BTreeMap::new(), vec!["greedy".to_string()]));
        // The engine's Templated chain at temperature 0 still runs DRY.
        let engine = canonical_sampler(
            &params(&[
                ("temperature", 0.0),
                ("dry_multiplier", 0.8),
                ("dry_base", 1.75),
                ("repeat_penalty", 1.0),
                ("presence_penalty", 0.0),
                ("frequency_penalty", 0.0),
                ("repeat_last_n", 128.0),
            ]),
            &["dry", "penalties", "greedy"],
        );
        assert_eq!(engine.1, vec!["dry".to_string(), "greedy".to_string()]);
        assert_eq!(engine.0.get("dry_multiplier"), Some(&0.8));
        assert!(
            !engine.0.contains_key("repeat_penalty"),
            "neutral penalties are dropped"
        );
        assert_ne!(server, engine);
    }

    #[test]
    fn a_sampling_chain_keeps_its_active_stages_in_order() {
        let (p, order) = canonical_sampler(
            &params(&[
                ("temperature", 0.7),
                ("top_k", 20.0),
                ("top_p", 1.0),
                ("min_p", 0.0),
                ("content_temperature", 0.0),
            ]),
            &[
                "penalties",
                "top_k",
                "top_p",
                "min_p",
                "temperature",
                "dist",
            ],
        );
        assert_eq!(order, vec!["top_k".to_string(), "temperature".to_string()]);
        assert_eq!(
            p,
            params(&[
                ("temperature", 0.7),
                ("top_k", 20.0),
                ("content_temperature", 0.0)
            ])
        );
    }

    #[test]
    fn nothing_is_recorded_without_a_sink() {
        let mut built = false;
        observe(|| {
            built = true;
            tokens(1)
        });
        assert!(!built, "the observation is not even built");
        assert!(!is_observing());
    }

    #[test]
    fn a_call_records_into_its_own_sink_and_leaves_the_thread_clean() {
        let ((), seen) = block_on(observed(async { observe(|| tokens(1)) }));
        assert_eq!(seen, vec![tokens(1)]);
        assert!(!is_observing(), "the sink is gone after the call");
    }

    #[test]
    fn carried_work_on_another_thread_records_into_the_same_sink() {
        let ((), seen) = block_on(observed(async {
            let work = carry(|| observe(|| tokens(2)));
            std::thread::spawn(work).join().unwrap();
            observe(|| tokens(3));
        }));
        assert_eq!(seen, vec![tokens(2), tokens(3)]);
    }

    #[test]
    fn a_carried_future_records_into_the_sink_it_was_made_under() {
        let (spawned, seen) = block_on(observed(async {
            carry_future(async { observe(|| tokens(4)) })
        }));
        // Polled after the call returned, as a spawned task would be.
        block_on(spawned);
        assert_eq!(seen, Vec::<Observation>::new(), "taken before the task ran");
        let ((), seen) = block_on(observed(async {
            let spawned = carry_future(async { observe(|| tokens(5)) });
            spawned.await;
        }));
        assert_eq!(seen, vec![tokens(5)]);
    }

    #[test]
    fn nested_calls_keep_their_observations_apart() {
        let (inner, outer) = block_on(observed(async {
            observe(|| tokens(1));
            let ((), inner) = observed(async { observe(|| tokens(2)) }).await;
            observe(|| tokens(3));
            inner
        }));
        assert_eq!(inner, vec![tokens(2)]);
        assert_eq!(outer, vec![tokens(1), tokens(3)]);
    }
}
