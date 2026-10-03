// SPDX-License-Identifier: AGPL-3.0-or-later
// LessonCard smoke tests — same props-in pattern as
// InformationRequestCard.test.ts. Pins the consent contract from
// TEACHABLE.md §4/§11: Save persists (with prompt_form only
// overwritten by an actual edit on the prompt rung, and the pre-edit
// sentence kept as the correction pair), "Not this" stores NOTHING,
// and a failed save keeps the card live.
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent } from "@testing-library/svelte";
import LessonCard from "./LessonCard.svelte";
import type { LessonProposedPayload } from "../types";

vi.mock("../api", () => ({
  saveLesson: vi.fn(async () => "note-1"),
}));

const api = await import("../api");

function proposal(
  overrides: Partial<LessonProposedPayload> = {},
): LessonProposedPayload {
  return {
    id: "draft-1",
    conversation_id: "c1",
    message_id: "m1",
    display: "Explain things simply.",
    prompt_form: "Explain like I'm five.",
    enforcement: "prompt",
    params: {},
    taught_from: "from now on explain things like i am five",
    ...overrides,
  };
}

describe("LessonCard", () => {
  beforeEach(() => {
    vi.mocked(api.saveLesson).mockClear();
    vi.mocked(api.saveLesson).mockResolvedValue("note-1");
  });

  it("renders nothing when proposal is null", () => {
    render(LessonCard, { props: { proposal: null, onHandled: vi.fn() } });
    expect(screen.queryByText(/learn this\?/i)).not.toBeInTheDocument();
  });

  it("renders the header and the drafted sentence", () => {
    render(LessonCard, { props: { proposal: proposal(), onHandled: vi.fn() } });
    expect(screen.getByText(/learn this\?/i)).toBeInTheDocument();
    expect(screen.getByText("Explain things simply.")).toBeInTheDocument();
  });

  it("Save (unedited) passes the compiled prompt_form through untouched", async () => {
    const onHandled = vi.fn();
    render(LessonCard, { props: { proposal: proposal(), onHandled } });
    await fireEvent.click(screen.getByRole("button", { name: /^save$/i }));

    await vi.waitFor(() => expect(onHandled).toHaveBeenCalled());
    expect(api.saveLesson).toHaveBeenCalledWith(
      expect.objectContaining({
        display: "Explain things simply.",
        prompt_form: "Explain like I'm five.",
      }),
      // No edit → no correction pair.
      null,
    );
  });

  it("'Not this' dismisses without any backend call — nothing stored", async () => {
    const onHandled = vi.fn();
    render(LessonCard, { props: { proposal: proposal(), onHandled } });
    await fireEvent.click(screen.getByRole("button", { name: /not this/i }));
    expect(onHandled).toHaveBeenCalled();
    expect(api.saveLesson).not.toHaveBeenCalled();
  });

  it("edited Save rewrites display AND prompt_form on the prompt rung, keeping the correction pair", async () => {
    const onHandled = vi.fn();
    render(LessonCard, { props: { proposal: proposal(), onHandled } });
    await fireEvent.click(screen.getByRole("button", { name: /^edit$/i }));
    const textarea = screen.getByRole("textbox");
    await fireEvent.input(textarea, {
      target: { value: "Explain everything like I'm five years old." },
    });
    await fireEvent.click(screen.getByRole("button", { name: /^save$/i }));

    await vi.waitFor(() => expect(onHandled).toHaveBeenCalled());
    expect(api.saveLesson).toHaveBeenCalledWith(
      expect.objectContaining({
        display: "Explain everything like I'm five years old.",
        prompt_form: "Explain everything like I'm five years old.",
      }),
      // The pre-edit sentence rides along as drafted_display.
      "Explain things simply.",
    );
  });

  it("edited Save on a param-rung lesson never touches the compiled machinery", async () => {
    const onHandled = vi.fn();
    render(LessonCard, {
      props: {
        proposal: proposal({
          display: "Keep answers short.",
          prompt_form: "",
          enforcement: "param",
          params: { soft_target_cap: 300 },
        }),
        onHandled,
      },
    });
    await fireEvent.click(screen.getByRole("button", { name: /^edit$/i }));
    await fireEvent.input(screen.getByRole("textbox"), {
      target: { value: "Keep every answer brief." },
    });
    await fireEvent.click(screen.getByRole("button", { name: /^save$/i }));

    await vi.waitFor(() => expect(onHandled).toHaveBeenCalled());
    expect(api.saveLesson).toHaveBeenCalledWith(
      expect.objectContaining({
        display: "Keep every answer brief.",
        prompt_form: "",
        params: { soft_target_cap: 300 },
      }),
      "Keep answers short.",
    );
  });

  it("a failed save shows the error and keeps the card live", async () => {
    const onHandled = vi.fn();
    vi.mocked(api.saveLesson).mockRejectedValueOnce(
      "Lessons unavailable: notes.db is not open",
    );
    render(LessonCard, { props: { proposal: proposal(), onHandled } });
    await fireEvent.click(screen.getByRole("button", { name: /^save$/i }));

    await vi.waitFor(() => {
      expect(screen.getByText(/notes\.db is not open/i)).toBeInTheDocument();
    });
    expect(onHandled).not.toHaveBeenCalled();
    // Buttons re-enabled for retry.
    expect(screen.getByRole("button", { name: /^save$/i })).toBeEnabled();
  });

  it("Cancel edit restores the read-only sentence", async () => {
    render(LessonCard, { props: { proposal: proposal(), onHandled: vi.fn() } });
    await fireEvent.click(screen.getByRole("button", { name: /^edit$/i }));
    expect(screen.getByRole("textbox")).toBeInTheDocument();
    await fireEvent.click(screen.getByRole("button", { name: /cancel edit/i }));
    expect(screen.queryByRole("textbox")).not.toBeInTheDocument();
    expect(screen.getByText("Explain things simply.")).toBeInTheDocument();
  });
});
