// SPDX-License-Identifier: AGPL-3.0-or-later
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, within } from "@testing-library/svelte";
import * as api from "../../api";
import { invokePlugin } from "../../invoke";
import LibraryView from "./LibraryView.svelte";

// Spread the real module so every transitively-imported binding
// (AddSheet, NotebookDetail, InProgressIngests pull their own) resolves,
// and stub only the three this view drives.
vi.mock("../../api", async (importOriginal) => {
  const actual = (await importOriginal()) as Record<string, unknown>;
  return {
    ...actual,
    notebookList: vi.fn(),
    meshMediaOffers: vi.fn(),
    meshMediaProbe: vi.fn(),
  };
});

vi.mock("../../invoke", async (importOriginal) => {
  const actual = (await importOriginal()) as Record<string, unknown>;
  return { ...actual, invokePlugin: vi.fn() };
});

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn().mockResolvedValue(() => {}),
}));

const NOTEBOOK = {
  id: "sf-assessor-roll",
  name: "SF Roll",
  source_kind: "installed",
  doc_count: 207792,
  explorable: true,
  updated_unix: 1790117635,
  scope: "local",
  open_conflicts: null,
};

const OFFER = {
  peer: "RuggedFox",
  node_id: "node-44ae76142b0c3c72",
  status: "online",
  offered_to: [],
  player_url: "http://127.0.0.1:22009",
  unreachable: null,
  media_available: null,
};

describe("LibraryView mesh libraries", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    vi.mocked(api.notebookList).mockResolvedValue([NOTEBOOK as never]);
    vi.mocked(api.meshMediaOffers).mockResolvedValue([OFFER as never]);
    vi.mocked(invokePlugin).mockResolvedValue(undefined);
  });

  // The RuggedFox shape (2026-09-22): the bridge dials, the far side
  // closes without a byte, and a browser opened blind shows its own
  // error page. The probe converts that into a named refusal.
  it("probes before opening and names the member whose library stayed silent", async () => {
    vi.mocked(api.meshMediaProbe).mockRejectedValue(
      new Error(
        "the bridge connected but the library did not answer (connection closed) — the member's media origin may be down on their side",
      ),
    );
    render(LibraryView);
    const card = await screen.findByTestId("mesh-library");
    await fireEvent.click(within(card).getByRole("button"));
    await vi.waitFor(() => {
      expect(screen.getByTestId("mesh-library-play-error").textContent).toContain(
        "RuggedFox's library did not answer",
      );
    });
    expect(api.meshMediaProbe).toHaveBeenCalledWith("http://127.0.0.1:22009");
    expect(invokePlugin).not.toHaveBeenCalled();
  });

  it("opens the bridge URL only after the probe gets an answer", async () => {
    vi.mocked(api.meshMediaProbe).mockResolvedValue(200);
    render(LibraryView);
    const card = await screen.findByTestId("mesh-library");
    await fireEvent.click(within(card).getByRole("button"));
    await vi.waitFor(() => {
      expect(invokePlugin).toHaveBeenCalledWith("plugin:shell|open", {
        path: "http://127.0.0.1:22009",
      });
    });
    expect(screen.queryByTestId("mesh-library-play-error")).toBeNull();
  });
});
