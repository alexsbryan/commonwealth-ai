// setupWizardMachine — minimal model-picker wizard.
//
// Prior version had 5 steps (persona, personaSetup, knowledge,
// websearch, finishing) with persona-specific branches that never
// earned their keep — users didn't care about picking "research" vs
// "assistant" vs "developer", and the knowledge-tier screen asked
// them to download Wikipedia before they knew what the product did.
//
// Collapsed to: pick a model → finish. Every other decision gets a
// sensible default and can be changed later in Settings. Time-to-
// first-value is the north star.
//
//   detecting ──▶ modelSetup ──(PERSONA_CONFIGURED)──▶ finishing
//                                                          │
//                                  onDone ──▶ done (terminal)
//                                  onError ──▶ modelSetup
//
// Persona is always set to "assistant" internally (the most general
// capability profile). If a future "advanced" entry point wants
// per-persona setup again, it can reuse the `Persona` type and the
// AssistantSetup / DeveloperSetup components (kept in the tree).

import { assign, fromPromise, setup } from "xstate";
import { produce } from "immer";
import type { BootstrapSnapshot, SetupConfig } from "../types";

export type Persona = "research" | "assistant" | "developer";

export interface SetupWizardContext {
  /// Always "assistant" post-collapse. Kept in the context so the
  /// `SetupConfig` shape downstream code expects stays stable.
  persona: Persona;
  config: SetupConfig;
  errorMessage: string;
  bootstrap: BootstrapSnapshot | null;
}

export type SetupWizardEvent =
  | { type: "PERSONA_CONFIGURED"; config: SetupConfig };

const emptyConfig: SetupConfig = {
  model_path: "",
  active_skills: [],
  enabled_tools: [],
};

export const setupWizardMachine = setup({
  types: {
    context: {} as SetupWizardContext,
    events: {} as SetupWizardEvent,
  },
  actors: {
    completeSetup: fromPromise(
      async (_: { input: { config: SetupConfig } }): Promise<void> => {
        throw new Error("completeSetup actor not provided");
      },
    ),
    detectBootstrap: fromPromise(async (): Promise<BootstrapSnapshot> => {
      throw new Error("detectBootstrap actor not provided");
    }),
  },
  guards: {
    // Persona-configured must include a real `model_path`. The UI
    // should prevent an empty submission but the machine enforces
    // it as a load-bearing invariant.
    hasModelPath: ({ event }) => {
      if (event.type !== "PERSONA_CONFIGURED") return false;
      return event.config.model_path.trim().length > 0;
    },
  },
}).createMachine({
  id: "setupWizard",
  initial: "detecting",
  context: {
    persona: "assistant",
    config: emptyConfig,
    errorMessage: "",
    bootstrap: null,
  },
  states: {
    // Gate state: runs the bootstrap probe before showing the model
    // picker. On success, transition to `modelSetup` carrying the
    // snapshot (used by SetupWizard.svelte to branch on an existing
    // CLI config — same behaviour as before, minus the persona/tier
    // flow).
    detecting: {
      invoke: {
        src: "detectBootstrap",
        onDone: {
          target: "modelSetup",
          actions: assign({
            bootstrap: ({ event }) => event.output as BootstrapSnapshot,
          }),
        },
        onError: {
          target: "modelSetup",
          actions: assign({ bootstrap: null }),
        },
      },
    },
    modelSetup: {
      on: {
        PERSONA_CONFIGURED: {
          guard: "hasModelPath",
          target: "finishing",
          actions: assign(({ context, event }) => ({
            config: produce(context.config, (draft) => {
              draft.model_path = event.config.model_path;
              draft.primary_model_path = event.config.primary_model_path;
              draft.embed_model_path = event.config.embed_model_path;
              draft.data_dir = event.config.data_dir;
              draft.active_skills = event.config.active_skills;
              draft.enabled_tools = event.config.enabled_tools;
            }),
          })),
        },
      },
    },
    finishing: {
      invoke: {
        src: "completeSetup",
        input: ({ context }) => ({ config: context.config }),
        onDone: { target: "done" },
        onError: {
          target: "modelSetup",
          actions: assign({
            errorMessage: ({ event }) => `Setup failed: ${String(event.error)}`,
          }),
        },
      },
    },
    done: {
      type: "final",
    },
  },
});
