// SPDX-License-Identifier: AGPL-3.0-or-later
// LessonsPanel tests — the "What I've learned" trust surface.
// Pins: rows render display sentences; the kept-by chip speaks USER
// language and the raw enforcement tokens never render (the no-jargon
// bar applies to our own settings copy); superseded rows strike
// through and resolve "replaced by" via the successor's `supersedes`
// pointer; toggle and delete call the commands; empty + error states.
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent } from "@testing-library/svelte";
import LessonsPanel from "./LessonsPanel.svelte";
import type { LessonRow } from "../types";

vi.mock("../api", () => ({
  listLessons: vi.fn(async () => []),
  setLessonEnabled: vi.fn(async () => true),
  deleteLesson: vi.fn(async () => true),
}));

const api = await import("../api");

function row(overrides: Partial<LessonRow> = {}): LessonRow {
  return {
    id: "n1",
    display: "Keep answers short.",
    prompt_form: "",
    enforcement: "param",
    params: { soft_target_cap: 300 },
    scope: [],
    taught_from: {
      excerpt: "keep answers shorter from now on",
      conversation_id: "c1",
      message_id: "m1",
    },
    enabled: true,
    created: 1_752_000_000,
    first_applied_at: null,
    last_affirmed: null,
    drafted_display: null,
    retired_at: null,
    retired_by: null,
    supersedes: null,
    ...overrides,
  };
}

/** Active param lesson + a superseded prompt lesson and its successor. */
function fixture(): LessonRow[] {
  return [
    row(),
    row({
      id: "n3",
      display: "Explain everything simply.",
      prompt_form: "Explain simply.",
      enforcement: "prompt",
      supersedes: "n2",
    }),
    row({
      id: "n2",
      display: "Explain things like I'm five.",
      prompt_form: "Explain like I'm five.",
      enforcement: "prompt",
      retired_at: 1_752_100_000,
      retired_by: "superseded by n3",
    }),
  ];
}

describe("LessonsPanel", () => {
  beforeEach(() => {
    vi.mocked(api.listLessons).mockClear();
    vi.mocked(api.setLessonEnabled).mockClear();
    vi.mocked(api.deleteLesson).mockClear();
    vi.mocked(api.listLessons).mockResolvedValue(fixture());
  });

  it("renders every lesson's display sentence", async () => {
    render(LessonsPanel);
    await vi.waitFor(() => {
      expect(screen.getByText("Keep answers short.")).toBeInTheDocument();
    });
    expect(screen.getByText("Explain things like I'm five.")).toBeInTheDocument();
  });

  it("kept-by chips speak user language — raw tokens never render", async () => {
    const { container } = render(LessonsPanel);
    await vi.waitFor(() => {
      expect(screen.getByText("answer length")).toBeInTheDocument();
    });
    expect(screen.getAllByText("standing reminder").length).toBeGreaterThan(0);
    // Jargon regression guard: the internal enforcement tokens must
    // never appear anywhere in the pane.
    const text = container.textContent ?? "";
    expect(text).not.toMatch(/\btransform\b/);
    expect(text).not.toMatch(/\bparam\b/);
    expect(text).not.toMatch(/\bprompt\b/);
  });

  it("superseded rows strike through and resolve 'replaced by' via the successor", async () => {
    render(LessonsPanel);
    await vi.waitFor(() => {
      expect(
        screen.getByText("Explain things like I'm five."),
      ).toBeInTheDocument();
    });
    const struck = screen.getByText("Explain things like I'm five.");
    expect(struck.classList.contains("lp-struck")).toBe(true);
    expect(screen.getByText(/replaced by:/i)).toBeInTheDocument();
    // The replaced-by line names the successor's sentence.
    expect(
      screen.getAllByText("Explain everything simply.").length,
    ).toBeGreaterThan(1);
  });

  it("toggle calls setLessonEnabled with the flipped state", async () => {
    render(LessonsPanel);
    await vi.waitFor(() => {
      expect(screen.getByText("Keep answers short.")).toBeInTheDocument();
    });
    const [toggle] = screen.getAllByRole("checkbox");
    await fireEvent.change(toggle);
    await vi.waitFor(() => {
      expect(api.setLessonEnabled).toHaveBeenCalledWith("n1", false);
    });
  });

  it("delete calls deleteLesson and removes the row", async () => {
    render(LessonsPanel);
    await vi.waitFor(() => {
      expect(screen.getByText("Keep answers short.")).toBeInTheDocument();
    });
    const [del] = screen.getAllByRole("button", { name: /^delete$/i });
    await fireEvent.click(del);
    await vi.waitFor(() => {
      expect(api.deleteLesson).toHaveBeenCalledWith("n1");
      expect(screen.queryByText("Keep answers short.")).not.toBeInTheDocument();
    });
  });

  it("shows the empty state when nothing is learned", async () => {
    vi.mocked(api.listLessons).mockResolvedValueOnce([]);
    render(LessonsPanel);
    await vi.waitFor(() => {
      expect(screen.getByText(/nothing learned yet/i)).toBeInTheDocument();
    });
  });

  it("shows the load error state", async () => {
    vi.mocked(api.listLessons).mockRejectedValueOnce(
      new Error("notes.db is not open"),
    );
    render(LessonsPanel);
    await vi.waitFor(() => {
      expect(screen.getByText(/couldn't read lessons/i)).toBeInTheDocument();
    });
  });
});
