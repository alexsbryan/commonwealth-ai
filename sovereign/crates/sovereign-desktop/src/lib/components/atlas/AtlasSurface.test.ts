// SPDX-License-Identifier: AGPL-3.0-or-later
// AtlasSurface — routing for *collection* corpora.
//
// A collection corpus (SEP) owns no atoms: its map lives in
// `<id>-<slug>` member atlases. Opening its Explore tab must land on
// the article picker, not on the empty atom browser, and picking an
// article must address the MEMBER id from there on — while "back"
// still returns to the picker rather than out of the notebook.
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent } from "@testing-library/svelte";
import AtlasSurface from "./AtlasSurface.svelte";
import type { AtlasMemberSummary, AtomListPage } from "../../types";

vi.mock("../../api", () => ({
  atlasListMembers: vi.fn(),
  atlasListConvCorpora: vi.fn(),
  atlasListCorpora: vi.fn(),
  atlasGetChunkEntityProgress: vi.fn(),
  atlasListAtoms: vi.fn(),
  atlasSubgraph: vi.fn(),
  atlasListConversations: vi.fn(),
  atlasGetEntityAggregate: vi.fn(),
  atlasGetAtomDetail: vi.fn(),
  atlasGetConvDetail: vi.fn(),
  lcReenrichNote: vi.fn(),
}));

const api = await import("../../api");

const MEMBERS: AtlasMemberSummary[] = [
  { corpus_id: "sep-abduction", title: "Abduction", total_atoms: 98 },
];
const EMPTY_PAGE: AtomListPage = { items: [], total_matching: 0 };

describe("AtlasSurface — collection routing", () => {
  beforeEach(() => {
    vi.mocked(api.atlasListConvCorpora).mockReset().mockResolvedValue([]);
    vi.mocked(api.atlasListMembers).mockReset().mockResolvedValue(MEMBERS);
    vi.mocked(api.atlasListAtoms).mockReset().mockResolvedValue(EMPTY_PAGE);
    // A scoped mount shows AtlasIndex for the tick before the kind
    // resolves, so its reads must answer too — an `undefined` here is
    // a crash inside the index, not a signal about this surface.
    vi.mocked(api.atlasListCorpora).mockReset().mockResolvedValue([]);
    vi.mocked(api.atlasGetChunkEntityProgress).mockReset().mockResolvedValue(null);
  });

  it("opens a collection notebook on the article picker, not the empty atom list", async () => {
    render(AtlasSurface, { props: { startingCorpusId: "sep" } });

    await screen.findByText("Abduction");
    // The atom browser must not have been consulted for the parent.
    expect(api.atlasListAtoms).not.toHaveBeenCalled();
  });

  it("addresses the member corpus after picking an article, and comes back to the picker", async () => {
    render(AtlasSurface, { props: { startingCorpusId: "sep" } });

    await fireEvent.click(await screen.findByRole("button", { name: /Explore Abduction/i }));

    // The atom list is now the MEMBER's atlas, not the collection's.
    await screen.findByText("sep-abduction");
    expect(vi.mocked(api.atlasListAtoms).mock.calls[0][0]).toBe("sep-abduction");

    // Back leads to the article picker — which exists even though this
    // is a scoped notebook mount with no global index behind it.
    await fireEvent.click(screen.getByRole("button", { name: /back to articles/i }));
    await screen.findByText("Abduction");
  });

  it("leaves an ordinary corpus on the atom browser", async () => {
    vi.mocked(api.atlasListMembers).mockResolvedValue([]);
    render(AtlasSurface, { props: { startingCorpusId: "wikipedia" } });

    await screen.findByText("wikipedia");
    expect(vi.mocked(api.atlasListAtoms).mock.calls[0][0]).toBe("wikipedia");
  });
});
