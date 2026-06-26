// SPDX-License-Identifier: AGPL-3.0-or-later
import { test, expect, bootToChat, type Page } from "../fixtures/test-base";

// Ask ↔ Explore continuity (elegance phase, Move 4 — the capstone).
//
// Inside a notebook, the conversation and the map are two views of one
// thing. This pins the Map → Ask leg: drilling into an atom on the map
// and choosing "Ask about this" lands on the notebook's Ask tab with a
// fresh, scoped question — without leaving the notebook.

const NOTEBOOK = {
  id: "vault",
  name: "Research Vault",
  source_kind: "obsidian",
  doc_count: 50,
  explorable: true,
  updated_unix: 0,
  scope: "local",
};

async function seed(page: Page) {
  await page.evaluate((nb) => {
    const w = window as unknown as {
      __sovereign_test__: {
        setHandler: (cmd: string, fn: (a: unknown) => unknown) => void;
      };
    };
    w.__sovereign_test__.setHandler("notebook_list", () => [nb]);
    w.__sovereign_test__.setHandler("atlas_list_conv_corpora", () => []);
    w.__sovereign_test__.setHandler("atlas_list_atoms", () => ({
      items: [
        {
          atom_id: "a1",
          stable_key: "stable-key-12345678",
          atom_type: "Entity",
          display_name: "Kestrel",
          enrichment_depth: "extracted",
          evidence_chunk_count: 0,
          curation_status: "generated",
          overlay_supports: false,
        },
      ],
      total_matching: 1,
    }));
    w.__sovereign_test__.setHandler("atlas_get_atom_detail", () => ({
      atom_id: "a1",
      stable_key: "stable-key-12345678",
      display_name: "Kestrel",
      atom_type: "Entity",
      corpus_id: "vault",
      curation_status: "generated",
      salience: 0.5,
      atom: { atom_type: "Entity", data: {} },
      evidence_excerpts: [],
      related: [],
      cross_corpus: [],
      referenced_atoms: {},
      overlay_supports: false,
    }));
  }, NOTEBOOK);
}

test.describe("Ask ↔ Explore continuity", () => {
  test("'Ask about this' on an atom lands on the notebook's Ask tab", async ({
    sovereignPage: page,
    chat,
  }) => {
    await bootToChat(page, chat);
    await seed(page);
    await page.getByTestId("nav-library").click();

    // Open the notebook's Explore tab and drill into an atom.
    await page.getByTestId("notebook-explore").first().click();
    await page.getByTestId("atlas-atom-row").first().locator("button").first().click();

    // The Map → Ask affordance switches to Ask, within the notebook.
    await expect(page.getByTestId("atom-ask-about")).toBeVisible();
    await page.getByTestId("atom-ask-about").click();
    await expect(page.getByTestId("notebook-tab-ask")).toHaveClass(/active/);
  });
});
