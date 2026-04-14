// skillsMachine — XState v5 machine for the Settings → Skills panel.
//
// Motivating bug (pre-migration): the old `SkillManager.svelte` had an
// `onMount` that called `invoke("list_skills")` immediately. If bootstrap
// was still in flight the Rust side returned "Backend is still loading",
// the try/catch swallowed it, and the panel froze on "No skills found."
// After a retry-on-backend-ready hotfix it could still freeze on
// "Loading skills…" because Tauri events aren't replayed — a listener
// attached after `backend-ready` fires never catches it.
//
// Design: model the flow as an explicit FSM with:
//
//   loading ──(fetch succeeds)──▶ ready
//      │
//      └──(fetch fails: "still loading")──▶ waitingForBackend
//                                              │   │
//                                              │   ├──(BOOTSTRAP_COMPLETE, fast path)
//                                              │   │
//                                              │   └──(after 2s, polling fallback)──▶ loading
//                                              │
//                                              └──(RETRY)──▶ loading
//      │
//      └──(fetch fails: other)──▶ error
//
//   ready ──(TOGGLE_SKILL id)──▶ toggling ──(succeeds)──▶ ready (updated)
//                                   └──(fails)──▶ ready (unchanged, logged)
//
//   error ──(RETRY)──▶ loading
//
// The `waitingForBackend` / polling fallback is the key insight: we don't
// rely on catching `backend-ready` — it's a fast path. Even if the event
// was missed, the polling timer will eventually re-attempt the fetch
// once the backend is actually up.
//
// Side effects are two promise-based actors (`fetchSkills`, `toggleSkill`)
// whose implementations are provided by the component wrapper. Tests
// swap them out with `vi.fn()`s — the machine itself is pure.
import { assign, fromPromise, setup } from "xstate";
import { produce } from "immer";

export interface SkillEntry {
  id: string;
  name: string;
  description: string;
  active: boolean;
  trust_level: string;
}

export interface SkillsContext {
  skills: SkillEntry[];
  errorMessage: string;
  /** id of the skill whose toggle is in flight, if any */
  togglingId: string | null;
}

export type SkillsEvent =
  /** Fired by the Svelte wrapper when `backend-ready` arrives. Ignored
   *  unless we're waiting for it. */
  | { type: "BOOTSTRAP_COMPLETE" }
  /** User-initiated retry from the `error` state. */
  | { type: "RETRY" }
  /** User toggled a skill in the UI. */
  | { type: "TOGGLE_SKILL"; id: string; active: boolean };

/** Retry cadence while waiting for bootstrap. Short enough that users
 *  perceive the list as "just a moment", long enough that we aren't
 *  hammering the Tauri command during startup. */
const BACKEND_POLL_INTERVAL_MS = 2000;

/** Text fragment the Rust side emits when `require_runtime!` short-
 *  circuits. Case-insensitive match. Keep in sync with
 *  `crates/sovereign-desktop/src-tauri/src/commands.rs`. */
const BACKEND_LOADING_HINT = "backend is still loading";

export function isBackendLoadingError(err: unknown): boolean {
  const msg = String(err).toLowerCase();
  return msg.includes(BACKEND_LOADING_HINT);
}

export const skillsMachine = setup({
  types: {
    context: {} as SkillsContext,
    events: {} as SkillsEvent,
  },
  actors: {
    // Placeholder promise actors — overridden via `.provide({ actors })`
    // both in the Svelte wrapper (real Tauri invokes) and in tests
    // (spies). Keeping them in `setup` so TypeScript sees the shape.
    fetchSkills: fromPromise(async (): Promise<SkillEntry[]> => {
      throw new Error("fetchSkills actor not provided");
    }),
    toggleSkill: fromPromise(
      async ({
        input: _input,
      }: {
        input: { id: string; active: boolean };
      }): Promise<void> => {
        throw new Error("toggleSkill actor not provided");
      },
    ),
  },
  guards: {
    isBackendLoading: ({ event }) => {
      // Guards only see serializable events. onError actor events carry
      // `event.error` as the thrown value.
      const err = (event as unknown as { error: unknown }).error;
      return isBackendLoadingError(err);
    },
  },
}).createMachine({
  id: "skills",
  initial: "loading",
  context: {
    skills: [],
    errorMessage: "",
    togglingId: null,
  },
  states: {
    loading: {
      invoke: {
        src: "fetchSkills",
        onDone: {
          target: "ready",
          actions: assign({
            skills: ({ event }) => event.output,
            errorMessage: () => "",
          }),
        },
        onError: [
          {
            guard: "isBackendLoading",
            target: "waitingForBackend",
          },
          {
            target: "error",
            actions: assign({
              errorMessage: ({ event }) => String(event.error),
            }),
          },
        ],
      },
    },

    waitingForBackend: {
      // Fast path: the Svelte wrapper converts the Tauri `backend-ready`
      // event into BOOTSTRAP_COMPLETE → immediate re-fetch.
      // Slow path: if the event fires before we attached (race) the
      // `after` transition re-attempts every 2s. Eventually one succeeds.
      on: {
        BOOTSTRAP_COMPLETE: { target: "loading" },
        RETRY: { target: "loading" },
      },
      after: {
        [BACKEND_POLL_INTERVAL_MS]: { target: "loading" },
      },
    },

    ready: {
      on: {
        TOGGLE_SKILL: {
          target: "toggling",
          actions: assign({
            togglingId: ({ event }) => event.id,
          }),
        },
      },
    },

    toggling: {
      invoke: {
        src: "toggleSkill",
        // Funnel the TOGGLE_SKILL payload into the actor input. This is
        // the XState v5 pattern for passing per-transition data to an
        // invoked promise.
        input: ({ context, event }) => {
          // `event` here can be the original TOGGLE_SKILL that entered
          // the state, or an internal event if something else targets
          // this state later. The fallback keeps TS happy.
          const e = event as Extract<SkillsEvent, { type: "TOGGLE_SKILL" }>;
          return { id: e.id ?? context.togglingId ?? "", active: e.active };
        },
        onDone: {
          target: "ready",
          actions: assign(({ context, event: _event }) =>
            produce(context, (draft) => {
              // The flip happens on the Rust side; reflect it locally by
              // inverting the `active` flag on the affected skill. The
              // wrapper passed the *target* active state via the event;
              // re-applying it is idempotent.
              const skill = draft.skills.find((s) => s.id === draft.togglingId);
              if (skill && draft.togglingId) {
                const target = findRequestedActive(
                  context.togglingId ?? "",
                  context.skills,
                );
                skill.active = target ?? !skill.active;
              }
              draft.togglingId = null;
            }),
          ),
        },
        onError: {
          // Keep existing skills; surface nothing to the user for now
          // (the Rust side already logs). Reset the togglingId so the
          // UI stops showing the pending state.
          target: "ready",
          actions: assign({
            togglingId: () => null,
          }),
        },
      },
    },

    error: {
      on: {
        RETRY: { target: "loading" },
      },
    },
  },
});

/** Shape helper: given a skill id and the snapshot's skills list,
 *  return the *opposite* of its current `active` state — which is
 *  what the user clicked toward when they emitted TOGGLE_SKILL. Used
 *  by the `toggling` success action. Separate so it's testable in
 *  isolation if it grows. */
function findRequestedActive(
  id: string,
  skills: SkillEntry[],
): boolean | undefined {
  const s = skills.find((x) => x.id === id);
  return s === undefined ? undefined : !s.active;
}
