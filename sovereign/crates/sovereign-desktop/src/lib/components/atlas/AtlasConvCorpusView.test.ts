// SPDX-License-Identifier: AGPL-3.0-or-later
// AtlasConvCorpusView — the per-corpus "all notes" browse view.
// Pins the back-button contract: the "← Atlas" control is only shown
// when it actually leads somewhere. In the standalone Atlas Inspector
// (unscoped) it returns to the corpus index, so it renders and clicking
// it fires onBack. In a notebook's scoped Explore tab this view IS the
// surface root — there is no index to return to — so the host passes
// showBack=false and the dead no-op button must NOT render. Regression
// guard for "the Atlas back button doesn't do anything."
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent } from "@testing-library/svelte";
import AtlasConvCorpusView from "./AtlasConvCorpusView.svelte";
import type { ConvListPage } from "../../types";

vi.mock("../../api", () => ({
  atlasListConversations: vi.fn(),
  // Imported by the (unmounted) EntityDrawer child — stub so the module
  // graph resolves.
  atlasGetEntityAggregate: vi.fn(),
}));

const api = await import("../../api");

function page(): ConvListPage {
  return {
    conversations: [
      {
        conv_uuid: "Parable of Yakumo.md",
        title: "Parable of Yakumo",
        state: "Ready",
        chunk_count: 7,
        top_entities: ["Yakumo", "Grandmother Sato"],
        updated_at: 1_700_000_000,
        is_tiny: false,
      },
    ],
    total_matching: 1,
    next_offset: undefined,
  };
}

describe("AtlasConvCorpusView — back button visibility", () => {
  beforeEach(() => {
    vi.mocked(api.atlasListConversations).mockReset();
    vi.mocked(api.atlasListConversations).mockResolvedValue(page());
  });

  it("shows the '← Atlas' button and fires onBack when it leads to the index (unscoped, default)", async () => {
    const onBack = vi.fn();
    render(AtlasConvCorpusView, {
      props: { corpusId: "obsidian-vault-x", onBack, onSelectConv: vi.fn() },
    });
    // Wait for the note list to load so the header is settled.
    await screen.findByText("Parable of Yakumo");

    const back = screen.getByRole("button", { name: /atlas/i });
    await fireEvent.click(back);
    expect(onBack).toHaveBeenCalledTimes(1);
  });

  it("hides the '← Atlas' button when this view is the surface root (scoped Explore, showBack=false)", async () => {
    const onBack = vi.fn();
    render(AtlasConvCorpusView, {
      props: {
        corpusId: "obsidian-vault-x",
        onBack,
        showBack: false,
        onSelectConv: vi.fn(),
      },
    });
    await screen.findByText("Parable of Yakumo");

    // The dead no-op control must not render at all.
    expect(
      screen.queryByRole("button", { name: /atlas/i }),
    ).not.toBeInTheDocument();
    expect(onBack).not.toHaveBeenCalled();
  });
});
