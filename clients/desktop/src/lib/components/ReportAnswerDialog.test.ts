// SPDX-License-Identifier: AGPL-3.0-or-later
//
// ReportAnswerDialog tests.
//
// The load-bearing behaviour here is not the layout — it is the
// consent gate. This dialog is the one place in the product where a
// user can put the contents of their own documents into a file they
// are about to hand to someone else, and the default has to be that
// they don't. The Rust renderer refuses to print unauthorised snippets
// as a second line of defence (`turn_report.rs`), but that backstop is
// only a backstop: the payload should never carry the text at all.

import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/svelte";
import ReportAnswerDialog from "./ReportAnswerDialog.svelte";
import type { TurnSnapshot } from "../api";

vi.mock("../api", () => ({
  prepareAnswerReport: vi.fn(),
}));

const api = await import("../api");

function turn(): TurnSnapshot {
  return {
    conversation_id: "conv-1",
    message_id: "msg-1",
    question: "what did the Q3 report say about margins?",
    answer: "I don't have anything on that.",
    route: "KnowledgeQuery",
    retrieved: [
      {
        title: "Q3 Board Report",
        corpus_id: "work",
        chunk_id: 7,
        snippet: "Gross margin fell to 41% on supplier repricing.",
      },
    ],
    include_source_text: false,
  };
}

function mount(overrides: Partial<TurnSnapshot> = {}) {
  const onclose = vi.fn();
  const utils = render(ReportAnswerDialog, {
    props: {
      turn: { ...turn(), ...overrides },
      sourceTitles: ["Q3 Board Report"],
      onclose,
    },
  });
  return { ...utils, onclose };
}

/** The snapshot the component actually sent to the backend. */
function sentTurn(): TurnSnapshot {
  const call = vi.mocked(api.prepareAnswerReport).mock.calls[0];
  return call[0];
}

describe("ReportAnswerDialog", () => {
  beforeEach(() => {
    vi.mocked(api.prepareAnswerReport).mockReset();
    vi.mocked(api.prepareAnswerReport).mockResolvedValue({
      report_path: "/home/u/Desktop/svrnmesh-answer-2AM-QSC.md",
      issues_url: "https://example.invalid/issues",
      reference_code: "2AM-QSC",
    });
  });

  it("withholds the text of the sources unless the reporter opts in", async () => {
    mount();
    await fireEvent.click(screen.getByText("Create report"));
    await waitFor(() => expect(api.prepareAnswerReport).toHaveBeenCalled());

    const sent = sentTurn();
    expect(sent.include_source_text).toBe(false);
    // The title travels — that is what answers "did retrieval find the
    // right document?". The passage body does not.
    expect(sent.retrieved?.[0].title).toBe("Q3 Board Report");
    expect(sent.retrieved?.[0].snippet).toBeNull();
    expect(JSON.stringify(sent)).not.toContain("Gross margin fell");
  });

  it("sends the source text once the reporter ticks the box", async () => {
    const { container } = mount();
    const box = container.querySelector<HTMLInputElement>(
      ".consent input[type=checkbox]",
    );
    expect(box).not.toBeNull();
    await fireEvent.click(box!);
    await fireEvent.click(screen.getByText("Create report"));
    await waitFor(() => expect(api.prepareAnswerReport).toHaveBeenCalled());

    const sent = sentTurn();
    expect(sent.include_source_text).toBe(true);
    expect(sent.retrieved?.[0].snippet).toContain("Gross margin fell");
  });

  it("names the documents in the consent prompt", async () => {
    // An abstract "include source text?" asks the user to consent to
    // something they cannot see. Naming the document is the difference
    // between a checkbox and an informed choice.
    mount();
    expect(screen.getByText(/Q3 Board Report/)).toBeTruthy();
  });

  it("offers no consent control when nothing was retrieved", async () => {
    const { container } = render(ReportAnswerDialog, {
      props: {
        turn: { ...turn(), retrieved: [] },
        sourceTitles: [],
        onclose: vi.fn(),
      },
    });
    expect(container.querySelector(".consent")).toBeNull();
  });

  it("shows the reference code back so the user can quote it", async () => {
    mount();
    await fireEvent.click(screen.getByText("Create report"));
    await waitFor(() => expect(screen.getByText("2AM-QSC")).toBeTruthy());
    // And the path, since the next step is "open it and read it".
    expect(
      screen.getByText("/home/u/Desktop/svrnmesh-answer-2AM-QSC.md"),
    ).toBeTruthy();
  });

  it("surfaces a failure instead of closing as though it worked", async () => {
    // Silently dismissing here would leave someone believing a report
    // exists on their Desktop that does not.
    vi.mocked(api.prepareAnswerReport).mockRejectedValue(
      new Error("could not resolve user Desktop directory"),
    );
    const { onclose } = mount();
    await fireEvent.click(screen.getByText("Create report"));
    await waitFor(() =>
      expect(screen.getByText(/could not resolve user Desktop/)).toBeTruthy(),
    );
    expect(onclose).not.toHaveBeenCalled();
  });

  it("passes the user's note through — it is the only field we can't derive", async () => {
    const { container } = mount();
    const box = container.querySelector<HTMLTextAreaElement>("textarea");
    await fireEvent.input(box!, {
      target: { value: "the report is in my library, I added it Tuesday" },
    });
    await fireEvent.click(screen.getByText("Create report"));
    await waitFor(() => expect(api.prepareAnswerReport).toHaveBeenCalled());
    expect(vi.mocked(api.prepareAnswerReport).mock.calls[0][1]).toContain(
      "added it Tuesday",
    );
  });
});
