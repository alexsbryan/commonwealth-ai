// SPDX-License-Identifier: AGPL-3.0-or-later
//! The rerank kind's client method: `rerank_batch` over the serving node's
//! `/v1/rerank` route (`sovereign_inference::served_kind::RERANK`).

use sovereign_contracts::error::{Error, Result};
use sovereign_contracts::oicp::openai_types::{RerankRequest, RerankResponse};

use crate::RemoteApiProvider;

impl RemoteApiProvider {
    /// Score `docs` against `query` on the remote's `/rerank` route. Scores
    /// come back in input order whatever order the server lists them in.
    pub(crate) async fn rerank_over_route(&self, query: &str, docs: &[String]) -> Result<Vec<f32>> {
        self.admit("rerank request", None)?;
        let url = format!("{}/rerank", self.endpoint.resolve().await?);
        let body = RerankRequest {
            model: self.model_id.clone(),
            query: query.to_string(),
            documents: docs.to_vec(),
        };
        let response = self
            .send_honouring_shed(
                || self.stamped(self.client.post(&url).json(&body)),
                "Rerank request",
            )
            .await?;
        let parsed: RerankResponse = response
            .json()
            .await
            .map_err(|e| Error::Inference(format!("Failed to parse rerank response: {e}")))?;
        let scores = parsed
            .scores_in_input_order(docs.len())
            .map_err(Error::Inference)?;
        tracing::debug!(target: "oicp_client", docs = docs.len(), "rerank answered over the route");
        Ok(scores)
    }
}

#[cfg(test)]
mod tests {
    use sovereign_contracts::traits::InferenceProvider;

    /// The attach-mode provider reranks by dialling `/v1/rerank`, and the
    /// scores land in input order even when the server lists them out of it.
    #[tokio::test]
    async fn the_attach_provider_reranks_over_the_route_in_input_order() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 8192];
            let n = stream.read(&mut buf).unwrap_or(0);
            let body = r#"{"model":"m","results":[{"index":1,"relevance_score":0.5},{"index":0,"relevance_score":2.0}]}"#;
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
            .rerank_batch("q", &["a".to_string(), "b".to_string()])
            .await
            .expect("rerank over the route");
        assert_eq!(scores, vec![2.0, 0.5]);
        let head = server.join().unwrap();
        assert!(head.starts_with("POST /v1/rerank "), "got: {head}");
    }
}
