// SPDX-License-Identifier: AGPL-3.0-or-later
//! Sampling for a `PromptShape::Conversation` turn, the way llama-server
//! samples the same OpenAI request: llama.cpp's defaults
//! (`common_params_sampling`, common/common.h), overridden by the GGUF's
//! own `general.sampling.*` recommendations (common/common.cpp, the
//! `get_float`/`get_int32` block), overridden by what the client sent. The
//! chain is llama.cpp's default order — penalties, top-k, top-p, min-p,
//! temperature — with nothing the daemon adds (no DRY, no profile).

use sovereign_contracts::types::CompletionRequest;

use crate::llama::cpp::model::LlamaModel;
use crate::llama::cpp::sampling::LlamaSampler;

/// The resolved parameters of one conversation turn.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ServerSampling {
    pub temp: f32,
    pub top_k: i32,
    pub top_p: f32,
    pub min_p: f32,
    pub penalty_last_n: i32,
    pub penalty_repeat: f32,
}

impl Default for ServerSampling {
    /// llama.cpp's `common_params_sampling` defaults.
    fn default() -> Self {
        Self {
            temp: 0.80,
            top_k: 40,
            top_p: 0.95,
            min_p: 0.05,
            penalty_last_n: 64,
            penalty_repeat: 1.00,
        }
    }
}

impl ServerSampling {
    /// Defaults, then the GGUF's `general.sampling.*` (read through
    /// `meta`), then the request's own temperature / top_p / top_k.
    pub(crate) fn resolve(
        meta: impl Fn(&str) -> Option<String>,
        request: &CompletionRequest,
    ) -> Self {
        let mut s = Self::default();
        let float = |key: &str| meta(key).and_then(|v| v.trim().parse::<f32>().ok());
        let int = |key: &str| meta(key).and_then(|v| v.trim().parse::<i32>().ok());
        if let Some(v) = int("general.sampling.top_k") {
            s.top_k = v;
        }
        if let Some(v) = float("general.sampling.top_p") {
            s.top_p = v;
        }
        if let Some(v) = float("general.sampling.min_p") {
            s.min_p = v;
        }
        if let Some(v) = float("general.sampling.temp") {
            s.temp = v;
        }
        if let Some(v) = int("general.sampling.penalty_last_n") {
            s.penalty_last_n = v;
        }
        if let Some(v) = float("general.sampling.penalty_repeat") {
            s.penalty_repeat = v;
        }
        if let Some(v) = request.temperature {
            s.temp = v;
        }
        if let Some(v) = request.top_p {
            s.top_p = v;
        }
        if let Some(v) = request.top_k {
            s.top_k = v as i32;
        }
        s
    }

    /// The parameters for `model`, read from its GGUF metadata.
    pub(crate) fn for_model(model: &LlamaModel, request: &CompletionRequest) -> Self {
        let s = Self::resolve(|key| model.meta_val_str(key, 64).ok(), request);
        tracing::debug!(
            temp = s.temp,
            top_k = s.top_k,
            top_p = s.top_p,
            min_p = s.min_p,
            penalty_repeat = s.penalty_repeat,
            client_temperature = request.temperature.is_some(),
            "sampler: conversation turn sampled as llama-server samples it"
        );
        s
    }

    /// llama.cpp's default chain order; at temperature <= 0, greedy.
    pub(crate) fn chain(&self, model: &LlamaModel, seed: u32) -> LlamaSampler {
        let mut samplers = vec![LlamaSampler::penalties(
            model,
            self.penalty_last_n,
            self.penalty_repeat,
            0.0,
            0.0,
        )];
        if self.temp <= 0.0 {
            samplers.push(LlamaSampler::greedy());
        } else {
            samplers.push(LlamaSampler::top_k(self.top_k));
            samplers.push(LlamaSampler::top_p(self.top_p, 1));
            samplers.push(LlamaSampler::min_p(self.min_p, 1));
            samplers.push(LlamaSampler::temp(self.temp));
            samplers.push(LlamaSampler::dist(seed));
        }
        LlamaSampler::chain_simple(samplers)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |k| {
            pairs
                .iter()
                .find(|(key, _)| *key == k)
                .map(|(_, v)| v.to_string())
        }
    }

    #[test]
    fn llama_cpp_defaults_when_the_gguf_and_client_say_nothing() {
        let s = ServerSampling::resolve(meta(&[]), &CompletionRequest::new("x"));
        assert_eq!(s, ServerSampling::default());
        assert_eq!((s.temp, s.top_k, s.top_p, s.min_p), (0.80, 40, 0.95, 0.05));
    }

    /// The values Qwen3.8-27B-UD-Q6_K_XL.gguf carries (read from the file
    /// 2026-10-07: top_k 20, top_p 0.95, temp 1.0, no min_p).
    #[test]
    fn the_gguf_recommendation_overrides_the_defaults() {
        let s = ServerSampling::resolve(
            meta(&[
                ("general.sampling.temp", "1.000000"),
                ("general.sampling.top_k", "20"),
                ("general.sampling.top_p", "0.950000"),
            ]),
            &CompletionRequest::new("x"),
        );
        assert_eq!((s.temp, s.top_k, s.top_p, s.min_p), (1.0, 20, 0.95, 0.05));
    }

    #[test]
    fn the_client_overrides_the_gguf() {
        let mut req = CompletionRequest::new("x");
        req.temperature = Some(0.2);
        req.top_k = Some(5);
        let s = ServerSampling::resolve(
            meta(&[
                ("general.sampling.temp", "1.0"),
                ("general.sampling.top_k", "20"),
            ]),
            &req,
        );
        assert_eq!((s.temp, s.top_k), (0.2, 5));
    }

    #[test]
    fn an_unparseable_gguf_value_is_ignored_as_llama_cpp_ignores_it() {
        let s = ServerSampling::resolve(
            meta(&[("general.sampling.temp", "warm")]),
            &CompletionRequest::new("x"),
        );
        assert_eq!(s.temp, 0.80);
    }
}
