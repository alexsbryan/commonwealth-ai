// SPDX-License-Identifier: AGPL-3.0-or-later
//! Host detection — what the endpoint at `--base-url` can do, DISCOVERED.
//!
//! `docs/CODE_TOOLING_BOUNDARY.md` §5.3: capability is detected, never
//! configured (rule 1); every degradation is printed (rule 2); a host that
//! only works against our own daemon is a bug (rule 3). The baseline any
//! frontend must satisfy is `POST /v1/embeddings` + `GET /v1/models`; an OICP
//! host additionally answers `GET /oicp/v1/capabilities`, and that is the only
//! thing the probe treats differently — it says so.
//!
//! ## Discovery — WHICH endpoint, when nobody said (order ei-6-distribution)
//!
//! `EPISTEMIC_INDEX.md` §4's three commands carry no URL: `corpus ingest
//! my-coins.toml` and `corpus serve --corpus my-coins` are what the
//! barely-technical person types. So when no endpoint is named, [`discover`]
//! walks a fixed ladder — Ollama's `:11434`, llama-server's `:8080`, then this
//! host's own OICP daemon — and every rung is PROBED and NAMED, whichever way
//! it goes (ARCH §9, §18.3). The findings are carried into [`HostProfile`] so
//! `corpus_list` can report them too: a person whose Ollama is stopped should
//! learn that from the tool output, not from an empty result.
//!
//! Two rules the ladder holds:
//!
//! - **An endpoint the caller NAMED is never substituted.** `--base-url` that
//!   does not answer is a refusal carrying that URL's own finding — falling
//!   through to Ollama would silently serve a different model than the one
//!   asked for (§18.3).
//! - **Discovery picks a FRONTEND, the probe validates it.** A rung is taken
//!   on `GET <v1>/models` alone; the embedding round-trip runs once, on the
//!   winner. Probing every rung fully would embed against hosts we discard,
//!   and falling through on "this one cannot embed" would substitute silently
//!   again. A winner that cannot embed is a named refusal listing what every
//!   rung said.

use anyhow::{bail, Context, Result};
use serde_json::Value;

/// What was learned about the endpoint before serving anything.
#[derive(Debug, Clone)]
pub struct HostProfile {
    pub base_url: String,
    pub embeddings_url: String,
    pub embed_model: String,
    /// Dimensionality of one probe embedding — checked against every served
    /// index so a mismatch is announced at boot, not discovered as an empty
    /// vector leg on the first query (§18.4: validate the instrument first).
    pub embed_dims: usize,
    pub kind: HostKind,
    /// Every endpoint candidate tried before this one was chosen, in ladder
    /// order, each with the sentence that decided it. One entry (the named
    /// URL) when the caller passed `--base-url`. Carried rather than only
    /// printed because `corpus_list` reports it: ARCH §18.3's "absence is
    /// reported" is not satisfied by a line that scrolled past at boot.
    pub attempts: Vec<Attempt>,
}

/// One rung of the discovery ladder, and what probing it found.
#[derive(Debug, Clone)]
pub struct Attempt {
    /// Where this candidate came from — `--base-url`, `ollama`,
    /// `llama-server`, `oicp daemon`. The name a person can act on.
    pub label: &'static str,
    pub url: String,
    pub outcome: Outcome,
}

/// What happened at one rung. Three states and not a bool, because the third
/// one is real: a rung BELOW the winner is never probed, and rendering that as
/// "unavailable" would report a failure that did not happen (ARCH §18.3, and
/// the eleven #9 — a closed set is an enum).
#[derive(Debug, Clone)]
pub enum Outcome {
    /// Probed, answered, and this is the endpoint in use.
    Chosen(String),
    /// Probed and did not answer. The sentence says why.
    Unavailable(String),
    /// Not probed: an earlier rung already won. Reported rather than omitted,
    /// because a gap in the ladder reads as a rung that failed.
    NotProbed,
}

impl Attempt {
    /// Did this rung answer? `false` for both the unreachable and the
    /// unprobed, which is why it is not the whole story and why
    /// [`Attempt::finding`] is printed beside it.
    pub fn reachable(&self) -> bool {
        matches!(self.outcome, Outcome::Chosen(_))
    }

    /// The one sentence a person reads for this rung.
    pub fn finding(&self) -> &str {
        match &self.outcome {
            Outcome::Chosen(what) | Outcome::Unavailable(what) => what,
            Outcome::NotProbed => "not probed — an earlier candidate answered",
        }
    }

    /// `<label> <url> — <finding>`, with the failures marked. One renderer,
    /// used by the boot line, the refusal text and `corpus_list` (§10.6).
    pub fn line(&self) -> String {
        format!(
            "{} {} — {}{}",
            self.label,
            self.url,
            match self.outcome {
                Outcome::Unavailable(_) => "unavailable: ",
                _ => "",
            },
            self.finding()
        )
    }
}

#[derive(Debug, Clone)]
pub enum HostKind {
    /// `GET <root>/oicp/v1/capabilities` answered 200.
    Oicp { capabilities: Value },
    /// It did not (404, connection refused, non-JSON) — the OpenAI-compatible
    /// baseline, which is the case the whole binary exists for.
    Baseline { reason: String },
}

impl HostKind {
    pub fn label(&self) -> &'static str {
        match self {
            HostKind::Oicp { .. } => "oicp",
            HostKind::Baseline { .. } => "baseline (OpenAI-compatible)",
        }
    }
}

/// Split a user-supplied endpoint into the two shapes every probe needs: the
/// `/v1` base a request path hangs off, and the ROOT the OICP capability
/// probe lives at. Accepts either form, because both are things a person
/// types: `http://localhost:8080` and `http://localhost:8080/v1` name the
/// same server.
pub fn split_base(base_url: &str) -> (String, String) {
    let base = base_url.trim_end_matches('/').to_string();
    match base.strip_suffix("/v1") {
        Some(root) => (base.clone(), root.to_string()),
        None => (format!("{base}/v1"), base),
    }
}

/// How long the discovery ladder waits on ONE rung. Deliberately short: this
/// budget is paid up to three times before anything happens, and the question
/// it answers is only "is a frontend listening here", which a local
/// `GET /v1/models` answers in milliseconds. Without a bound at all — which is
/// reqwest's default — a host that accepts the connection and then says
/// nothing hangs `corpus serve` forever, with no output at all.
const DISCOVERY_TIMEOUT_SECS: u64 = 3;

/// How long a real request waits. Generous, because the request under it is
/// an EMBEDDING, and the first one an endpoint serves may include loading the
/// model — tens of seconds on a cold Ollama. Bounding that at the discovery
/// budget would refuse a setup that works, which is the §18.3 failure in the
/// other direction.
const REQUEST_TIMEOUT_SECS: u64 = 120;

/// The ONE `reqwest::Client` construction in this binary (ARCH §10.6; the F26
/// egress census counts construction sites per file). Every probe in this
/// crate — capability, `/v1/models`, the embedding round-trip, `corpus
/// ingest`'s chat probe, and the discovery ladder — comes through here.
///
/// The timeout is a PARAMETER and not a second constructor because "is
/// anything there" and "did the work finish" are two questions with two right
/// answers, and answering them from one site is what keeps them from becoming
/// two clients with two independently-drifting budgets.
pub fn client_with_timeout(secs: u64) -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(secs))
        .build()
        // The builder fails only when the TLS backend cannot initialise, which
        // is not a condition a local-endpoint probe can do anything about.
        .unwrap_or_else(|e| panic!("corpus-mcp: cannot build an HTTP client: {e}"))
}

/// The client for doing the work, at [`REQUEST_TIMEOUT_SECS`].
pub fn client() -> reqwest::Client {
    client_with_timeout(REQUEST_TIMEOUT_SECS)
}

/// Ollama's OpenAI-compatible surface. One process serves chat AND embeddings,
/// which is why `EPISTEMIC_INDEX.md` §4 names it the default shape.
const OLLAMA_URL: &str = "http://localhost:11434/v1";
/// llama-server's default port. One model per process, so a host found here
/// serves EITHER chat or embeddings — `corpus ingest` against it wants the
/// explicit `--chat-url` / `--embed-url` pair, and says so when a probe fails.
const LLAMA_SERVER_URL: &str = "http://localhost:8080/v1";

/// The ladder, in order. `explicit` short-circuits it to a single rung: an
/// endpoint the caller named is the only one considered, so a failure there is
/// a refusal rather than a fallback (§18.3).
fn candidates(explicit: Option<&str>) -> Vec<(&'static str, String)> {
    if let Some(url) = explicit {
        return vec![("--base-url", url.to_string())];
    }
    vec![
        ("ollama", OLLAMA_URL.to_string()),
        ("llama-server", LLAMA_SERVER_URL.to_string()),
        // This host's own daemon, through the ONE accessor that resolves it
        // (`SOVEREIGN_DAEMON_URL` > `SVRNMESH_DAEMON_URL` > `[daemon]
        // client_port` > compiled default). Last rung deliberately: the whole
        // point of this binary is that it does NOT need our daemon, so the
        // daemon is what it falls back TO, never what it assumes.
        (
            "oicp daemon",
            sovereign_contracts::setup_config::client_daemon_base(),
        ),
    ]
}

/// Is an OpenAI-compatible frontend answering here? `GET <v1>/models` with a
/// listed model id is the whole test — it is the cheapest thing that
/// distinguishes "a frontend is up" from "the port is open".
async fn probe_models(client: &reqwest::Client, v1: &str) -> std::result::Result<String, String> {
    let url = format!("{v1}/models");
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("GET {url}: {e}"))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(format!("GET {url} returned {status}"));
    }
    let body: Value = resp
        .json()
        .await
        .map_err(|e| format!("GET {url} answered {status} but not JSON ({e})"))?;
    let ids: Vec<&str> = body["data"]
        .as_array()
        .map(|d| d.iter().filter_map(|m| m["id"].as_str()).collect())
        .unwrap_or_default();
    match ids.first() {
        Some(first) => Ok(format!("{} model(s) listed, first `{first}`", ids.len())),
        None => Err(format!("GET {url} lists no models")),
    }
}

/// Walk the ladder. Returns the chosen endpoint and EVERY rung's finding, in
/// order — including the ones after the winner, which are not probed and are
/// recorded as such rather than omitted (a gap in the list would read as a
/// rung that failed).
///
/// Traced at debug as well as printed: a decision invisible at
/// `tracing=debug` is not finished (ARCH §9).
pub async fn discover(explicit: Option<&str>) -> Result<(String, Vec<Attempt>)> {
    let client = client_with_timeout(DISCOVERY_TIMEOUT_SECS);
    let ladder = candidates(explicit);
    let mut attempts: Vec<Attempt> = Vec::new();
    let mut chosen: Option<String> = None;
    for (label, url) in ladder {
        if chosen.is_some() {
            attempts.push(Attempt {
                label,
                url,
                outcome: Outcome::NotProbed,
            });
            continue;
        }
        let (v1, _) = split_base(&url);
        let outcome = match probe_models(&client, &v1).await {
            Ok(what) => Outcome::Chosen(what),
            Err(why) => Outcome::Unavailable(why),
        };
        let attempt = Attempt {
            label,
            url,
            outcome,
        };
        tracing::debug!(
            candidate = attempt.label,
            url = %attempt.url,
            reachable = attempt.reachable(),
            finding = %attempt.finding(),
            "corpus-mcp: endpoint discovery"
        );
        eprintln!("corpus-mcp: endpoint candidate {}", attempt.line());
        if attempt.reachable() {
            chosen = Some(attempt.url.clone());
        }
        attempts.push(attempt);
    }
    match chosen {
        Some(url) => Ok((url, attempts)),
        None if explicit.is_some() => {
            // Named, and it did not answer. The finding, not a fallback.
            let a = &attempts[0];
            bail!("--base-url {} did not answer: {}", a.url, a.finding())
        }
        None => bail!(
            "no inference endpoint found. Tried, in order:\n{}\nStart one (`ollama \
             serve`, or `llama-server -m <model.gguf> --embeddings --port 8080`), or \
             name yours with --base-url <url>.",
            attempts
                .iter()
                .map(|a| format!("  {}", a.line()))
                .collect::<Vec<_>>()
                .join("\n")
        ),
    }
}

/// Discovery THEN the full probe — the one entry point `corpus serve` and
/// `corpus ingest` both use, so the two verbs cannot disagree about which
/// endpoint they found (ARCH §10.6).
pub async fn discover_and_probe(
    explicit: Option<&str>,
    embed_model: Option<String>,
) -> Result<HostProfile> {
    let (url, attempts) = discover(explicit).await?;
    let mut profile = probe(&url, embed_model).await?;
    profile.attempts = attempts;
    Ok(profile)
}

/// `GET <root>/oicp/v1/capabilities` — capability DETECTED, never configured
/// (`docs/CODE_TOOLING_BOUNDARY.md` §5.3 rule 1), and the result printed
/// whichever way it goes.
///
/// Its own function since order ei-5b-build-verb: `corpus ingest` probes TWO
/// endpoints (chat and embeddings, one URL apart under llama-server) and both
/// answer this question the same way. One implementation of it, not two
/// (ARCH §10.6).
pub async fn probe_capability(client: &reqwest::Client, root: &str, label: &str) -> HostKind {
    let cap_url = format!("{root}/oicp/v1/capabilities");
    let kind = match client.get(&cap_url).send().await {
        Ok(r) if r.status().is_success() => match r.json::<Value>().await {
            Ok(capabilities) => HostKind::Oicp { capabilities },
            Err(e) => HostKind::Baseline {
                reason: format!("{cap_url} answered 200 but not JSON ({e})"),
            },
        },
        Ok(r) => HostKind::Baseline {
            reason: format!("{cap_url} returned {}", r.status()),
        },
        Err(e) => HostKind::Baseline {
            reason: format!("{cap_url}: {e}"),
        },
    };
    match &kind {
        HostKind::Oicp { capabilities } => eprintln!(
            "corpus-mcp: {label} {root}: OICP capabilities detected ({} top-level keys)",
            capabilities.as_object().map(|o| o.len()).unwrap_or(0)
        ),
        HostKind::Baseline { reason } => {
            eprintln!("corpus-mcp: {label} {root}: baseline OpenAI-compatible path — {reason}")
        }
    }
    kind
}

pub async fn probe(base_url: &str, embed_model: Option<String>) -> Result<HostProfile> {
    let (base, root) = split_base(base_url);
    let client = client();

    // 1. Capability — detected, never configured.
    let kind = probe_capability(&client, &root, "host").await;

    // 2. The embedding model id — from the flag, else from what the host
    //    says it serves. Absence is refused, never defaulted (§18.3).
    let models_url = format!("{base}/models");
    let embed_model = match embed_model {
        Some(m) => m,
        None => {
            let listed = client
                .get(&models_url)
                .send()
                .await
                .with_context(|| format!("GET {models_url}"))?
                .json::<Value>()
                .await
                .with_context(|| format!("GET {models_url}: not JSON"))?;
            let first = listed["data"]
                .as_array()
                .and_then(|d| d.first())
                .and_then(|m| m["id"].as_str())
                .map(str::to_string);
            match first {
                Some(id) => id,
                None => bail!(
                    "{models_url} lists no models, so there is no embedding model id to \
                     send; pass --embed-model <id>"
                ),
            }
        }
    };

    // 3. One probe embedding: proves the endpoint works and learns its width.
    let embeddings_url = format!("{base}/embeddings");
    let resp = client
        .post(&embeddings_url)
        .json(&serde_json::json!({ "input": "corpus-mcp probe", "model": embed_model }))
        .send()
        .await
        .with_context(|| format!("POST {embeddings_url}"))?;
    let status = resp.status();
    let body: Value = resp
        .json()
        .await
        .with_context(|| format!("POST {embeddings_url}: {status}, body not JSON"))?;
    if !status.is_success() {
        bail!("POST {embeddings_url} returned {status}: {body}");
    }
    let embed_dims = body["data"][0]["embedding"]
        .as_array()
        .map(|a| a.len())
        .with_context(|| format!("POST {embeddings_url}: no data[0].embedding in {body}"))?;
    if embed_dims == 0 {
        bail!("POST {embeddings_url} returned an empty embedding for model `{embed_model}`");
    }
    eprintln!(
        "corpus-mcp: embeddings via {embeddings_url}, model `{embed_model}`, {embed_dims}-d ({})",
        kind.label()
    );

    Ok(HostProfile {
        base_url: base,
        embeddings_url,
        embed_model,
        embed_dims,
        kind,
        // `probe` alone knows nothing about a ladder; `discover_and_probe`
        // fills this in. Empty means "this endpoint was handed to us", which
        // is exactly what `corpus_list` should say about it.
        attempts: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ladder's ORDER is the decision, and it is testable without a
    /// socket. `discover` itself is not unit-testable — its third rung is the
    /// developer's own running daemon, so the result would depend on what
    /// happens to be up; that half is proven in `tests/verbs.rs` against a
    /// dead port, in a subprocess whose env can be controlled.
    #[test]
    fn the_ladder_is_ollama_then_llama_server_then_the_daemon() {
        let rungs = candidates(None);
        let labels: Vec<&str> = rungs.iter().map(|(l, _)| *l).collect();
        assert_eq!(labels, ["ollama", "llama-server", "oicp daemon"]);
        assert_eq!(rungs[0].1, OLLAMA_URL);
        assert_eq!(rungs[1].1, LLAMA_SERVER_URL);
    }

    /// ARCH §18.3, as a shape rather than a message: an endpoint the caller
    /// NAMED is the ONLY candidate. If a rung were ever appended after it,
    /// a dead `--base-url` would silently fall through to whatever else
    /// answers — which is the substitution this whole ladder must not make.
    #[test]
    fn a_named_endpoint_is_the_only_candidate() {
        let rungs = candidates(Some("http://named:9/v1"));
        assert_eq!(
            rungs.len(),
            1,
            "the ladder grew a rung past a named endpoint"
        );
        assert_eq!(rungs[0].0, "--base-url");
        assert_eq!(rungs[0].1, "http://named:9/v1");
    }

    /// A rung below the winner is NOT PROBED, and that is a third state — not
    /// "unavailable". Rendering it as a failure would report something that
    /// never happened (ARCH §18.3).
    #[test]
    fn an_unprobed_rung_does_not_render_as_unavailable() {
        let a = Attempt {
            label: "llama-server",
            url: LLAMA_SERVER_URL.to_string(),
            outcome: Outcome::NotProbed,
        };
        assert!(!a.reachable());
        assert!(!a.line().contains("unavailable"), "{}", a.line());
        assert!(a.line().contains("not probed"), "{}", a.line());

        let bad = Attempt {
            label: "ollama",
            url: OLLAMA_URL.to_string(),
            outcome: Outcome::Unavailable("connection refused".into()),
        };
        assert!(
            bad.line().contains("unavailable: connection refused"),
            "{}",
            bad.line()
        );
    }
}
