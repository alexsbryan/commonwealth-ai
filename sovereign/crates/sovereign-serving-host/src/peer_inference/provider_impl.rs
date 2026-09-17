// SPDX-License-Identifier: AGPL-3.0-or-later
//! `InferenceProvider` for `InferenceRouter` — the two entry points
//! (`complete`, `stream_complete`) and the thin adapters they share.
//! Split out of `peer_inference.rs` at REVIEW-audit-8 (ARCH §3.1): the
//! parent crossed its frozen 1200-line slack when the router gained its
//! builder. `use super::*` keeps every private helper and import in scope,
//! and an impl block has no module path, so no caller changes.

use super::*;

#[async_trait]
impl InferenceProvider for InferenceRouter {
    /// Non-streaming completion, driven by the SAME `select_route`
    /// cascade the two streaming entry points use.
    ///
    /// Until 2026-08-07 this method resolved its own route inline: its
    /// own shared-primary rewrite, its own named dispatch, its own
    /// single-peer ranked pick. That is why FOUR separate features —
    /// the forward budget, the privacy gate, the outcome join, and the
    /// `LocalAlternative` fallback — each had to be written twice to
    /// reach both surfaces, the last of them as a same-day regression
    /// fix. The routing DECISION now has exactly one implementation.
    /// What remains per-method is only how a step's terminus is built,
    /// which is genuinely different: a response here, a stream there.
    async fn complete(&self, request: &CompletionRequest) -> Result<CompletionResponse> {
        let RoutePlan {
            steps,
            decision_id,
            oicp_request_id,
        } = self.select_route(request).await?;
        let mut failovers: Vec<decision_log::FailoverAttempt> = Vec::new();
        let mut last_err: Option<sovereign_contracts::error::Error> = None;

        for (attempt_index, step) in steps.into_iter().enumerate() {
            let attempt_index = attempt_index as u32;
            match step {
                RouteDecision::Lender { lender, model_id } => {
                    // The real model id on the wire, and NO `X-Node-Id`. Both
                    // differ from `provider_for_peer` deliberately — see the
                    // `crate::guest_lender` module docs. A placeholder id
                    // resolves to nobody on the lender AND cannot satisfy its
                    // scope check, which matches on the model NAME; a node
                    // stamp would run the lender's PEER admission on something
                    // that is not a peer, and mis-attribute it in their tally.
                    let rp = RemoteApiProvider::with_client_and_bearer(
                        &lender.base_url,
                        // The wrapper's OWN pooled client, not a fresh one per
                        // request: a new `reqwest::Client` builds a new
                        // connection pool and throws away keep-alive, which on
                        // a tunnelled route costs a full QUIC round-trip every
                        // turn. It is also two fewer egress construction sites
                        // for the F26 census to have to reason about.
                        self.http.clone(),
                        lender.bearer.clone(),
                        &model_id,
                        LENDER_CONTEXT,
                    );
                    let serve_request = pinned_request(request, Some(model_id.as_str()));
                    match rp.complete(serve_request.as_ref()).await {
                        Ok(mut resp) => {
                            resp.model_id = model_id.clone();
                            return Ok(resp);
                        }
                        Err(e) => {
                            // Any refusal invalidates the cached scope: the
                            // grant may have expired, been revoked, or died
                            // with the lender's process (its store is RAM
                            // only). Re-resolving is cheap; continuing to
                            // claim the model is reachable is not.
                            self.guest_source().invalidate().await;
                            // No failover, and no local substitute. The caller
                            // named a model this node does not have and
                            // explicitly borrowed; serving something else is
                            // the substitution this surface exists to refuse.
                            return Err(sovereign_contracts::error::Error::Routing(format!(
                                "the guest link for {} could not serve '{}': {}. The grant \
                                 may have expired, been revoked, or the lending node may \
                                 have restarted (grants are held in memory). Nothing was \
                                 served in its place.",
                                lender.display, model_id, e
                            )));
                        }
                    }
                }
                RouteDecision::LocalNamed { attribution, guard } => {
                    return self
                        .complete_named_locally(
                            request,
                            &attribution,
                            guard,
                            &decision_id,
                            &oicp_request_id,
                            attempt_index,
                            &failovers,
                        )
                        .await;
                }
                RouteDecision::Peer {
                    peer,
                    peer_cand,
                    disposition,
                    pinned_model_id,
                } => {
                    let serve_request = pinned_request(request, pinned_model_id.as_deref());
                    let serve_request = serve_request.as_ref();
                    // The contribution ledger is emitted from the STREAM
                    // wrapper's lifecycle and has never had a non-streaming
                    // equivalent. Left off deliberately rather than quietly
                    // switched on: emitting here would start booking
                    // contribution for traffic that has never been booked,
                    // which is a ledger-visible change and belongs in its
                    // own commit with its own note.

                    // Raise the peer's observed in-flight count BEFORE
                    // handing off. `locate_named_model`'s load-balance rule
                    // and the ranked scorer both read it, so without this
                    // every concurrent caller sees `peer_inflight = 0` and
                    // floods one peer. The named branch did this; the ranked
                    // branch did not. Uniform now — see the commit note.
                    self.record_dispatch(Some(&peer.name)).await;
                    let started = Instant::now();
                    let mut last_transport_err: Option<String> = None;
                    let node_id_hex = self.local_node_id_hex().await;
                    let transport = self.pinned_transports().resolve(&peer.node_id).await;
                    for url in &peer.base_urls {
                        let rp = provider_for_peer(
                            &peer,
                            url,
                            transport.as_ref(),
                            node_id_hex.as_deref(),
                        );
                        match rp.complete(serve_request).await {
                            Ok(mut resp) => {
                                // Prefer the peer's OICP-advertised model id
                                // over whatever label the wire response
                                // carried — the advertised id is what the
                                // selector actually scored, so attribution
                                // should match it. (Some backends echo a
                                // request hint instead of the served model.)
                                resp.model_id = peer_cand.model_id.clone();
                                self.peer_health.record_success(&peer.name);
                                self.record_success(Some(&peer.name)).await;
                                self.outcome_ctx(
                                    &decision_id,
                                    &oicp_request_id,
                                    ServedBy::Peer {
                                        name: peer.name.clone(),
                                        node_id: Some(peer.node_id.to_hex()),
                                        model_id: peer_cand.model_id.clone(),
                                    },
                                    attempt_index,
                                    &failovers,
                                )
                                .complete(
                                    None,
                                    Some(started.elapsed().as_secs_f64() * 1000.0),
                                    None,
                                    recorder::now_unix_ms(),
                                );
                                return Ok(Self::annotate(resp, &peer.name));
                            }
                            Err(e) => {
                                tracing::info!(
                                    peer = %peer.name,
                                    url = %url,
                                    error = %e,
                                    "mesh-inference: peer complete() transport error, \
                                     trying next address"
                                );
                                last_transport_err = Some(format!("{e}"));
                            }
                        }
                    }
                    // Every address for this peer failed. One failure per
                    // PEER, not per address — a peer is unreachable as a
                    // unit.
                    let err_text = last_transport_err.unwrap_or_else(|| "unreachable".into());
                    let shed = decision_log::looks_shed(&err_text);
                    self.book_peer_failure(&peer.name, &err_text, shed);
                    self.record_failure(Some(&peer.name)).await;
                    failovers.push(decision_log::FailoverAttempt {
                        peer: peer.name.clone(),
                        error: err_text.clone(),
                        shed,
                        yield_retry_after_secs: decision_log::parse_yield_refusal(&err_text),
                    });
                    match disposition {
                        VenueFailureDisposition::Hard { model_id } => {
                            // Terminal: the peer is the only holder, so no
                            // later step can serve the name that was asked
                            // for. This is where the join closes.
                            self.outcome_ctx(
                                &decision_id,
                                &oicp_request_id,
                                ServedBy::Failed,
                                attempt_index,
                                &failovers,
                            )
                            .failed(
                                err_text.clone(),
                                shed,
                                recorder::now_unix_ms(),
                            );
                            return Err(sovereign_contracts::error::Error::Routing(format!(
                                "model '{}' is advertised by peer '{}' but all peer \
                                 addresses failed: {}",
                                model_id, peer.name, err_text
                            )));
                        }
                        VenueFailureDisposition::Soft => {
                            tracing::info!(
                                peer = %peer.name,
                                shed,
                                "mesh-inference: peer step failed, continuing the cascade"
                            );
                            last_err =
                                Some(sovereign_contracts::error::Error::Routing(err_text.clone()));
                        }
                    }
                }
                RouteDecision::LocalFallback { total } => {
                    // `total` is the eagerly-entered gossip counter: this
                    // node is about to produce load and peers must see it.
                    let _total = total;
                    let started = Instant::now();
                    let result = self.local.complete(request).await;
                    // `failovers` is what distinguishes the two flows that
                    // arrive here: the selector chose nobody (a `stay_local`
                    // decision — still a decision, empty list), or peers were
                    // tried and failed. `ttft_ms` is None by construction —
                    // there is no stream to time a first token against, and
                    // reading one off a non-streaming call would be a
                    // fabrication.
                    let ctx = self.outcome_ctx(
                        &decision_id,
                        &oicp_request_id,
                        ServedBy::LocalFallback {
                            model_id: self.local.model_id_for(request.preferred_speed),
                        },
                        attempt_index,
                        &failovers,
                    );
                    match &result {
                        Ok(_) => {
                            ctx.complete(
                                None,
                                Some(started.elapsed().as_secs_f64() * 1000.0),
                                None,
                                recorder::now_unix_ms(),
                            );
                        }
                        Err(e) => ctx.failed(e.to_string(), false, recorder::now_unix_ms()),
                    }
                    return result;
                }
            }
        }

        // Only reachable if a plan ended without a serving step, which
        // `select_route` does not currently produce. Reported rather
        // than unwrapped so a future plan shape cannot panic here.
        Err(last_err.unwrap_or_else(|| {
            sovereign_contracts::error::Error::Routing(
                "the route plan ended with no step able to serve".into(),
            )
        }))
    }

    async fn complete_stream(
        &self,
        request: &CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<String>> + Send>>> {
        // Kept for trait object compatibility; the richer
        // `complete_stream_with_id` is what the runtime actually
        // calls and what carries the peer attribution back out.
        // Delegate here so any caller using the legacy shape still
        // gets the same routing behaviour, just without the
        // attribution string.
        Ok(self.complete_stream_with_id(request).await?.0)
    }

    /// Streaming + attribution in one call, on the legacy
    /// `Result<String>` shape.
    ///
    /// The cascade itself lives in
    /// [`complete_stream_with_id_and_finish`][Self::complete_stream_with_id_and_finish].
    /// Until noun-convergence rung nc-17 this method carried a second
    /// copy of it — ~197 lines, identical step for step, differing only
    /// in the stream item type and in two log strings that had already
    /// drifted apart. Delegating keeps one cascade, so a routing change
    /// can no longer land on one shape and not the other.
    ///
    /// Adapting typed frames back down is lossless here: every terminus
    /// the typed cascade reaches is the same one this shape reached,
    /// because no remote provider overrides
    /// `complete_stream_with_finish` — they inherit the trait default,
    /// which is `complete_stream` plus a synthesised terminal frame.
    async fn complete_stream_with_id(
        &self,
        request: &CompletionRequest,
    ) -> Result<(Pin<Box<dyn Stream<Item = Result<String>> + Send>>, String)> {
        let (frames, model_id) = self.complete_stream_with_id_and_finish(request).await?;
        Ok((
            sovereign_contracts::traits::frames_to_text_stream(frames),
            model_id,
        ))
    }

    /// THE mesh route cascade. Cascade shape comes from
    /// `select_route`; each `RouteDecision` step constructs its stream
    /// via `self.local.complete_stream_with_finish` /
    /// `rp.complete_stream_with_finish`, propagating typed
    /// `StreamFrame::Finish { reason, usage }` all the way to the
    /// runtime so cutoff truncation lights up the desktop chip with
    /// the real reason (not the prior chars-per-token heuristic).
    ///
    /// Every other streaming entry on this provider is a thin adapter
    /// over this one: `complete_stream_with_id` drops to the legacy
    /// item type, `complete_stream_with_finish` drops the attribution
    /// string, and `complete_stream` drops both.
    async fn complete_stream_with_id_and_finish(
        &self,
        request: &CompletionRequest,
    ) -> Result<(
        Pin<Box<dyn Stream<Item = sovereign_contracts::types::StreamFrame> + Send>>,
        String,
    )> {
        use sovereign_contracts::types::StreamFrame;
        let RoutePlan {
            steps,
            decision_id,
            oicp_request_id,
        } = self.select_route(request).await?;
        let mut last_err: Option<sovereign_contracts::error::Error> = None;
        let mut failovers: Vec<decision_log::FailoverAttempt> = Vec::new();
        for (attempt_index, step) in steps.into_iter().enumerate() {
            let attempt_index = attempt_index as u32;
            match step {
                RouteDecision::Lender { lender, model_id } => {
                    // The real model id on the wire, and NO `X-Node-Id`. Both
                    // differ from `provider_for_peer` deliberately — see the
                    // `crate::guest_lender` module docs. A placeholder id
                    // resolves to nobody on the lender AND cannot satisfy its
                    // scope check, which matches on the model NAME; a node
                    // stamp would run the lender's PEER admission on something
                    // that is not a peer, and mis-attribute it in their tally.
                    let rp = RemoteApiProvider::with_client_and_bearer(
                        &lender.base_url,
                        // The wrapper's OWN pooled client, not a fresh one per
                        // request: a new `reqwest::Client` builds a new
                        // connection pool and throws away keep-alive, which on
                        // a tunnelled route costs a full QUIC round-trip every
                        // turn. It is also two fewer egress construction sites
                        // for the F26 census to have to reason about.
                        self.http.clone(),
                        lender.bearer.clone(),
                        &model_id,
                        LENDER_CONTEXT,
                    );
                    let serve_request = pinned_request(request, Some(model_id.as_str()));
                    match rp.complete_stream_with_finish(serve_request.as_ref()).await {
                        Ok(stream) => return Ok((stream, model_id)),
                        Err(e) => {
                            // Any refusal invalidates the cached scope: the
                            // grant may have expired, been revoked, or died
                            // with the lender's process (its store is RAM
                            // only). Re-resolving is cheap; continuing to
                            // claim the model is reachable is not.
                            self.guest_source().invalidate().await;
                            // No failover, and no local substitute. The caller
                            // named a model this node does not have and
                            // explicitly borrowed; serving something else is
                            // the substitution this surface exists to refuse.
                            return Err(sovereign_contracts::error::Error::Routing(format!(
                                "the guest link for {} could not serve '{}': {}. The grant \
                                 may have expired, been revoked, or the lending node may \
                                 have restarted (grants are held in memory). Nothing was \
                                 served in its place.",
                                lender.display, model_id, e
                            )));
                        }
                    }
                }
                RouteDecision::LocalNamed { attribution, guard } => {
                    let stream = self.local.complete_stream_with_finish(request).await?;
                    let observed: Pin<Box<dyn Stream<Item = StreamFrame> + Send>> =
                        Box::pin(InflightGuardedStream::new(
                            ThroughputObservedStream::new(
                                stream,
                                ThroughputTarget::Local(Arc::clone(&self.local_observations)),
                            )
                            .with_outcome(self.outcome_ctx(
                                &decision_id,
                                &oicp_request_id,
                                ServedBy::Local {
                                    model_id: attribution.clone(),
                                },
                                attempt_index,
                                &failovers,
                            )),
                            guard,
                        ));
                    return Ok((observed, attribution));
                }
                RouteDecision::Peer {
                    peer,
                    peer_cand,
                    disposition,
                    pinned_model_id,
                } => {
                    let serve_request = pinned_request(request, pinned_model_id.as_deref());
                    let serve_request = serve_request.as_ref();
                    let mut last_transport_err: Option<String> = None;
                    let node_id_hex = self.local_node_id_hex().await;
                    let transport = self.pinned_transports().resolve(&peer.node_id).await;
                    // The ledger port this venue's completion may emit
                    // through; `None` for a pinned pod (spec §8).
                    let ledger_emitter = ledger_emitter_for_venue(&self.host, &peer).await;
                    for url in &peer.base_urls {
                        let rp = provider_for_peer(
                            &peer,
                            url,
                            transport.as_ref(),
                            node_id_hex.as_deref(),
                        );
                        match rp.complete_stream_with_finish(serve_request).await {
                            Ok(stream) => {
                                let attribution =
                                    format!("{} @ peer {}", peer_cand.model_id, peer.name);
                                let mut wrapper = ThroughputObservedStream::new(
                                    stream,
                                    ThroughputTarget::Peer {
                                        name: peer.name.clone(),
                                        map: Arc::clone(&self.peer_observations),
                                    },
                                )
                                .with_outcome(self.outcome_ctx(
                                    &decision_id,
                                    &oicp_request_id,
                                    ServedBy::Peer {
                                        name: peer.name.clone(),
                                        node_id: Some(peer.node_id.to_hex()),
                                        model_id: peer_cand.model_id.clone(),
                                    },
                                    attempt_index,
                                    &failovers,
                                ));
                                if let Some(em) = ledger_emitter.clone() {
                                    wrapper = wrapper.with_ledger(em);
                                }
                                let observed: Pin<Box<dyn Stream<Item = StreamFrame> + Send>> =
                                    Box::pin(wrapper);
                                self.peer_health.record_success(&peer.name);
                                // INVARIANT (no double-emit): once we return the
                                // peer's LIVE stream here, the routing cascade is
                                // over. A failure that surfaces *mid-stream* (the
                                // peer dies after ≥1 token) must NOT re-enter the
                                // cascade or restart locally — the client would
                                // then see duplicated / garbled output. The
                                // ranked failover below only retries from the
                                // pre-`Ok` `Err` arm (connect / headers / 503),
                                // never from a stream already handed out. Pinned
                                // by `peer_dies_mid_stream_does_not_duplicate`.
                                return Ok((observed, attribution));
                            }
                            Err(e) => {
                                tracing::info!(
                                    peer = %peer.name,
                                    url = %url,
                                    error = %e,
                                    "mesh-inference: typed peer transport error, trying next address"
                                );
                                last_transport_err = Some(format!("{e}"));
                            }
                        }
                    }
                    let step_err = last_transport_err
                        .clone()
                        .unwrap_or_else(|| "unreachable".into());
                    let shed = decision_log::looks_shed(&step_err);
                    self.book_peer_failure(&peer.name, &step_err, shed);
                    failovers.push(decision_log::FailoverAttempt {
                        peer: peer.name.clone(),
                        error: step_err.clone(),
                        shed,
                        yield_retry_after_secs: decision_log::parse_yield_refusal(&step_err),
                    });
                    match disposition {
                        VenueFailureDisposition::Hard { model_id } => {
                            // Terminal: no further step will serve, so
                            // this is where the join closes.
                            self.outcome_ctx(
                                &decision_id,
                                &oicp_request_id,
                                ServedBy::Failed,
                                attempt_index,
                                &failovers,
                            )
                            .failed(
                                step_err,
                                shed,
                                recorder::now_unix_ms(),
                            );
                            return Err(sovereign_contracts::error::Error::Routing(format!(
                                "model '{}' is advertised by peer '{}' but all peer \
                                 addresses failed: {}",
                                model_id,
                                peer.name,
                                last_transport_err.unwrap_or_else(|| "unreachable".into())
                            )));
                        }
                        VenueFailureDisposition::Soft => {
                            tracing::info!(
                                peer = %peer.name,
                                "mesh-inference: typed peer failed, falling through to next route"
                            );
                            last_err = last_transport_err
                                .map(sovereign_contracts::error::Error::Inference);
                            continue;
                        }
                    }
                }
                RouteDecision::LocalFallback { total } => {
                    let stream = self.local.complete_stream_with_finish(request).await?;
                    let model_id = self.local.model_id_for(request.preferred_speed);
                    let observed: Pin<Box<dyn Stream<Item = StreamFrame> + Send>> =
                        Box::pin(TotalGuardedStream::new(
                            ThroughputObservedStream::new(
                                stream,
                                ThroughputTarget::Local(Arc::clone(&self.local_observations)),
                            )
                            .with_outcome(self.outcome_ctx(
                                &decision_id,
                                &oicp_request_id,
                                ServedBy::LocalFallback {
                                    model_id: model_id.clone(),
                                },
                                attempt_index,
                                &failovers,
                            )),
                            total,
                        ));
                    return Ok((observed, model_id));
                }
            }
        }
        tracing::error!(
            target: "mesh.health",
            last_err = ?last_err,
            "mesh-inference: typed route cascade exhausted — every candidate peer and the local fallback failed for this request"
        );
        // Nothing served. Close the join anyway: a decision with no
        // outcome is indistinguishable from a lost record, and the
        // calibration contract needs "the mesh could not serve this"
        // to be a *measurable* result rather than a gap.
        {
            let err_text = last_err
                .as_ref()
                .map(|e| e.to_string())
                .unwrap_or_else(|| "cascade exhausted".to_string());
            let shed = decision_log::looks_shed(&err_text);
            self.outcome_ctx(
                &decision_id,
                &oicp_request_id,
                ServedBy::Failed,
                failovers.len() as u32,
                &failovers,
            )
            .failed(err_text, shed, recorder::now_unix_ms());
        }
        Err(last_err.unwrap_or_else(|| {
            sovereign_contracts::error::Error::Routing(
                "mesh-inference: route cascade exhausted with no success".into(),
            )
        }))
    }

    /// Plain typed-stream surface — delegates to the cascade sibling
    /// and drops the attribution string, exactly like
    /// `complete_stream` does for the legacy shape. Without this
    /// override the trait default wraps `complete_stream` and
    /// synthesizes `Finish{Stop}` for every stream, erasing the real
    /// `Length`/`Cancelled` reasons the FIM inline-completion route
    /// and the desktop cutoff chip depend on.
    async fn complete_stream_with_finish(
        &self,
        request: &CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = sovereign_contracts::types::StreamFrame> + Send>>> {
        Ok(self.complete_stream_with_id_and_finish(request).await?.0)
    }

    async fn warmup_primary(&self) -> Result<()> {
        // Warm only the local primary slot. We deliberately don't
        // poke peers — that would tie up their lazy mutex during a
        // user's typing window, and the desktop already covers
        // the local case (which is what the user will hit by default
        // when the OICP scorer isn't strictly outscored by a peer).
        self.local.warmup_primary().await
    }

    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        self.local.embed(text).await
    }

    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.local.embed_batch(texts).await
    }

    async fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        self.local.embed_query(text).await
    }

    fn model_id_for(&self, speed: Speed) -> String {
        self.local.model_id_for(speed)
    }

    fn embed_model_id(&self) -> String {
        // Embeds always run locally (see `embed`/`embed_batch` above),
        // so the local slot's id is the honest answer.
        self.local.embed_model_id()
    }

    fn code_model_id(&self) -> Option<String> {
        // Delegate so the mesh-level self-advertisement sees the
        // same code slot the underlying `EmbeddedLlamaCpp` sees.
        self.local.code_model_id()
    }

    async fn rerank_batch(&self, query: &str, docs: &[String]) -> Result<Vec<f32>> {
        // Reranking is local-slot work like embeds and FIM: the peer
        // path never carries it. Without this forward the mesh wrapper
        // — again, the provider the daemon actually installs — reports
        // the trait's `NotImplemented`, and `search_with_rerank`
        // catches that and silently returns un-reranked fusion. So a
        // configured `[rerank]` slot would sit loaded and unused, with
        // retrieval quietly worse and nothing in the logs to say so.
        self.local.rerank_batch(query, docs).await
    }

    fn edit_slot_info(&self) -> Option<sovereign_contracts::types::EditSlotInfo> {
        // FIM serving is inherently local (the keystroke path never
        // leaves this machine), so the honest answer is the local
        // engine's arrangement. Without this forward the mesh wrapper
        // — the provider the daemon actually installs — would report
        // the empty default and `/v1/completions` would 503 forever.
        self.local.edit_slot_info()
    }

    fn resident_slots(&self) -> Vec<sovereign_contracts::traits::ResidentSlot> {
        // Residency is about THIS node's locally-loaded weights, so
        // delegate to the underlying local engine. Without this forward
        // the mesh wrapper (the provider the daemon actually installs)
        // would report the empty default and `/status.inference.resident`
        // would always be blank.
        self.local.resident_slots()
    }

    fn compute_children(&self) -> Vec<sovereign_contracts::traits::ComputeChildStatus> {
        // Same reason as `resident_slots`: the compute children live under
        // THIS node's local routing facade, and the mesh wrapper is the
        // installed provider — without this forward `/status.inference.
        // compute_children` would always be empty.
        self.local.compute_children()
    }

    /// This node IS the mesh-aware forwarder, so it is the only provider
    /// that can answer this. Delegates to `reachable_peer_manifests`, which
    /// shares its peer filter with `locate_named_model` — see that method
    /// for why the two must not drift.
    async fn peer_manifests(&self) -> Vec<(String, ProviderManifest)> {
        self.reachable_peer_manifests().await
    }

    /// Same source `locate_named_model` consults, so the listing and the
    /// routing cannot disagree about what a guest link buys.
    async fn lender_manifest(&self) -> Option<(String, Vec<String>)> {
        match self.guest_source().posture().await {
            crate::guest_lender::GrantPosture::Granted { lender, ids } => Some((lender, ids)),
            // A refused grant advertises nothing — the listing must not name
            // models the lender will not serve. The REFUSAL is reported on the
            // routing path (`select_route`), which is where an operator is
            // waiting on an answer; a listing is not the place to raise it.
            crate::guest_lender::GrantPosture::NoLink
            | crate::guest_lender::GrantPosture::Unusable { .. } => None,
        }
    }

    fn effective_context_size(&self) -> Option<u32> {
        self.local.effective_context_size()
    }

    fn n_ctx_train_for_primary(&self) -> Option<u32> {
        self.local.n_ctx_train_for_primary()
    }

    /// Delegate so the runtime budget calc sees the real BPE count
    /// (when local is `EmbeddedLlamaCpp`). Mesh-forwarded chat
    /// requests still budget against the *local* slot's ctx —
    /// `InferenceRouter` doesn't know what tokenizer the peer
    /// will use, and the runtime decides compaction before routing.
    fn count_tokens(&self, text: &str) -> u32 {
        self.local.count_tokens(text)
    }

    fn capabilities(&self) -> ProviderCapabilities {
        self.local.capabilities()
    }

    // Runtime slot management delegates to the wrapped local provider.
    // Without this override the trait's default-impl returns the
    // generic "this inference provider does not support runtime slot
    // load — only the embedded llama.cpp provider does" error even
    // when `local` is a real EmbeddedLlamaCpp. The bug surfaced
    // 2026-05-20 when `POST /internal/models/load` could not hot-load
    // Gemma into a daemon whose primary slot was Qwen3.6 — the load
    // adapter calls `self.provider.load_extra_slot`, which on the
    // InferenceRouter path always hit the default.
    //
    // After a successful mutation we ALSO rebuild `self_manifest` so
    // mesh routing (`locate_named_model`) sees the new slot
    // immediately. Without the refresh, a hot-loaded slot serves chat
    // completions on a routing-by-model-id call only when the caller
    // bypasses InferenceRouter's locator — which is not the
    // case for `/v1/chat/completions`. Confirmed 2026-05-20: bench
    // could load gemma-* into a Qwen-primary daemon but every
    // request 503'd with "no node in this mesh advertises model".
    fn load_extra_slot(
        &self,
        slot_name: String,
        path: std::path::PathBuf,
        context_size: u32,
    ) -> Result<String> {
        let model_id = self.local.load_extra_slot(slot_name, path, context_size)?;
        self.refresh_self_manifest();
        Ok(model_id)
    }

    fn unload_extra_slot(&self, slot_name: &str) -> Result<Option<String>> {
        let result = self.local.unload_extra_slot(slot_name)?;
        if result.is_some() {
            self.refresh_self_manifest();
        }
        Ok(result)
    }

    fn extras_inventory(&self) -> Vec<(String, String)> {
        self.local.extras_inventory()
    }
}
