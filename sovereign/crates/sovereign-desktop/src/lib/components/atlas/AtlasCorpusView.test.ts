// SPDX-License-Identifier: AGPL-3.0-or-later
// AtlasCorpusView — empty-state honesty.
//
// An empty atom list has two very different causes and they used to
// share one line of copy ("No atoms match the current filter"). With no
// filter applied that sentence is a lie about the user's input: the
// truth is that this corpus's map was never built. SEP shipped exactly
// that way — a 44-byte `{"atoms":[]}` parent atlas reading as a filter
// problem — which is what sent people looking for a filter to clear.
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/svelte";
import AtlasCorpusView from "./AtlasCorpusView.svelte";
import type { AtlasCorpusSummary, AtomListPage } from "../../types";

vi.mock("../../api", () => ({
  atlasListAtoms: vi.fn(),
  atlasListCorpora: vi.fn(),
  atlasSubgraph: vi.fn(),
}));

const api = await import("../../api");

const EMPTY: AtomListPage = { items: [], total_matching: 0 };

/** The real `wessex-hoard` row, from
 *  `~/.svrnmesh/indexes/wessex-hoard/atlas/_summary.json`. */
const WESSEX: AtlasCorpusSummary = {
  corpus_id: "wessex-hoard",
  display_name: "wessex-hoard",
  total_atoms: 146,
  atom_counts: {
    Entity: 37,
    Event: 4,
    State: 27,
    Relation: 19,
    Claim: 37,
    Question: 22,
  },
  subtype_counts: {
    attribution: 37,
    coin: 13,
    mint: 3,
    person: 13,
    place: 2,
    ruler: 9,
    sceatta: 2,
    work: 4,
  },
  // Alphabetical: `_summary.json` v4 carries the declaration as a
  // BTreeMap, so the recipe's order is gone before a viewer sees it.
  declared_types: [
    { name: "attribution", kind: "claim" },
    { name: "coin", kind: "entity", identity_criterion: "external:catalogue_ref" },
    { name: "mint", kind: "entity" },
    { name: "ruler", kind: "entity" },
    { name: "sceatta", kind: "entity", specializes: "coin" },
  ],
};


describe("AtlasCorpusView — empty state", () => {
  beforeEach(() => {
    vi.mocked(api.atlasListAtoms).mockReset();
    vi.mocked(api.atlasListAtoms).mockResolvedValue(EMPTY);
    vi.mocked(api.atlasListCorpora).mockReset();
    vi.mocked(api.atlasListCorpora).mockResolvedValue([]);
  });


  it("names the real cause when nothing is filtering the list", async () => {
    render(AtlasCorpusView, { props: { corpusId: "sep", onBack: vi.fn() } });

    const empty = await screen.findByTestId("atlas-atoms-empty");
    expect(empty.textContent).toMatch(/has no atoms yet/i);
    expect(empty.textContent).not.toMatch(/current filter/i);
  });

  it("blames the filter only once a search is actually narrowing", async () => {
    render(AtlasCorpusView, { props: { corpusId: "sep", onBack: vi.fn() } });
    await screen.findByTestId("atlas-atoms-empty");

    await fireEvent.input(screen.getByLabelText(/filter atoms by name/i), {
      target: { value: "kant" },
    });

    // The query is debounced (200ms) before it reaches the backend, so
    // wait for the copy to flip rather than asserting synchronously.
    await screen.findByText(/No atoms match the current filter/i);
  });

  it("labels the back control for wherever it actually leads", async () => {
    render(AtlasCorpusView, {
      props: { corpusId: "sep-abduction", onBack: vi.fn(), backLabel: "Articles" },
    });

    await screen.findByRole("button", { name: /back to articles/i });
  });
});

// ─── Pills: the user sees their own nouns ───────────────────────
//
// A corpus that declared `coin`, `sceatta specializes coin`, `ruler
// role_of person`, `mint` and `attribution` offers THOSE as filters. A
// corpus that declared nothing must look exactly as it did before this
// existed — which is the back-compat guarantee for SEP, Wikipedia and
// Enron and is asserted below as a full list, not a spot-check.
describe("AtlasCorpusView — filter pills", () => {
  beforeEach(() => {
    vi.mocked(api.atlasListAtoms).mockReset();
    vi.mocked(api.atlasListAtoms).mockResolvedValue(EMPTY);
    vi.mocked(api.atlasListCorpora).mockReset();
    vi.mocked(api.atlasListCorpora).mockResolvedValue([]);
  });

  const labels = () =>
    screen
      .getAllByTestId("atlas-pill")
      .map((b) => (b.textContent ?? "").trim().split(/\s+/)[0]);

  it("renders the declared types, with the family count on the parent", async () => {
    render(AtlasCorpusView, {
      props: { corpusId: "wessex-hoard", onBack: vi.fn(), summary: WESSEX },
    });

    await waitFor(() => expect(screen.getAllByTestId("atlas-pill").length).toBeGreaterThan(1));
    // `sceatta` sits with the `coin` it specializes, not where the
    // alphabetical wire order would put it.
    expect(labels().slice(0, 6)).toEqual([
      "All",
      "attribution",
      "coin",
      "sceatta",
      "mint",
      "ruler",
    ]);
  });


  it("puts the rolled-up count on the coin pill and the own count in its title", async () => {
    render(AtlasCorpusView, {
      props: { corpusId: "wessex-hoard", onBack: vi.fn(), summary: WESSEX },
    });
    const coin = await waitFor(() => {
      const el = screen
        .getAllByTestId("atlas-pill")
        .find((b) => b.dataset.pill === "subtype:coin");
      if (!el) throw new Error("no coin pill");
      return el;
    });
    expect(coin.textContent).toMatch(/\b15\b/);
    expect(coin.getAttribute("title")).toMatch(/13 are coin itself/);
  });

  it("filters by subtype — never by kind — when a declared pill is clicked", async () => {
    render(AtlasCorpusView, {
      props: { corpusId: "wessex-hoard", onBack: vi.fn(), summary: WESSEX },
    });
    const ruler = await waitFor(() => {
      const el = screen
        .getAllByTestId("atlas-pill")
        .find((b) => b.dataset.pill === "subtype:ruler");
      if (!el) throw new Error("no ruler pill");
      return el;
    });

    vi.mocked(api.atlasListAtoms).mockClear();
    await fireEvent.click(ruler);

    await waitFor(() =>
      expect(vi.mocked(api.atlasListAtoms).mock.calls.length).toBeGreaterThan(0),
    );
    const filter = vi.mocked(api.atlasListAtoms).mock.calls.at(-1)?.[1];
    // `ruler role_of person` lands as State atoms on Entity-kind
    // people. Pairing `atom_type` with the subtype would return none.
    // `ruler` declares no descendants, so the family is just itself.
    expect(filter?.subtypes).toEqual(["ruler"]);
    expect(filter?.atom_type).toBeUndefined();
  });

  it("still offers exactly today's eight kinds when nothing is declared", async () => {
    render(AtlasCorpusView, {
      props: {
        corpusId: "sep",
        onBack: vi.fn(),
        summary: {
          corpus_id: "sep",
          display_name: "sep",
          total_atoms: 12,
          atom_counts: { Entity: 12 },
        },
      },
    });

    await waitFor(() => expect(screen.getAllByTestId("atlas-pill").length).toBe(9));
    expect(labels()).toEqual([
      "All",
      "Entity",
      "Event",
      "State",
      "Relation",
      "Claim",
      "Question",
      "Config",
      "Argument",
    ]);
  });

  it("fetches its own row when the host has no summary to hand", async () => {
    // The scoped notebook mount (an Explore tab) never sees the corpus
    // index, so nothing upstream holds the declared types.
    vi.mocked(api.atlasListCorpora).mockResolvedValue([WESSEX]);
    render(AtlasCorpusView, {
      props: { corpusId: "wessex-hoard", onBack: vi.fn() },
    });

    await waitFor(() =>
      expect(
        screen.getAllByTestId("atlas-pill").some((b) => b.dataset.pill === "subtype:coin"),
      ).toBe(true),
    );
  });
});

