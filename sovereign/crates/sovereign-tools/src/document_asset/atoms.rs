// SPDX-License-Identifier: AGPL-3.0-or-later
//! Action-atom extraction and the document overview — the last skeleton pass.

// One cooperating unit split for size (ARCH §3.2), not independent modules:
// the manager, its three phases and the skeleton free functions all name each
// other's types. The import surface stays in `mod.rs`.
use super::*;

/// Run a Fast-slot extraction over the top-N entities' chunks and
/// emit `ActionAtom`s. One LLM call per entity, batching that entity's
/// appearance chunks into a single prompt. The model is asked for a
/// JSON list of `{verb, object, chunk_index, evidence}`; any chunk
/// we can't parse cleanly is silently dropped — atoms are an additive
/// retrieval surface, not a load-bearing index, so missing data is
/// degraded behaviour, not a failure mode.
pub(super) async fn extract_action_atoms(
    inference: &Arc<dyn InferenceProvider>,
    chunks: &[TextChunk],
    main_entities: &[RankedEntity],
    entity_index: &std::collections::HashMap<String, EntityAppearances>,
) -> Vec<ActionAtom> {
    // ── Build every entity's prompt first, then fan out.
    //
    // These six calls used to run in a sequential `for` loop: ~3s each,
    // ~19s of the T2 tail, all of it on the critical path between
    // rag_available and multi_hop_ready. They're independent — each
    // entity's atoms depend only on that entity's sampled chunks — so
    // the serialization bought nothing. Prompt construction stays here
    // (it borrows `chunks`/`entity_index`); only the calls fan out.
    struct AtomCall {
        entity: String,
        sample_indices: Vec<usize>,
        prompt: String,
    }
    let mut calls: Vec<AtomCall> = Vec::new();
    // Top-6: covers the load-bearing characters/concepts in a typical
    // narrative; running the full top-30 would blow the budget for
    // marginal lift on peripheral entities.
    for ent in main_entities.iter().take(6) {
        let Some(appearances) = entity_index.get(&ent.name) else {
            continue;
        };
        // Cap appearance chunks at 6 to bound the prompt size. The
        // entity's earliest appearances are usually introductory;
        // we sample stride-wise from the appearance list so we cover
        // beginning, middle, end of the entity's arc.
        let total = appearances.chunk_indices.len();
        let sample_indices: Vec<usize> = if total <= 6 {
            appearances.chunk_indices.clone()
        } else {
            let stride = (total / 6).max(1);
            appearances
                .chunk_indices
                .iter()
                .step_by(stride)
                .take(6)
                .copied()
                .collect()
        };

        // Compose a single prompt listing the sampled chunks with
        // their indices. Cap each chunk excerpt at 500 chars so the
        // total prompt stays inside Fast-slot context limits even
        // when an entity hits 6 long chunks.
        let mut passages = String::new();
        for &idx in &sample_indices {
            if let Some(chunk) = chunks.get(idx) {
                let excerpt: String = chunk.content.chars().take(500).collect();
                passages.push_str(&format!("\n[chunk {idx}]\n{}\n", excerpt.trim(),));
            }
        }
        if passages.trim().is_empty() {
            continue;
        }

        let prompt = format!(
            "Extract what \"{name}\" DOES in these passages. For each chunk \
             where {name} performs a notable action, emit one JSON object:\n\
             {{\"chunk_index\": <int>, \"verb\": \"<lowercase verb>\", \
             \"object\": \"<short noun phrase>\", \"evidence\": \"<verbatim snippet ≤140 chars>\"}}\n\n\
             Rules:\n\
             - Skip chunks where {name} is only mentioned in passing.\n\
             - Verb is a single lowercase past-tense verb (e.g. \"stitched\", \"discovered\", \"killed\").\n\
             - Object is what the verb acts on, in the document's own wording.\n\
             - Evidence is verbatim text from the chunk, ≤140 chars, that contains the verb+object.\n\
             - Skip if nothing notable happens to/by {name} in the chunk.\n\n\
             Passages:\n{passages}\n\n\
             Respond with a JSON array, no commentary:\n[",
            name = ent.name,
        );

        calls.push(AtomCall {
            entity: ent.name.clone(),
            sample_indices,
            prompt,
        });
    }

    // Fan out. `buffered` yields in input order, so the atom list stays
    // in main-entity rank order exactly as the sequential loop left it.
    const ATOM_CONCURRENCY: usize = 6;
    let responses: Vec<(AtomCall, Option<String>)> = stream::iter(calls)
        .map(|call| {
            let inference = Arc::clone(inference);
            async move {
                // SLOT_POLICY §3 Housekeep: per-entity action-atom extraction —
                // advisory enrichment kept on the Fast slot (P1 neutrality).
                // Housekeep's Some(0) think budget matches this site verbatim.
                let mut request =
                    Workload::Housekeep.request(call.prompt.clone())
                        // POLICY-DEBT(SLOT_POLICY §4.5 Housekeep): 768 > 512 forfeits the
                        // batched FastShort claim; the JSON action array needs the room.
                        .with_output_budget(768);
                request.temperature = Some(0.1);
                let text = match inference.complete(&request).await {
                    Ok(r) => Some(r.text),
                    Err(e) => {
                        tracing::debug!(entity = %call.entity, error = %e, "extract_action_atoms — LLM call failed; skipping entity");
                        None
                    }
                };
                (call, text)
            }
        })
        .buffered(ATOM_CONCURRENCY)
        .collect()
        .await;

    let mut out: Vec<ActionAtom> = Vec::new();
    for (call, text) in responses {
        let Some(text) = text else { continue };
        let ent_name = call.entity;
        let sample_indices = call.sample_indices;

        // Tolerant JSON parse — the model sometimes wraps the array
        // in ```json fences or appends explanatory prose. Isolate
        // the first `[` to the last `]`.
        let start = text.find('[');
        let end = text.rfind(']');
        let (start, end) = match (start, end) {
            (Some(s), Some(e)) if e > s => (s, e),
            _ => continue,
        };
        let payload = &text[start..=end];
        let parsed: Vec<ActionAtomDraft> = match serde_json::from_str(payload) {
            Ok(p) => p,
            Err(e) => {
                tracing::debug!(
                    entity = %ent_name,
                    error = %e,
                    payload = %payload.chars().take(200).collect::<String>(),
                    "extract_action_atoms — parse failed; skipping entity"
                );
                continue;
            }
        };

        for draft in parsed {
            // Sanity: drop atoms whose chunk_index isn't in the
            // sampled set — the model occasionally hallucinates
            // chunk numbers when extracting.
            if !sample_indices.contains(&draft.chunk_index) {
                continue;
            }
            let evidence = draft.evidence.trim();
            if evidence.is_empty() {
                continue;
            }
            out.push(ActionAtom {
                entity: ent_name.clone(),
                verb: draft.verb.trim().to_lowercase(),
                object: draft.object.trim().to_string(),
                chunk_index: draft.chunk_index,
                evidence: evidence.chars().take(140).collect(),
            });
        }
    }

    tracing::info!(atoms = out.len(), "extract_action_atoms — done");
    out
}

#[derive(Debug, Deserialize)]
pub(super) struct ActionAtomDraft {
    pub(super) chunk_index: usize,
    pub(super) verb: String,
    pub(super) object: String,
    pub(super) evidence: String,
}

/// Generate a one-paragraph overview of the document.
pub(super) async fn generate_overview(
    inference: &Arc<dyn InferenceProvider>,
    chunks: &[TextChunk],
    doc_type: &DocumentTypeTag,
) -> String {
    let sample: String = chunks
        .iter()
        .take(5)
        .map(|c| c.content.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");

    let prompt = format!(
        "Write a single paragraph (3-5 sentences) overview of this {doc_type} document \
         based on its opening sections. Focus on: what it's about, the main entities \
         or characters, and the central question or theme.\n\n\
         Opening:\n{sample}\n\n\
         Overview:",
        doc_type = doc_type.label(),
    );

    // SLOT_POLICY §3 Housekeep: one-paragraph document overview —
    // advisory context, not durable truth. Housekeep's Some(0) think
    // budget matches this site verbatim.
    let mut request = Workload::Housekeep.request(prompt).with_output_budget(256);
    request.temperature = Some(0.3);
    inference
        .complete(&request)
        .await
        .map(|r| r.text)
        .unwrap_or_else(|_| "Overview not available.".to_string())
}
