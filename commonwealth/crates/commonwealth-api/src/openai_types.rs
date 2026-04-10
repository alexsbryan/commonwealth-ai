use serde::{Deserialize, Serialize};

use commonwealth_inference::oicp::InferenceRequirements;

/// OpenAI-compatible chat completion request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatCompletionRequest {
    pub model: Option<String>,
    pub messages: Vec<ChatMessage>,
    #[serde(default)]
    pub temperature: Option<f32>,
    #[serde(default)]
    pub max_tokens: Option<u32>,
    #[serde(default)]
    pub stream: Option<bool>,
    #[serde(default)]
    pub top_p: Option<f32>,
    #[serde(default)]
    pub frequency_penalty: Option<f32>,
    #[serde(default)]
    pub presence_penalty: Option<f32>,
    #[serde(default)]
    pub stop: Option<Vec<String>>,
    /// Commonwealth extension: OICP requirements for model selection.
    #[serde(default)]
    pub oicp: Option<InferenceRequirements>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

/// OpenAI-compatible chat completion response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatCompletionResponse {
    pub id: String,
    pub object: String,
    pub created: u64,
    pub model: String,
    pub choices: Vec<ChatChoice>,
    pub usage: Option<Usage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatChoice {
    pub index: u32,
    pub message: ChatMessage,
    pub finish_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

/// OpenAI-compatible model list response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelListResponse {
    pub object: String,
    pub data: Vec<ModelObject>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelObject {
    pub id: String,
    pub object: String,
    pub created: u64,
    pub owned_by: String,
    /// Commonwealth extension: OICP capabilities.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<serde_json::Value>,
    /// Commonwealth extension: performance estimates.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub performance: Option<ModelPerformance>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelPerformance {
    pub estimated_tokens_per_sec: f32,
    pub estimated_ttft_ms: u32,
    pub loaded: bool,
}

/// OpenAI-compatible error response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorResponse {
    pub error: ErrorDetail,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorDetail {
    pub message: String,
    #[serde(rename = "type")]
    pub error_type: String,
    pub code: Option<String>,
}

impl ErrorResponse {
    pub fn new(message: impl Into<String>, error_type: impl Into<String>) -> Self {
        Self {
            error: ErrorDetail {
                message: message.into(),
                error_type: error_type.into(),
                code: None,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_completion_request_deserialize_minimal() {
        let json = r#"{
            "messages": [{"role": "user", "content": "Hello"}]
        }"#;
        let req: ChatCompletionRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.messages.len(), 1);
        assert!(req.model.is_none());
        assert!(req.oicp.is_none());
    }

    #[test]
    fn chat_completion_request_with_oicp() {
        let json = r#"{
            "messages": [{"role": "user", "content": "Write code"}],
            "oicp": {
                "oicp_version": "0.2.0",
                "capabilities": {
                    "required": {"code": 2},
                    "preferred": {"code": 4}
                }
            }
        }"#;
        let req: ChatCompletionRequest = serde_json::from_str(json).unwrap();
        assert!(req.oicp.is_some());
        let oicp = req.oicp.unwrap();
        assert_eq!(oicp.oicp_version, "0.2.0");
    }

    #[test]
    fn chat_completion_response_serialize() {
        let resp = ChatCompletionResponse {
            id: "chatcmpl-123".into(),
            object: "chat.completion".into(),
            created: 1700000000,
            model: "qwen3-coder-30b".into(),
            choices: vec![ChatChoice {
                index: 0,
                message: ChatMessage {
                    role: "assistant".into(),
                    content: "Hello!".into(),
                },
                finish_reason: Some("stop".into()),
            }],
            usage: Some(Usage {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
            }),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("chatcmpl-123"));
        assert!(json.contains("Hello!"));
    }

    #[test]
    fn error_response_serialize() {
        let err = ErrorResponse::new("model not found", "invalid_request_error");
        let json = serde_json::to_string(&err).unwrap();
        assert!(json.contains("model not found"));
    }

    #[test]
    fn model_list_response_serialize() {
        let resp = ModelListResponse {
            object: "list".into(),
            data: vec![ModelObject {
                id: "qwen3-coder-30b".into(),
                object: "model".into(),
                created: 1700000000,
                owned_by: "mesh".into(),
                capabilities: None,
                performance: Some(ModelPerformance {
                    estimated_tokens_per_sec: 45.0,
                    estimated_ttft_ms: 1100,
                    loaded: true,
                }),
            }],
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("qwen3-coder-30b"));
    }
}
