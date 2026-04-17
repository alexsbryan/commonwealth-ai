// Unit tests for setupWizardMachine. Covers happy paths per persona,
// the developer shortcut, the PERSONA_CONFIGURED guard, and
// finishing-failure recovery.
import { describe, it, expect, vi } from "vitest";
import { createActor, fromPromise } from "xstate";
import { setupWizardMachine } from "./setupWizard.machine";
import type { BootstrapSnapshot, SetupConfig } from "../types";

/** Snapshot that looks like a first-time user (no CLI config, no
 *  daemon running). The machine should fall through the `detecting`
 *  state into the full wizard. */
const FRESH_SNAPSHOT: BootstrapSnapshot = {
  daemon_running: false,
  cli_config_present: false,
  desktop_setup_complete: false,
  client_port: 9741,
};

/** Snapshot indicating `sovereign setup` already ran. Wizard should
 *  skip the personaSetup (model picker) step. */
const CLI_CONFIG_SNAPSHOT: BootstrapSnapshot = {
  daemon_running: false,
  cli_config_present: true,
  desktop_setup_complete: false,
  client_port: 9741,
};

function configWithModel(): SetupConfig {
  return {
    model_path: "/models/fast.gguf",
    primary_model_path: undefined,
    embed_model_path: "/models/embed.gguf",
    data_dir: undefined,
    active_skills: ["collaborative-research"],
    enabled_tools: ["shell", "search"],
  };
}

function makeMachine(opts: {
  completeSetup?: (input: { config: SetupConfig }) => Promise<void>;
  /** Override the bootstrap probe's return value. Defaults to the
   *  fresh-install snapshot — no CLI config, no daemon — so existing
   *  tests exercise the full wizard path unchanged. */
  bootstrap?: BootstrapSnapshot;
} = {}) {
  const impl = opts.completeSetup ?? (async () => {});
  const snap = opts.bootstrap ?? FRESH_SNAPSHOT;
  return setupWizardMachine.provide({
    actors: {
      completeSetup: fromPromise(
        ({ input }: { input: { config: SetupConfig } }) => impl(input),
      ),
      detectBootstrap: fromPromise(async () => snap),
    },
  });
}

/** Start the actor and wait for the `detecting` gate to clear. All
 *  tests go through this so the startup transition is consistent. */
async function startAtPersona(
  actor: ReturnType<typeof createActor>,
): Promise<void> {
  actor.start();
  await waitFor(actor, (s) => !s.matches("detecting"));
}

function waitFor(
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
          new Error(`waitFor timeout at ${String(actor.getSnapshot().value)}`),
        );
      }
      setTimeout(check, 5);
    };
    check();
  });
}

describe("setupWizardMachine — happy paths", () => {
  it("research persona: persona → personaSetup → knowledge → websearch → done", async () => {
    const complete = vi.fn<
      (input: { config: SetupConfig }) => Promise<void>
    >(async () => {});
    const actor = createActor(makeMachine({ completeSetup: complete }));
    await startAtPersona(actor);

    expect(actor.getSnapshot().matches("persona")).toBe(true);
    actor.send({ type: "PERSONA_SELECTED", persona: "research" });
    expect(actor.getSnapshot().matches("personaSetup")).toBe(true);

    actor.send({ type: "PERSONA_CONFIGURED", config: configWithModel() });
    expect(actor.getSnapshot().matches("knowledge")).toBe(true);

    actor.send({ type: "TIER_SELECTED", tierId: "research" });
    expect(actor.getSnapshot().matches("websearch")).toBe(true);
    expect(actor.getSnapshot().context.config.selected_tier).toBe("research");

    actor.send({ type: "WEB_CONFIGURED", provider: "brave", apiKey: "k" });
    await waitFor(actor, (s) => s.matches("done"));
    expect(complete).toHaveBeenCalledTimes(1);
    expect(complete.mock.calls[0][0].config.model_path).toBe(
      "/models/fast.gguf",
    );
    expect(complete.mock.calls[0][0].config.search_provider).toBe("brave");
  });

  it("developer persona shortcut: knowledge → finishing (skips websearch)", async () => {
    const complete = vi.fn<
      (input: { config: SetupConfig }) => Promise<void>
    >(async () => {});
    const actor = createActor(makeMachine({ completeSetup: complete }));
    await startAtPersona(actor);
    actor.send({ type: "PERSONA_SELECTED", persona: "developer" });
    actor.send({ type: "PERSONA_CONFIGURED", config: configWithModel() });
    actor.send({ type: "TIER_SELECTED", tierId: "technical" });
    // Straight to finishing — must never enter websearch.
    await waitFor(actor, (s) => s.matches("done"));
    expect(complete).toHaveBeenCalled();
  });

  it("research persona SKIP_KNOWLEDGE goes to websearch", async () => {
    const actor = createActor(makeMachine());
    await startAtPersona(actor);
    actor.send({ type: "PERSONA_SELECTED", persona: "research" });
    actor.send({ type: "PERSONA_CONFIGURED", config: configWithModel() });
    actor.send({ type: "SKIP_KNOWLEDGE" });
    expect(actor.getSnapshot().matches("websearch")).toBe(true);
  });

  it("developer persona SKIP_KNOWLEDGE goes straight to finishing", async () => {
    const complete = vi.fn<
      (input: { config: SetupConfig }) => Promise<void>
    >(async () => {});
    const actor = createActor(makeMachine({ completeSetup: complete }));
    await startAtPersona(actor);
    actor.send({ type: "PERSONA_SELECTED", persona: "developer" });
    actor.send({ type: "PERSONA_CONFIGURED", config: configWithModel() });
    actor.send({ type: "SKIP_KNOWLEDGE" });
    await waitFor(actor, (s) => s.matches("done"));
    expect(complete).toHaveBeenCalled();
  });

  it("SKIP_WEBSEARCH advances to finishing", async () => {
    const complete = vi.fn<
      (input: { config: SetupConfig }) => Promise<void>
    >(async () => {});
    const actor = createActor(makeMachine({ completeSetup: complete }));
    await startAtPersona(actor);
    actor.send({ type: "PERSONA_SELECTED", persona: "assistant" });
    actor.send({ type: "PERSONA_CONFIGURED", config: configWithModel() });
    actor.send({ type: "SKIP_KNOWLEDGE" });
    actor.send({ type: "SKIP_WEBSEARCH" });
    await waitFor(actor, (s) => s.matches("done"));
    expect(complete).toHaveBeenCalled();
  });
});

describe("setupWizardMachine — CLI config detected", () => {
  it("non-developer persona skips personaSetup and goes to knowledge", async () => {
    const actor = createActor(
      makeMachine({ bootstrap: CLI_CONFIG_SNAPSHOT }),
    );
    await startAtPersona(actor);
    expect(actor.getSnapshot().matches("persona")).toBe(true);

    actor.send({ type: "PERSONA_SELECTED", persona: "research" });
    // Must skip past personaSetup (model picker) entirely.
    expect(actor.getSnapshot().matches("personaSetup")).toBe(false);
    expect(actor.getSnapshot().matches("knowledge")).toBe(true);
  });

  it("developer persona goes straight to finishing (no model + no knowledge)", async () => {
    const complete = vi.fn<
      (input: { config: SetupConfig }) => Promise<void>
    >(async () => {});
    const actor = createActor(
      makeMachine({
        completeSetup: complete,
        bootstrap: CLI_CONFIG_SNAPSHOT,
      }),
    );
    await startAtPersona(actor);
    actor.send({ type: "PERSONA_SELECTED", persona: "developer" });
    await waitFor(actor, (s) => s.matches("done"));
    // completeSetup runs with empty model_path — the backend
    // `complete_setup` command backfills from SetupConfig on disk.
    expect(complete).toHaveBeenCalledTimes(1);
    expect(complete.mock.calls[0][0].config.model_path).toBe("");
  });

  it("bootstrap probe failure falls back to full wizard", async () => {
    const machine = setupWizardMachine.provide({
      actors: {
        completeSetup: fromPromise(async () => {}),
        detectBootstrap: fromPromise<BootstrapSnapshot>(async () => {
          throw new Error("probe failed");
        }),
      },
    });
    const actor = createActor(machine);
    actor.start();
    await waitFor(actor, (s) => !s.matches("detecting"));
    expect(actor.getSnapshot().matches("persona")).toBe(true);
    expect(actor.getSnapshot().context.bootstrap).toBeNull();

    // Persona selected → goes to personaSetup because no CLI config.
    actor.send({ type: "PERSONA_SELECTED", persona: "research" });
    expect(actor.getSnapshot().matches("personaSetup")).toBe(true);
  });
});

describe("setupWizardMachine — guards", () => {
  it("PERSONA_CONFIGURED with empty model_path is rejected", async () => {
    const actor = createActor(makeMachine());
    await startAtPersona(actor);
    actor.send({ type: "PERSONA_SELECTED", persona: "research" });
    // Empty model path — the guard blocks advancement.
    const badConfig: SetupConfig = {
      ...configWithModel(),
      model_path: "",
    };
    actor.send({ type: "PERSONA_CONFIGURED", config: badConfig });
    expect(actor.getSnapshot().matches("personaSetup")).toBe(true);
  });

  it("PERSONA_CONFIGURED with whitespace-only model_path is rejected", async () => {
    const actor = createActor(makeMachine());
    await startAtPersona(actor);
    actor.send({ type: "PERSONA_SELECTED", persona: "research" });
    actor.send({
      type: "PERSONA_CONFIGURED",
      config: { ...configWithModel(), model_path: "   " },
    });
    expect(actor.getSnapshot().matches("personaSetup")).toBe(true);
  });
});

describe("setupWizardMachine — failure recovery", () => {
  it("completeSetup rejection bounces back to knowledge with errorMessage", async () => {
    const actor = createActor(
      makeMachine({
        completeSetup: async () => {
          throw new Error("disk full");
        },
      }),
    );
    await startAtPersona(actor);
    actor.send({ type: "PERSONA_SELECTED", persona: "research" });
    actor.send({ type: "PERSONA_CONFIGURED", config: configWithModel() });
    actor.send({ type: "TIER_SELECTED", tierId: "research" });
    actor.send({ type: "WEB_CONFIGURED", provider: "duckduckgo", apiKey: null });

    await waitFor(actor, (s) => s.matches("knowledge"));
    expect(actor.getSnapshot().context.errorMessage).toContain("disk full");
  });

  it("retrying from knowledge after a failure clears the error and succeeds", async () => {
    let attempt = 0;
    const actor = createActor(
      makeMachine({
        completeSetup: async () => {
          attempt++;
          if (attempt === 1) throw new Error("transient");
        },
      }),
    );
    await startAtPersona(actor);
    actor.send({ type: "PERSONA_SELECTED", persona: "research" });
    actor.send({ type: "PERSONA_CONFIGURED", config: configWithModel() });
    actor.send({ type: "TIER_SELECTED", tierId: "research" });
    actor.send({ type: "SKIP_WEBSEARCH" });

    await waitFor(actor, (s) => s.matches("knowledge"));
    expect(actor.getSnapshot().context.errorMessage).not.toBe("");

    // Retry path: TIER_SELECTED re-entry clears the error on entry,
    // then advances to websearch → finishing successfully.
    actor.send({ type: "TIER_SELECTED", tierId: "research" });
    expect(actor.getSnapshot().context.errorMessage).toBe("");
    actor.send({ type: "SKIP_WEBSEARCH" });
    await waitFor(actor, (s) => s.matches("done"));
    expect(attempt).toBe(2);
  });
});

describe("setupWizardMachine — context accumulation", () => {
  it("preserves persona through the flow", async () => {
    const actor = createActor(makeMachine());
    await startAtPersona(actor);
    actor.send({ type: "PERSONA_SELECTED", persona: "assistant" });
    actor.send({ type: "PERSONA_CONFIGURED", config: configWithModel() });
    actor.send({ type: "SKIP_KNOWLEDGE" });
    expect(actor.getSnapshot().context.persona).toBe("assistant");
  });

  it("DuckDuckGo as provider is not persisted to config (default elision)", async () => {
    const complete = vi.fn<
      (input: { config: SetupConfig }) => Promise<void>
    >(async () => {});
    const actor = createActor(makeMachine({ completeSetup: complete }));
    await startAtPersona(actor);
    actor.send({ type: "PERSONA_SELECTED", persona: "research" });
    actor.send({ type: "PERSONA_CONFIGURED", config: configWithModel() });
    actor.send({ type: "SKIP_KNOWLEDGE" });
    actor.send({
      type: "WEB_CONFIGURED",
      provider: "duckduckgo",
      apiKey: null,
    });
    await waitFor(actor, (s) => s.matches("done"));
    expect(complete.mock.calls[0][0].config.search_provider).toBeUndefined();
  });
});
