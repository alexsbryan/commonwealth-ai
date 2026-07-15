// SPDX-License-Identifier: AGPL-3.0-or-later
// MeshSettings smoke tests focused on the Phase A1 paste-link input.
// Heavy lifting (mesh creation, state polling, diagnostics) is covered
// elsewhere; these tests verify the dev-mode bypass for the OS
// deep-link handler works and rejects malformed URLs before they hit
// the parser.
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, within } from "@testing-library/svelte";
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
  // Mesh Health calls — only fire once `meshIsRunning` is true, but
  // still imported eagerly. Stub them so the eager import doesn't
  // throw and a future test that flips `running=true` doesn't have
  // to re-mock everything.
  meshGetContributions: vi.fn(async () => []),
  meshListPeerPreferences: vi.fn(async () => []),
  meshSetPeerPreference: vi.fn(async () => undefined),
  meshClearPeerPreference: vi.fn(async () => true),
  // Other api functions touched on mount paths.
  meshRotateInvite: vi.fn(),
  meshRelayCandidates: vi.fn(async () => []),
  getConfig: vi.fn(async () => ({
    node_name: "",
    embedding_model: null,
    chat_model: null,
    mesh_enabled: false,
  })),
  saveConfig: vi.fn(),
  suggestNodeName: vi.fn(async () => "TestyMcTest"),
}));

const { joinLinkStore } = await import("../stores/joinLink.svelte");
const { meshMembership } = await import("../stores/meshMembership.svelte");
const api = await import("../api");

/** Minimal joined-mesh state — enough for the active-mesh status card
 *  (`meshState.status.name` heading + member counts) to render. */
const JOINED_STATE = {
  status: {
    name: "Lab Squad",
    members_online: 2,
    members_total: 3,
    model_name: null,
    knowledge_corpora: [],
    is_connected: true,
    join_link: null,
    join_key: null,
  },
  members: [],
  corpora: [],
  contribution: null,
};

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
      screen.getByText(/doesn't look like a svrnmesh join link/i),
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

// Regression guard for the stale-page bug: a MeshSettings instance
// that mounted before a join kept `running = false` forever — the 5s
// poll early-returned on `!running` and nothing else re-pulled state,
// so the user kept seeing the pre-join "Create a mesh" landing state
// until they navigated away and back.
describe("MeshSettings — refresh after membership change", () => {
  beforeEach(() => {
    joinLinkStore.clear();
    meshMembership.clear();
    vi.mocked(api.meshIsRunning).mockResolvedValue(false);
    vi.mocked(api.meshGetState).mockResolvedValue(null);
  });

  it("re-pulls mesh state when meshMembership.noteJoined() fires while mounted", async () => {
    render(MeshSettings);
    // Mounted pre-join: idle landing state is showing.
    expect(
      await screen.findByRole("button", { name: /create a mesh/i }),
    ).toBeInTheDocument();

    // A join completes elsewhere (MeshJoinDialog in App.svelte).
    vi.mocked(api.meshIsRunning).mockResolvedValue(true);
    vi.mocked(api.meshGetState).mockResolvedValue(JOINED_STATE as never);
    meshMembership.noteJoined();

    // The already-mounted page flips to the joined view on its own —
    // no unmount/remount, no waiting for a poll tick.
    expect(await screen.findByText("Lab Squad")).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: /create a mesh/i }),
    ).not.toBeInTheDocument();
  });

  it("poll re-probes meshIsRunning while idle, catching joins made outside the app", async () => {
    vi.useFakeTimers();
    try {
      render(MeshSettings);
      // Flush the mount-time refresh chain.
      await vi.advanceTimersByTimeAsync(0);

      // Mesh appears underneath us (e.g. CLI `svrn mesh join`) — no
      // epoch bump, only the poll can notice.
      vi.mocked(api.meshIsRunning).mockResolvedValue(true);
      vi.mocked(api.meshGetState).mockResolvedValue(JOINED_STATE as never);

      await vi.advanceTimersByTimeAsync(5000);

      expect(screen.getByText("Lab Squad")).toBeInTheDocument();
    } finally {
      vi.useRealTimers();
    }
  });
});

// Leaving a mesh makes the standalone daemon exit so the service manager
// relaunches it into a fresh solo mesh — ~10-20s where every :9741 call
// fails at the transport layer. The old code called refresh() straight
// after leave and flashed a hard "Failed to load mesh state" the instant
// the daemon went away. It must now show a reconnecting state and poll
// through the outage, then render the fresh solo mesh.
describe("MeshSettings — resilient leave (daemon bounce)", () => {
  beforeEach(() => {
    joinLinkStore.clear();
    meshMembership.clear();
    vi.mocked(api.meshIsRunning).mockResolvedValue(true);
    vi.mocked(api.meshGetState).mockResolvedValue(JOINED_STATE as never);
    vi.mocked(api.meshLeave).mockResolvedValue(undefined as never);
  });

  it("shows reconnecting (not a hard error) while the daemon restarts, then recovers", async () => {
    vi.useFakeTimers();
    try {
      render(MeshSettings);
      await vi.advanceTimersByTimeAsync(0);
      // In a mesh.
      expect(screen.getByText("Lab Squad")).toBeInTheDocument();

      // Open the leave confirmation modal.
      await fireEvent.click(
        screen.getByRole("button", { name: /^leave$/i }),
      );
      const dialog = screen.getByRole("dialog");

      // Model the bounce: leave() succeeds, then the first two readiness
      // probes fail at the transport layer before the relaunched daemon
      // answers into a fresh solo mesh.
      let probes = 0;
      vi.mocked(api.meshIsRunning).mockImplementation(async () => {
        probes += 1;
        if (probes <= 2) {
          throw new Error(
            "error sending request for url (http://localhost:9741/v1/mesh/status)",
          );
        }
        return true;
      });
      const SOLO_STATE = {
        ...JOINED_STATE,
        status: {
          ...JOINED_STATE.status,
          name: "My Solo Mesh",
          members_total: 1,
          members_online: 1,
        },
      };
      vi.mocked(api.meshGetState).mockResolvedValue(SOLO_STATE as never);

      // Confirm the leave (danger button inside the dialog).
      await fireEvent.click(
        within(dialog).getByRole("button", { name: /^leave$/i }),
      );
      await vi.advanceTimersByTimeAsync(0);

      // During the outage: reconnecting message, NOT a hard error.
      expect(screen.getByText(/restarting the daemon/i)).toBeInTheDocument();
      expect(
        screen.queryByText(/Failed to load mesh state/i),
      ).not.toBeInTheDocument();
      expect(
        screen.queryByText(/Failed to leave mesh/i),
      ).not.toBeInTheDocument();

      // Drive the reconnect poll to completion (800ms head start +
      // two 1000ms retry gaps + success).
      await vi.advanceTimersByTimeAsync(3200);

      // Recovered into the fresh solo mesh, reconnecting cleared, no error.
      expect(screen.getByText("My Solo Mesh")).toBeInTheDocument();
      expect(
        screen.queryByText(/restarting the daemon/i),
      ).not.toBeInTheDocument();
      expect(vi.mocked(api.meshLeave)).toHaveBeenCalledTimes(1);
    } finally {
      vi.useRealTimers();
    }
  });
});

// "Leaving" a solo (one-node) mesh only bounces the daemon into another
// identical solo mesh — pointless and jarring. So for solo we hide Leave
// and promote joining another mesh (which uses an in-process auto-leave,
// no bounce). Leave stays for real groups where it's meaningful.
describe("MeshSettings — solo mesh promotes join over leave", () => {
  beforeEach(() => {
    joinLinkStore.clear();
    meshMembership.clear();
  });

  const SOLO_STATE = {
    ...JOINED_STATE,
    status: {
      ...JOINED_STATE.status,
      name: "My Mesh",
      members_online: 1,
      members_total: 1,
      join_link: "sovereign://join/cwth-aaaa-bbbb-cccc",
    },
  };

  it("hides Leave and promotes 'Join another mesh' when solo", async () => {
    vi.mocked(api.meshIsRunning).mockResolvedValue(true);
    vi.mocked(api.meshGetState).mockResolvedValue(SOLO_STATE as never);

    render(MeshSettings);
    expect(await screen.findByText("My Mesh")).toBeInTheDocument();

    expect(
      screen.queryByRole("button", { name: /^leave$/i }),
    ).not.toBeInTheDocument();
    expect(screen.getByText(/join another mesh/i)).toBeInTheDocument();
  });

  it("shows Leave (and no solo join-promotion) when the mesh has other members", async () => {
    vi.mocked(api.meshIsRunning).mockResolvedValue(true);
    // JOINED_STATE has members_total: 3.
    vi.mocked(api.meshGetState).mockResolvedValue(JOINED_STATE as never);

    render(MeshSettings);
    expect(await screen.findByText("Lab Squad")).toBeInTheDocument();

    expect(
      screen.getByRole("button", { name: /^leave$/i }),
    ).toBeInTheDocument();
    expect(
      screen.queryByText(/join another mesh/i),
    ).not.toBeInTheDocument();
  });
});
