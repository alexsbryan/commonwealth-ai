// SPDX-License-Identifier: AGPL-3.0-or-later
// The knowledge-grounded golden path against the real stack: a
// conversation sealed to the fixture corpus must answer from it, cite
// it, and every citation must resolve through the reading surface.
// This is the strongest form of the glassbox contract: the chain
// user question → retrieval → cited chunk → re-readable source text
// is verified end to end with no synthetic links.
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { assertTurnInvariants, sendAndAwaitTurn } from "./invariants";
import { expect, realBootToChat, test } from "./test-base-real";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const FIXTURE_INFO = path.resolve(__dirname, "../../../test-artifacts/real-fixture.json");

test("sealed knowledge turn answers from the fixture corpus with resolvable citations", async ({
  sovereignPage: page,
  bridge,
}) => {
  const fixture = JSON.parse(fs.readFileSync(FIXTURE_INFO, "utf8")) as {
    corpus_id: string;
  };

  // Create + seal the conversation through the same commands the
  // corpus chip strip uses, with a recognizable title to select by.
  const conv = await bridge.invoke<{ id: string }>("create_conversation");
  await bridge.invoke("rename_conversation", {
    conversationId: conv.id,
    title: "golden-sealed-knowledge",
  });
  await bridge.invoke("set_conversation_enabled_corpora", {
    conversationId: conv.id,
    enabledCorpora: [fixture.corpus_id],
  });

  await realBootToChat(page);
  await page
    .locator(".convo-title", { hasText: "golden-sealed-knowledge" })
    .first()
    .click();

  const messageId = await sendAndAwaitTurn(
    page,
    "When was the Meridian Lighthouse automated, and how tall is the tower?",
  );

  const facts = await assertTurnInvariants(page, bridge, messageId, {
    requireCitations: true,
  });

  // Citations must come from the sealed fixture corpus (the seal is
  // the user's privacy/scope control — leakage is a real bug).
  const corpusCited = facts.citations.filter((c) => c.provenance_tier !== "web");
  expect(corpusCited.length).toBeGreaterThan(0);
  for (const c of corpusCited) {
    expect(c.corpus_id).toBe(fixture.corpus_id);
  }

  // Competence floor: the two facts are stated verbatim in one chunk.
  expect(facts.complete.full_text).toMatch(/1974|47\s*met/i);

  // The answer rendered in the UI bubble.
  const rendered = (await page.locator(".sv-ai-msg .sv-prose").last().textContent()) ?? "";
  expect(rendered.trim().length).toBeGreaterThan(0);
});
