// SPDX-License-Identifier: AGPL-3.0-or-later
import { defineConfig } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";
import { fileURLToPath, URL } from "node:url";

const host = process.env.TAURI_DEV_HOST;

export default defineConfig({
  plugins: [svelte()],
  resolve: {
    alias: {
      // Shared chat render surface, consumed as source (no build step).
      // See packages/chat-ui. Mirrored in tsconfig.json + vitest.config.ts.
      "@sovereign/chat-ui": fileURLToPath(
        new URL("../../../packages/chat-ui/src/index.ts", import.meta.url),
      ),
    },
  },
  clearScreen: false,
  server: {
    port: 5173,
    strictPort: true,
    // Pin IPv4 loopback when no dev host is named. Node resolves `localhost`
    // to `::1` for the bind while the Tauri webview (WebKitGTK) resolves it to
    // `127.0.0.1` — the webview then connects to a family nothing listens on
    // and the window renders blank (measured 2026-09-22: `ss` showed
    // `[::1]:5173`, `curl 127.0.0.1:5173` refused, app blank). tauri.conf.json's
    // devUrl names 127.0.0.1 for the same reason; TAURI_DEV_HOST still wins for
    // device testing.
    host: host || "127.0.0.1",
    hmr: host ? { protocol: "ws", host, port: 5174 } : undefined,
    watch: {
      ignored: ["**/src-tauri/**"],
    },
  },
});
