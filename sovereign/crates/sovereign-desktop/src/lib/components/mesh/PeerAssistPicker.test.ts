// SPDX-License-Identifier: AGPL-3.0-or-later
// PeerAssistPicker template tests. Pure props-in — no store, no api. The
// contract under test: eligible peers are selectable checkboxes; ineligible
// peers are shown (never silently dropped) with the correct reason copy.

import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent } from "@testing-library/svelte";
import PeerAssistPicker from "./PeerAssistPicker.svelte";
import type { AssistEligiblePeer } from "../../types";

const eligibleA: AssistEligiblePeer = {
  node_id: "aaaa1111bbbb2222",
  name: "Studio",
  online: true,
  eligible: true,
  reason: "ok",
};
const eligibleB: AssistEligiblePeer = {
  node_id: "cccc3333dddd4444",
  name: "Laptop",
  online: true,
  eligible: true,
  reason: "ok",
};
const offlinePeer: AssistEligiblePeer = {
  node_id: "eeee5555ffff6666",
  name: "Server",
  online: false,
  eligible: false,
  reason: "offline",
};
const mismatchPeer: AssistEligiblePeer = {
  node_id: "7777888899990000",
  name: "OldBox",
  online: true,
  eligible: false,
  reason: "embed_model_mismatch",
};

function noop() {}

describe("PeerAssistPicker", () => {
  it("renders eligible peers as checkboxes reflecting the selected set", () => {
    render(PeerAssistPicker, {
      props: {
        peers: [eligibleA, eligibleB],
        selected: [eligibleA.node_id],
        onToggle: noop,
        onSelectAll: noop,
        onClear: noop,
      },
    });
    const boxes = screen.getAllByRole("checkbox") as HTMLInputElement[];
    expect(boxes).toHaveLength(2);
    expect(screen.getByText("Studio")).toBeInTheDocument();
    expect(screen.getByText("Laptop")).toBeInTheDocument();
    // First is selected, second is not.
    expect(boxes[0].checked).toBe(true);
    expect(boxes[1].checked).toBe(false);
  });

  it("shows ineligible peers dimmed with the mapped reason copy", () => {
    render(PeerAssistPicker, {
      props: {
        peers: [eligibleA, offlinePeer, mismatchPeer],
        selected: [],
        onToggle: noop,
        onSelectAll: noop,
        onClear: noop,
      },
    });
    // Ineligible peers are visible (glassbox) but not checkboxes.
    expect(screen.getByText("Server")).toBeInTheDocument();
    expect(screen.getByText("OldBox")).toBeInTheDocument();
    expect(screen.getByText("offline right now")).toBeInTheDocument();
    expect(
      screen.getByText("different embedding model — results wouldn't match"),
    ).toBeInTheDocument();
    // Only the one eligible peer is a checkbox.
    expect(screen.getAllByRole("checkbox")).toHaveLength(1);
  });

  it("toggling a checkbox calls onToggle with that peer's node_id", async () => {
    const onToggle = vi.fn();
    render(PeerAssistPicker, {
      props: {
        peers: [eligibleA],
        selected: [],
        onToggle,
        onSelectAll: noop,
        onClear: noop,
      },
    });
    await fireEvent.click(screen.getByRole("checkbox"));
    expect(onToggle).toHaveBeenCalledWith(eligibleA.node_id);
  });

  it("All / None controls fire onSelectAll / onClear", async () => {
    const onSelectAll = vi.fn();
    const onClear = vi.fn();
    render(PeerAssistPicker, {
      props: {
        peers: [eligibleA, eligibleB],
        selected: [],
        onToggle: noop,
        onSelectAll,
        onClear,
      },
    });
    await fireEvent.click(screen.getByRole("button", { name: /^all$/i }));
    await fireEvent.click(screen.getByRole("button", { name: /^none$/i }));
    expect(onSelectAll).toHaveBeenCalled();
    expect(onClear).toHaveBeenCalled();
  });

  it("shows the local-only line when no peers are on the mesh", () => {
    render(PeerAssistPicker, {
      props: {
        peers: [],
        selected: [],
        onToggle: noop,
        onSelectAll: noop,
        onClear: noop,
      },
    });
    expect(
      screen.getByText(/no other machines are on your mesh/i),
    ).toBeInTheDocument();
  });

  it("falls back to a truncated node_id when a peer has no name", () => {
    render(PeerAssistPicker, {
      props: {
        peers: [{ ...eligibleA, name: "" }],
        selected: [],
        onToggle: noop,
        onSelectAll: noop,
        onClear: noop,
      },
    });
    expect(screen.getByText("aaaa1111")).toBeInTheDocument();
  });
});
