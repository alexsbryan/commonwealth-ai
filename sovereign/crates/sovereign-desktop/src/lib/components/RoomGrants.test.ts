// SPDX-License-Identifier: AGPL-3.0-or-later
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/svelte";
import * as api from "../api";
import RoomGrants from "./RoomGrants.svelte";

// vi.mock is hoisted: the static import above resolves to these stubs.
vi.mock("../api", () => ({
  createGuestGrant: vi.fn(),
  listGuestGrants: vi.fn(),
  revokeGuestGrant: vi.fn(),
}));

// The QR is rendered by a dynamic import of `qrcode` (settings-panel pattern).
vi.mock("qrcode", () => ({
  toDataURL: vi.fn().mockResolvedValue("data:image/png;base64,AAAA"),
}));

const row = (over: Partial<api.GuestGrantRow> = {}): api.GuestGrantRow => ({
  token_prefix: "7d5b1815",
  summary: "primary; the wall",
  label: "front door",
  expires_at_ms: Date.now() + 3_600_000,
  revoked: false,
  live: true,
  ...over,
});

beforeEach(() => {
  vi.mocked(api.listGuestGrants).mockResolvedValue([]);
  vi.mocked(api.createGuestGrant).mockReset();
  vi.mocked(api.revokeGuestGrant).mockReset();
});

describe("RoomGrants", () => {
  it("lists the outstanding grants the daemon reports", async () => {
    vi.mocked(api.listGuestGrants).mockResolvedValue([
      row(),
      row({
        token_prefix: "aabbccdd",
        summary: "primary; house-ledger",
        label: null,
        live: false,
        revoked: true,
      }),
    ]);
    render(RoomGrants);
    await waitFor(() => expect(api.listGuestGrants).toHaveBeenCalled());
    expect(await screen.findByText("primary; the wall")).toBeTruthy();
    expect(screen.getByText("front door")).toBeTruthy();
    // Revoked rows say so rather than reading as live.
    expect(screen.getByText(/revoked/)).toBeTruthy();
  });

  it("mints a link, shows the QR, and grants the wall by default", async () => {
    vi.mocked(api.createGuestGrant).mockResolvedValue({
      token: "tok-1",
      expires_at_ms: Date.now() + 7_200_000,
      summary: "primary; the wall",
      link: "https://svrnme.sh/ring/#token=tok-1&exp=1&iroh=abc",
    });
    render(RoomGrants);

    fireEvent.input(screen.getByPlaceholderText("optional, shown in the list"), {
      target: { value: "front door" },
    });
    await fireEvent.click(screen.getByRole("button", { name: /mint guest link/i }));

    await waitFor(() => expect(api.createGuestGrant).toHaveBeenCalled());
    // Empty scope means the whole wall; no models named means the daemon's
    // primary alias — the same defaults the CLI applies.
    expect(vi.mocked(api.createGuestGrant).mock.calls[0][0]).toMatchObject({
      scope: "",
      models: [],
      baseUrl: "https://svrnme.sh",
      label: "front door",
    });
    // The link the daemon composed is shown, and its QR rendered.
    expect(await screen.findByText(/token=tok-1/)).toBeTruthy();
    expect(await screen.findByAltText("Guest link QR code")).toBeTruthy();
  });
});
