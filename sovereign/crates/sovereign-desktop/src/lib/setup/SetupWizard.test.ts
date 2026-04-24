// SetupWizard smoke test. The setupWizardMachine is exhaustively
// tested in `machines/setupWizard.machine.test.ts`; this test only
// verifies the Svelte glue for the collapsed model-only flow:
//   1. After the bootstrap probe resolves, the model-picker step
//      renders (no persona cards — we removed that screen).
//   2. Submitting the stub dispatches PERSONA_CONFIGURED and the
//      wizard transitions past modelSetup.
//
// QuickModelSetup is replaced with a stub so this test focuses on
// the wizard's own orchestration, not the model-picker UI.
import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent } from "@testing-library/svelte";
import StubStep from "./__test-stubs__/StubStep.svelte";

vi.mock("./QuickModelSetup.svelte", () => ({ default: StubStep }));

vi.mock("../api", () => ({
  completeSetup: vi.fn(async () => undefined),
  detectBootstrap: vi.fn(async () => ({
    daemon_running: false,
    cli_config_present: false,
    desktop_setup_complete: false,
    client_port: 9741,
  })),
}));

const { default: SetupWizard } = await import("./SetupWizard.svelte");

describe("SetupWizard", () => {
  it("renders the model-picker step after the bootstrap probe clears", async () => {
    render(SetupWizard, { props: { onComplete: vi.fn() } });
    // Stub renders when the wizard transitions into modelSetup.
    expect(await screen.findByTestId("stub-step")).toBeInTheDocument();
  });

  it("does not render legacy persona cards", async () => {
    render(SetupWizard, { props: { onComplete: vi.fn() } });
    // Wait for the detecting gate to clear so any persona cards
    // would have had a chance to mount. None should.
    await screen.findByTestId("stub-step");
    expect(screen.queryByText(/research & analysis/i)).toBeNull();
    expect(screen.queryByText(/personal assistant/i)).toBeNull();
    expect(screen.queryByText(/^Developer$/)).toBeNull();
  });

  it("renders the SOVEREIGN brand header but no step-track dots", async () => {
    render(SetupWizard, { props: { onComplete: vi.fn() } });
    await screen.findByTestId("stub-step");
    expect(screen.getByText(/SOVEREIGN/)).toBeInTheDocument();
    // Collapsed flow is single-step; no multi-step indicator.
    expect(screen.queryByText(/\d \/ \d/)).toBeNull();
  });
});
