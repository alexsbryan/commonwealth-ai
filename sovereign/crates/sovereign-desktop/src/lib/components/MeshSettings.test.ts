// MeshSettings smoke tests focused on the Phase A1 paste-link input.
// Heavy lifting (mesh creation, state polling, diagnostics) is covered
// elsewhere; these tests verify the dev-mode bypass for the OS
// deep-link handler works and rejects malformed URLs before they hit
// the parser.
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent } from "@testing-library/svelte";
import MeshSettings from "./MeshSettings.svelte";

// The component calls several api.ts functions on mount — make them
// no-op so mounting doesn't spam real Tauri invokes.
vi.mock("../api", () => ({
  meshCreate: vi.fn(),
  meshGetState: vi.fn(async () => null),
  meshIsRunning: vi.fn(async () => false),
  meshLeave: vi.fn(),
  // Used by MeshDiagnosticsPanel mounted inside MeshSettings.
  meshDiagnostics: vi.fn(async () => ({
    discovered_peers: [],
    daemon_running: false,
  })),
}));

const { joinLinkStore } = await import("../stores/joinLink.svelte");

describe("MeshSettings — paste join link", () => {
  beforeEach(() => {
    joinLinkStore.clear();
  });

  it("renders the paste-link input and Preview button in the idle state", async () => {
    render(MeshSettings);
    // `meshIsRunning` resolves async, so the idle-state section mounts
    // on the next tick. findByPlaceholderText waits.
    const input = await screen.findByPlaceholderText(
      /sovereign:\/\/join\/cwth-/i,
    );
    expect(input).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /preview/i })).toBeInTheDocument();
  });

  it("rejects a malformed URL with an inline error", async () => {
    render(MeshSettings);
    const input = await screen.findByPlaceholderText(/sovereign:\/\/join\/cwth-/i);
    await fireEvent.input(input, { target: { value: "http://wrong" } });
    const preview = screen.getByRole("button", { name: /preview/i });
    await fireEvent.click(preview);

    expect(
      screen.getByText(/doesn't look like a sovereign join link/i),
    ).toBeInTheDocument();
    expect(joinLinkStore.pending).toBeNull();
  });

  it("writes a valid URL to joinLinkStore, which triggers MeshJoinDialog globally", async () => {
    render(MeshSettings);
    const input = await screen.findByPlaceholderText(/sovereign:\/\/join\/cwth-/i);
    const valid = "sovereign://join/cwth-d26f-cae1-65c6?name=Test+Mesh";
    await fireEvent.input(input, { target: { value: valid } });
    const preview = screen.getByRole("button", { name: /preview/i });
    await fireEvent.click(preview);

    expect(joinLinkStore.pending).toBe(valid);
  });
});
