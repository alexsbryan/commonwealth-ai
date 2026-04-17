// setupWizardMachine — first-run wizard that walks the user through
// persona selection, model picker, knowledge tier, web search, and
// final bootstrap. Previously 5 `$state` vars + manual step strings
// in a 686-line Svelte component; the guards and branching (developer
// persona skips websearch, an error during finishing bounces back to
// knowledge) were implicit. Modeling as an FSM makes them explicit
// and unit-testable.
//
//   persona ──(PERSONA_SELECTED)──▶ personaSetup
//      │
//      ▲
//      └──────── (back navigation — future; not wired yet)
//
//   personaSetup ──(PERSONA_CONFIGURED with model_path)──▶ knowledge
//
//   knowledge ──(TIER_SELECTED | SKIP_KNOWLEDGE)──▶ websearch
//                                               │
//                                               │ (guard: developer)
//                                               └──▶ finishing
//
//   websearch ──(WEB_CONFIGURED | SKIP_WEBSEARCH)──▶ finishing
//
//   finishing ──(invoke completeSetup)
//                onDone ──▶ done (terminal)
//                onError ──▶ knowledge (with error message)
//
// Context carries `selectedPersona`, the accumulated `partialConfig`
// (empty shell until persona-setup, then progressively filled), and
// a string `errorMessage` surfaced after a failed finishing invocation.
import { assign, fromPromise, setup } from "xstate";
import { produce } from "immer";
import type { BootstrapSnapshot, SetupConfig } from "../types";

export type Persona = "research" | "assistant" | "developer";

export interface SetupWizardContext {
  persona: Persona | null;
  /** Accumulated setup config. Always has `active_skills` and
   *  `enabled_tools` as arrays — they're required by the backend
   *  `complete_setup` command schema — but the actual values are
   *  filled in by PERSONA_CONFIGURED. */
  config: SetupConfig;
  errorMessage: string;
  /** Result of the bootstrap probe the machine runs on entry. When
   *  `cli_config_present` is true, the wizard skips the model and
   *  knowledge screens because `SetupConfig` already covers them. */
  bootstrap: BootstrapSnapshot | null;
}

export type SetupWizardEvent =
  | { type: "PERSONA_SELECTED"; persona: Persona }
  | { type: "PERSONA_CONFIGURED"; config: SetupConfig }
  | { type: "BACK_TO_PERSONA" }
  | { type: "TIER_SELECTED"; tierId: string }
  | { type: "SKIP_KNOWLEDGE" }
  | {
      type: "WEB_CONFIGURED";
      provider: string;
      apiKey: string | null;
    }
  | { type: "SKIP_WEBSEARCH" };

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
    // Probes the daemon + disk config to decide whether to run the
    // full wizard or skip the model-selection screens. Consumers
    // provide the real implementation; the default throws so a
    // missing provider fails loudly rather than silently falling
    // through to the full wizard.
    detectBootstrap: fromPromise(async (): Promise<BootstrapSnapshot> => {
      throw new Error("detectBootstrap actor not provided");
    }),
  },
  guards: {
    // Developer persona has no web-search step — collapses straight
    // to the finishing phase after knowledge.
    isDeveloperPersona: ({ context }) => context.persona === "developer",
    // Persona-configured must include a real `model_path`. The UI
    // should prevent an empty submission but the machine enforces
    // it as a load-bearing invariant.
    hasModelPath: ({ event }) => {
      if (event.type !== "PERSONA_CONFIGURED") return false;
      return event.config.model_path.trim().length > 0;
    },
    // CLI setup already wrote model paths + data dir to
    // `~/.config/sovereign/config.toml`. The wizard still needs
    // persona (local UI preference) and optionally web search, but
    // everything in personaSetup's model picker is covered.
    hasCliConfig: ({ context }) =>
      context.bootstrap?.cli_config_present === true ||
      context.bootstrap?.daemon_running === true,
  },
}).createMachine({
  id: "setupWizard",
  initial: "detecting",
  context: {
    persona: null,
    config: emptyConfig,
    errorMessage: "",
    bootstrap: null,
  },
  states: {
    // Gate state that runs the bootstrap probe before showing anything.
    // Branches on the snapshot: if the CLI has already written model
    // paths, jump past the model picker entirely (`personaSetup` →
    // straight to persona selection, then knowledge/websearch).
    // Otherwise fall through to the existing `persona` flow.
    detecting: {
      invoke: {
        src: "detectBootstrap",
        onDone: {
          target: "persona",
          actions: assign({
            bootstrap: ({ event }) => event.output as BootstrapSnapshot,
          }),
        },
        // On probe failure we still need to let the user set up —
        // degrade gracefully to the full wizard rather than blocking.
        onError: {
          target: "persona",
          actions: assign({ bootstrap: null }),
        },
      },
    },
    persona: {
      on: {
        PERSONA_SELECTED: [
          // CLI setup path — skip the model picker entirely. The
          // backend `complete_setup` command pulls model paths from
          // `SetupConfig` when `model_path` is empty (see commands.rs).
          // Developer persona also skips knowledge because tier choice
          // there is orthogonal to the CLI's corpus plumbing; go
          // straight to finishing.
          {
            target: "finishing",
            guard: ({ context, event }) =>
              (context.bootstrap?.cli_config_present === true ||
                context.bootstrap?.daemon_running === true) &&
              event.persona === "developer",
            actions: assign({
              persona: ({ event }) => event.persona,
            }),
          },
          {
            target: "knowledge",
            guard: "hasCliConfig",
            actions: assign({
              persona: ({ event }) => event.persona,
            }),
          },
          {
            target: "personaSetup",
            actions: assign({
              persona: ({ event }) => event.persona,
            }),
          },
        ],
      },
    },
    personaSetup: {
      on: {
        PERSONA_CONFIGURED: {
          guard: "hasModelPath",
          target: "knowledge",
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
        BACK_TO_PERSONA: {
          target: "persona",
          actions: assign({ persona: null }),
        },
      },
    },
    knowledge: {
      // After a finishing failure we return here with `errorMessage`
      // set. Clearing happens when the user takes the next action
      // (not on `entry`, because `entry` runs AFTER the transition's
      // action — an `onError` that assigns the error would get
      // immediately wiped by this state's entry, defeating the point).
      on: {
        TIER_SELECTED: [
          {
            guard: "isDeveloperPersona",
            target: "finishing",
            actions: assign(({ context, event }) => ({
              config: produce(context.config, (draft) => {
                draft.selected_tier = event.tierId;
              }),
              errorMessage: "",
            })),
          },
          {
            target: "websearch",
            actions: assign(({ context, event }) => ({
              config: produce(context.config, (draft) => {
                draft.selected_tier = event.tierId;
              }),
              errorMessage: "",
            })),
          },
        ],
        SKIP_KNOWLEDGE: [
          {
            guard: "isDeveloperPersona",
            target: "finishing",
            actions: assign({ errorMessage: () => "" }),
          },
          {
            target: "websearch",
            actions: assign({ errorMessage: () => "" }),
          },
        ],
      },
    },
    websearch: {
      on: {
        WEB_CONFIGURED: {
          target: "finishing",
          actions: assign(({ context, event }) => ({
            config: produce(context.config, (draft) => {
              // DuckDuckGo is the silent default — only save the
              // provider when the user picked a non-default one.
              // Mirrors the pre-migration component logic.
              draft.search_provider =
                event.provider !== "duckduckgo" ? event.provider : undefined;
              draft.search_api_key = event.apiKey ?? undefined;
            }),
          })),
        },
        SKIP_WEBSEARCH: { target: "finishing" },
      },
    },
    finishing: {
      invoke: {
        src: "completeSetup",
        input: ({ context }) => ({ config: context.config }),
        onDone: { target: "done" },
        onError: {
          target: "knowledge",
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
