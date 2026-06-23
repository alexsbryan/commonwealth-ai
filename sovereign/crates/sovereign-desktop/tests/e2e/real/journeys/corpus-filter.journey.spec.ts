// SPDX-License-Identifier: AGPL-3.0-or-later
// J3 (Tier 1) — scope retrieval to a selected corpus, and prove the
// allow-list is enforced.
//
// The hermetic profile installs exactly the fixture corpus, so this
// journey asserts scoping with the levers a single-corpus setup offers
// (deterministic, model-independent — assertions are on
// retrieved_chunks[].corpus_id and on the send affordance, not prose):
//   • Enabled (default): retrieval pulls from the fixture corpus.
//   • Disabled (the only source): the app refuses an empty-scope query —
//     Send goes disabled with "enable at least one source" — i.e. the
//     empty allow-list is enforced, not silently widened to "all". Seal
//     semantics: sovereign-core/src/context.rs (Some([]) → zero corpora).
//   • Re-enabled: Send returns and retrieval is scoped to the fixture.
// (Selective A-vs-B scoping wants a 2+ corpus fixture — a follow-up.)
import fs from "node:fs";
import { FIXTURE_INFO } from "../global-setup";
import { expect, journeyTest, realBootToChat } from "./journey";
import { J_CORPUS_FILTER } from "./manifest";

journeyTest(J_CORPUS_FILTER, async ({ page, run }) => {
  const fixture = JSON.parse(fs.readFileSync(FIXTURE_INFO, "utf8")) as {
    corpus_id: string;
    display_name: string;
  };

  await realBootToChat(page);
  const question = "How tall is the Meridian Lighthouse?";

  // ── Enabled (default): retrieval includes the fixture corpus ──
  const withFixture = await run.turn(question, { requireCitations: true });
  const localCites = withFixture.citations.filter((c) => c.provenance_tier !== "web");
  expect(
    localCites.length,
    "default scope must retrieve from the fixture corpus",
  ).toBeGreaterThan(0);
  for (const c of localCites) {
    expect(c.corpus_id, `unexpected corpus in default scope: ${JSON.stringify(c)}`).toBe(
      fixture.corpus_id,
    );
  }

  // Type a query so the Send affordance reflects the CORPUS state, not
  // the empty-input state — Send is disabled on either
  // (ChatView.svelte:1559: !inputText.trim() || ... || allSourcesMuted).
  const input = page.locator(".input-area textarea");
  await input.fill(question);
  const sendBtn = page.locator(".send-btn");
  await expect(
    sendBtn,
    "with a source enabled and text typed, Send is available",
  ).toBeEnabled();

  // ── Disable the only corpus: the empty allow-list is enforced ──
  const strip = page.locator(".corpus-filter-strip");
  await expect(
    strip,
    "corpus filter strip must render for an installed corpus",
  ).toBeVisible();
  const chips = strip.locator(".kb-tag");
  await expect(chips.first(), "the fixture corpus must show a filter chip").toBeVisible();
  const byName = strip.locator(".kb-tag", { hasText: fixture.display_name });
  const fixtureChip = (await byName.count()) > 0 ? byName.first() : chips.first();

  await fixtureChip.click();
  await expect(fixtureChip, "the muted chip must reflect disabled state").toHaveClass(
    /disabled/,
  );
  await expect(
    sendBtn,
    "with no source enabled the app must refuse the query (allow-list enforced)",
  ).toBeDisabled();
  await expect(sendBtn).toHaveAttribute("title", /enable at least one source/i);

  // ── Re-enable: Send returns and retrieval is scoped to the fixture ──
  await fixtureChip.click();
  await expect(fixtureChip, "re-enabling must clear the disabled state").not.toHaveClass(
    /disabled/,
  );
  await expect(sendBtn, "Send must return once a source is enabled").toBeEnabled();

  const reEnabled = await run.turn(question, { requireCitations: true });
  for (const c of reEnabled.citations.filter((c) => c.provenance_tier !== "web")) {
    expect(c.corpus_id, `unexpected corpus after re-enabling: ${JSON.stringify(c)}`).toBe(
      fixture.corpus_id,
    );
  }
  run.note(
    "allow-list enforced: enabled → cites fixture; disabled → query blocked; re-enabled → restored",
  );
});
