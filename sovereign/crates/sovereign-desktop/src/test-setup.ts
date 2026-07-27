// SPDX-License-Identifier: AGPL-3.0-or-later
// Vitest setup: imported once per test file via `setupFiles` in
// vitest.config.ts. Extends `expect` with jest-dom matchers (toBeInTheDocument,
// toHaveTextContent, etc.) and stubs the Tauri invoke channel so components
// that call `invoke("...")` during tests don't try to reach a native runtime.
import "@testing-library/jest-dom/vitest";
import { vi } from "vitest";

// Default Tauri stub: every command returns undefined. Individual tests
// override via `vi.mocked(invoke).mockImplementation(...)`.
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => undefined),
}));

// Default Tauri event stub: `listen(...)` returns a no-op unlistener.
// Tests that need to drive events do so via the mocked return value.
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async () => () => {}),
  emit: vi.fn(async () => {}),
}));

// jsdom ships no ResizeObserver, and components that measure their own
// layout (the atlas views size their virtual scroll window from it)
// throw on mount without it. A no-op observer is the honest stub: jsdom
// reports zero-size boxes anyway, so a "real" implementation would only
// ever deliver zeros. Components must therefore not depend on a resize
// callback firing for their content to render — which is the behaviour
// we want to hold them to.
if (typeof globalThis.ResizeObserver === "undefined") {
  globalThis.ResizeObserver = class {
    observe() {}
    unobserve() {}
    disconnect() {}
  } as unknown as typeof ResizeObserver;
}
