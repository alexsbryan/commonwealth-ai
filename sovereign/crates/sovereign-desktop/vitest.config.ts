import { defineConfig } from "vitest/config";
import { svelte } from "@sveltejs/vite-plugin-svelte";

// Separate Vite config for Vitest so the dev server settings (Tauri
// host, strict port, src-tauri watch ignores) don't leak into the test
// runner. The svelte plugin is required because we render real
// components in a few integration tests via @testing-library/svelte.
export default defineConfig({
  plugins: [svelte({ hot: false })],
  test: {
    environment: "jsdom",
    globals: true,
    include: ["src/**/*.{test,spec}.{ts,js}"],
    setupFiles: ["./src/test-setup.ts"],
  },
  resolve: {
    // Prefer the browser build for tests — @testing-library/svelte
    // renders into a jsdom window, matching real browser behaviour.
    conditions: ["browser"],
  },
});
