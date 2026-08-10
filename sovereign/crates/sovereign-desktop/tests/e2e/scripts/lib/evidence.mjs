// SPDX-License-Identifier: AGPL-3.0-or-later
// Evidence resolution — the ONE implementation, shared by chaos.mjs and
// personas.mjs.
//
// WHY THIS IS A MODULE. Both harnesses resolve a turn's retrieved chunks to
// full text and then compare that pool against what the prompt actually
// carried. Until 2026-08-07 each carried its own copy, and the copies had
// already diverged (personas.mjs computed `resolutionDegraded` inside
// `splitDeliveredEvidence`; chaos.mjs recomputed it inline at the journal
// write). A paired A/B whose two arms measure with two rulers is not paired
// (`ARCH_PRINCIPLES.md` §10.6 — one decider, one name).
//
// WHAT WAS WRONG WITH THE OLD RESOLVER. It had two silent substitutions and
// no instrument on either:
//
//   1. `read_get_chunk` fails → the ~200-char `snippet` was pushed into the
//      pool indistinguishably from a full body. The grounding oracle then
//      judged 200 chars while the model had read up to MAX_CHUNK_CHARS of
//      the same passage — the oracle seeing LESS than the system, the
//      opposite direction from truncation.
//   2. No snippet either → the chunk was dropped from the pool entirely,
//      with nothing recorded. `resolved` silently under-counted.
//
// Both were invisible because the failure path was a bare `catch {}` (§9.1 —
// no untraced branch) and because degradation was INFERRED downstream by
// comparing `promptText.length > text.length`. That heuristic only fires when
// the delivered body happens to be longer than the substituted snippet: a
// short delivered body hides case 1, and case 2 is invisible to it entirely.
// Measured on arm A (2026-08-06): 21 of 99 chaos turns and 3 of 11 persona
// turns reported an impossible >100% delivery ratio, i.e. one turn in five was
// unusable and the taxonomy built on it would have been unsound.
//
// THE FIX IS PROVENANCE, NOT A BETTER GUESS. Every position records how it was
// resolved at the moment the decision is made, and nothing is ever dropped.
// `resolutionDegraded` is now a count of recorded facts rather than a symptom
// heuristic, and `resolutionErrors` carries WHY so a run can be diagnosed
// without being re-run (§18.3 — absence is reported, never defaulted).

/// Runaway bound on chunks resolved per turn.
///
/// Resolve ALL retrieved chunks, not a top-N slice: the production gate
/// grounds against `gate_evidence_chunks(&chunks)` — the entire retrieved set
/// — so an answer's supporting quote can legitimately live in a chunk ranked
/// 13th or later. Capping at 12 made the oracle judge correctly-grounded
/// answers against evidence that omitted their grounding chunk (proven
/// 2026-07-01: 13 of 15 gen75 "fabrications" had retrieved > 12). 48 sits
/// above the largest observed retrieval (39).
export const MAX_RESOLVE_CHUNKS = 48;

/// How one chunk's pool text was obtained. `full` is the only value on which
/// a "the answer is not in the evidence" verdict is safe to act.
export const RESOLUTION = Object.freeze({
  FULL: "full", // read_get_chunk returned the stored body
  SNIPPET: "snippet", // it did not; the ~200-char preview stood in
  MISSING: "missing", // it did not, and there was no snippet either
});

/// Normalize a failure into a short, groupable reason.
///
/// Kept coarse on purpose: the point is to answer "why did one turn in five
/// degrade?" across a run, not to preserve every distinct daemon message. The
/// raw text is carried on `sample` for the one case where the shape matters.
function failureReason(err, rec, rawChunkId) {
  if (err) {
    const msg = String(err?.message ?? err);
    if (/timed? ?out|timeout/i.test(msg)) return "invoke-timeout";
    if (/404|not found/i.test(msg)) return "not-found";
    // `read_get_chunk` declares `chunk_id: u64`, so Tauri rejects a
    // non-numeric id before the command body runs. Distinguished because the
    // repair is in the harness (coerce the id), not the daemon.
    if (/invalid type|invalid value|missing field|deserial/i.test(msg)) {
      return `arg-rejected(${typeof rawChunkId})`;
    }
    return "invoke-error";
  }
  // The command returns Ok(None) for a chunk the corpus does not hold — a
  // 404 on the daemon's internal route in attach mode. Not an error at any
  // layer, which is exactly why it was invisible.
  if (rec == null) return "chunk-absent";
  return "empty-content";
}

/// Resolve a turn's retrieved chunks to full text, recording how each one
/// resolved and what the prompt held for it.
///
/// `invoke` is injected because chaos.mjs and personas.mjs each own their own
/// bridge; the resolution POLICY is what must be shared, not the transport.
///
/// Every returned array is aligned 1:1 with the first `MAX_RESOLVE_CHUNKS` of
/// `chunks` — including unresolvable positions, which hold `""`. Callers that
/// need the oracle's evidence must use `pool`, which is the compacted view and
/// is byte-identical to what the pre-2026-08-07 resolver returned as `texts`.
/// That equality is deliberate: it keeps `resolved`, `chars` and `text`
/// comparable against every journal already on disk.
///
/// `inPrompt` and `promptTexts` stay three-state — present, absent, or `null`
/// meaning the runtime did not say (the planning path projects candidates
/// before a prompt exists). Null folds into neither; an unknown is reported as
/// unknown.
export async function resolveChunkTexts(chunks, invoke, { timeoutMs = 15_000 } = {}) {
  const texts = [];
  const inPrompt = [];
  const promptTexts = [];
  const resolution = [];
  // Legitimate SOURCE LABELS (corpus ids + chunk titles) — what synthesis
  // presents as `[Source: …]` headers. A re-judge needs these: a citation
  // naming a corpus id is REAL even though those words never appear in the
  // evidence BODY. Without the list, "[Source: institutional-notes]" scores
  // as fabricated (observed).
  const labels = [];
  const seenLabels = new Set();
  const addLabel = (v) => {
    const t = String(v ?? "").trim();
    if (t && !seenLabels.has(t)) {
      seenLabels.add(t);
      labels.push(t);
    }
  };

  const errors = new Map(); // reason → { reason, count, sample }
  const noteFailure = (reason, sample) => {
    const hit = errors.get(reason);
    if (hit) hit.count += 1;
    else errors.set(reason, { reason, count: 1, sample: String(sample ?? "").slice(0, 200) });
  };

  const push = (text, how, c) => {
    texts.push(String(text));
    resolution.push(how);
    inPrompt.push(c?.in_prompt ?? c?.inPrompt ?? null);
    const pt = c?.prompt_text ?? c?.promptText ?? null;
    promptTexts.push(typeof pt === "string" ? pt : null);
  };

  for (const c of (chunks ?? []).slice(0, MAX_RESOLVE_CHUNKS)) {
    const corpusId = c?.corpus_id ?? c?.corpusId;
    const rawChunkId = c?.chunk_id ?? c?.chunkId;
    addLabel(corpusId);
    addLabel(c?.title);

    if (corpusId == null || rawChunkId == null) {
      noteFailure("no-ids", `corpusId=${corpusId} chunkId=${rawChunkId}`);
    } else {
      // The command declares `chunk_id: u64`. A numeric string is the common
      // shape on the wire and Tauri will reject it, so coerce here and record
      // the id we could NOT coerce rather than letting it fail as a generic
      // invoke error.
      const chunkId = typeof rawChunkId === "number" ? rawChunkId : Number(rawChunkId);
      if (!Number.isFinite(chunkId)) {
        noteFailure(`non-numeric-id(${typeof rawChunkId})`, rawChunkId);
      } else {
        let rec = null;
        let err = null;
        try {
          rec = await invoke("read_get_chunk", { corpusId, chunkId }, timeoutMs);
        } catch (e) {
          err = e;
        }
        addLabel(rec?.title);
        const content = err ? null : (rec?.content ?? rec?.text);
        if (content) {
          push(content, RESOLUTION.FULL, c);
          continue;
        }
        noteFailure(failureReason(err, rec, rawChunkId), err ?? `${corpusId}/${rawChunkId}`);
      }
    }

    // Resolution failed. Record WHICH degraded state this is — the snippet is
    // real text and still worth judging, but it is NOT the stored body and a
    // verdict of "not in the evidence" must not be read off it.
    if (c?.snippet) push(c.snippet, RESOLUTION.SNIPPET, c);
    else push("", RESOLUTION.MISSING, c);
  }

  const count = (how) => resolution.filter((r) => r === how).length;
  return {
    texts,
    resolution,
    inPrompt,
    promptTexts,
    labels,
    // The oracle's evidence: the compacted, non-empty view. Byte-identical to
    // the pre-fix `texts`, so journals stay comparable.
    pool: texts.filter((t) => t.length > 0),
    resolvedFull: count(RESOLUTION.FULL),
    resolvedSnippet: count(RESOLUTION.SNIPPET),
    resolvedMissing: count(RESOLUTION.MISSING),
    // Positions where the pool is NOT the stored body, so the oracle's view is
    // not authoritative. A recorded fact, not a length heuristic.
    resolutionDegraded: count(RESOLUTION.SNIPPET) + count(RESOLUTION.MISSING),
    resolutionErrors: [...errors.values()].sort((a, b) => b.count - a.count),
  };
}

/// Reduce resolved evidence to exactly what the prompt carried.
///
/// When the runtime reported `prompt_text` for at least one chunk, `delivered`
/// IS the prompt's evidence — the formatter has already applied both
/// truncation and eviction. When it reported none (an older binary, or the
/// planning path), `known` is false and `delivered` falls back to the pool:
/// the honest statement is then "this turn cannot distinguish the two", not a
/// fabricated split. Read `known` before reading any ratio off this.
export function splitDeliveredEvidence(texts, inPrompt, promptTexts) {
  const known = promptTexts.some((t) => typeof t === "string");
  const delivered = known ? promptTexts.filter((t) => typeof t === "string" && t.length) : texts;
  const chars = (xs) => xs.reduce((n, x) => n + x.length, 0);
  return {
    delivered,
    known,
    // Chunks the budget dropped entirely. Kept separate from the char figures
    // because the two losses need different fixes: eviction argues for packing
    // more chunks, truncation for longer ones.
    evicted: inPrompt.filter((f) => f === false).length,
    deliveredChars: chars(delivered),
    resolvedChars: chars(texts),
  };
}
