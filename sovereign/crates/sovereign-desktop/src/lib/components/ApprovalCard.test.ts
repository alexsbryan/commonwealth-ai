// SPDX-License-Identifier: AGPL-3.0-or-later
// ApprovalCard smoke tests. The card reads from the `approvalStore`
// singleton; driving the store with events exercises the real
// machine, the real subscribe path, and the Svelte rendering glue
// together.
//
// Tests share the same singleton instance, so each test ends by
// clearing its own pending state (dispatching *_SUBMIT or waiting
// for the region to return to `idle`). The Tauri API mock in
// test-setup.ts makes the invoke a no-op.
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent } from "@testing-library/svelte";
import ApprovalCard from "./ApprovalCard.svelte";
import type {
  ApprovalRequestPayload,
  UserInputRequestPayload,
} from "../types";

// Mock the api layer so `submitApproval` / `submitInput` resolve
// instantly — we assert on the command being *called*, not its
// return.
vi.mock("../api", () => ({
  submitApproval: vi.fn(async () => true),
  submitInput: vi.fn(async () => true),
}));

const api = await import("../api");
const { approvalStore } = await import("../stores/approval.svelte");

function approvalPayload(
  overrides: Partial<ApprovalRequestPayload> = {},
): ApprovalRequestPayload {
  return {
    task_id: "t",
    step_id: 0,
    key: "k1",
    tool_id: "shell",
    description: "Run `ls`",
    params: { command: "ls" },
    ...overrides,
  };
}

function inputPayload(
  overrides: Partial<UserInputRequestPayload> = {},
): UserInputRequestPayload {
  return {
    task_id: "t",
    key: "i1",
    question: "What directory?",
    ...overrides,
  };
}

/** Wait until `approvalStore.snapshot.matches(value)` is true. Short
 *  poll — machine transitions settle within a few microtasks. */
async function whenMatches(
  value: Parameters<typeof approvalStore.snapshot.matches>[0],
  timeoutMs = 500,
): Promise<void> {
  const start = Date.now();
  while (!approvalStore.snapshot.matches(value)) {
    if (Date.now() - start > timeoutMs) {
      throw new Error(
        `whenMatches timeout waiting for ${JSON.stringify(value)}; got ${JSON.stringify(approvalStore.snapshot.value)}`,
      );
    }
    await Promise.resolve();
    await new Promise((r) => setTimeout(r, 2));
  }
}

async function resetStore(): Promise<void> {
  // If a previous test left a slot pending, force it to idle. Sending
  // _SUBMIT from `idle` is a no-op — safe to call unconditionally.
  if (approvalStore.snapshot.matches({ approval: "pending" })) {
    approvalStore.send({ type: "APPROVAL_SUBMIT", key: "cleanup", approved: false });
    await whenMatches({ approval: "idle" });
  }
  if (approvalStore.snapshot.matches({ input: "pending" })) {
    approvalStore.send({ type: "INPUT_SUBMIT", key: "cleanup", response: "" });
    await whenMatches({ input: "idle" });
  }
}

describe("ApprovalCard", () => {
  beforeEach(async () => {
    vi.mocked(api.submitApproval).mockClear();
    vi.mocked(api.submitInput).mockClear();
    await resetStore();
  });

  it("renders nothing when no request is pending", () => {
    render(ApprovalCard);
    expect(screen.queryByText(/approval required/i)).not.toBeInTheDocument();
    expect(screen.queryByText(/input needed/i)).not.toBeInTheDocument();
  });

  it("renders the approval card when one arrives on the store", async () => {
    render(ApprovalCard);
    approvalStore.send({
      type: "APPROVAL_REQUEST_ARRIVED",
      payload: approvalPayload({ description: "Run `ls -la`" }),
    });
    expect(await screen.findByText(/approval required/i)).toBeInTheDocument();
    expect(screen.getByText("Run `ls -la`")).toBeInTheDocument();
  });

  it("clicking Allow dispatches APPROVAL_SUBMIT with approved=true", async () => {
    render(ApprovalCard);
    approvalStore.send({
      type: "APPROVAL_REQUEST_ARRIVED",
      payload: approvalPayload({ key: "k-allow" }),
    });
    const allow = await screen.findByRole("button", { name: /allow/i });
    await fireEvent.click(allow);
    // The machine invokes the api mock; after it resolves, the card
    // clears (idle again).
    await whenMatches({ approval: "idle" });
    expect(api.submitApproval).toHaveBeenCalledWith("k-allow", true);
  });

  it("renders the input card and submits typed text", async () => {
    render(ApprovalCard);
    approvalStore.send({
      type: "INPUT_REQUEST_ARRIVED",
      payload: inputPayload({ key: "i-ask", question: "Target directory?" }),
    });
    expect(await screen.findByText("Target directory?")).toBeInTheDocument();

    const textbox = screen.getByRole("textbox");
    await fireEvent.input(textbox, { target: { value: "/home" } });
    const submit = screen.getByRole("button", { name: /submit/i });
    await fireEvent.click(submit);

    await whenMatches({ input: "idle" });
    expect(api.submitInput).toHaveBeenCalledWith("i-ask", "/home");
  });
});
