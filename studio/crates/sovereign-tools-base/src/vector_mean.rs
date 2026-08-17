// SPDX-License-Identifier: AGPL-3.0-or-later
//! `vector_mean` — the weighted centroid of a collection of vectors. The taste
//! profile of the personalized recommender: average the embedding vectors of the
//! movies you rated, weighted by rating, into one "what you like" vector that
//! `corpus_search` then ranks the catalog against.
//!
//! Pure deterministic arithmetic (no model, no IO) — `Σ(wᵢ·vᵢ) / Σwᵢ`. Mirrors the
//! shape of `raptor_atlas::mean_vector`, extended with per-item weights.

use async_trait::async_trait;

use sovereign_contracts::error::{Error, Result};
use sovereign_contracts::traits::Tool;
use sovereign_contracts::types::*;

pub struct VectorMeanTool;

#[async_trait]
impl Tool for VectorMeanTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "vector_mean".to_string(),
            name: "vector_mean".to_string(),
            description: "Weighted centroid of a collection of vectors — Σ(wᵢ·vᵢ)/Σwᵢ. Each \
                          item carries a vector and a numeric weight; returns one vector."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "items": { "type": "string", "description": "Collection of objects, each with a vector + a weight field — e.g. {profiled.output}" },
                    "vector_key": { "type": "string", "description": "Field holding each item's vector (default 'vector')" },
                    "weight_key": { "type": "string", "description": "Field holding each item's weight; dotted paths ok (default 'weight')" },
                    "baseline": { "type": "string", "description": "Subtracted from each weight before averaging (default 0). Raise it to down-weight low ratings." }
                },
                "required": ["items"]
            }),
            examples: vec![],
            effect: Effect::Read,
            idempotency: Idempotency::Idempotent,
            latency: Latency::Instant,
            scope: Scope::Session,
            output_schema: Some(serde_json::json!({
                "type": "array",
                "items": { "type": "number" }
            })),
        }
    }

    fn required_permissions(&self) -> Vec<Permission> {
        vec![]
    }

    async fn execute(&self, params: &serde_json::Value, _ctx: &ToolContext) -> Result<StepOutput> {
        let items = collection_param(params, "items")?;
        let items = items
            .as_array()
            .ok_or_else(|| Error::Execution("vector_mean: `items` must be a collection".into()))?;
        let vector_key = params
            .get("vector_key")
            .and_then(|v| v.as_str())
            .unwrap_or("vector");
        let weight_key = params
            .get("weight_key")
            .and_then(|v| v.as_str())
            .unwrap_or("weight");
        let baseline = params.get("baseline").and_then(num).unwrap_or(0.0);

        let mut acc: Vec<f64> = Vec::new();
        let mut wsum: f64 = 0.0;
        for item in items {
            let vec = get_path(item, vector_key)
                .and_then(|v| v.as_array())
                .ok_or_else(|| {
                    Error::Execution(format!(
                        "vector_mean: item missing vector at `{vector_key}`"
                    ))
                })?;
            let w = get_path(item, weight_key).and_then(num).ok_or_else(|| {
                Error::Execution(format!(
                    "vector_mean: item missing numeric weight at `{weight_key}`"
                ))
            })? - baseline;
            if w == 0.0 {
                continue;
            }
            if acc.is_empty() {
                acc = vec![0.0; vec.len()];
            } else if acc.len() != vec.len() {
                return Err(Error::Execution(format!(
                    "vector_mean: vector dim mismatch ({} vs {})",
                    acc.len(),
                    vec.len()
                )));
            }
            for (a, x) in acc.iter_mut().zip(vec) {
                *a += w * x.as_f64().unwrap_or(0.0);
            }
            wsum += w;
        }

        if acc.is_empty() || wsum.abs() < 1e-9 {
            return Err(Error::Execution(
                "vector_mean: no weighted vectors to average (empty, or weights sum to zero)"
                    .into(),
            ));
        }
        let mean: Vec<serde_json::Value> = acc
            .into_iter()
            .map(|a| serde_json::Value::from(a / wsum))
            .collect();
        Ok(StepOutput::Json(serde_json::Value::Array(mean)))
    }
}

/// A JSON value as `f64`, whether it's a JSON number or a stringified number
/// (templating turns most params into strings).
fn num(v: &serde_json::Value) -> Option<f64> {
    v.as_f64()
        .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
}

/// `items` as a JSON value, accepting a JSON string (templated) or a structured array.
fn collection_param(params: &serde_json::Value, key: &str) -> Result<serde_json::Value> {
    match params.get(key) {
        Some(serde_json::Value::String(s)) => serde_json::from_str(s)
            .map_err(|e| Error::Execution(format!("vector_mean: parse `{key}`: {e}"))),
        Some(other) => Ok(other.clone()),
        None => Err(Error::Execution(format!(
            "vector_mean: missing required `{key}`"
        ))),
    }
}

/// Traverse a dotted path (`a.b`) into a JSON object value.
fn get_path<'a>(value: &'a serde_json::Value, path: &str) -> Option<&'a serde_json::Value> {
    let mut cur = value;
    for seg in path.split('.') {
        cur = cur.get(seg)?;
    }
    Some(cur)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> ToolContext {
        ToolContext {
            conversation_id: Default::default(),
            task_id: None,
            working_directory: None,
            in_reasoning_loop: false,
            agent_session_token: None,
            turn_index: 0,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn weighted_centroid_with_dotted_weight_key() {
        // weights 3 and 1 → centroid pulled toward the first vector.
        let items = serde_json::json!([
            { "vec": [1.0, 0.0], "row": { "Rating": "3" } },
            { "vec": [0.0, 1.0], "row": { "Rating": "1" } }
        ]);
        let out = VectorMeanTool
            .execute(
                &serde_json::json!({ "items": items, "vector_key": "vec", "weight_key": "row.Rating" }),
                &ctx(),
            )
            .await
            .unwrap();
        let arr = match out {
            StepOutput::Json(serde_json::Value::Array(a)) => a,
            o => panic!("expected array; got {o:?}"),
        };
        let v: Vec<f64> = arr.iter().map(|x| x.as_f64().unwrap()).collect();
        // (3·[1,0] + 1·[0,1]) / 4 = [0.75, 0.25]
        assert!((v[0] - 0.75).abs() < 1e-6, "{v:?}");
        assert!((v[1] - 0.25).abs() < 1e-6, "{v:?}");
    }

    #[tokio::test]
    async fn zero_weight_sum_errors() {
        let items = serde_json::json!([{ "vector": [1.0], "weight": "0" }]);
        let err = VectorMeanTool
            .execute(&serde_json::json!({ "items": items }), &ctx())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("average"), "{err}");
    }
}
