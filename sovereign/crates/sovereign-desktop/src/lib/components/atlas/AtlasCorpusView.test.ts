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
import { render, screen, fireEvent } from "@testing-library/svelte";
import AtlasCorpusView from "./AtlasCorpusView.svelte";
import type { AtomListPage } from "../../types";

vi.mock("../../api", () => ({
  atlasListAtoms: vi.fn(),
  atlasSubgraph: vi.fn(),
}));

const api = await import("../../api");

const EMPTY: AtomListPage = { items: [], total_matching: 0 };

describe("AtlasCorpusView — empty state", () => {
  beforeEach(() => {
    vi.mocked(api.atlasListAtoms).mockReset();
    vi.mocked(api.atlasListAtoms).mockResolvedValue(EMPTY);
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
