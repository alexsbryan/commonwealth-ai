// Unit tests for setupWizardMachine — collapsed model-only flow.
// Covers the happy path, the hasModelPath guard, and finishing-
// failure recovery. The old persona / knowledge / websearch tests
// were removed when those states were collapsed.
import { describe, it, expect, vi } from "vitest";
import { createActor, fromPromise } from "xstate";
import { setupWizardMachine } from "./setupWizard.machine";
import type { BootstrapSnapshot, SetupConfig } from "../types";

/** Snapshot that looks like a first-time user. The machine should
 *  fall through `detecting` into `modelSetup`. */
const FRESH_SNAPSHOT: BootstrapSnapshot = {
  daemon_running: false,
  cli_config_present: false,
  desktop_setup_complete: false,
  client_port: 9741,
};

function configWithModel(): SetupConfig {
  return {
    model_path: "/models/fast.gguf",
    primary_model_path: undefined,
    embed_model_path: "/models/embed.gguf",
    data_dir: undefined,
    active_skills: [],
    enabled_tools: ["shell", "search", "web_fetch", "document"],
  };
}

function makeMachine(opts: {
  completeSetup?: (input: { config: SetupConfig }) => Promise<void>;
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

/** Start the actor and wait for the `detecting` gate to clear. */
async function startAtModel(
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

describe("setupWizardMachine — collapsed model-only flow", () => {
  it("happy path: detecting → modelSetup → finishing → done", async () => {
    const complete = vi.fn<
      (input: { config: SetupConfig }) => Promise<void>
    >(async () => {});
    const actor = createActor(makeMachine({ completeSetup: complete }));
    await startAtModel(actor);

    // Collapsed machine lands on modelSetup directly. No persona
    // selection step, no knowledge step, no websearch step.
    expect(actor.getSnapshot().matches("modelSetup")).toBe(true);
    // Context carries a default "assistant" persona for SetupConfig
    // compatibility.
    expect(actor.getSnapshot().context.persona).toBe("assistant");

    actor.send({ type: "PERSONA_CONFIGURED", config: configWithModel() });
    // Either finishing (pending) or done (already resolved).
    expect(
      actor.getSnapshot().matches("finishing") ||
        actor.getSnapshot().matches("done"),
    ).toBe(true);

    await waitFor(actor, (s) => s.matches("done"));
    expect(complete).toHaveBeenCalledOnce();
    expect(complete).toHaveBeenCalledWith({ config: configWithModel() });
  });

  it("rejects PERSONA_CONFIGURED with empty model_path (hasModelPath guard)", async () => {
    const actor = createActor(makeMachine());
    await startAtModel(actor);

    const bad: SetupConfig = { ...configWithModel(), model_path: "   " };
    actor.send({ type: "PERSONA_CONFIGURED", config: bad });
    // Guard refuses — machine stays on modelSetup.
    expect(actor.getSnapshot().matches("modelSetup")).toBe(true);
  });

  it("returns to modelSetup with errorMessage on finishing failure", async () => {
    const fail = vi.fn<(input: { config: SetupConfig }) => Promise<void>>(
      async () => {
        throw new Error("daemon not reachable");
      },
    );
    const actor = createActor(makeMachine({ completeSetup: fail }));
    await startAtModel(actor);

    actor.send({ type: "PERSONA_CONFIGURED", config: configWithModel() });

    await waitFor(
      actor,
      (s) => s.matches("modelSetup") && !!s.context.errorMessage,
    );
    const err = actor.getSnapshot().context.errorMessage;
    expect(err).toContain("Setup failed");
    expect(err).toContain("daemon not reachable");
  });

  it("accumulates model fields into context.config", async () => {
    const actor = createActor(makeMachine());
    await startAtModel(actor);

    actor.send({ type: "PERSONA_CONFIGURED", config: configWithModel() });

    const ctx = actor.getSnapshot().context;
    expect(ctx.config.model_path).toBe("/models/fast.gguf");
    expect(ctx.config.embed_model_path).toBe("/models/embed.gguf");
    expect(ctx.config.enabled_tools).toContain("shell");
    expect(ctx.config.enabled_tools).toContain("document");
  });
});
