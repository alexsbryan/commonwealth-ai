// SPDX-License-Identifier: AGPL-3.0-or-later
// J3 (Tier 1) — scope retrieval to a selected corpus, and prove the
// allow-list is enforced.
//
// The hermetic profile installs MORE THAN ONE corpus (the fixture corpus
// plus the governance fixture), so this journey drives the strip as a
// user would with a real shelf (deterministic, model-independent —
// assertions are on retrieved_chunks[].corpus_id and on the send
// affordance, not prose):
//   • Enabled (default): retrieval pulls from the fixture corpus.
//   • All sources muted: the app refuses an empty-scope query — Send goes
//     disabled with "enable at least one source" — i.e. the empty
//     allow-list is enforced, not silently widened to "all". Seal
//     semantics: sovereign-core/src/context.rs (Some([]) → zero corpora).
//   • Only the fixture re-enabled: Send returns AND retrieval is scoped
//     to that one corpus while its neighbour stays muted — the selective
//     A-vs-B scoping this journey could only gesture at while the
//     profile held a single corpus.
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

  // The scope now lives behind the AskScopeBar (elegance refactor — Move 1):
  // the strip is revealed by clicking the "Asking ‹…›" bar, not always shown.
  await page.getByTestId("ask-scope-bar").click();

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

  // Mute EVERY chip: "no source enabled" is a property of the whole
  // shelf, and the profile carries more than one corpus. Muting only the
  // fixture would leave a neighbour enabled and prove nothing about the
  // empty allow-list. Each click is confirmed before the next so the
  // strip's in-flight guard (`toggleInFlight`) can't swallow one.
  const chipCount = await chips.count();
  for (let i = 0; i < chipCount; i++) {
    const chip = chips.nth(i);
    if (((await chip.getAttribute("class")) ?? "").includes("disabled")) continue;
    await chip.click();
    await expect(chip, "a muted chip must reflect its disabled state").toHaveClass(
      /disabled/,
    );
  }
  await expect(
    sendBtn,
    "with no source enabled the app must refuse the query (allow-list enforced)",
  ).toBeDisabled();
  await expect(sendBtn).toHaveAttribute("title", /enable at least one source/i);

  // ── Re-enable ONLY the fixture: Send returns and retrieval is scoped
  // to it, with every other corpus still muted ──
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
