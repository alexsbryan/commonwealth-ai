// SPDX-License-Identifier: AGPL-3.0-or-later
// NotebookOpenQuestions tests (initiative I2-D). The panel mines the
// notebook's atlas for Question atoms and renders them as chips wired to
// the Map→Ask bridge. It must (1) render the highest-salience questions,
// (2) forward the question text on tap, and (3) render nothing when the
// atlas has no questions or isn't built — so it never adds empty chrome.
import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/svelte";
import NotebookOpenQuestions from "./NotebookOpenQuestions.svelte";
import type { AtomListPage, AtomSummary } from "../../types";

vi.mock("../../api", () => ({
  atlasListAtoms: vi.fn(),
}));
const api = await import("../../api");

function questionAtom(
  atom_id: string,
  display_name: string,
  salience: number,
): AtomSummary {
  return {
    atom_id,
    stable_key: `sk-${atom_id}`,
    atom_type: "Question",
    display_name,
    salience,
    enrichment_depth: "extracted",
    evidence_chunk_count: 1,
    curation_status: "generated",
    overlay_supports: false,
  } as unknown as AtomSummary;
}

function page(items: AtomSummary[]): AtomListPage {
  return { items, total_matching: items.length };
}

describe("NotebookOpenQuestions", () => {
  it("renders Question atoms (highest salience first) and forwards the text on tap", async () => {
    vi.mocked(api.atlasListAtoms).mockResolvedValue(
      page([
        questionAtom("question-0002", "What causes the divergence?", 0.3),
        questionAtom("question-0001", "Why does the model abstain here?", 0.9),
      ]),
    );
    const onAsk = vi.fn();
    render(NotebookOpenQuestions, { props: { corpusId: "notes", onAsk } });

    await waitFor(() =>
      expect(screen.getByText("Why does the model abstain here?")).toBeTruthy(),
    );
    expect(screen.getByText("What causes the divergence?")).toBeTruthy();
    // Only Question atoms were requested.
    expect(api.atlasListAtoms).toHaveBeenCalledWith("notes", {
      atom_type: "Question",
    });

    await fireEvent.click(screen.getByText("Why does the model abstain here?"));
    expect(onAsk).toHaveBeenCalledWith("Why does the model abstain here?");
  });

  it("respects the limit, keeping the highest-salience questions", async () => {
    vi.mocked(api.atlasListAtoms).mockResolvedValue(
      page([
        questionAtom("q1", "Low", 0.1),
        questionAtom("q2", "High", 0.9),
        questionAtom("q3", "Mid", 0.5),
      ]),
    );
    render(NotebookOpenQuestions, {
      props: { corpusId: "notes", onAsk: () => {}, limit: 2 },
    });
    await waitFor(() => expect(screen.getByText("High")).toBeTruthy());
    expect(screen.getByText("Mid")).toBeTruthy();
    // The lowest-salience question is dropped by the limit.
    expect(screen.queryByText("Low")).toBeNull();
  });

  it("renders nothing when the atlas has no Question atoms", async () => {
    vi.mocked(api.atlasListAtoms).mockResolvedValue(page([]));
    const { container } = render(NotebookOpenQuestions, {
      props: { corpusId: "notes", onAsk: () => {} },
    });
    await waitFor(() => expect(api.atlasListAtoms).toHaveBeenCalled());
    expect(
      container.querySelector('[data-testid="notebook-open-questions"]'),
    ).toBeNull();
  });

  it("renders nothing when the atlas isn't built (call rejects)", async () => {
    vi.mocked(api.atlasListAtoms).mockRejectedValue(new Error("no atlas"));
    const { container } = render(NotebookOpenQuestions, {
      props: { corpusId: "notes", onAsk: () => {} },
    });
    await waitFor(() => expect(api.atlasListAtoms).toHaveBeenCalled());
    expect(
      container.querySelector('[data-testid="notebook-open-questions"]'),
    ).toBeNull();
  });
});
