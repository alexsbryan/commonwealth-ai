// SPDX-License-Identifier: AGPL-3.0-or-later
// AtlasCollectionView — the article picker for a *collection* notebook
// (SEP: one atlas per encyclopedia entry, empty parent atlas).
//
// Pins two contracts:
//   1. Picking a row hands the surface the MEMBER's corpus id, which is
//      the id every downstream atlas call takes.
//   2. "This notebook has no maps" and "your search hid them" are
//      DIFFERENT messages. Sharing one line of copy is the defect that
//      made SEP's Explore tab read as a filter problem when the real
//      state was "the parent atlas is empty".
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent } from "@testing-library/svelte";
import AtlasCollectionView from "./AtlasCollectionView.svelte";
import type { AtlasMemberSummary } from "../../types";

vi.mock("../../api", () => ({
  atlasListMembers: vi.fn(),
}));

const api = await import("../../api");

function members(): AtlasMemberSummary[] {
  return [
    { corpus_id: "sep-abduction", title: "Abduction", total_atoms: 98 },
    { corpus_id: "sep-logic-modal", title: "Logic Modal", total_atoms: 221 },
  ];
}

describe("AtlasCollectionView", () => {
  beforeEach(() => {
    vi.mocked(api.atlasListMembers).mockReset();
    vi.mocked(api.atlasListMembers).mockResolvedValue(members());
  });

  it("lists the member articles with their atom counts", async () => {
    render(AtlasCollectionView, {
      props: { corpusId: "sep", onSelectMember: vi.fn(), onBack: vi.fn() },
    });

    await screen.findByText("Abduction");
    expect(screen.getByText("Logic Modal")).toBeTruthy();
    expect(screen.getByText("98 atoms")).toBeTruthy();
    // Header census: 2 articles, 319 atoms between them.
    expect(screen.getByText(/2 articles · 319\s+atoms/)).toBeTruthy();
    expect(api.atlasListMembers).toHaveBeenCalledWith("sep");
  });

  it("hands the member's corpus id to onSelectMember", async () => {
    const onSelectMember = vi.fn();
    render(AtlasCollectionView, {
      props: { corpusId: "sep", onSelectMember, onBack: vi.fn() },
    });

    await fireEvent.click(await screen.findByRole("button", { name: /Explore Abduction/i }));
    expect(onSelectMember).toHaveBeenCalledWith("sep-abduction");
  });

  it("filters on the slug as well as the derived title", async () => {
    render(AtlasCollectionView, {
      props: { corpusId: "sep", onSelectMember: vi.fn(), onBack: vi.fn() },
    });
    await screen.findByText("Abduction");

    // The title is slug-DERIVED ("Logic Modal", not the upstream
    // "Modal Logic"), so someone who knows the slug must still find it.
    await fireEvent.input(screen.getByTestId("atlas-collection-search"), {
      target: { value: "logic-modal" },
    });

    expect(screen.queryByText("Abduction")).toBeNull();
    expect(screen.getByText("Logic Modal")).toBeTruthy();
    expect(screen.getByText(/Showing 1 of 2/)).toBeTruthy();
  });

  it("says the SEARCH found nothing when a query excludes every article", async () => {
    render(AtlasCollectionView, {
      props: { corpusId: "sep", onSelectMember: vi.fn(), onBack: vi.fn() },
    });
    await screen.findByText("Abduction");

    await fireEvent.input(screen.getByTestId("atlas-collection-search"), {
      target: { value: "zzzz" },
    });

    expect(screen.getByText(/No article matches/)).toBeTruthy();
    expect(screen.queryByText(/no article maps have been built/i)).toBeNull();
  });

  it("says NO MAPS EXIST when the collection is empty — not that a filter hid them", async () => {
    vi.mocked(api.atlasListMembers).mockResolvedValue([]);
    render(AtlasCollectionView, {
      props: { corpusId: "sep", onSelectMember: vi.fn(), onBack: vi.fn() },
    });

    await screen.findByText(/No article maps have been built/i);
    expect(screen.queryByText(/No article matches/)).toBeNull();
  });

  it("surfaces a load failure instead of rendering an empty picker", async () => {
    vi.mocked(api.atlasListMembers).mockRejectedValue(new Error("index unreadable"));
    render(AtlasCollectionView, {
      props: { corpusId: "sep", onSelectMember: vi.fn(), onBack: vi.fn() },
    });

    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain("index unreadable");
  });

  it("hides the back control when this view IS the surface root", async () => {
    render(AtlasCollectionView, {
      props: {
        corpusId: "sep",
        onSelectMember: vi.fn(),
        onBack: vi.fn(),
        showBack: false,
      },
    });
    await screen.findByText("Abduction");
    expect(screen.queryByRole("button", { name: /back to atlas index/i })).toBeNull();
  });
});
