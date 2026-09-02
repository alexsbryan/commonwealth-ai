// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Real-stack proof of ontology v1's whole point: **the user sees their
// own nouns.**
//
// A numismatist declares `coin`, `sceatta specializes coin`, `ruler
// role_of person`, `mint` and `attribution` in the recipe. This spec
// opens their notebook's Explore tab and asserts the surface speaks
// that vocabulary end to end — the filter row, the browse rows, and the
// atom inspector — through the real Tauri commands over a real ingested
// corpus. The atlas on top of it is checked in (`plantNumismaticsCorpus`
// in global-setup) so the atoms are known without an LLM enrich, and its
// `ontology.json` is what makes the corpus DECLARED at all.
//
// Four things here are not guessable from the wire and each has a
// specific way of going wrong:
//
//  - `coin` is 5 in the badge and 3 in the list. `subtype_counts` are
//    OWN counts, so the badge adds the 2 sceattas via `specializes`
//    while the backend's `subtype` filter matches EXACTLY. Both numbers
//    are correct and they are different; the spec pins both so the pair
//    cannot drift apart silently.
//  - `ruler` is declared `kind = "entity"` and lands as a STATE atom on
//    the person. Filtering it by kind finds nothing; filtering it by
//    subtype finds the king.
//  - an `attribution` claim's `subject` is the coin it dates, which is
//    a different atom from its `attributed_to` voice.
//  - a `ref` attribute holds an atom id when it resolved and the
//    source's own words when it did not, and the two must not render
//    the same way.
//
// `governance.real.spec.ts` is the sibling proof for the Conflicts tab
// and is unchanged by this.
import fs from "node:fs";
import { expect, realBootToChat, test } from "./test-base-real";
import { NUM_FIXTURE_INFO } from "./global-setup";

test("real stack: a declared corpus browses by the author's own nouns", async ({
  sovereignPage: page,
}) => {
  test.setTimeout(120_000);

  const info = JSON.parse(fs.readFileSync(NUM_FIXTURE_INFO, "utf8")) as {
    corpus_id: string;
  };
  const corpusId = info.corpus_id;

  await realBootToChat(page);
  await page.getByTestId("nav-library").click();

  const card = page.locator(`[data-notebook-id="${corpusId}"]`);
  await expect(card).toBeVisible();
  await card.getByTestId("notebook-explore").click();

  // ── The filter row is the author's vocabulary, not the system's. ──
  const pill = (key: string) =>
    page.locator(`[data-testid="atlas-pill"][data-pill="${key}"]`);

  await expect(pill("subtype:coin")).toBeVisible();
  // 3 coins + 2 sceattas. Neither the census nor any single wire field
  // carries this total — it is the roll-up over `specializes`.
  await expect(pill("subtype:coin")).toContainText("5");
  await expect(pill("subtype:sceatta")).toContainText("2");
  await expect(pill("subtype:mint")).toContainText("2");
  await expect(pill("subtype:attribution")).toContainText("2");
  // The `role_of` type. Nine-tenths of the reason this pill filters by
  // subtype rather than kind.
  await expect(pill("subtype:ruler")).toContainText("1");

  // The generic kinds are still there. Two of the nine Entities are
  // `person`, which nobody declared — dropping "the kinds a declaration
  // covers" would leave them reachable only by name search.
  await expect(pill("kind:Entity")).toContainText("9");

  // ── Clicking a declared pill filters by that type. ──
  const rows = page.locator('[data-testid="atlas-atom-row"]');
  await pill("subtype:coin").click();
  // EXACT match, no roll-up: the three coins, not the five of the badge.
  // The backend's `subtype` filter is deliberately own-only — see
  // `AtomFilter::subtype`.
  await expect(rows).toHaveCount(3);
  for (const name of ["Wessex Down 1", "Wessex Down 2", "Wessex Down 3"]) {
    await expect(rows.getByText(name, { exact: true })).toBeVisible();
  }
  await expect(rows.getByText("Wessex Down 4", { exact: true })).toHaveCount(0);

  // The row's type chip says the author's noun, not "Entity".
  await expect(rows.first().getByText("coin", { exact: true })).toBeVisible();

  // ── The `role_of` type resolves to a State atom on the person. ──
  await pill("subtype:ruler").click();
  await expect(rows).toHaveCount(1);
  await expect(rows.getByText("King of Mercia", { exact: true })).toBeVisible();

  // ── A sceatta's declared attributes are rows in the inspector. ──
  await pill("subtype:sceatta").click();
  await expect(rows).toHaveCount(2);
  await rows.filter({ hasText: "Wessex Down 4" }).getByRole("button").first().click();

  await expect(page.getByTestId("atom-attributes")).toBeVisible();
  const attrNames = page.getByTestId("atom-attribute-name");
  await expect(attrNames.filter({ hasText: /^weight$/ })).toBeVisible();
  await expect(attrNames.filter({ hasText: /^struck$/ })).toBeVisible();
  const attrs = page.getByTestId("atom-attributes");
  await expect(attrs).toContainText("0.95");
  await expect(attrs).toContainText("between 710 and 725");
  // The `ref` that resolved renders as the mint's NAME…
  await expect(attrs).toContainText("Canterbury");
});

test("real stack: an attribution claim links to the coin it is about", async ({
  sovereignPage: page,
}) => {
  test.setTimeout(120_000);

  const info = JSON.parse(fs.readFileSync(NUM_FIXTURE_INFO, "utf8")) as {
    corpus_id: string;
  };

  await realBootToChat(page);
  await page.getByTestId("nav-library").click();
  await page
    .locator(`[data-notebook-id="${info.corpus_id}"]`)
    .getByTestId("notebook-explore")
    .click();

  await page
    .locator('[data-testid="atlas-pill"][data-pill="subtype:attribution"]')
    .click();

  const rows = page.locator('[data-testid="atlas-atom-row"]');
  await expect(rows).toHaveCount(2);
  await rows
    .filter({ hasText: "Wessex Down 4 was struck at Canterbury" })
    .getByRole("button")
    .first()
    .click();

  // The declared claim type is named on the atom, not just implied by
  // the pill the user came in through.
  await expect(page.getByTestId("claim-declared-type")).toHaveText("attribution");

  // The REFERENT — what this claim is about. Distinct from the voice
  // that made it, which is the whole reason a declared claim type names
  // a `subject`. This link resolved only after `subject` was added to
  // `AtomEnvelope::referenced_atom_ids`; before that it rendered the raw
  // `entity-0008`.
  const about = page.getByTestId("claim-subject");
  await expect(about).toBeVisible();
  await expect(about).toContainText("Wessex Down 4");
  // And the voice is still its own, different atom.
  await expect(page.getByText("Michael Metcalf")).toBeVisible();
});
