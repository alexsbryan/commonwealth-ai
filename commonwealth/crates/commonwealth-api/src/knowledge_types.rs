use serde::{Deserialize, Serialize};

/// Knowledge search request body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeSearchRequest {
    #[serde(default)]
    pub query_embedding: Vec<f32>,
    #[serde(default)]
    pub query_text: String,
    #[serde(default)]
    pub corpora: Vec<String>,
    #[serde(default = "default_limit")]
    pub limit: u32,
}

fn default_limit() -> u32 {
    20
}

/// Knowledge search response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeSearchResponse {
    pub results: Vec<KnowledgeResult>,
}

/// A single knowledge search result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeResult {
    pub content: String,
    pub title: String,
    pub corpus_id: String,
    pub score: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default)]
    pub metadata: serde_json::Value,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_request_deserialize_minimal() {
        let json = r#"{"query_text": "Ostrom design principles"}"#;
        let req: KnowledgeSearchRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.query_text, "Ostrom design principles");
        assert_eq!(req.limit, 20); // default
        assert!(req.corpora.is_empty());
    }

    #[test]
    fn search_request_full() {
        let json = r#"{
            "query_embedding": [0.123, -0.456],
            "query_text": "commons governance",
            "corpora": ["wikipedia", "sep"],
            "limit": 10
        }"#;
        let req: KnowledgeSearchRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.query_embedding.len(), 2);
        assert_eq!(req.corpora, vec!["wikipedia", "sep"]);
        assert_eq!(req.limit, 10);
    }

    #[test]
    fn search_response_serialize() {
        let resp = KnowledgeSearchResponse {
            results: vec![KnowledgeResult {
                content: "Elinor Ostrom identified eight design principles...".into(),
                title: "Elinor Ostrom".into(),
                corpus_id: "wikipedia".into(),
                score: 0.89,
                url: Some("https://en.wikipedia.org/wiki/Elinor_Ostrom".into()),
                metadata: serde_json::json!({"source": "wikipedia"}),
            }],
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("Elinor Ostrom"));
        assert!(json.contains("0.89"));
    }
}
