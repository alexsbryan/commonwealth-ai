// SPDX-License-Identifier: AGPL-3.0-or-later
// ConflictsPanel component tests. The panel loads its data via
// `governanceGetView` and mutates via the governance_* commands; we mock
// the api layer, assert on the commands being called with the right
// args, and on the rendered agenda/behaviour. The friction-proportional-
// to-authority contract is the load-bearing thing to pin: dismiss is one
// click (no dialog), accept blocks on an empty note, resolve pre-fills a
// dated rationale.
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/svelte";
import ConflictsPanel from "./ConflictsPanel.svelte";
import type { GovernanceViewPayload, TensionView } from "../../types";

vi.mock("../../api", () => ({
  governanceGetView: vi.fn(),
  governanceResolve: vi.fn(async () => ["op1", "op2"]),
  governanceAccept: vi.fn(async () => ["op1"]),
  governanceDismiss: vi.fn(async () => ["op1"]),
  governanceUndoTension: vi.fn(async () => "rev1"),
  governanceExportWrite: vi.fn(async () => {}),
  governancePostBuildSeed: vi.fn(async () => 0),
  // In-process re-enrichment path (replaces the old `enrichBuildAsync`
  // CLI subprocess). `EnrichPollProgress` polls `enrichmentStatus`.
  lcEnrichReset: vi.fn(async () => {}),
  lcEnrichNow: vi.fn(async () => {}),
  enrichmentStatus: vi.fn(async () => ({
    corpus_id: "maple",
    state: null,
    is_terminal: false,
    is_stalled: false,
    fraction_complete: 0,
  })),
}));
vi.mock("@tauri-apps/plugin-dialog", () => ({
  save: vi.fn(async () => "/tmp/rules.md"),
}));

const api = await import("../../api");

function openTension(overrides: Partial<TensionView> = {}): TensionView {
  return {
    id: "edge-1",
    rule_a: "claim-a",
    text_a: "Quiet hours begin at 11 PM.",
    rule_b: "claim-b",
    text_b: "Quiet hours begin at 10 PM on weeknights.",
    why: "When do quiet hours begin now?",
    confidence: 0.9,
    disposition: { disposition: "open" },
    ...overrides,
  };
}

function payload(
  tensions: TensionView[],
  overrides: Partial<GovernanceViewPayload> = {},
): GovernanceViewPayload {
  return {
    view: {
      rules: [
        {
          id: "claim-a",
          text: "Quiet hours begin at 11 PM.",
          status: { status: "active" },
          citation: { chunk_id: "sec_1" },
        },
        {
          id: "claim-b",
          text: "Quiet hours begin at 10 PM on weeknights.",
          status: { status: "active" },
          citation: { chunk_id: "sec_2" },
        },
      ],
      tensions,
      issues: [],
    },
    section_titles: { sec_1: "Charter, Article II", sec_2: "Decision — Feb 10" },
    section_chunks: {},
    scope_names: {},
    vocabulary: null,
    decisions: {},
    docs_changed_since_build: false,
    ...overrides,
  };
}

describe("ConflictsPanel", () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it("renders open conflicts with both rule texts and source titles", async () => {
    vi.mocked(api.governanceGetView).mockResolvedValue(payload([openTension()]));
    render(ConflictsPanel, { corpusId: "maple", notebookName: "Maple" });
    expect(await screen.findByText("Charter, Article II")).toBeInTheDocument();
    expect(screen.getByText("Decision — Feb 10")).toBeInTheDocument();
    expect(screen.getByText("Quiet hours begin at 11 PM.")).toBeInTheDocument();
  });

  it("dismiss is one click — no dialog — and calls governance_dismiss", async () => {
    vi.mocked(api.governanceGetView).mockResolvedValue(payload([openTension()]));
    render(ConflictsPanel, { corpusId: "maple", notebookName: "Maple" });
    const dismiss = await screen.findByTestId("conflict-dismiss");
    await fireEvent.click(dismiss);
    await waitFor(() =>
      expect(api.governanceDismiss).toHaveBeenCalledWith("maple", "edge-1"),
    );
  });

  it("resolve pre-fills a dated rationale and confirms with the kept rule", async () => {
    vi.mocked(api.governanceGetView).mockResolvedValue(payload([openTension()]));
    render(ConflictsPanel, { corpusId: "maple", notebookName: "Maple" });
    const keepButtons = await screen.findAllByText(/Keep this rule/i);
    await fireEvent.click(keepButtons[0]); // keep rule_a
    const textarea = await screen.findByRole("textbox");
    expect((textarea as HTMLTextAreaElement).value).toMatch(/^Meeting — /);
    const confirm = screen.getByRole("button", { name: /Confirm/i });
    await fireEvent.click(confirm);
    await waitFor(() =>
      expect(api.governanceResolve).toHaveBeenCalledWith(
        "maple",
        "edge-1",
        "claim-a",
        expect.stringMatching(/^Meeting — /),
      ),
    );
  });

  it("accept blocks confirm until a note is entered", async () => {
    vi.mocked(api.governanceGetView).mockResolvedValue(payload([openTension()]));
    render(ConflictsPanel, { corpusId: "maple", notebookName: "Maple" });
    const both = await screen.findByText(/Both can stand/i);
    await fireEvent.click(both);
    const confirm = screen.getByRole("button", { name: /Confirm/i });
    expect(confirm).toBeDisabled();
    const textarea = screen.getByRole("textbox");
    await fireEvent.input(textarea, { target: { value: "intentional" } });
    expect(confirm).not.toBeDisabled();
    await fireEvent.click(confirm);
    await waitFor(() =>
      expect(api.governanceAccept).toHaveBeenCalledWith(
        "maple",
        "edge-1",
        "intentional",
      ),
    );
  });

  it("shows the needs-attention strip when the view reports issues", async () => {
    vi.mocked(api.governanceGetView).mockResolvedValue(
      payload([openTension()], {
        view: {
          rules: [],
          tensions: [openTension()],
          issues: [
            { issue: "adjudicated_tension_not_surfaced", tension: "edge-9" },
          ],
        },
      }),
    );
    render(ConflictsPanel, { corpusId: "maple", notebookName: "Maple" });
    expect(await screen.findByText(/needs attention/i)).toBeInTheDocument();
  });

  it("shows the staleness banner and triggers an in-process rebuild on update", async () => {
    vi.mocked(api.governanceGetView).mockResolvedValue(
      payload([openTension()], { docs_changed_since_build: true }),
    );
    render(ConflictsPanel, { corpusId: "maple", notebookName: "Maple" });
    const update = await screen.findByText(/Update from documents/i);
    await fireEvent.click(update);
    // Clears any zombie enrichment state, then kicks the daemon's
    // in-process tiered build — no `sovereign-cli` subprocess.
    await waitFor(() =>
      expect(api.lcEnrichReset).toHaveBeenCalledWith("maple"),
    );
    expect(api.lcEnrichNow).toHaveBeenCalledWith("maple");
  });

  it("settled conflicts appear in a collapsed history with no actions on moot", async () => {
    const settled = openTension({
      id: "edge-2",
      disposition: { disposition: "moot", dead_endpoint: "claim-a" },
    });
    vi.mocked(api.governanceGetView).mockResolvedValue(payload([settled]));
    render(ConflictsPanel, { corpusId: "maple", notebookName: "Maple" });
    const group = await screen.findByText(/Settled \(1\)/i);
    await fireEvent.click(group);
    expect(
      screen.getByText(/Superseded by a later decision/i),
    ).toBeInTheDocument();
    // Moot rows are not undoable.
    expect(screen.queryByText(/^Undo$/)).not.toBeInTheDocument();
  });

  it("copies an agenda containing both rule quotes", async () => {
    const writeText = vi.fn(async (_text: string) => {});
    Object.assign(navigator, { clipboard: { writeText } });
    vi.mocked(api.governanceGetView).mockResolvedValue(payload([openTension()]));
    render(ConflictsPanel, { corpusId: "maple", notebookName: "Maple" });
    const copy = await screen.findByText(/Copy agenda/i);
    await fireEvent.click(copy);
    await waitFor(() => expect(writeText).toHaveBeenCalled());
    const agenda = writeText.mock.calls[0][0] as string;
    expect(agenda).toContain("Quiet hours begin at 11 PM.");
    expect(agenda).toContain("Charter, Article II");
  });
});
