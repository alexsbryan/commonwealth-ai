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
