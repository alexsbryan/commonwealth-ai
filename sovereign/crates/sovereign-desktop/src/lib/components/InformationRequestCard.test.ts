// InformationRequestCard smoke tests. The card is pure props-in — no
// store, no machine — so these are straightforward template tests.
// Goal: verify each section renders its payload field and that
// Submit / Skip invoke the Tauri command with the expected args.
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent } from "@testing-library/svelte";
import InformationRequestCard from "./InformationRequestCard.svelte";
import type { InformationRequestPayload } from "../types";

vi.mock("../api", () => ({
  submitInformationResponse: vi.fn(async () => undefined),
}));

const api = await import("../api");

function payload(
  overrides: Partial<InformationRequestPayload> = {},
): InformationRequestPayload {
  return {
    task_id: "t",
    step_id: 0,
    key: "r1",
    current_understanding: "CU text",
    gap: "Gap question",
    relevance: "Relevance note",
    satisfying_source: "A 2024 paper",
    search_hints: ["hint one", "hint two"],
    ...overrides,
  };
}

describe("InformationRequestCard", () => {
  beforeEach(() => {
    vi.mocked(api.submitInformationResponse).mockClear();
  });

  it("renders nothing when request is null", () => {
    const onHandled = vi.fn();
    render(InformationRequestCard, { props: { request: null, onHandled } });
    expect(
      screen.queryByText(/information request/i),
    ).not.toBeInTheDocument();
  });

  it("renders every populated field from the payload", () => {
    const onHandled = vi.fn();
    render(InformationRequestCard, {
      props: { request: payload(), onHandled },
    });
    expect(screen.getByText(/information request/i)).toBeInTheDocument();
    expect(screen.getByText("CU text")).toBeInTheDocument();
    expect(screen.getByText("Gap question")).toBeInTheDocument();
    expect(screen.getByText("Relevance note")).toBeInTheDocument();
    expect(screen.getByText("A 2024 paper")).toBeInTheDocument();
    expect(screen.getByText("hint one")).toBeInTheDocument();
    expect(screen.getByText("hint two")).toBeInTheDocument();
  });

  it("Submit forwards pasted text via submitInformationResponse + calls onHandled", async () => {
    const onHandled = vi.fn();
    render(InformationRequestCard, {
      props: { request: payload({ key: "r-sub" }), onHandled },
    });
    const textarea = screen.getByRole("textbox");
    await fireEvent.input(textarea, {
      target: { value: "Here is the passage." },
    });
    const submit = screen.getByRole("button", { name: /^submit$/i });
    await fireEvent.click(submit);

    expect(api.submitInformationResponse).toHaveBeenCalledWith(
      "r-sub",
      "Here is the passage.",
    );
    // onHandled fires after the async submit resolves.
    await vi.waitFor(() => expect(onHandled).toHaveBeenCalled());
  });

  it("Skip submits a null response + calls onHandled", async () => {
    const onHandled = vi.fn();
    render(InformationRequestCard, {
      props: { request: payload({ key: "r-skip" }), onHandled },
    });
    const skip = screen.getByRole("button", { name: /skip/i });
    await fireEvent.click(skip);
    expect(api.submitInformationResponse).toHaveBeenCalledWith("r-skip", null);
    await vi.waitFor(() => expect(onHandled).toHaveBeenCalled());
  });
});
