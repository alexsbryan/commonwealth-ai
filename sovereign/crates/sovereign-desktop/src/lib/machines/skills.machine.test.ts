// Unit tests for skillsMachine. No DOM, no Svelte — we drive the actor
// directly via `createActor` and assert on `getSnapshot()`. This is the
// template for every Phase 2+ machine test.
import { describe, it, expect, vi } from "vitest";
import { createActor, fromPromise } from "xstate";
import {
  skillsMachine,
  isBackendLoadingError,
  type SkillEntry,
} from "./skills.machine";

function skill(id: string, active = false): SkillEntry {
  return {
    id,
    name: `Skill ${id}`,
    description: `desc-${id}`,
    active,
    trust_level: "unsigned",
  };
}

/**
 * Build a machine with both actors mocked. Each test picks the
 * behaviours it wants; defaults are reasonable.
 */
function makeMachine(opts: {
  fetch?: () => Promise<SkillEntry[]>;
  toggle?: (input: { id: string; active: boolean }) => Promise<void>;
} = {}) {
  const fetchImpl = opts.fetch ?? (async () => []);
  const toggleImpl = opts.toggle ?? (async () => {});

  return skillsMachine.provide({
    actors: {
      fetchSkills: fromPromise(fetchImpl),
      toggleSkill: fromPromise(
        ({ input }: { input: { id: string; active: boolean } }) =>
          toggleImpl(input),
      ),
    },
  });
}

/**
 * Wait for an actor to reach a given state value. XState is synchronous
 * in its event processing but promise actors flush on microtasks — we
 * use a small event-loop wait + polling loop capped at `timeoutMs`.
 */
async function waitFor(
  actor: ReturnType<typeof createActor>,
  predicate: (snapshot: ReturnType<typeof actor.getSnapshot>) => boolean,
  timeoutMs = 1000,
): Promise<void> {
  const start = Date.now();
  while (!predicate(actor.getSnapshot())) {
    if (Date.now() - start > timeoutMs) {
      throw new Error(
        `waitFor: timed out after ${timeoutMs}ms in state ${String(
          actor.getSnapshot().value,
        )}`,
      );
    }
    // Yield to microtask queue so pending promise resolutions flush.
    await Promise.resolve();
    await new Promise((r) => setTimeout(r, 5));
  }
}

describe("isBackendLoadingError", () => {
  it("matches the exact Rust phrase", () => {
    expect(
      isBackendLoadingError(new Error("Backend is still loading. Please wait.")),
    ).toBe(true);
  });

  it("is case-insensitive", () => {
    expect(isBackendLoadingError("BACKEND IS STILL LOADING")).toBe(true);
  });

  it("ignores unrelated errors", () => {
    expect(isBackendLoadingError(new Error("Skill not found"))).toBe(false);
    expect(isBackendLoadingError("")).toBe(false);
  });
});

describe("skillsMachine", () => {
  describe("happy path", () => {
    it("fetches on entry and lands in ready with the skills", async () => {
      const skills = [skill("a"), skill("b", true)];
      const machine = makeMachine({ fetch: async () => skills });
      const actor = createActor(machine);
      actor.start();

      await waitFor(actor, (s) => s.matches("ready"));
      expect(actor.getSnapshot().context.skills).toEqual(skills);
      expect(actor.getSnapshot().context.errorMessage).toBe("");
      actor.stop();
    });
  });

  describe("backend-loading race", () => {
    it("waits for BOOTSTRAP_COMPLETE when initial fetch reports backend loading", async () => {
      let attempt = 0;
      const machine = makeMachine({
        fetch: async () => {
          attempt++;
          if (attempt === 1) {
            throw new Error("Backend is still loading. Please wait.");
          }
          return [skill("a")];
        },
      });
      const actor = createActor(machine);
      actor.start();

      await waitFor(actor, (s) => s.matches("waitingForBackend"));

      // Fast path: simulate the Tauri `backend-ready` listener.
      actor.send({ type: "BOOTSTRAP_COMPLETE" });

      await waitFor(actor, (s) => s.matches("ready"));
      expect(actor.getSnapshot().context.skills).toHaveLength(1);
      expect(attempt).toBe(2);
      actor.stop();
    });

    it("polls as a fallback if BOOTSTRAP_COMPLETE is never delivered", async () => {
      // This exercises the `after: { 2000: ... }` transition. Real time
      // would be painful; use vitest's fake timers and advance manually.
      vi.useFakeTimers();
      try {
        let attempt = 0;
        const machine = makeMachine({
          fetch: async () => {
            attempt++;
            if (attempt < 2) {
              throw new Error("Backend is still loading. Please wait.");
            }
            return [skill("polled")];
          },
        });
        const actor = createActor(machine);
        actor.start();

        // Let the first (failing) fetch flush.
        await vi.advanceTimersByTimeAsync(0);
        expect(actor.getSnapshot().value).toBe("waitingForBackend");

        // Advance past the polling interval. The after transition fires,
        // re-enters loading, and the second fetch succeeds.
        await vi.advanceTimersByTimeAsync(2100);
        // Let the resolved promise's microtask chain flush.
        await vi.advanceTimersByTimeAsync(0);

        expect(actor.getSnapshot().value).toBe("ready");
        expect(actor.getSnapshot().context.skills).toEqual([skill("polled")]);
        actor.stop();
      } finally {
        vi.useRealTimers();
      }
    });
  });

  describe("non-backend errors", () => {
    it("transitions to error and surfaces the message", async () => {
      const machine = makeMachine({
        fetch: async () => {
          throw new Error("Database is corrupt");
        },
      });
      const actor = createActor(machine);
      actor.start();

      await waitFor(actor, (s) => s.matches("error"));
      expect(actor.getSnapshot().context.errorMessage).toContain(
        "Database is corrupt",
      );
      actor.stop();
    });

    it("RETRY from error re-enters loading", async () => {
      let attempt = 0;
      const machine = makeMachine({
        fetch: async () => {
          attempt++;
          if (attempt === 1) throw new Error("Transient");
          return [skill("recovered")];
        },
      });
      const actor = createActor(machine);
      actor.start();

      await waitFor(actor, (s) => s.matches("error"));
      actor.send({ type: "RETRY" });
      await waitFor(actor, (s) => s.matches("ready"));
      expect(actor.getSnapshot().context.skills).toEqual([skill("recovered")]);
      actor.stop();
    });
  });

  describe("toggle", () => {
    it("flips a skill's active state on success", async () => {
      const toggle = vi.fn(async () => {});
      const machine = makeMachine({
        fetch: async () => [skill("x", false)],
        toggle,
      });
      const actor = createActor(machine);
      actor.start();
      await waitFor(actor, (s) => s.matches("ready"));

      actor.send({ type: "TOGGLE_SKILL", id: "x", active: true });
      await waitFor(actor, (s) => s.matches("toggling"));
      await waitFor(actor, (s) => s.matches("ready"));

      expect(actor.getSnapshot().context.skills[0].active).toBe(true);
      expect(actor.getSnapshot().context.togglingId).toBeNull();
      expect(toggle).toHaveBeenCalledWith({ id: "x", active: true });
      actor.stop();
    });

    it("leaves skills untouched on toggle failure and clears togglingId", async () => {
      const machine = makeMachine({
        fetch: async () => [skill("x", false)],
        toggle: async () => {
          throw new Error("toggle failed");
        },
      });
      const actor = createActor(machine);
      actor.start();
      await waitFor(actor, (s) => s.matches("ready"));

      actor.send({ type: "TOGGLE_SKILL", id: "x", active: true });
      await waitFor(
        actor,
        (s) => s.matches("ready") && s.context.togglingId === null,
      );
      // Active flag should remain false — we never confirmed the toggle.
      expect(actor.getSnapshot().context.skills[0].active).toBe(false);
      actor.stop();
    });
  });
});
