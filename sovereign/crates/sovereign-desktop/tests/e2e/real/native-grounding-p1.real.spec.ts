// SPDX-License-Identifier: AGPL-3.0-or-later
// P1's DESKTOP RENDERING WITNESS — the real app, a real turn, both arms.
//
// Order `native-grounding-p1-desktop`, deliverable (2)'s proof: the
// desktop renders `metadata.answer_segments` and the typed abstention on
// flag-on turns, and renders NOTHING on flag-off ones. The unit suite
// (`src/lib/components/answerProvenance.test.ts`) already pins the
// reading of a hand-written metadata blob; what it cannot say is whether
// a REAL turn through a REAL daemon puts those keys on the wire at all.
// That is this file's whole job.
//
// ── THE TWO ARMS ──────────────────────────────────────────────────────
// The flag is read once, in the process that runs the turn
// (`native_grounding::admission::native_grounding_enabled`,
// admission.rs:153) — under this harness that is the managed daemon
// global-setup.ts spawns, which inherits our env. So the arm is chosen by
// the env of the `playwright test` invocation, and this spec asserts the
// branch that arm implies:
//
//   off  →  `metadata.answer_segments` is null/absent AND no strip renders.
//   on   →  segments present, every Grounded row carries an openable
//           address, and the strip is on screen.
//
// Absent and empty are different facts and are asserted differently
// (ARCH §18.3): a flag-off turn has no measurement, a flag-on turn that
// resolved nothing has a zero-count one.
//
// ── VALIDATE THE INSTRUMENT BEFORE THE RESULT (ARCH §18.4) ────────────
// H1 needs a cross-encoder. With no reranker wired and no `rerank_score`
// left on the chunks by retrieval, `admit()` returns `NoInstrument`
// (admission.rs:264), `native_verdict` is None (knowledge_query.rs:678),
// `answer_segments` is never computed (streaming.rs:1751) — and a flag-on
// run would render exactly what a flag-off run renders while reporting
// itself as "flag on". `beforeAll` REFUSES that run rather than filming
// it, the same refusal `bench/calibration/ab/run_ab.sh` makes.
//
// Keep `SOVEREIGN_RERANK_MODEL_PATH` set in BOTH arms — it changes which
// chunks survive retrieval, so an arm-pair that differs in the reranker
// too is measuring two changes (run_ab.sh's header states this for the
// bench A/B; the same holds here).
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { assertTurnInvariants, sendAndAwaitTurn } from "./invariants";
import { expect, realBootToChat, test } from "./test-base-real";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const ARTIFACTS = path.resolve(__dirname, "../../../test-artifacts");
const FIXTURE_INFO = path.join(ARTIFACTS, "real-fixture.json");

/** Mirrors `native_grounding_enabled()` (admission.rs:153) exactly — same
 *  three spellings, same trim, same default-off. One question, one
 *  answer (ARCH §10.6). */
const FLAG_ON = ((): boolean => {
  const v = (process.env.SOVEREIGN_NATIVE_GROUNDING ?? "").trim();
  return v === "1" || v.toLowerCase() === "true" || v.toLowerCase() === "on";
})();
const ARM = FLAG_ON ? "on" : "off";

/** Wire shape of one `AnswerSegment` (grounding_verdict.rs::SegmentKind). */
interface SegmentWire {
  text_range: { start: number; end: number };
  kind: {
    kind: string;
    chunk_id?: string;
    address?: { corpus_id: string; chunk_id: number } | null;
  };
}

interface GateMeta {
  action?: string;
  native_answerability?: number | null;
  native_decision?: string | null;
}

function writeArtifact(name: string, body: unknown): string {
  fs.mkdirSync(ARTIFACTS, { recursive: true });
  const file = path.join(ARTIFACTS, name);
  fs.writeFileSync(file, JSON.stringify(body, null, 2));
  return file;
}

test.beforeAll(() => {
  if (FLAG_ON && !process.env.SOVEREIGN_RERANK_MODEL_PATH) {
    throw new Error(
      "REFUSING the flag-on arm: SOVEREIGN_NATIVE_GROUNDING is on but no " +
        "SOVEREIGN_RERANK_MODEL_PATH is set. H1 would report NoInstrument on " +
        "every turn (admission.rs:264), no verdict would ride the turn, no " +
        "segments would be computed (streaming.rs:1751) — and this spec would " +
        "film a flag-off screen while calling it flag-on. Set the reranker (the " +
        "same qwen3-reranker-0.6b-q8_0 the H1 calibration was fitted on) in BOTH " +
        "arms, or run the off arm.",
    );
  }
  console.log(
    `[p1-render] arm=${ARM} rerank=${process.env.SOVEREIGN_RERANK_MODEL_PATH ?? "(unset)"}`,
  );
});

test("P1 provenance strip: a sealed knowledge turn renders segments iff the native path ran", async ({
  sovereignPage: page,
  bridge,
}) => {
  const fixture = JSON.parse(fs.readFileSync(FIXTURE_INFO, "utf8")) as {
    corpus_id: string;
  };

  // Same seam the golden spec uses: a conversation sealed to the fixture
  // corpus, so retrieval has real passages to ground against and the
  // Grounded addresses point somewhere this app instance can read.
  const conv = await bridge.invoke<{ id: string }>("create_conversation");
  await bridge.invoke("rename_conversation", {
    conversationId: conv.id,
    title: `p1-provenance-${ARM}`,
  });
  await bridge.invoke("set_conversation_enabled_corpora", {
    conversationId: conv.id,
    enabledCorpora: [fixture.corpus_id],
  });

  await realBootToChat(page);
  await page
    .locator(".convo-title", { hasText: `p1-provenance-${ARM}` })
    .first()
    .click();

  const t0 = Date.now();
  const messageId = await sendAndAwaitTurn(
    page,
    "When was the Meridian Lighthouse automated, and how tall is the tower?",
  );
  const wallMs = Date.now() - t0;

  const facts = await assertTurnInvariants(page, bridge, messageId, {
    requireCitations: true,
  });
  const meta = (facts.complete.metadata ?? {}) as Record<string, unknown>;
  const raw = meta.answer_segments as SegmentWire[] | null | undefined;
  const gate = (meta.grounding_gate ?? null) as GateMeta | null;
  const strip = page.locator(".sv-ai-msg").last().getByTestId("answer-provenance");

  // Per-turn latency, from the runtime's own provenance rather than the
  // harness's stopwatch (the stopwatch also carries UI polling), plus the
  // wall time for context. Written per arm so the two runs can be diffed.
  const provenance = (meta.provenance ?? {}) as { total_latency_ms?: number };

  if (!FLAG_ON) {
    // NOT COMPUTED. `answer_segments` serialises from an Option
    // (streaming.rs:2137), so the wire carries JSON null; `== null`
    // covers null and absent alike.
    expect(
      raw == null,
      `flag-off turn must carry no segments, got ${JSON.stringify(raw)?.slice(0, 200)}`,
    ).toBe(true);
    await expect(
      strip,
      "the provenance strip must not render on a flag-off turn — the " +
        "incumbent bubble is unchanged, which is what makes the A/B a " +
        "comparison of one thing",
    ).toHaveCount(0);
    const file = writeArtifact(`p1-desktop-${ARM}.json`, {
      arm: ARM,
      message_id: messageId,
      total_latency_ms: provenance.total_latency_ms ?? null,
      wall_ms: wallMs,
      segments: null,
      gate_action: gate?.action ?? null,
      native_answerability: gate?.native_answerability ?? null,
      citations: facts.citations.length,
    });
    console.log(`[p1-render] arm=off wrote ${file}`);
    return;
  }

  // ── flag-on ──
  expect(
    Array.isArray(raw),
    "flag-on turn carries no answer_segments. Either the daemon this app " +
      "attached to does not have SOVEREIGN_NATIVE_GROUNDING set (the flag is " +
      "read in the process that runs the turn, not in the app), or H1 found " +
      "no instrument — check the daemon log for " +
      '"no answerability instrument".',
  ).toBe(true);
  const segs = raw as SegmentWire[];
  expect(segs.length, "flag-on turn segmented into nothing").toBeGreaterThan(0);

  const grounded = segs.filter((s) => s.kind?.kind === "grounded");
  const addressed = grounded.filter((s) => s.kind.address != null);
  // The P1 citability bar (parity plan §4.1): every Grounded badge
  // resolves. `chunk_id` is a POOL INDEX and opens nothing — `address` is
  // the handle the reading surface takes (streaming.rs:1777).
  expect(
    addressed.length,
    `citability: ${grounded.length - addressed.length} of ${grounded.length} ` +
      `Grounded segments carry no openable address`,
  ).toBe(grounded.length);

  // Every address must dereference through the same command the reading
  // desk uses — a badge that opens a null is worse than no badge.
  for (const s of addressed) {
    const a = s.kind.address!;
    const chunk = await bridge.invoke<{ content: string } | null>("read_get_chunk", {
      corpusId: a.corpus_id,
      chunkId: a.chunk_id,
    });
    expect(
      chunk && typeof chunk.content === "string" && chunk.content.length > 0,
      `Grounded segment address (${a.corpus_id}, ${a.chunk_id}) does not resolve`,
    ).toBe(true);
  }

  // ── the UI half: the strip is on screen and says what the wire says ──
  await expect(strip, "the P1 provenance strip did not render").toHaveCount(1);
  const summary = (await strip.locator("summary").textContent()) ?? "";
  expect(summary).toMatch(/Provenance:/);
  expect(summary).toContain(`${addressed.length} of ${segs.length}`);

  await strip.locator("summary").click();
  await expect(strip.locator("li.ap-row")).toHaveCount(segs.length);
  await expect(strip.locator("li.ap-row.ap-grounded")).toHaveCount(grounded.length);
  // "no openable address" is the honest rendering of an unresolved slot;
  // at the P1 bar there must be none of them.
  await expect(
    strip.locator(".ap-noaddr"),
    "a Grounded row rendered as un-openable — the citability bar is not met",
  ).toHaveCount(0);
  if (grounded.length > 0) {
    await expect(strip.locator("button.ap-open").first()).toBeVisible();
  }

  // The operator-facing evidence: the bubble with the strip open.
  const shot = path.join(ARTIFACTS, `p1-desktop-${ARM}-provenance.png`);
  await page.locator(".sv-ai-msg").last().screenshot({ path: shot });

  const file = writeArtifact(`p1-desktop-${ARM}.json`, {
    arm: ARM,
    message_id: messageId,
    total_latency_ms: provenance.total_latency_ms ?? null,
    wall_ms: wallMs,
    segments: segs.length,
    grounded: grounded.length,
    grounded_addressed: addressed.length,
    unverified: segs.filter((s) => s.kind?.kind === "unverified").length,
    parametric: segs.filter((s) => s.kind?.kind === "parametric").length,
    inference: segs.filter((s) => s.kind?.kind === "inference").length,
    gate_action: gate?.action ?? null,
    native_answerability: gate?.native_answerability ?? null,
    native_decision: gate?.native_decision ?? null,
    citations: facts.citations.length,
    screenshot: shot,
  });
  console.log(
    `[p1-render] arm=on segments=${segs.length} grounded=${grounded.length} ` +
      `addressed=${addressed.length} latency_ms=${provenance.total_latency_ms ?? "?"} → ${file}`,
  );
});

test("P1 grounded row: a verbatim-quote turn renders an OPENABLE Grounded badge", async ({
  sovereignPage: page,
  bridge,
}) => {
  test.skip(!FLAG_ON, "flag-off turns carry no segments — nothing to open");
  const fixture = JSON.parse(fs.readFileSync(FIXTURE_INFO, "utf8")) as {
    corpus_id: string;
  };

  const conv = await bridge.invoke<{ id: string }>("create_conversation");
  await bridge.invoke("rename_conversation", {
    conversationId: conv.id,
    title: `p1-grounded-${ARM}`,
  });
  await bridge.invoke("set_conversation_enabled_corpora", {
    conversationId: conv.id,
    enabledCorpora: [fixture.corpus_id],
  });

  await realBootToChat(page);
  await page
    .locator(".convo-title", { hasText: `p1-grounded-${ARM}` })
    .first()
    .click();

  // A Grounded segment needs the released text to contain a passage
  // sentence VERBATIM (span_resolver: verbatim containment, nothing
  // looser). The first witness turn paraphrased and produced zero
  // grounded rows, so this probe asks for the quote directly. It is a
  // RENDERING witness, not a quality probe — whether the model complies
  // is its own business, and a paraphrase is a skip, not a failure.
  const messageId = await sendAndAwaitTurn(
    page,
    "Quote the exact sentence from the source that states how tall the tower is. " +
      "Reply with that one sentence copied word for word, and nothing else.",
  );
  const facts = await assertTurnInvariants(page, bridge, messageId, {});
  const meta = (facts.complete.metadata ?? {}) as Record<string, unknown>;
  const raw = meta.answer_segments as SegmentWire[] | null | undefined;
  expect(Array.isArray(raw), "flag-on turn carries no answer_segments").toBe(true);
  const segs = raw as SegmentWire[];
  const grounded = segs.filter((s) => s.kind?.kind === "grounded");

  writeArtifact(`p1-desktop-${ARM}-grounded.json`, {
    arm: ARM,
    message_id: messageId,
    segments: segs.length,
    grounded: grounded.length,
    grounded_addressed: grounded.filter((s) => s.kind.address != null).length,
    kinds: segs.map((s) => s.kind?.kind),
    full_text: facts.complete.full_text,
  });

  // COULD-NOT-JUDGE, not passed. The model decides whether it copies a
  // sentence; when it paraphrases there is no Grounded row and the
  // assertions below would pass at 0 === 0 — a check with no failing
  // input (ARCH §18.1).
  test.skip(
    grounded.length === 0,
    `the model paraphrased — ${segs.length} segments, none grounded, so the ` +
      `openable-badge rendering was not exercised on this run`,
  );

  const strip = page.locator(".sv-ai-msg").last().getByTestId("answer-provenance");
  await expect(strip).toHaveCount(1);
  await strip.locator("summary").click();
  await expect(strip.locator("li.ap-row.ap-grounded")).toHaveCount(grounded.length);
  await expect(strip.locator(".ap-noaddr")).toHaveCount(0);

  for (const s of grounded) {
    const a = s.kind.address!;
    const chunk = await bridge.invoke<{ content: string } | null>("read_get_chunk", {
      corpusId: a.corpus_id,
      chunkId: a.chunk_id,
    });
    expect(
      chunk && typeof chunk.content === "string" && chunk.content.length > 0,
      `Grounded address (${a.corpus_id}, ${a.chunk_id}) does not resolve`,
    ).toBe(true);
  }

  const shot = path.join(ARTIFACTS, `p1-desktop-${ARM}-grounded.png`);
  await page.locator(".sv-ai-msg").last().screenshot({ path: shot });

  // The end of the chain the badge promises: clicking it opens the same
  // reading surface an inline citation opens.
  await strip.locator("button.ap-open").first().click();
  await expect(page.locator(".reading-surface")).toBeVisible({ timeout: 30_000 });
  await page.screenshot({ path: path.join(ARTIFACTS, `p1-desktop-${ARM}-reading.png`) });
  console.log(
    `[p1-render] arm=${ARM} grounded=${grounded.length} badge opened the reading surface`,
  );
});

test("P1 typed abstention: an unanswerable turn discloses that it withheld", async ({
  sovereignPage: page,
  bridge,
}) => {
  const fixture = JSON.parse(fs.readFileSync(FIXTURE_INFO, "utf8")) as {
    corpus_id: string;
  };

  const conv = await bridge.invoke<{ id: string }>("create_conversation");
  await bridge.invoke("rename_conversation", {
    conversationId: conv.id,
    title: `p1-abstention-${ARM}`,
  });
  await bridge.invoke("set_conversation_enabled_corpora", {
    conversationId: conv.id,
    enabledCorpora: [fixture.corpus_id],
  });

  await realBootToChat(page);
  await page
    .locator(".convo-title", { hasText: `p1-abstention-${ARM}` })
    .first()
    .click();

  // Sealed to a three-document corpus about a lighthouse, a marsh and a
  // lamp mechanism, and asked about something none of them mentions.
  const messageId = await sendAndAwaitTurn(
    page,
    "What is the annual maintenance budget for the Kestrel Ridge wind turbines?",
  );
  const facts = await assertTurnInvariants(page, bridge, messageId, {});
  const meta = (facts.complete.metadata ?? {}) as Record<string, unknown>;
  const gate = (meta.grounding_gate ?? null) as GateMeta | null;
  const action = gate?.action ?? null;
  const abstention = page.locator(".sv-ai-msg").last().getByTestId("typed-abstention");

  writeArtifact(`p1-desktop-${ARM}-abstention.json`, {
    arm: ARM,
    message_id: messageId,
    gate_action: action,
    native_answerability: gate?.native_answerability ?? null,
    native_decision: gate?.native_decision ?? null,
    full_text: facts.complete.full_text,
  });

  // COULD-NOT-JUDGE, not passed. The gate decides whether a turn
  // abstained; this spec cannot make it. When it released instead, the
  // typed-abstention branch was never exercised and saying so as a SKIP
  // keeps the four verdicts distinct (ARCH §18.1) — a green tick here
  // would claim a rendering nobody watched.
  test.skip(
    !(action ?? "").startsWith("abstained"),
    `the gate released this turn (action=${JSON.stringify(action)}) — the typed ` +
      `abstention branch was not exercised on this run`,
  );

  await expect(
    abstention,
    "the gate abstained but the bubble carries no typed disclosure",
  ).toHaveCount(1);
  await expect(abstention).toContainText("withheld rather than guessed");
  if (FLAG_ON && typeof gate?.native_answerability === "number") {
    // Telemetry, and the copy must say so — it did not decide this turn
    // (parity plan §4.1: admission is never enforced at P1).
    await expect(abstention.locator(".ta-score")).toContainText("answerability");
  }
  const shot = path.join(ARTIFACTS, `p1-desktop-${ARM}-abstention.png`);
  await page.locator(".sv-ai-msg").last().screenshot({ path: shot });
  console.log(`[p1-render] arm=${ARM} abstention action=${action} → ${shot}`);
});
