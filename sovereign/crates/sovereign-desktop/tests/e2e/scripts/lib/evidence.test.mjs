// SPDX-License-Identifier: AGPL-3.0-or-later
// node --test tests/e2e/scripts/lib/evidence.test.mjs
//
// These assert the property the old resolver violated: the pool never
// substitutes or omits without saying so. Each was watched RED against the
// pre-2026-08-07 behaviour (snippet pushed indistinguishably; unresolvable
// chunk dropped; degradation inferred from a length comparison).

import test from "node:test";
import assert from "node:assert/strict";
import { resolveChunkTexts, splitDeliveredEvidence, RESOLUTION } from "./evidence.mjs";

/// A bridge whose `read_get_chunk` behaviour is scripted per chunk id.
const stubBridge = (byId) => async (_cmd, { chunkId }) => {
  const behaviour = byId[chunkId];
  if (behaviour instanceof Error) throw behaviour;
  return behaviour ?? null;
};

const chunk = (id, extra = {}) => ({
  corpus_id: "c",
  chunk_id: id,
  snippet: "SNIPPET",
  ...extra,
});

test("a full resolution is recorded as full and carries the stored body", async () => {
  const r = await resolveChunkTexts([chunk(1)], stubBridge({ 1: { content: "FULL BODY" } }));
  assert.deepEqual(r.resolution, [RESOLUTION.FULL]);
  assert.deepEqual(r.pool, ["FULL BODY"]);
  assert.equal(r.resolutionDegraded, 0);
  assert.deepEqual(r.resolutionErrors, []);
});

test("a snippet substitution is NAMED, not silently pooled as if it were the body", async () => {
  // The old resolver pushed the snippet and returned nothing distinguishing
  // it — the oracle then judged 200 chars while the model read up to 600.
  const r = await resolveChunkTexts([chunk(1)], stubBridge({ 1: null }));
  assert.deepEqual(r.resolution, [RESOLUTION.SNIPPET]);
  assert.deepEqual(r.pool, ["SNIPPET"]);
  assert.equal(r.resolutionDegraded, 1);
  assert.equal(r.resolvedSnippet, 1);
  assert.equal(r.resolutionErrors[0].reason, "chunk-absent");
});

test("an unresolvable chunk is recorded as missing, never dropped", async () => {
  // The old resolver pushed nothing at all here, so `resolved` under-counted
  // with no record anywhere that a chunk had vanished.
  const chunks = [chunk(1, { snippet: null }), chunk(2)];
  const r = await resolveChunkTexts(chunks, stubBridge({ 2: { content: "B" } }));
  assert.equal(r.resolution.length, 2, "arrays stay 1:1 with the retrieved chunks");
  assert.equal(r.resolution[0], RESOLUTION.MISSING);
  assert.equal(r.texts[0], "");
  assert.equal(r.resolvedMissing, 1);
  assert.deepEqual(r.pool, ["B"], "the compacted pool still holds only real text");
});

test("degradation is counted even when the delivered body is SHORTER than the snippet", async () => {
  // The exact case the old length heuristic missed: it only fired when
  // promptText.length > text.length, so a short delivered body hid the
  // substitution entirely.
  const c = chunk(1, { snippet: "A".repeat(200), prompt_text: "A".repeat(10) });
  const r = await resolveChunkTexts([c], stubBridge({ 1: null }));
  assert.equal(r.resolutionDegraded, 1);
});

test("failure reasons are grouped and counted so a run can be diagnosed", async () => {
  const chunks = [chunk(1), chunk(2), chunk(3, { chunk_id: "not-a-number" })];
  const r = await resolveChunkTexts(chunks, stubBridge({ 1: null, 2: null }));
  const byReason = Object.fromEntries(r.resolutionErrors.map((e) => [e.reason, e.count]));
  assert.equal(byReason["chunk-absent"], 2);
  assert.ok("non-numeric-id(string)" in byReason, "a non-coercible id is its own reason");
});

test("a thrown invoke is classified, not swallowed", async () => {
  const r = await resolveChunkTexts(
    [chunk(1)],
    stubBridge({ 1: new Error("invoke timed out after 15000ms") }),
  );
  assert.equal(r.resolutionErrors[0].reason, "invoke-timeout");
  assert.equal(r.resolution[0], RESOLUTION.SNIPPET);
});

test("a numeric-string chunk id is coerced rather than rejected by the u64 command", async () => {
  // `read_get_chunk` declares `chunk_id: u64`; Tauri rejects "7" before the
  // command body runs, which surfaced only as a generic invoke failure.
  let sawType = null;
  const r = await resolveChunkTexts([chunk("7")], async (_cmd, { chunkId }) => {
    sawType = typeof chunkId;
    return { content: "OK" };
  });
  assert.equal(sawType, "number");
  assert.deepEqual(r.pool, ["OK"]);
});

test("splitDeliveredEvidence reports unknown as unknown rather than a fabricated split", async () => {
  const r = await resolveChunkTexts([chunk(1)], stubBridge({ 1: { content: "FULL" } }));
  const s = splitDeliveredEvidence(r.pool, r.inPrompt, r.promptTexts);
  assert.equal(s.known, false, "the runtime reported no prompt view");
  assert.deepEqual(s.delivered, ["FULL"], "delivered falls back to the pool, by ignorance");
  assert.equal(s.deliveredChars, s.resolvedChars);
});

test("evicted counts chunks the formatter dropped, including unresolvable ones", async () => {
  // Aligning the flags to the RETRIEVED list (not to the resolved subset) is
  // what makes this countable at all.
  const chunks = [
    chunk(1, { in_prompt: false, snippet: null }),
    chunk(2, { in_prompt: true, prompt_text: "P" }),
  ];
  const r = await resolveChunkTexts(chunks, stubBridge({ 2: { content: "FULL" } }));
  const s = splitDeliveredEvidence(r.pool, r.inPrompt, r.promptTexts);
  assert.equal(s.evicted, 1);
  assert.equal(s.known, true);
  assert.deepEqual(s.delivered, ["P"]);
});
