// SPDX-License-Identifier: AGPL-3.0-or-later
// J1 (Tier 1) — the core loop: ask a grounded question, stream a real
// reply, and read a citation back to its source.
//
// Deterministic gate: assertTurnInvariants(requireCitations) proves the
// turn carries retrieved_chunks AND each resolves via read_get_chunk to
// a real passage (no dangling provenance) — the citation contract,
// proven at the data layer, independent of the model's prose.
//
// Best-effort gate: the reading-surface CLICK-THROUGH. The only UI path
// from a chat message to the reading surface is the inline
// `.source-citation` span, which renders only when the model emits a
// `[Source: …]` marker (confirmed: SourceAttribution's "Sources:" block
// is not clickable to openCitation). The 2B fast profile doesn't always
// emit markers, so if none rendered we note-and-skip rather than fail on
// model nondeterminism.
import { expect, journeyTest, realBootToChat } from "./journey";
import { J_CHAT_CITATION } from "./manifest";

journeyTest(J_CHAT_CITATION, async ({ page, run }) => {
  await realBootToChat(page);

  // Grounded in the fixture corpus (the Meridian Lighthouse set), so
  // retrieval reliably returns chunks regardless of the chat model.
  const facts = await run.turn(
    "How tall is the Meridian Lighthouse, and what is its light signal?",
    { requireCitations: true },
  );
  expect(
    facts.citations.length,
    "grounded turn must surface at least one retrieved chunk",
  ).toBeGreaterThan(0);

  // Reading-surface click-through (best-effort — see file header).
  const lastMsg = page.locator(".sv-ai-msg").last();
  const citations = lastMsg.locator(".source-citation");
  if ((await citations.count()) > 0) {
    await citations.first().click();
    const surface = page.locator(".reading-surface");
    await expect(
      surface,
      "clicking an inline citation must open the reading surface",
    ).toBeVisible({ timeout: 15_000 });
    await expect(
      surface.locator(".content"),
      "reading surface must render the cited passage",
    ).toContainText(/\S/);
    run.note("reading-surface opened and rendered a passage from an inline citation");
  } else {
    run.note(
      "model emitted no inline [Source:] marker; reading-surface click-through skipped " +
        "(data-layer citation resolution still asserted by the invariant pack)",
    );
  }
});
