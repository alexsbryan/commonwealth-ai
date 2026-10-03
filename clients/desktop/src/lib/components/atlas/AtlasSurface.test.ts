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
import type {
  AtlasMemberSummary,
  AtomListPage,
  ConvCorpusSummary,
  ConvListPage,
} from "../../types";

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

// The SEP shape after a partial tiered-enrichment run: the parent id
// answers YES to both signals at once.
const SEP_AS_CONV: ConvCorpusSummary[] = [
  {
    corpus_id: "sep",
    display_name: "sep",
    conv_count: 14,
    state_counts: { Ready: 14 },
  },
];
const CONV_PAGE: ConvListPage = { conversations: [], total_matching: 0 };

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
    // Defaulted so a routing REGRESSION renders the conv view for real
    // rather than dying on an unhandled rejection — the failure we
    // want to see is "wrong surface", not "crashed".
    vi.mocked(api.atlasListConversations).mockReset().mockResolvedValue(CONV_PAGE);
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

  // The two signals are NOT exclusive, and the router must not treat
  // them as if they were. A partial tiered-enrichment run over a
  // collection writes `conv_skeletons` rows under the PARENT id, so
  // SEP answers yes to both. `resolveCorpusKind` asked conv first and
  // returned on the first match, so 14 hollow rows (no RAPTOR nodes,
  // no entities, overview = the URL slug) hid 1,770 member atlases
  // behind a "14 conversations" list. Every other case in this file
  // mocks the conv listing empty, which is exactly why nothing caught
  // it — this is the failing input that was missing.
  it("prefers the article picker when a collection is ALSO conv-listed", async () => {
    vi.mocked(api.atlasListConvCorpora).mockResolvedValue(SEP_AS_CONV);

    render(AtlasSurface, { props: { startingCorpusId: "sep" } });

    await screen.findByText("Abduction");
    expect(api.atlasListConversations).not.toHaveBeenCalled();
  });

  // The other half of the same rule: reordering must not strand a
  // genuine conv corpus on the atom browser. No members → conv wins.
  it("still routes a conv corpus with no member atlases to the conv view", async () => {
    vi.mocked(api.atlasListMembers).mockResolvedValue([]);
    vi.mocked(api.atlasListConvCorpora).mockResolvedValue([
      { ...SEP_AS_CONV[0], corpus_id: "conversations-anthropic" },
    ]);

    render(AtlasSurface, { props: { startingCorpusId: "conversations-anthropic" } });

    await vi.waitFor(() => expect(api.atlasListConversations).toHaveBeenCalled());
    expect(api.atlasListAtoms).not.toHaveBeenCalled();
  });

  it("leaves an ordinary corpus on the atom browser", async () => {
    vi.mocked(api.atlasListMembers).mockResolvedValue([]);
    render(AtlasSurface, { props: { startingCorpusId: "wikipedia" } });

    await screen.findByText("wikipedia");
    expect(vi.mocked(api.atlasListAtoms).mock.calls[0][0]).toBe("wikipedia");
  });
});
