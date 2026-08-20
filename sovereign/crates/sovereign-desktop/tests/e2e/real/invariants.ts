// SPDX-License-Identifier: AGPL-3.0-or-later
// The invariant pack: glassbox assertions every real-mode turn must
// satisfy, regardless of what the spec is otherwise testing. This is
// what turns automated exercising into an automated bug bash — each
// turn is self-checking, so a spec doesn't need to anticipate a
// failure mode to catch it.
//
// Invariants (see plan Phase 2):
//   1. Stream integrity  — concat(message-chunk) === message-complete
//      .full_text, no SSE lag holes, complete arrives exactly once.
//   2. Provenance present — metadata.provenance.intent is a non-empty
//      string; latency + finish_reason are sane. (TurnProvenance via
//      get_last_turn_provenance is inner-work-only — asserted in the
//      inner-work spec, not here.)
//   3. Citations resolve — every retrieved_chunk carries (corpus_id,
//      chunk_id); for corpora installed in THIS app instance the pair
//      must resolve via read_get_chunk (no dangling provenance).
//      Attach-mode caveat: chunks served by the external daemon's
//      corpora aren't readable through this instance's reading
//      surface, so resolution is only enforced for locally-installed
//      corpus ids; shape is enforced for all.
//   4. Numeric honesty — when the runtime ran its numeric audit
//      (provenance.self_assessment), the verdict must not report
//      untraceable figures.
//   5. Page errors / fatal Svelte diagnostics — enforced continuously
//      by the sovereignPage fixture in test-base-real.ts.
import { expect, type Page } from "@playwright/test";

export interface RetrievedChunkMeta {
  title: string;
  corpus_id: string;
  chunk_id: number;
  url?: string | null;
  snippet?: string | null;
  provenance_tier?: string | null;
}

export interface CompletePayload {
  message_id: string;
  full_text: string;
  metadata: {
    provenance?: {
      intent?: string;
      total_latency_ms?: number;
      finish_reason?: string;
      self_assessment?: string;
      sources?: Array<{ origin: string; count: number }>;
    };
    retrieved_chunks?: RetrievedChunkMeta[];
    intent?: string;
  } | null;
}

export interface TurnFacts {
  complete: CompletePayload;
  chunkCount: number;
  citations: RetrievedChunkMeta[];
}

interface BridgeLike {
  invoke<T = unknown>(cmd: string, args?: Record<string, unknown>): Promise<T>;
}

export interface TurnInvariantOptions {
  /** Expected finish_reason (exact). When omitted, any of
   *  "stop" / "length" is accepted — "length" is a legitimate outcome
   *  for verbose synthesis under a token budget, not a stream defect
   *  (the desktop renders it as the truncation chip). Pass null to
   *  skip the check entirely. */
  expectFinish?: string | null;
  /** Require at least one retrieved chunk (knowledge-grounded turns). */
  requireCitations?: boolean;
}

/** Pull the captured stream rows for one message id out of the page. */
async function turnCapture(page: Page, messageId: string) {
  return page.evaluate((mid) => {
    const api = window.__sovereign_real__;
    const completes = api.captured.filter(
      (r) =>
        r.event === "message-complete" &&
        (r.payload as { message_id?: string })?.message_id === mid,
    );
    return {
      chunks: api.chunksFor(mid),
      completes: completes.map((r) => r.payload),
      lagged: api.lagged(),
    };
  }, messageId);
}

export async function assertTurnInvariants(
  page: Page,
  bridge: BridgeLike,
  messageId: string,
  opts: TurnInvariantOptions = {},
): Promise<TurnFacts> {
  const cap = await turnCapture(page, messageId);

  // ── 1. Stream integrity ──
  expect(cap.lagged, "SSE consumer lagged — stream assertions invalid").toBe(false);
  expect(
    cap.completes.length,
    `expected exactly one message-complete for ${messageId}`,
  ).toBe(1);
  const complete = cap.completes[0] as CompletePayload;
  const concat = cap.chunks.join("");
  expect(
    concat,
    "concat(message-chunk) must equal message-complete.full_text byte-for-byte",
  ).toBe(complete.full_text);

  // ── 2. Glassbox intent presence ──
  // Two metadata contracts coexist (see routing_replay.rs): referential
  // handlers attach a full ResponseProvenance under `provenance`;
  // speech-act handlers (conation/commissive/expressive/metalingual)
  // attach a top-level `intent` only. Every turn must carry ONE of
  // them — a turn with neither is invisible to the user's provenance
  // surfaces.
  const meta = complete.metadata;
  expect(meta, "message-complete.metadata must be present").toBeTruthy();
  const prov = meta?.provenance;
  const intent = prov?.intent ?? meta?.intent;
  expect(
    typeof intent === "string" && intent.length > 0,
    `turn carries no intent in provenance.intent OR metadata.intent ` +
      `(metadata keys: ${Object.keys(meta ?? {}).join(", ")})`,
  ).toBe(true);
  if (opts.expectFinish !== null && prov) {
    if (opts.expectFinish !== undefined) {
      expect(
        prov.finish_reason,
        `finish_reason should be ${JSON.stringify(opts.expectFinish)}`,
      ).toBe(opts.expectFinish);
    } else if (prov.finish_reason !== undefined) {
      expect(
        ["stop", "length"],
        `unexpected finish_reason ${JSON.stringify(prov.finish_reason)}`,
      ).toContain(prov.finish_reason);
    }
  }

  // ── 3. Citations resolve ──
  const citations = (meta?.retrieved_chunks ?? []) as RetrievedChunkMeta[];
  if (opts.requireCitations) {
    expect(
      citations.length,
      "knowledge-grounded turn must carry retrieved_chunks",
    ).toBeGreaterThan(0);
  }
  if (citations.length > 0) {
    const localIds = await localCorpusIds(bridge);
    for (const c of citations) {
      // Web results cite by URL, not by chunk handle.
      if (c.provenance_tier === "web") continue;
      expect(
        typeof c.corpus_id === "string" && c.corpus_id.length > 0,
        `citation missing corpus_id: ${JSON.stringify(c)}`,
      ).toBe(true);
      expect(
        Number.isFinite(c.chunk_id),
        `citation missing chunk_id: ${JSON.stringify(c)}`,
      ).toBe(true);
      if (!localIds.has(c.corpus_id)) continue; // attach-mode external corpus
      const chunk = await bridge.invoke<{ content: string } | null>("read_get_chunk", {
        corpusId: c.corpus_id,
        chunkId: c.chunk_id,
      });
      expect(
        chunk,
        `dangling citation: read_get_chunk(${c.corpus_id}, ${c.chunk_id}) returned null`,
      ).toBeTruthy();
      expect(
        (chunk as { content: string }).content.length,
        `citation resolved to an empty chunk: read_get_chunk(${c.corpus_id}, ` +
          `${c.chunk_id}) returned a record with no content — the handle is ` +
          `live but the text behind it is gone`,
      ).toBeGreaterThan(0);
    }
  }

  // ── 4. Numeric honesty ──
  // The MESSAGE carries the answer text; the CONDITION is untouched. Ring 2
  // of `sec-filings-close` (2026-08-18) failed here naming four numerals —
  // `2024, 2023, 2027, 40%` — and nothing in the run's evidence recorded the
  // prose they came from, so "is 2023 a prose year or a figure claiming to be
  // a datum?" could not be decided without re-running a live SEC install. An
  // assertion that judges text must print the text it judged.
  if (prov?.self_assessment) {
    expect(
      prov.self_assessment.includes("not traceable"),
      `numeric audit failed: ${prov.self_assessment}\n` +
        `--- the answer text this audit judged ---\n${complete.full_text}`,
    ).toBe(false);
  }

  return { complete, chunkCount: cap.chunks.length, citations };
}

/** Corpus ids installed in THIS app instance (resolvable via the
 *  reading surface). Includes catalog installs + local-folder corpora. */
async function localCorpusIds(bridge: BridgeLike): Promise<Set<string>> {
  const ids = new Set<string>();
  try {
    const lc = await bridge.invoke<Array<{ corpus_id: string }>>("lc_list");
    for (const c of lc) ids.add(c.corpus_id);
  } catch {
    /* none registered */
  }
  try {
    const corpora = await bridge.invoke<Array<{ id?: string; corpus_id?: string }>>(
      "list_corpora",
    );
    for (const c of corpora) {
      const id = c.corpus_id ?? c.id;
      if (id) ids.add(id);
    }
  } catch {
    /* listing unavailable */
  }
  return ids;
}

/** Send a message through the real UI and wait for its terminal
 *  message-complete. Returns the message id for invariant assertions. */
export async function sendAndAwaitTurn(
  page: Page,
  text: string,
  opts: { timeoutMs?: number } = {},
): Promise<string> {
  const before = await page.evaluate(
    () =>
      window.__sovereign_real__.captured.filter((r) => r.event === "message-complete")
        .length,
  );
  const input = page.locator(".input-area textarea");
  await input.fill(text);
  await page.locator(".send-btn").click();
  await expect
    .poll(
      () =>
        page.evaluate(
          () =>
            window.__sovereign_real__.captured.filter(
              (r) => r.event === "message-complete",
            ).length,
        ),
      { timeout: opts.timeoutMs ?? 150_000, intervals: [500, 1000, 2000] },
    )
    .toBeGreaterThan(before);
  return page.evaluate(() => {
    const completes = window.__sovereign_real__.captured.filter(
      (r) => r.event === "message-complete",
    );
    return (completes[completes.length - 1].payload as { message_id: string })
      .message_id;
  });
}
