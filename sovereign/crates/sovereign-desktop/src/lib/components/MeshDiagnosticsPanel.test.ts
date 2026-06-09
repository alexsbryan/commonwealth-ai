// SPDX-License-Identifier: AGPL-3.0-or-later
// MeshDiagnosticsPanel smoke tests. The panel is simple — renders a
// table of peers from a polled Tauri command — so the test just
// asserts that peers returned by the mocked `meshDiagnostics` show
// up in the DOM.
import { describe, it, expect, vi } from "vitest";
import { render, screen } from "@testing-library/svelte";
import MeshDiagnosticsPanel from "./MeshDiagnosticsPanel.svelte";

vi.mock("../api", () => ({
  meshDiagnostics: vi.fn(async () => ({
    daemon_running: true,
    discovered_peers: [
      {
        node_id: "node-abcd1234",
        mesh_id_hex: "abcdef0123456789abcdef0123456789",
        mesh_name: "The Masonics",
        name: "laptop",
        address: "192.168.1.10:9742",
      },
      {
        node_id: "node-beef5678",
        mesh_id_hex: "1122334455667788aabbccddeeff0011",
        mesh_name: "The Masonics",
        name: "studio",
        address: "192.168.1.11:9742",
      },
    ],
  })),
}));

describe("MeshDiagnosticsPanel", () => {
  it("renders rows for every discovered peer returned by meshDiagnostics", async () => {
    render(MeshDiagnosticsPanel);

    // First tick resolves on mount; findByText waits.
    expect(await screen.findByText("192.168.1.10:9742")).toBeInTheDocument();
    expect(screen.getByText("192.168.1.11:9742")).toBeInTheDocument();
    // Each peer's node label is distinct; the mesh_name column is
    // shared and appears twice.
    expect(screen.getByText("laptop")).toBeInTheDocument();
    expect(screen.getByText("studio")).toBeInTheDocument();
    expect(screen.getAllByText("The Masonics")).toHaveLength(2);
    // Daemon status chip shows the mocked `daemon_running: true`.
    expect(screen.getByText(/daemon running/i)).toBeInTheDocument();
  });

  it("shows the 'no peers yet' message when the daemon is running but alone", async () => {
    const api = await import("../api");
    vi.mocked(api.meshDiagnostics).mockResolvedValueOnce({
      daemon_running: true,
      discovered_peers: [],
    });
    render(MeshDiagnosticsPanel);
    expect(await screen.findByText(/no peers yet/i)).toBeInTheDocument();
  });
});
