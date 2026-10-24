// SPDX-License-Identifier: AGPL-3.0-or-later
//! The rerank kind's client method: `rerank_batch` over the serving node's
//! `/v1/rerank` route (`sovereign_inference::served_kind::RERANK`).

use sovereign_contracts::error::{Error, Result};
use sovereign_contracts::oicp::openai_types::{RerankRequest, RerankResponse};

use crate::RemoteApiProvider;

/// The inverse of a server's relevance sigmoid, in the one place a rerank
/// score enters this client: `ln(p / (1 - p))`.
///
/// Why `rerank_batch` returns a MARGIN and not the probability the route
/// sends (svrn-docs ENGINE_SWAP_PLAN A4): every caller compares the score
/// against `0` — the PPR admission bar (`query_expansion.rs`, a chunk
/// answers iff the cross-encoder's yes logit beats its no logit), the
/// min-max rerank/fusion blend (`search_with_rerank`, where `0` is
/// relevance par), the H1 band's fitted Platt map. A probability cannot
/// carry that bar: it is never `<= 0`. The logit can, and it is the same
/// ruler the embedded engine's `YesNoLogit` reads.
fn margin_from_probability(p: f32) -> f32 {
    let p = (p as f64).clamp(f64::MIN_POSITIVE, 1.0);
    if p >= 1.0 {
        tracing::warn!(
            target: "oicp_client",
            "rerank: a relevance score of 1.0 saturates the margin; the ranking it yields is still monotone"
        );
        return f32::MAX;
    }
    (p / (1.0 - p)).ln() as f32
}

impl RemoteApiProvider {
    /// Score `docs` against `query` on the remote's `/rerank` route, which
    /// serves the model `model` names (the preset's rerank section; the chat
    /// model's id is not one the route knows). Scores come back in input
    /// order whatever order the server lists them in, as margins
    /// ([`margin_from_probability`]).
    pub(crate) async fn rerank_over_route(
        &self,
        model: &str,
        query: &str,
        docs: &[String],
    ) -> Result<Vec<f32>> {
        let admitted = self.outbound(crate::Payload::Texts)?;
        let url = format!("{}/rerank", self.endpoint.resolve().await?);
        let body = RerankRequest {
            model: model.to_string(),
            query: query.to_string(),
            documents: docs.to_vec(),
        };
        let response = self
            .send_honouring_shed(|| admitted.post(&url).json(&body), "Rerank request")
            .await?;
        let parsed: RerankResponse = response
            .json()
            .await
            .map_err(|e| Error::Inference(format!("Failed to parse rerank response: {e}")))?;
        let scores = parsed
            .scores_in_input_order(docs.len())
            .map_err(Error::Inference)?
            .into_iter()
            .map(margin_from_probability)
            .collect();
        tracing::debug!(target: "oicp_client", model, docs = docs.len(), "rerank answered over the route");
        Ok(scores)
    }
}

#[cfg(test)]
mod tests {
    use sovereign_contracts::traits::InferenceProvider;

    /// The attach-mode provider reranks by dialling `/v1/rerank` under the
    /// id the caller names (the rerank model, not the chat one), the scores
    /// land in input order even when the server lists them out of it, and
    /// each is returned as `ln(p/(1-p))` so the `> 0` bars keep their
    /// meaning. Failing input: send the chat id (the request head reads
    /// `"chat-model"`), or return the probabilities untouched (`0.5` reads
    /// `0.5` where the margin is `0.0`).
    #[tokio::test]
    async fn the_attach_provider_reranks_over_the_route_in_input_order_as_a_margin() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 8192];
            let n = stream.read(&mut buf).unwrap_or(0);
            let body = r#"{"model":"reranker","results":[{"index":1,"relevance_score":0.25},{"index":0,"relevance_score":0.5}]}"#;
            let reply = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(reply.as_bytes()).unwrap();
            String::from_utf8_lossy(&buf[..n]).to_string()
        });
        let provider = crate::SplitInferenceProvider::new(
            &format!("http://127.0.0.1:{port}/v1"),
            "chat-model".to_string(),
            "embed-model".to_string(),
            8192,
            String::new(),
        );
        let scores = provider
            .rerank_batch(Some("reranker"), "q", &["a".to_string(), "b".to_string()])
            .await
            .expect("rerank over the route");
        // p = 0.5 is the relevance par, i.e. a margin of exactly 0; p = 0.25
        // sits below it.
        assert_eq!(scores[0], 0.0, "{scores:?}");
        assert!(
            (scores[1] - (0.25f32 / 0.75).ln()).abs() < 1e-6,
            "{scores:?}"
        );
        assert!(scores[1] < 0.0, "{scores:?}");
        let head = server.join().unwrap();
        assert!(head.starts_with("POST /v1/rerank "), "got: {head}");
        assert!(
            head.contains(r#""model":"reranker""#),
            "the request names the chat model, not the rerank section: {head}"
        );
    }
}
