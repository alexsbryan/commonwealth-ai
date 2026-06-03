import { defineConfig } from "vitest/config";
import { svelte } from "@sveltejs/vite-plugin-svelte";
import { fileURLToPath } from "node:url";
import { resolve } from "node:path";

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
    // `@xstate/svelte` v5 ships a broken `exports.import` that
    // points at a `.cjs.mjs` wrapper around its CJS bundle, which
    // `require()`s `svelte/store` — and Svelte 5 only exports that
    // subpath as ESM. Routing the package through Vitest's web
    // dep optimizer forces esbuild to rewrite the CJS `require` to
    // a dynamic `import()`, unwedging the load.
    deps: {
      optimizer: {
        web: {
          include: ["@xstate/svelte"],
        },
      },
    },
  },
  resolve: {
    // Prefer the browser build for tests — @testing-library/svelte
    // renders into a jsdom window, matching real browser behaviour.
    conditions: ["browser"],
    alias: [
      // Shared chat render surface, consumed as source. Mirrors
      // vite.config.ts + tsconfig.json so tests resolve it identically.
      {
        find: /^@sovereign\/chat-ui$/,
        replacement: resolve(
          fileURLToPath(new URL(".", import.meta.url)),
          "../../../packages/chat-ui/src/index.ts",
        ),
      },
      // `@xstate/svelte` v5.0.0's package.json `exports.import`
      // routes to a `.cjs.mjs` wrapper around its CJS bundle, which
      // `require()`s `svelte/store`. Svelte 5 exports that subpath
      // as ESM only → crash under Node's module loader. Bypass the
      // exports map and point directly at the ESM build that ships
      // alongside (listed as `module` but not in `exports`).
      {
        find: /^@xstate\/svelte$/,
        replacement: resolve(
          fileURLToPath(new URL(".", import.meta.url)),
          "node_modules/@xstate/svelte/dist/xstate-svelte.esm.js",
        ),
      },
    ],
  },
});
