// SetupWizard smoke test. The setupWizardMachine is exhaustively
// tested in `machines/setupWizard.machine.test.ts`; this test only
// verifies the Svelte glue:
//   1. Step 1 renders the three persona cards.
//   2. Clicking one dispatches `PERSONA_SELECTED` and advances to
//      `personaSetup`.
//   3. The step indicator updates to match.
//
// Heavy sub-components (ModelSelector, WebSearchSetup, …) are replaced
// with stubs so the test focuses on the wizard's own orchestration
// rather than the persona forms.
import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent } from "@testing-library/svelte";
import StubStep from "./__test-stubs__/StubStep.svelte";

// Substitute each persona / step component with a stub that renders
// its name as a data-testid marker. Lets the wizard render and
// transition without pulling in the whole ModelSelector tree.
vi.mock("./ResearchSetup.svelte", () => ({ default: StubStep }));
vi.mock("./AssistantSetup.svelte", () => ({ default: StubStep }));
vi.mock("./DeveloperSetup.svelte", () => ({ default: StubStep }));
vi.mock("./KnowledgeBaseSetup.svelte", () => ({ default: StubStep }));
vi.mock("./WebSearchSetup.svelte", () => ({ default: StubStep }));

vi.mock("../api", () => ({
  completeSetup: vi.fn(async () => undefined),
}));

const { default: SetupWizard } = await import("./SetupWizard.svelte");

describe("SetupWizard", () => {
  it("renders the three persona cards on first mount", () => {
    render(SetupWizard, { props: { onComplete: vi.fn() } });
    expect(screen.getByText(/research & analysis/i)).toBeInTheDocument();
    expect(screen.getByText(/personal assistant/i)).toBeInTheDocument();
    expect(screen.getByText(/^Developer$/)).toBeInTheDocument();
  });

  it("advances to personaSetup after a persona click", async () => {
    render(SetupWizard, { props: { onComplete: vi.fn() } });
    const researchCard = screen.getByText(/research & analysis/i).closest(
      "button",
    );
    expect(researchCard).toBeTruthy();
    await fireEvent.click(researchCard!);

    // Stub renders its own content once the wizard transitions to
    // personaSetup + routes into the ResearchSetup stub.
    expect(await screen.findByTestId("stub-step")).toBeInTheDocument();
    // Step indicator moves from 1 to 2 (out of 4 for non-developer).
    expect(screen.getByText(/2 \/ 4/)).toBeInTheDocument();
  });

  it("developer persona collapses totalSteps to 3", async () => {
    render(SetupWizard, { props: { onComplete: vi.fn() } });
    const dev = screen
      .getByText(/^Developer$/)
      .closest("button");
    await fireEvent.click(dev!);
    expect(screen.getByText(/2 \/ 3/)).toBeInTheDocument();
  });
});
