import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, within } from "@testing-library/svelte";
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

  it("lists every catalog app with Install & Open, and wires the grant + open", async () => {
    render(MeshAppsSection);
    const btns = await screen.findAllByRole("button", { name: /install & open/i });
    expect(btns.length).toBe(2); // LVT + Blue Book
    expect(screen.getByText("SF Land-Value Tax")).toBeInTheDocument();
    expect(screen.getByText("Project Blue Book")).toBeInTheDocument();
    // Click the first card (LVT) → records lvt grant + opens lvt.
    await fireEvent.click(btns[0]);
    await vi.waitFor(() => {
      expect(api.recordMeshAppInstall).toHaveBeenCalledWith(
        "lvt",
        "SF Land-Value Tax",
        expect.objectContaining({ mesh_store_read: true }),
      );
      expect(api.openMeshApp).toHaveBeenCalledWith("lvt");
    });
  });

  it("installs the Blue Book app from its own card", async () => {
    render(MeshAppsSection);
    const card = (await screen.findByText("Project Blue Book")).closest(".app-card") as HTMLElement;
    const btn = within(card).getByRole("button", { name: /install & open/i });
    await fireEvent.click(btn);
    await vi.waitFor(() => {
      expect(api.recordMeshAppInstall).toHaveBeenCalledWith(
        "uap",
        "Project Blue Book",
        expect.objectContaining({ mesh_store_read: true }),
      );
      expect(api.openMeshApp).toHaveBeenCalledWith("uap");
    });
  });

  it("shows Open + Uninstall for an installed app", async () => {
    vi.mocked(api.listMeshApps).mockResolvedValue([INSTALL]); // lvt installed
    render(MeshAppsSection);
    // LVT shows Open (exact) + Uninstall; the not-installed Blue Book card
    // still shows Install & Open (which /^open$/ does not match).
    expect(await screen.findByRole("button", { name: /^open$/i })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /uninstall/i })).toBeInTheDocument();
  });
});
