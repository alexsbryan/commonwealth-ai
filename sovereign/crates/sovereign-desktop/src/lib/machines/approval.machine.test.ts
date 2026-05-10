// Unit tests for approvalMachine. Drives the actor directly via
// createActor — no DOM, no Svelte, no singleton store.
import { describe, it, expect, vi } from "vitest";
import { createActor, fromPromise } from "xstate";
import { approvalMachine } from "./approval.machine";
import type {
  ApprovalRequestPayload,
  UserInputRequestPayload,
} from "../types";

function approvalPayload(key = "t:1:0"): ApprovalRequestPayload {
  return {
    task_id: "t",
    step_id: 0,
    key,
    tool_id: "shell",
    description: "Run `ls`",
    params: { command: "ls" },
  };
}

function inputPayload(key = "t:input"): UserInputRequestPayload {
  return {
    task_id: "t",
    key,
    question: "What's the target dir?",
  };
}

function waitFor<T>(
  actor: ReturnType<typeof createActor>,
  predicate: (snap: ReturnType<typeof actor.getSnapshot>) => boolean,
  timeoutMs = 1000,
): Promise<void> {
  return new Promise((resolve, reject) => {
    const start = Date.now();
    const check = () => {
      if (predicate(actor.getSnapshot())) return resolve();
      if (Date.now() - start > timeoutMs) {
        return reject(
          new Error(
            `waitFor timeout. Last state: ${JSON.stringify(
              actor.getSnapshot().value,
            )}`,
          ),
        );
      }
      setTimeout(check, 5);
    };
    check();
  });
  void 0 as unknown as T; // silence the unused generic
}

function makeMachine(opts: {
  submitApproval?: (input: { key: string; approved: boolean }) => Promise<void>;
  submitInput?: (input: { key: string; response: string }) => Promise<void>;
} = {}) {
  const approveImpl = opts.submitApproval ?? (async () => {});
  const inputImpl = opts.submitInput ?? (async () => {});
  return approvalMachine.provide({
    actors: {
      submitApproval: fromPromise(
        ({ input }: { input: { key: string; approved: boolean } }) =>
          approveImpl(input),
      ),
      submitInput: fromPromise(
        ({ input }: { input: { key: string; response: string } }) =>
          inputImpl(input),
      ),
    },
  });
}

describe("approvalMachine — approval region", () => {
  it("starts idle with no pending request", () => {
    const actor = createActor(makeMachine());
    actor.start();
    expect(actor.getSnapshot().matches({ approval: "idle" })).toBe(true);
    expect(actor.getSnapshot().context.pendingApproval).toBeNull();
  });

  it("APPROVAL_REQUEST_ARRIVED transitions to pending", () => {
    const actor = createActor(makeMachine());
    actor.start();
    const p = approvalPayload();
    actor.send({ type: "APPROVAL_REQUEST_ARRIVED", payload: p });
    expect(actor.getSnapshot().matches({ approval: "pending" })).toBe(true);
    expect(actor.getSnapshot().context.pendingApproval).toEqual(p);
  });

  it("APPROVAL_SUBMIT invokes submitApproval and returns to idle", async () => {
    const submitApproval = vi.fn(async () => {});
    const actor = createActor(makeMachine({ submitApproval }));
    actor.start();
    actor.send({
      type: "APPROVAL_REQUEST_ARRIVED",
      payload: approvalPayload("k1"),
    });
    actor.send({ type: "APPROVAL_SUBMIT", key: "k1", approved: true });

    await waitFor(actor, (s) => s.matches({ approval: "idle" }));
    expect(submitApproval).toHaveBeenCalledWith({ key: "k1", approved: true });
    expect(actor.getSnapshot().context.pendingApproval).toBeNull();
  });

  it("clears pending even when submitApproval rejects", async () => {
    // Stale key — Rust side returns false / throws. UI should still
    // clear the card so the user isn't stuck.
    const actor = createActor(
      makeMachine({
        submitApproval: async () => {
          throw new Error("stale key");
        },
      }),
    );
    actor.start();
    actor.send({
      type: "APPROVAL_REQUEST_ARRIVED",
      payload: approvalPayload("k1"),
    });
    actor.send({ type: "APPROVAL_SUBMIT", key: "k1", approved: false });
    await waitFor(actor, (s) => s.matches({ approval: "idle" }));
    expect(actor.getSnapshot().context.pendingApproval).toBeNull();
  });

  it("second APPROVAL_REQUEST_ARRIVED while pending overwrites (last wins)", () => {
    const actor = createActor(makeMachine());
    actor.start();
    actor.send({
      type: "APPROVAL_REQUEST_ARRIVED",
      payload: approvalPayload("first"),
    });
    actor.send({
      type: "APPROVAL_REQUEST_ARRIVED",
      payload: approvalPayload("second"),
    });
    expect(actor.getSnapshot().context.pendingApproval?.key).toBe("second");
    expect(actor.getSnapshot().matches({ approval: "pending" })).toBe(true);
  });
});

describe("approvalMachine — input region", () => {
  it("INPUT_REQUEST_ARRIVED + submit roundtrip", async () => {
    const submitInput = vi.fn(async () => {});
    const actor = createActor(makeMachine({ submitInput }));
    actor.start();
    actor.send({
      type: "INPUT_REQUEST_ARRIVED",
      payload: inputPayload("i1"),
    });
    expect(actor.getSnapshot().context.pendingInput?.key).toBe("i1");

    actor.send({ type: "INPUT_SUBMIT", key: "i1", response: "home" });
    await waitFor(actor, (s) => s.matches({ input: "idle" }));
    expect(submitInput).toHaveBeenCalledWith({ key: "i1", response: "home" });
    expect(actor.getSnapshot().context.pendingInput).toBeNull();
  });
});

describe("approvalMachine — parallel regions", () => {
  it("approval and input pending simultaneously don't clobber each other", async () => {
    const actor = createActor(makeMachine());
    actor.start();
    actor.send({
      type: "APPROVAL_REQUEST_ARRIVED",
      payload: approvalPayload("a"),
    });
    actor.send({
      type: "INPUT_REQUEST_ARRIVED",
      payload: inputPayload("i"),
    });
    expect(actor.getSnapshot().matches({ approval: "pending" })).toBe(true);
    expect(actor.getSnapshot().matches({ input: "pending" })).toBe(true);
    expect(actor.getSnapshot().context.pendingApproval?.key).toBe("a");
    expect(actor.getSnapshot().context.pendingInput?.key).toBe("i");

    // Resolve approval — input should still be pending.
    actor.send({ type: "APPROVAL_SUBMIT", key: "a", approved: true });
    await waitFor(actor, (s) => s.matches({ approval: "idle" }));
    expect(actor.getSnapshot().context.pendingInput?.key).toBe("i");
    expect(actor.getSnapshot().matches({ input: "pending" })).toBe(true);
  });
});
