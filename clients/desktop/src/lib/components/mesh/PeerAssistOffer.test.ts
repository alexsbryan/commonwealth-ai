// SPDX-License-Identifier: AGPL-3.0-or-later
// PeerAssistOffer tests. This component probes `meshAssistEligiblePeers` on
// mount and self-hides unless the corpus is grantable AND a peer is online —
// the graceful local-only degrade path. We mock the api and assert the
// show/hide decision, the disclosure toggle, and the onChange contract.

import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/svelte";
import PeerAssistOffer from "./PeerAssistOffer.svelte";
import type { AssistEligiblePeersResponse } from "../../types";

vi.mock("../../api", () => ({
  meshAssistEligiblePeers: vi.fn(),
}));

const api = await import("../../api");

const eligibleResp: AssistEligiblePeersResponse = {
  grantable: true,
  peers: [
    {
      node_id: "aaaa1111bbbb2222",
      name: "Studio",
      online: true,
      eligible: true,
      reason: "ok",
    },
  ],
};

function mockPeers(resp: AssistEligiblePeersResponse) {
  vi.mocked(api.meshAssistEligiblePeers).mockResolvedValue(resp);
}

describe("PeerAssistOffer", () => {
  beforeEach(() => {
    vi.mocked(api.meshAssistEligiblePeers).mockReset();
  });

  it("renders nothing when the corpus is not grantable", async () => {
    mockPeers({ grantable: false, peers: eligibleResp.peers });
    const onChange = vi.fn();
    const { container } = render(PeerAssistOffer, {
      props: { corpusId: "c1", surface: "folder", onChange },
    });
    // Let the mount probe resolve.
    await waitFor(() => expect(api.meshAssistEligiblePeers).toHaveBeenCalled());
    expect(container.querySelector(".offer")).toBeNull();
  });

  it("renders nothing when grantable but no peers are on the mesh", async () => {
    mockPeers({ grantable: true, peers: [] });
    const { container } = render(PeerAssistOffer, {
      props: { corpusId: "c1", surface: "folder", onChange: vi.fn() },
    });
    await waitFor(() => expect(api.meshAssistEligiblePeers).toHaveBeenCalled());
    expect(container.querySelector(".offer")).toBeNull();
  });

  it("renders nothing when the daemon probe throws (mesh down)", async () => {
    vi.mocked(api.meshAssistEligiblePeers).mockRejectedValue("mesh offline");
    const { container } = render(PeerAssistOffer, {
      props: { corpusId: "c1", surface: "folder", onChange: vi.fn() },
    });
    await waitFor(() => expect(api.meshAssistEligiblePeers).toHaveBeenCalled());
    expect(container.querySelector(".offer")).toBeNull();
  });

  it("shows the collapsed offer with peer count when eligible", async () => {
    mockPeers(eligibleResp);
    render(PeerAssistOffer, {
      props: { corpusId: "c1", surface: "folder", onChange: vi.fn() },
    });
    await waitFor(() =>
      expect(
        screen.getByText(/speed this up with your mesh — 1 peer can help/i),
      ).toBeInTheDocument(),
    );
  });

  it("expands into the picker + guarantees and emits enabled=true", async () => {
    mockPeers(eligibleResp);
    const onChange = vi.fn();
    render(PeerAssistOffer, {
      props: { corpusId: "c1", surface: "folder", onChange },
    });
    const toggle = await screen.findByRole("button", {
      name: /speed this up with your mesh/i,
    });
    await fireEvent.click(toggle);
    // Guarantees strip + picker appear.
    expect(screen.getByText(/you pick the peers/i)).toBeInTheDocument();
    expect(screen.getByText(/nothing is kept/i)).toBeInTheDocument();
    expect(screen.getByText("Studio")).toBeInTheDocument();
    // After expanding with a default-selected eligible peer, the decision is
    // enabled.
    await waitFor(() => {
      const calls = vi.mocked(onChange).mock.calls;
      const last = calls[calls.length - 1][0];
      expect(last.enabled).toBe(true);
      expect(last.peerNodeIds).toContain("aaaa1111bbbb2222");
    });
  });

  it("emits enabled=false before the user expands the offer", async () => {
    mockPeers(eligibleResp);
    const onChange = vi.fn();
    render(PeerAssistOffer, {
      props: { corpusId: "c1", surface: "folder", onChange },
    });
    await waitFor(() => expect(onChange).toHaveBeenCalled());
    // The very first emit (collapsed) must be disabled — picking peers is
    // opt-in via the disclosure.
    expect(vi.mocked(onChange).mock.calls[0][0].enabled).toBe(false);
  });

  // `explainWhenUnavailable` is what ingest surfaces pass. The three
  // unavailable cases above are indistinguishable to a user when the component
  // renders nothing — that was the 2026-07-27 Obsidian-vault report ("I didn't
  // have an option to pull in a peer"). Each must now name its own cause, and
  // they must NOT be interchangeable: an empty mesh and a dead daemon need
  // different actions from the user.
  describe("explainWhenUnavailable", () => {
    const explainProps = {
      corpusId: "obsidian-vault-abc",
      surface: "vault" as const,
      explainWhenUnavailable: true,
      onChange: vi.fn(),
    };

    it("names an unreachable mesh service when the probe throws", async () => {
      vi.mocked(api.meshAssistEligiblePeers).mockRejectedValue("mesh offline");
      render(PeerAssistOffer, { props: { ...explainProps } });
      expect(
        await screen.findByText(/can't reach the mesh service/i),
      ).toBeInTheDocument();
    });

    it("names an empty mesh when grantable but no peers exist", async () => {
      mockPeers({ grantable: true, peers: [] });
      render(PeerAssistOffer, { props: { ...explainProps } });
      expect(
        await screen.findByText(/no other machines have joined your mesh/i),
      ).toBeInTheDocument();
    });

    it("names an unshareable source when peers exist but not grantable", async () => {
      mockPeers({ grantable: false, peers: eligibleResp.peers });
      render(PeerAssistOffer, { props: { ...explainProps } });
      expect(
        await screen.findByText(/isn't shareable with peers/i),
      ).toBeInTheDocument();
    });

    it("still shows the real offer when assist IS available", async () => {
      mockPeers(eligibleResp);
      render(PeerAssistOffer, { props: { ...explainProps } });
      expect(
        await screen.findByText(/speed this up with your mesh/i),
      ).toBeInTheDocument();
      expect(screen.queryByText(/unavailable/i)).toBeNull();
    });
  });
});
