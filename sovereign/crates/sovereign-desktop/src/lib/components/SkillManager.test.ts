// SkillManager smoke tests. Component-level coverage for the
// skillsMachine integration. The machine itself is exhaustively
// tested in `machines/skills.machine.test.ts`; these tests only
// confirm the Svelte ↔ machine glue (template branches off
// `$snapshot.matches(...)`, button handlers dispatch the right
// events).
//
// Keep these fast (< 50ms each). Mock the api layer; don't render
// real Tauri state; prefer `findByText` only where we actually
// need to wait for an async transition.
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent } from "@testing-library/svelte";
import SkillManager from "./SkillManager.svelte";
import type { SkillEntry } from "../types";

vi.mock("../api", () => ({
  listSkills: vi.fn(),
  toggleSkill: vi.fn(),
}));

const api = await import("../api");

function skill(id: string, overrides: Partial<SkillEntry> = {}): SkillEntry {
  return {
    id,
    name: `Skill ${id}`,
    description: `Does ${id}`,
    active: false,
    trust_level: "unsigned",
    ...overrides,
  };
}

describe("SkillManager", () => {
  beforeEach(() => {
    vi.mocked(api.listSkills).mockReset();
    vi.mocked(api.toggleSkill).mockReset();
  });

  it("renders the skill list when listSkills resolves", async () => {
    vi.mocked(api.listSkills).mockResolvedValue([
      skill("code-review", { name: "Code Review" }),
      skill("inner-work", { name: "Inner Work" }),
    ]);
    render(SkillManager);
    // The machine starts in `loading`; once the fetch resolves we
    // transition to `ready`. `findByText` waits up to ~1s for the
    // async transition — sufficient for a promise that resolves on
    // the next microtask.
    expect(await screen.findByText("Code Review")).toBeInTheDocument();
    expect(screen.getByText("Inner Work")).toBeInTheDocument();
  });

  it("shows Retry when listSkills throws a non-backend error", async () => {
    vi.mocked(api.listSkills).mockRejectedValue(new Error("Database locked"));
    render(SkillManager);
    const retry = await screen.findByRole("button", { name: /retry/i });
    expect(retry).toBeInTheDocument();
    expect(screen.getByText(/could not load skills/i)).toBeInTheDocument();
  });

  it("clicking the toggle invokes toggleSkill with the new target state", async () => {
    vi.mocked(api.listSkills).mockResolvedValue([skill("sk", { active: false })]);
    vi.mocked(api.toggleSkill).mockResolvedValue(undefined);
    render(SkillManager);
    await screen.findByText("Skill sk");
    const checkbox = screen.getByRole("checkbox");
    await fireEvent.click(checkbox);
    // The machine dispatches TOGGLE_SKILL { id, active: true }, which
    // invokes the toggleSkill actor with { id, active }.
    expect(api.toggleSkill).toHaveBeenCalledWith("sk", true);
  });
});
