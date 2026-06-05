import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent } from "@testing-library/svelte";
import * as api from "../api";
import MeshAppsSection from "./MeshAppsSection.svelte";

// vi.mock is hoisted above the imports, so the static `import * as api`
// resolves to these stubs. We assert against them via vi.mocked().
vi.mock("../api", () => ({
  listMeshApps: vi.fn(),
  recordMeshAppInstall: vi.fn(),
  openMeshApp: vi.fn(),
  uninstallMeshApp: vi.fn(),
}));

const INSTALL = {
  app_id: "lvt",
  name: "SF Land-Value Tax",
  granted: {
    mesh_store_read: true,
    mesh_store_write: false,
    inference_access: false,
    knowledge_access: false,
  },
  trust: "unsigned",
  recorded_at_unix: 0,
};

describe("MeshAppsSection", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    vi.mocked(api.listMeshApps).mockResolvedValue([]);
    vi.mocked(api.recordMeshAppInstall).mockResolvedValue(INSTALL);
    vi.mocked(api.openMeshApp).mockResolvedValue(undefined);
    vi.mocked(api.uninstallMeshApp).mockResolvedValue(undefined);
  });

  it("offers Install & Open when not installed, and wires the grant + open", async () => {
    render(MeshAppsSection);
    const btn = await screen.findByRole("button", { name: /install & open/i });
    await fireEvent.click(btn);
    // installAndOpen awaits record → refresh → open; wait for the chain.
    await vi.waitFor(() => {
      expect(api.recordMeshAppInstall).toHaveBeenCalledWith(
        "lvt",
        "SF Land-Value Tax",
        expect.objectContaining({ mesh_store_read: true }),
      );
      expect(api.openMeshApp).toHaveBeenCalledWith("lvt");
    });
  });

  it("shows Open + Uninstall when already installed", async () => {
    vi.mocked(api.listMeshApps).mockResolvedValue([INSTALL]);
    render(MeshAppsSection);
    expect(await screen.findByRole("button", { name: /^open$/i })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /uninstall/i })).toBeInTheDocument();
  });
});
