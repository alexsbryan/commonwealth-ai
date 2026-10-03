// SPDX-License-Identifier: AGPL-3.0-or-later
// WatchedFolderBanner tests. Regression guard for the "massive error
// overflowing the UI + no way to clear it" report: an errored watched folder
// whose initial ingest never completed showed the full multi-sentence worker
// diagnostic verbatim and only offered "Retry" (which re-hits the same guard
// forever). The fix: a bounded reason line (full text in the title tooltip)
// plus a Remove action wired to the deregister+wipe path.
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent } from "@testing-library/svelte";
import WatchedFolderBanner from "./WatchedFolderBanner.svelte";
import type { WatchedFolderListEntry } from "../../types";

vi.mock("../../api", () => ({
  lcWatchConfirmDeletion: vi.fn(async () => ({ corpus_id: "x", ok: true })),
  lcWatchResume: vi.fn(async () => ({ corpus_id: "x", ok: true })),
  lcWatchRemove: vi.fn(async () => ({ corpus_id: "x", ok: true })),
}));

const api = await import("../../api");

const LONG_MESSAGE =
  "watched_folder: index for 'watched-25378eeeed13' is missing " +
  "`_corpus_meta.json` at /Users/x/.sovereign/indexes/watched-25378eeeed13/" +
  "_corpus_meta.json — initial ingest never completed or the index was wiped " +
  "out-of-band. Re-register the folder (Settings → Local Knowledge → remove " +
  "+ re-add) to rebuild from scratch.";

function erroredEntry(): WatchedFolderListEntry {
  return {
    corpus_id: "watched-25378eeeed13",
    display_name: "Sovereign Test",
    root_path: "/Users/x/Downloads/Sovereign Test",
    status: { kind: "errored", message: LONG_MESSAGE, errored_unix: 1 },
    sync_mode: "continuous",
    sensitive: false,
    additional_roots_count: 0,
  } as unknown as WatchedFolderListEntry;
}

describe("WatchedFolderBanner errored entry", () => {
  beforeEach(() => {
    vi.mocked(api.lcWatchRemove).mockClear();
    vi.mocked(api.lcWatchResume).mockClear();
    vi.spyOn(window, "confirm").mockReturnValue(true);
  });

  it("renders a bounded reason headline, not the full diagnostic", () => {
    render(WatchedFolderBanner, {
      props: { blocked: [erroredEntry()], onChanged: vi.fn() },
    });
    const reason = document.querySelector(".reason") as HTMLElement;
    expect(reason).toBeTruthy();
    // The path dump + remediation clause must NOT be in the visible text…
    expect(reason.textContent).not.toContain("_corpus_meta.json");
    expect(reason.textContent).not.toContain("remove + re-add");
    // …but the full message is preserved in the tooltip for glassbox.
    expect(reason.getAttribute("title")).toBe(LONG_MESSAGE);
  });

  it("offers a Remove action that calls the deregister+wipe path", async () => {
    const onChanged = vi.fn();
    render(WatchedFolderBanner, {
      props: { blocked: [erroredEntry()], onChanged },
    });
    const removeBtn = screen.getByRole("button", { name: /^remove$/i });
    await fireEvent.click(removeBtn);
    expect(api.lcWatchRemove).toHaveBeenCalledWith("watched-25378eeeed13");
    await vi.waitFor(() => expect(onChanged).toHaveBeenCalled());
  });

  it("still offers Retry alongside Remove", () => {
    render(WatchedFolderBanner, {
      props: { blocked: [erroredEntry()], onChanged: vi.fn() },
    });
    expect(screen.getByRole("button", { name: /^retry$/i })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /^remove$/i })).toBeInTheDocument();
  });

  it("renders nothing when there is nothing blocked", () => {
    const { container } = render(WatchedFolderBanner, {
      props: { blocked: [], onChanged: vi.fn() },
    });
    expect(container.querySelector(".banner")).toBeNull();
  });
});
