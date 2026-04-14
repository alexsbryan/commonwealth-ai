// Singleton wrapper around `approvalMachine`. Exposed as a runed
// module so every component (App.svelte, ApprovalCard, any future
// consumer) subscribes to the same actor without prop drilling.
//
// Construction is lazy: the actor is created + started on first
// access. This matters because the module is imported by components
// that mount/unmount during the setup wizard flow — we want a single
// long-lived actor, not one per component mount.
import { createActor, fromPromise, type Actor } from "xstate";
import { approvalMachine } from "../machines/approval.machine";
import { submitApproval, submitInput } from "../api";

// Real Tauri actors wired in here. Tests inject their own via
// `setApprovalActorForTesting` below (only compiled into the dev
// harness; the app always uses the default wiring).
const wired = approvalMachine.provide({
  actors: {
    submitApproval: fromPromise(
      async ({
        input,
      }: {
        input: { key: string; approved: boolean };
      }) => {
        await submitApproval(input.key, input.approved);
      },
    ),
    submitInput: fromPromise(
      async ({
        input,
      }: {
        input: { key: string; response: string };
      }) => {
        await submitInput(input.key, input.response);
      },
    ),
  },
});

// The actor is constructed and started at module load. A single
// long-lived instance; the app never tears it down. The $state is
// seeded with the actor's initial snapshot and kept in sync via
// subscribe(). Consumers read `approvalStore.snapshot.*` in .svelte
// components, so Svelte 5 picks up updates reactively.
const _actor: Actor<typeof wired> = createActor(wired);
type ApprovalSnapshot = ReturnType<typeof _actor.getSnapshot>;

let _snapshot: ApprovalSnapshot = $state(_actor.getSnapshot());
_actor.subscribe((snap) => {
  _snapshot = snap;
});
_actor.start();

export const approvalStore = {
  /** Reactive snapshot — reads `$state`, so consumers in .svelte
   *  components automatically re-render on updates. */
  get snapshot() {
    return _snapshot;
  },
  get pendingApproval() {
    return _snapshot.context.pendingApproval;
  },
  get pendingInput() {
    return _snapshot.context.pendingInput;
  },
  send(event: Parameters<Actor<typeof wired>["send"]>[0]) {
    _actor.send(event);
  },
};
