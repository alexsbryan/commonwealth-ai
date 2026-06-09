// SPDX-License-Identifier: AGPL-3.0-or-later
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
  // Returns a SearchAugmentation by default (matches the real Tauri
  // command's return shape). Individual tests override via
  // mockResolvedValueOnce / mockRejectedValueOnce when they need to
  // simulate the no-results / network-error / channel-race paths.
  submitInformationSearch: vi.fn(async () => ({
    query: "mock query",
    backend_id: "duckduckgo",
    sources: [{ title: "Mock", url: "https://example.test/x" }],
    accepted: true,
  })),
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
    kind: "refinement",
    task_title: "",
    ...overrides,
  };
}

describe("InformationRequestCard", () => {
  beforeEach(() => {
    vi.mocked(api.submitInformationResponse).mockClear();
    vi.mocked(api.submitInformationSearch).mockClear();
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

  it("Search the web forwards the gap text + calls onHandled on success", async () => {
    const onHandled = vi.fn();
    render(InformationRequestCard, {
      props: {
        request: payload({ key: "r-search", gap: "Mac Studio next gen date" }),
        onHandled,
      },
    });
    const searchBtn = screen.getByRole("button", { name: /search the web/i });
    await fireEvent.click(searchBtn);
    expect(api.submitInformationSearch).toHaveBeenCalledWith(
      "r-search",
      "Mac Studio next gen date",
      // conversationId — component threads through; test scenario
      // doesn't pass one so it's `null`. `submitInformationSearch`
      // signature: (key, query, conversationId?). Pinned here so a
      // future signature change can't silently regress the spy.
      null,
    );
    await vi.waitFor(() => expect(onHandled).toHaveBeenCalled());
  });

  it("Search the web shows the error inline + keeps the card live on failure", async () => {
    const onHandled = vi.fn();
    // Tauri command-handler errors arrive as plain strings on the JS
    // side. Simulate a DDG-bot-block / zero-results path.
    vi.mocked(api.submitInformationSearch).mockRejectedValueOnce(
      "web search returned 0 results via duckduckgo",
    );
    render(InformationRequestCard, {
      props: { request: payload({ key: "r-fail" }), onHandled },
    });
    const searchBtn = screen.getByRole("button", { name: /search the web/i });
    await fireEvent.click(searchBtn);
    await vi.waitFor(() => {
      expect(
        screen.getByText(/web search returned 0 results/i),
      ).toBeInTheDocument();
    });
    // Card must stay live — onHandled must NOT have been called so the
    // user can still paste / skip / retry.
    expect(onHandled).not.toHaveBeenCalled();
  });
});
