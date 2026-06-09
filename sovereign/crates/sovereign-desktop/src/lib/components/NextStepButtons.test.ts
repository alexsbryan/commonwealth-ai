// SPDX-License-Identifier: AGPL-3.0-or-later
// NextStepButtons smoke tests. The component is pure props-in: no
// store, no FSM, no Tauri calls — it receives an `offers` array and
// an `onselect` callback. These tests verify rendering + callback
// shape; the click → chat.machine → resumeSession path is exercised
// in ChatView-level tests and by the Rust integration suite.
import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent } from "@testing-library/svelte";
import { NextStepButtons } from "@sovereign/chat-ui";
import type { NextStepOffer } from "../types";

function offer(overrides: Partial<NextStepOffer> = {}): NextStepOffer {
  return {
    label: "Tell me about X",
    description: "Drawn from retrieval",
    follow_up_query: "tell me about x",
    session_ref: "sess-1",
    intent_hint: "knowledge_query",
    ...overrides,
  };
}

describe("NextStepButtons", () => {
  it("renders nothing when offers is empty", () => {
    const onselect = vi.fn();
    const { container } = render(NextStepButtons, {
      props: { offers: [], onselect },
    });
    expect(container.querySelector(".next-steps")).toBeNull();
  });

  it("renders one chip per offer with the label text", () => {
    const onselect = vi.fn();
    render(NextStepButtons, {
      props: {
        offers: [offer({ label: "Compare perspectives" }), offer({ label: "Go deeper" })],
        onselect,
      },
    });
    expect(screen.getByText("Compare perspectives")).toBeInTheDocument();
    expect(screen.getByText("Go deeper")).toBeInTheDocument();
  });

  it("click forwards the full offer to onselect", async () => {
    const onselect = vi.fn();
    const target = offer({ label: "Forward me" });
    render(NextStepButtons, {
      props: { offers: [target], onselect },
    });
    await fireEvent.click(screen.getByText("Forward me"));
    expect(onselect).toHaveBeenCalledTimes(1);
    expect(onselect).toHaveBeenCalledWith(target);
  });

  it("exposes the description as the button's title attribute", () => {
    const onselect = vi.fn();
    render(NextStepButtons, {
      props: {
        offers: [
          offer({ label: "X", description: "Hint text", follow_up_query: "q" }),
        ],
        onselect,
      },
    });
    const btn = screen.getByText("X");
    expect(btn.getAttribute("title")).toBe("Hint text");
  });

  it("falls back to follow_up_query for title when description is absent", () => {
    const onselect = vi.fn();
    render(NextStepButtons, {
      props: {
        offers: [offer({ label: "X", description: null, follow_up_query: "q fallback" })],
        onselect,
      },
    });
    const btn = screen.getByText("X");
    expect(btn.getAttribute("title")).toBe("q fallback");
  });
});
