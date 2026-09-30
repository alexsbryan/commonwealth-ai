// SPDX-License-Identifier: AGPL-3.0-or-later
//! svrn, dialed as any client dials it (FIVE_PROGRAMS §2: bench dials the
//! subject). A bench turn goes to svrn's turn route through `TurnClient`,
//! the one client of that route, and its metadata comes back from
//! `GET /v1/conversations/{id}`; bench builds no `Runtime` of its own
//! (pb-bench-dials-turns).
//!
//! What the in-process `Runtime` took from `ChatGlobals` either has a wire
//! form or is refused by name here, never dropped (ARCH principle 6):
//! `--temperature` and `--max-tokens` ride the turn's `sampling`; a
//! `--chat-model` / `--embed-model` svrn does not serve, and `--data-dir`
//! (the in-process store and indexes), have none.

use std::sync::Arc;

use sovereign_contracts::daemon_wire::meshapp::ChunkDto;
use sovereign_contracts::traits::InferenceProvider;
use sovereign_contracts::types::Speed;
use sovereign_turn_client::{
    Intent, RerankOverrides, SamplingOverrides, TurnClient, TurnMode, TurnObserver,
};

use sovereign_cli_base::chat_globals::ChatGlobals;

/// svrn's chat and embed models over HTTP, resolved for `globals`: the
/// `(inference, daemon_base, embed_model_id)` of
/// `oicp_client::daemon_inference::build_inference`, fed the `ChatGlobals`
/// fields it reads. svrn's `chat_cmd::bootstrap::build_inference` is the same
/// call; bench reaches it here so it names no svrn CLI module.
pub(crate) async fn build_inference(
    globals: &ChatGlobals,
) -> sovereign_contracts::error::Result<(Arc<dyn InferenceProvider>, String, String)> {
    oicp_client::daemon_inference::build_inference(
        &globals.daemon_base,
        globals.bearer.as_deref(),
        globals.chat_model.as_deref(),
        globals.embed_model.as_deref(),
        globals.guest_link_active,
        globals.guest_lender_url.as_deref(),
    )
    .await
}

/// A reachable svrn and the pins every turn to it carries.
pub struct SubjectDial {
    client: TurnClient,
    base: String,
    sampling: Option<SamplingOverrides>,
    rerank: Option<RerankOverrides>,
    /// svrn's own chat and embed models over HTTP — what the judges and the
    /// naked arm call. A completion, not a turn.
    pub inference: Arc<dyn InferenceProvider>,
    /// The chat model svrn answers with.
    pub chat_model: String,
}

/// One dialed turn: the answer and the metadata svrn persisted with it.
pub struct DialedTurn {
    pub message_id: String,
    pub text: String,
    /// The assistant message's persisted metadata blob, verbatim
    /// (`retrieved_chunks`, `grounding_gate`, `provenance`, …). `None` when
    /// svrn stored the message without one (a naked turn).
    pub metadata: Option<serde_json::Value>,
}

impl SubjectDial {
    /// Reach svrn at `globals.daemon_base`. An unreachable base is a
    /// could-not-judge naming the base, never a lane that scores empty
    /// answers; a pin with no wire form is refused by name.
    pub async fn dial(globals: &ChatGlobals) -> Result<Self, String> {
        if globals.data_dir_explicit {
            return Err(format!(
                "--data-dir {} picked the in-process Runtime's store and indexes; a bench turn \
                 is answered by svrn at {} from its own. Drop --data-dir, or point --daemon at \
                 a svrn serving that root.",
                globals.data_dir.display(),
                globals.daemon_base
            ));
        }
        let mut served = globals.clone();
        served.chat_model = None;
        served.embed_model = None;
        let (inference, base, embed_model) = build_inference(&served).await.map_err(|e| {
            format!(
                "could-not-judge: no svrn answering at {}: {e}",
                globals.daemon_base
            )
        })?;
        let chat_model = inference.model_id_for(Speed::Slow);
        for (flag, pinned, serves) in [
            ("--chat-model", &globals.chat_model, &chat_model),
            ("--embed-model", &globals.embed_model, &embed_model),
        ] {
            if let Some(p) = pinned.as_ref().filter(|p| *p != serves) {
                return Err(format!(
                    "{flag} {p} has no wire form: svrn at {base} answers with {serves}. \
                     Drop {flag}, or load {p} in svrn."
                ));
            }
        }
        let sampling = (globals.temperature.is_some() || globals.max_tokens.is_some()).then(|| {
            SamplingOverrides {
                temperature: globals.temperature,
                top_p: None,
                max_tokens: globals.max_tokens.map(|n| n as u32),
            }
        });
        tracing::debug!(%base, %chat_model, ?sampling, "bench dials svrn");
        Ok(Self {
            client: TurnClient::new(&base),
            base,
            sampling,
            rerank: None,
            inference,
            chat_model,
        })
    }

    /// Pin every later turn's rerank (`TurnRequest::Message.rerank`): a
    /// `svrn bench promote` arm's settings, which in-process were env vars
    /// its own `Runtime` read.
    pub fn pin_rerank(&mut self, pins: RerankOverrides) {
        tracing::debug!(?pins, "bench pins svrn's rerank");
        self.rerank = Some(pins);
    }

    /// The base every turn goes to.
    pub fn base(&self) -> &str {
        &self.base
    }

    /// Open a conversation, sealed to `seal` when given. svrn mints the id
    /// and refuses a corpus it does not have, so a sealing that cannot be
    /// applied is an error rather than an unscoped turn.
    pub async fn open(&self, seal: Option<&str>) -> Result<String, String> {
        let allow = seal.map(|c| vec![c.to_string()]);
        self.client
            .create_conversation(None, allow.as_deref())
            .await
            .map(|c| c.id)
            .map_err(|e| format!("create_conversation at {}: {e}", self.base))
    }

    /// One turn on an open conversation, then its persisted metadata.
    pub async fn turn(
        &self,
        conversation_id: &str,
        question: &str,
        mode: TurnMode,
        intent: Option<Intent>,
    ) -> Result<DialedTurn, String> {
        let outcome = self
            .client
            .run_turn(
                conversation_id,
                question,
                mode,
                intent,
                self.sampling.clone(),
                self.rerank,
                &mut TurnObserver::default(),
            )
            .await
            .map_err(|e| format!("run_turn at {}: {e}", self.base))?;
        let metadata = self
            .client
            .get_conversation(conversation_id)
            .await
            .map_err(|e| format!("get_conversation at {}: {e}", self.base))?
            .messages
            .into_iter()
            .find(|m| m.id == outcome.message_id)
            .and_then(|m| m.metadata);
        Ok(DialedTurn {
            message_id: outcome.message_id,
            text: outcome.text,
            metadata,
        })
    }

    /// A fresh conversation (sealed to `seal` when given) and one turn on it.
    pub async fn ask(
        &self,
        seal: Option<&str>,
        question: &str,
        mode: TurnMode,
        intent: Option<Intent>,
    ) -> Result<DialedTurn, String> {
        let conversation_id = self.open(seal).await?;
        self.turn(&conversation_id, question, mode, intent).await
    }

    /// A retrieved chunk's full text, read from svrn's index by id.
    pub async fn chunk_text(&self, corpus_id: &str, chunk_id: u64) -> Option<String> {
        self.client
            .meshapp_read_chunk::<ChunkDto>(corpus_id, chunk_id)
            .await
            .ok()
            .map(|c| c.content)
    }
}

#[cfg(test)]
#[path = "subject_tests.rs"]
mod tests;
