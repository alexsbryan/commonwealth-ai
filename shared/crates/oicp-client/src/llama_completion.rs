// SPDX-License-Identifier: AGPL-3.0-or-later
//! A raw prompt on llama-server's own `POST /completion`: the prompt
//! tokenized verbatim (special tokens parsed, no chat template), which is
//! what a `PromptShape::Raw` request asks for and what the chat wire cannot
//! give it (svrn-docs ENGINE_CONFORMANCE_ISSUES.md C2: templated silently).
//!
//! The family stop strings go as `stop`, and no `preserved_tokens`: a stop
//! token generated ends the completion with `stop_type: eos` and renders
//! nothing (ENGINE_SWAP_PLAN.md F2), which is the embedded engine's EOG stop.

use std::pin::Pin;

use futures::Stream;
use serde::Deserialize;
use sovereign_contracts::error::{Error, Result};
use sovereign_contracts::types::{CompletionRequest, FinishReason, StreamFrame, StreamUsage};

/// One `data:` frame of the server's native completion stream.
#[derive(Debug, Deserialize)]
struct Frame {
    #[serde(default)]
    content: String,
    #[serde(default)]
    stop: bool,
    stop_type: Option<String>,
    tokens_predicted: Option<u32>,
    tokens_evaluated: Option<u32>,
    error: Option<FrameError>,
}

#[derive(Debug, Deserialize)]
struct FrameError {
    message: String,
}

/// The request body: `model` routes it through a router; sampling fields
/// the request leaves unset are the server's.
fn body(model: &str, request: &CompletionRequest, stop: &[&str]) -> serde_json::Value {
    let mut body = serde_json::json!({
        "model": model,
        "prompt": request.prompt,
        "stop": stop,
        "stream": true,
    });
    let set = |body: &mut serde_json::Value, key: &str, v: serde_json::Value| {
        body[key] = v;
    };
    if let Some(n) = request.max_tokens {
        set(&mut body, "n_predict", n.into());
    }
    if let Some(t) = request.temperature {
        set(&mut body, "temperature", t.into());
    }
    if let Some(k) = request.top_k {
        set(&mut body, "top_k", k.into());
    }
    if let Some(p) = request.top_p {
        set(&mut body, "top_p", p.into());
    }
    body
}

/// What one line of the stream contributes: text, the terminal frame, or
/// nothing (a blank line, a comment, a frame with no text).
fn read_line(line: &str) -> Vec<StreamFrame> {
    let Some(data) = line.trim().strip_prefix("data: ") else {
        return Vec::new();
    };
    let frame: Frame = match serde_json::from_str(data) {
        Ok(f) => f,
        Err(e) => {
            return vec![StreamFrame::Error(format!(
                "llama-server /completion sent an unreadable frame ({e}): {data}"
            ))]
        }
    };
    if let Some(e) = frame.error {
        return vec![StreamFrame::Error(e.message)];
    }
    let mut out = Vec::new();
    if !frame.content.is_empty() {
        out.push(StreamFrame::Token(frame.content));
    }
    if frame.stop {
        let reason = match frame.stop_type.as_deref() {
            Some("eos") | Some("word") => FinishReason::Stop,
            Some("limit") => FinishReason::Length,
            other => FinishReason::Error(format!(
                "llama-server /completion stopped with stop_type {other:?}"
            )),
        };
        tracing::debug!(target: "oicp_client", stop_type = ?frame.stop_type, ?reason, "llama-server: raw completion finished");
        let usage =
            frame
                .tokens_evaluated
                .zip(frame.tokens_predicted)
                .map(|(prompt, completion)| StreamUsage {
                    prompt_tokens: prompt,
                    completion_tokens: completion,
                    total_tokens: prompt + completion,
                });
        out.push(StreamFrame::Finish { reason, usage });
    }
    out
}

/// Stream `request.prompt`, verbatim, through `{base}/completion` to
/// `model`, ending at any of `stop`. Every stream ends in a `Finish` or an
/// `Error` frame; one the server closes without its final frame ends in an
/// `Error`, never a `Stop` nobody observed.
pub async fn raw_completion_stream(
    base: &str,
    model: &str,
    request: &CompletionRequest,
    stop: &[&str],
) -> Result<Pin<Box<dyn Stream<Item = StreamFrame> + Send>>> {
    let url = format!("{base}/completion");
    tracing::debug!(target: "oicp_client", %url, model, prompt_chars = request.prompt.len(), stops = stop.len(), "llama-server: raw completion");
    let response = reqwest::Client::new()
        .post(&url)
        .json(&body(model, request, stop))
        .send()
        .await
        .map_err(|e| Error::Inference(format!("POST {url} for {model}: {e}")))?;
    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        return Err(Error::Inference(format!(
            "POST {url} for {model}: {status}: {text}"
        )));
    }
    let (tx, rx) = tokio::sync::mpsc::channel::<StreamFrame>(32);
    let mut bytes = response.bytes_stream();
    tokio::spawn(async move {
        use futures::StreamExt;
        let mut buf = String::new();
        while let Some(chunk) = bytes.next().await {
            let chunk = match chunk {
                Ok(b) => b,
                Err(e) => {
                    let _ = tx
                        .send(StreamFrame::Error(format!(
                            "raw completion stream broke: {e}"
                        )))
                        .await;
                    return;
                }
            };
            buf.push_str(&String::from_utf8_lossy(&chunk));
            while let Some(pos) = buf.find('\n') {
                let line: String = buf.drain(..=pos).collect();
                for frame in read_line(&line) {
                    let terminal = !matches!(frame, StreamFrame::Token(_));
                    if tx.send(frame).await.is_err() || terminal {
                        return;
                    }
                }
            }
        }
        let _ = tx
            .send(StreamFrame::Error(
                "llama-server closed the raw completion stream without its final frame".into(),
            ))
            .await;
    });
    Ok(Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_body_is_the_prompt_verbatim_with_the_stops_and_no_preserved_tokens() {
        let mut r = CompletionRequest::new("<|fim_prefix|>x<|fim_suffix|><|fim_middle|>");
        r.max_tokens = Some(48);
        r.temperature = Some(0.0);
        let b = body("coder", &r, &["<|endoftext|>", "<|fim_pad|>"]);
        assert_eq!(b["model"], "coder");
        assert_eq!(b["prompt"], "<|fim_prefix|>x<|fim_suffix|><|fim_middle|>");
        assert_eq!(
            b["stop"],
            serde_json::json!(["<|endoftext|>", "<|fim_pad|>"])
        );
        assert_eq!(b["n_predict"], 48);
        assert_eq!(b["temperature"], 0.0);
        assert!(b.get("preserved_tokens").is_none(), "{b}");
        assert!(b.get("messages").is_none(), "{b}");
        assert!(
            b.get("top_k").is_none(),
            "an unset knob is the server's: {b}"
        );
    }

    /// The frames the shipped server sent for an FIM prompt (es-r3-fim
    /// probe, Qwen3.5-0.8B through the router).
    #[test]
    fn native_frames_read_as_tokens_then_the_server_s_stop() {
        let tok = r#"data: {"index":0,"content":" b","tokens":[292],"stop":false,"tokens_predicted":6,"tokens_evaluated":11}"#;
        assert!(matches!(&read_line(tok)[..], [StreamFrame::Token(t)] if t == " b"));
        let eos_token = r#"data: {"index":0,"content":"","tokens":[248044],"stop":false}"#;
        assert!(read_line(eos_token).is_empty());
        assert!(read_line("").is_empty());
        let done = r#"data: {"content":"","stop":true,"stop_type":"eos","tokens_predicted":8,"tokens_evaluated":11}"#;
        match &read_line(done)[..] {
            [StreamFrame::Finish {
                reason: FinishReason::Stop,
                usage: Some(u),
            }] => assert_eq!(
                (u.prompt_tokens, u.completion_tokens, u.total_tokens),
                (11, 8, 19)
            ),
            other => panic!("{other:?}"),
        }
        let limit = r#"data: {"content":"x","stop":true,"stop_type":"limit"}"#;
        assert!(matches!(
            &read_line(limit)[..],
            [
                StreamFrame::Token(_),
                StreamFrame::Finish {
                    reason: FinishReason::Length,
                    usage: None
                }
            ]
        ));
    }

    /// A stop the server does not name, an error frame and an unreadable
    /// one are errors, never a clean stop.
    #[test]
    fn what_is_not_a_known_stop_is_an_error() {
        let odd = r#"data: {"content":"","stop":true,"stop_type":"none"}"#;
        assert!(matches!(
            &read_line(odd)[..],
            [StreamFrame::Finish {
                reason: FinishReason::Error(_),
                ..
            }]
        ));
        let err = r#"data: {"error":{"code":500,"message":"slot unavailable"}}"#;
        assert!(matches!(&read_line(err)[..], [StreamFrame::Error(m)] if m == "slot unavailable"));
        assert!(matches!(
            &read_line("data: {nope")[..],
            [StreamFrame::Error(_)]
        ));
    }
}
