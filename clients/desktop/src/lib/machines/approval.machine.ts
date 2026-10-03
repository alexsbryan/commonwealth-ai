// SPDX-License-Identifier: AGPL-3.0-or-later
// approvalMachine — owns the UI side of the agent's oneshot
// request/response loops for both *approval* (yes/no on a tool call)
// and *user input* (free-text question). The Rust side emits two
// Tauri events with identical machinery underneath
// (`approval.rs:request_approval` + `ask_user`), and Phase 3 folds
// both into a single machine.
//
// Pre-migration shape:
//   - App.svelte held `pendingApproval` + `pendingInput` as $state
//   - Drilled through ChatView → ApprovalCard as 4 props
//   - Listeners in events.ts set state; ApprovalCard called Tauri
//     commands directly and called `on*Handled` callbacks to clear
//
// Post-migration shape:
//   - Machine owns both request slots + in-flight submission state
//   - Exposed via `stores/approval.svelte.ts` singleton so every
//     consumer (App, ChatView, ApprovalCard, test harness) reaches
//     the same actor without prop drilling
//   - Parallel regions: approval + input can be pending concurrently
//     (uncommon but not structurally forbidden)
//
// Two regions, each with the same shape:
//
//   idle ──(REQUEST_ARRIVED)──▶ pending ──(SUBMIT)──▶ submitting
//          │                                               │
//          └─(second REQUEST_ARRIVED, last wins) ───────── │
//                                                          │
//                   submitting ──(onDone | onError)───▶ idle
//
// SUBMIT carries the chosen value (bool for approval, string for
// input). Actors are supplied via `.provide({ actors })` at the
// singleton construction site (real Tauri commands) and in tests
// (spies).
import { assign, fromPromise, setup } from "xstate";
import type {
  ApprovalRequestPayload,
  UserInputRequestPayload,
} from "../types";

export interface ApprovalContext {
  pendingApproval: ApprovalRequestPayload | null;
  pendingInput: UserInputRequestPayload | null;
}

export type ApprovalEvent =
  // Tauri-forwarded events (wrapper converts listen() payloads).
  | { type: "APPROVAL_REQUEST_ARRIVED"; payload: ApprovalRequestPayload }
  | { type: "INPUT_REQUEST_ARRIVED"; payload: UserInputRequestPayload }
  // User-driven submissions — the machine invokes the real Tauri
  // command as an actor so failures can transition cleanly.
  | { type: "APPROVAL_SUBMIT"; key: string; approved: boolean }
  | { type: "INPUT_SUBMIT"; key: string; response: string };

export const approvalMachine = setup({
  types: {
    context: {} as ApprovalContext,
    events: {} as ApprovalEvent,
  },
  actors: {
    submitApproval: fromPromise(
      async (_: {
        input: { key: string; approved: boolean };
      }): Promise<void> => {
        throw new Error("submitApproval actor not provided");
      },
    ),
    submitInput: fromPromise(
      async (_: {
        input: { key: string; response: string };
      }): Promise<void> => {
        throw new Error("submitInput actor not provided");
      },
    ),
  },
}).createMachine({
  id: "approval",
  type: "parallel",
  context: {
    pendingApproval: null,
    pendingInput: null,
  },
  states: {
    approval: {
      initial: "idle",
      states: {
        idle: {
          on: {
            APPROVAL_REQUEST_ARRIVED: {
              target: "pending",
              actions: assign({
                pendingApproval: ({ event }) => event.payload,
              }),
            },
          },
        },
        pending: {
          on: {
            // A second request while one is pending overwrites. In
            // practice the backend blocks on the oneshot channel, so
            // a newer request means the previous was either resolved
            // or cancelled — last wins is safe.
            APPROVAL_REQUEST_ARRIVED: {
              actions: assign({
                pendingApproval: ({ event }) => event.payload,
              }),
            },
            APPROVAL_SUBMIT: { target: "submitting" },
          },
        },
        submitting: {
          invoke: {
            src: "submitApproval",
            input: ({ event }) => {
              const e = event as Extract<
                ApprovalEvent,
                { type: "APPROVAL_SUBMIT" }
              >;
              return { key: e.key, approved: e.approved };
            },
            onDone: {
              target: "idle",
              actions: assign({ pendingApproval: () => null }),
            },
            onError: {
              // If the Tauri command throws (rare — the backend's
              // submit_approval always succeeds unless the key is
              // stale), fall back to idle anyway. The worst case is
              // the user sees a cleared card for an approval the
              // backend never recorded; the backend will eventually
              // emit a fresh event or the turn will time out.
              target: "idle",
              actions: assign({ pendingApproval: () => null }),
            },
          },
        },
      },
    },
    input: {
      initial: "idle",
      states: {
        idle: {
          on: {
            INPUT_REQUEST_ARRIVED: {
              target: "pending",
              actions: assign({
                pendingInput: ({ event }) => event.payload,
              }),
            },
          },
        },
        pending: {
          on: {
            INPUT_REQUEST_ARRIVED: {
              actions: assign({
                pendingInput: ({ event }) => event.payload,
              }),
            },
            INPUT_SUBMIT: { target: "submitting" },
          },
        },
        submitting: {
          invoke: {
            src: "submitInput",
            input: ({ event }) => {
              const e = event as Extract<
                ApprovalEvent,
                { type: "INPUT_SUBMIT" }
              >;
              return { key: e.key, response: e.response };
            },
            onDone: {
              target: "idle",
              actions: assign({ pendingInput: () => null }),
            },
            onError: {
              target: "idle",
              actions: assign({ pendingInput: () => null }),
            },
          },
        },
      },
    },
  },
});
