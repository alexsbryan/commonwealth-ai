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
  loadCatalog: vi.fn(),
  listCorpora: vi.fn(),
  installCorpus: vi.fn(),
  stageCorpusRecipe: vi.fn(),
}));

// The component subscribes to `corpus-progress`; stub the event module.
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn().mockResolvedValue(() => {}),
}));

// Minimal CorpusEntry stand-ins (the component reads only id + status).
const corpora = (status: string) =>
  ["sf-assessor-roll", "uap-blue-book", "enron-sample-multi-wide"].map((id) => ({ id, status })) as never;

const READ_GRANTS = {
  mesh_store_read: true,
  mesh_store_write: false,
  inference_access: false,
  knowledge_access: false,
};

// The catalog is manifest-driven: the component renders whatever loadCatalog
// returns. These three manifests stand in for the bundled meshapp.json files.
const MANIFESTS = [
  { id: "lvt", name: "SF Land-Value Tax", version: "0.1.0", blurb: "Parcels.", corpus: "sf-assessor-roll", entry: "index.html", grants: READ_GRANTS, trust: "unsigned" },
  { id: "uap", name: "Project Blue Book", version: "0.1.0", blurb: "UFO archive.", corpus: "uap-blue-book", entry: "index.html", grants: READ_GRANTS, trust: "unsigned" },
  { id: "enron", name: "Enron Task Force", version: "0.2.0", blurb: "Enron email.", corpus: "enron-sample-multi-wide", entry: "index.html", grants: READ_GRANTS, trust: "unsigned" },
];

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
    vi.mocked(api.loadCatalog).mockResolvedValue(MANIFESTS);
    // Default: the apps' corpora are already present → "Install & Open" / "Open".
    vi.mocked(api.listCorpora).mockResolvedValue(corpora("installed"));
    vi.mocked(api.installCorpus).mockResolvedValue(undefined);
    vi.mocked(api.stageCorpusRecipe).mockResolvedValue(undefined);
  });

  it("offers 'Get data' when the corpus isn't downloaded yet", async () => {
    vi.mocked(api.listCorpora).mockResolvedValue(corpora("not_installed"));
    render(MeshAppsSection);
    const btns = await screen.findAllByRole("button", { name: /get data/i });
    expect(btns.length).toBe(3); // no corpus present → all three prompt to download
  });

  it("lists every catalog app with Install & Open, and wires the grant + open", async () => {
    render(MeshAppsSection);
    const btns = await screen.findAllByRole("button", { name: /install & open/i });
    expect(btns.length).toBe(3); // LVT + Blue Book + Enron
    expect(screen.getByText("SF Land-Value Tax")).toBeInTheDocument();
    expect(screen.getByText("Project Blue Book")).toBeInTheDocument();
    expect(screen.getByText("Enron Task Force")).toBeInTheDocument();
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

  it("installs the Enron app from its own card", async () => {
    render(MeshAppsSection);
    const card = (await screen.findByText("Enron Task Force")).closest(".app-card") as HTMLElement;
    const btn = within(card).getByRole("button", { name: /install & open/i });
    await fireEvent.click(btn);
    await vi.waitFor(() => {
      expect(api.recordMeshAppInstall).toHaveBeenCalledWith(
        "enron",
        "Enron Task Force",
        expect.objectContaining({ mesh_store_read: true }),
      );
      expect(api.openMeshApp).toHaveBeenCalledWith("enron");
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
