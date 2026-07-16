// SPDX-License-Identifier: AGPL-3.0-or-later
// ConvDetail — the "flag a wrong summary → re-enrich this note" revision
// loop (docs/specs/SUMMARY_REVISION_LOOP.md). Pins the flag flow: the
// per-cluster "fix" control opens a hint form, submitting calls
// lcReenrichNote with (corpusId, convUuid, hint, the flagged summary),
// then reloads the detail; a busy re-enrich surfaces the error and keeps
// the form live; an already-applied correction shows the provenance badge.
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent } from "@testing-library/svelte";
import ConvDetail from "./ConvDetail.svelte";
import type { ConvDetailView } from "../../types";

vi.mock("../../api", () => ({
  atlasGetConvDetail: vi.fn(),
  lcReenrichNote: vi.fn(async () => undefined),
  // Imported by the (unmounted) EntityDrawer child — stub so the module
  // graph resolves.
  atlasGetEntityAggregate: vi.fn(),
}));

const api = await import("../../api");

const WRONG_SUMMARY = "Following Yakumo's death, Haruki inherits her journal.";
const RIGHT_SUMMARY = "In the village of Yakumo, Grandmother Sato keeps the journal.";

function detail(overrides: Partial<ConvDetailView> = {}): ConvDetailView {
  return {
    corpus_id: "obsidian-vault-x",
    conv_uuid: "Parable of Yakumo.md",
    title: "Parable of Yakumo",
    state: "Ready",
    chunk_count: 12,
    updated_at: 1_700_000_000,
    max_level: 0,
    raptor_nodes: [
      {
        node_id: "n1",
        level: 0,
        summary: WRONG_SUMMARY,
        primary_entities: ["Yakumo", "Haruki"],
        direct_member_chunk_ids: [1, 2],
        evidence_chunk_count: 2,
        cluster_coherence: 0.8,
        is_synthetic_tiny: false,
      },
    ],
    correction: null,
    ...overrides,
  };
}

describe("ConvDetail — summary revision loop", () => {
  beforeEach(() => {
    vi.mocked(api.atlasGetConvDetail).mockReset();
    vi.mocked(api.lcReenrichNote).mockReset();
    vi.mocked(api.lcReenrichNote).mockResolvedValue(undefined);
  });

  it("flag → hint → re-enrich calls lcReenrichNote with the note id, hint, and flagged summary, then reloads", async () => {
    const corrected = detail({
      raptor_nodes: [
        {
          node_id: "n2",
          level: 0,
          summary: RIGHT_SUMMARY,
          primary_entities: ["Yakumo", "Grandmother Sato"],
          direct_member_chunk_ids: [1, 2],
          evidence_chunk_count: 2,
          cluster_coherence: 0.9,
          is_synthetic_tiny: false,
        },
      ],
      correction: {
        status: "applied",
        correction_hint: "Yakumo is the setting",
        created_at: 1_700_000_500,
      },
    });
    vi.mocked(api.atlasGetConvDetail)
      .mockResolvedValueOnce(detail()) // onMount
      .mockResolvedValueOnce(corrected); // reload after re-enrich

    render(ConvDetail, {
      props: {
        corpusId: "obsidian-vault-x",
        convUuid: "Parable of Yakumo.md",
        onBack: vi.fn(),
      },
    });

    await screen.findByText(WRONG_SUMMARY);

    await fireEvent.click(screen.getByRole("button", { name: /fix/i }));
    const textarea = await screen.findByRole("textbox");
    const hint =
      "Yakumo is the village/setting; Grandmother Sato is the character.";
    await fireEvent.input(textarea, { target: { value: hint } });
    await fireEvent.click(
      screen.getByRole("button", { name: /re-enrich this note/i }),
    );

    await vi.waitFor(() => expect(api.lcReenrichNote).toHaveBeenCalled());
    expect(api.lcReenrichNote).toHaveBeenCalledWith(
      "obsidian-vault-x",
      "Parable of Yakumo.md",
      hint,
      WRONG_SUMMARY,
    );

    // Reloaded → corrected summary + provenance badge.
    await screen.findByText(RIGHT_SUMMARY);
    expect(screen.getByText(/revised by you/i)).toBeInTheDocument();
  });

  it("a busy re-enrich surfaces the daemon's message and keeps the form open", async () => {
    vi.mocked(api.atlasGetConvDetail).mockResolvedValue(detail());
    vi.mocked(api.lcReenrichNote).mockRejectedValueOnce(
      "enrichment is busy with a full build right now — try again when it finishes",
    );

    render(ConvDetail, {
      props: { corpusId: "c", convUuid: "N.md", onBack: vi.fn() },
    });
    await screen.findByText(WRONG_SUMMARY);

    await fireEvent.click(screen.getByRole("button", { name: /fix/i }));
    await fireEvent.click(
      screen.getByRole("button", { name: /re-enrich this note/i }),
    );

    await vi.waitFor(() =>
      expect(screen.getByText(/busy with a full build/i)).toBeInTheDocument(),
    );
    // Form still open + re-enrich button available for retry.
    expect(screen.getByRole("textbox")).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: /re-enrich this note/i }),
    ).toBeEnabled();
  });

  it("renders the 'revised by you' badge when a correction is already applied", async () => {
    vi.mocked(api.atlasGetConvDetail).mockResolvedValue(
      detail({
        correction: {
          status: "applied",
          correction_hint: "Yakumo is the setting",
          created_at: 1_700_000_000,
        },
      }),
    );
    render(ConvDetail, {
      props: { corpusId: "c", convUuid: "N.md", onBack: vi.fn() },
    });
    await screen.findByText(/revised by you/i);
  });
});
