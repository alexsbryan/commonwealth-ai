import { defineConfig } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";
import { fileURLToPath, URL } from "node:url";

// On a physical iOS/Android device, the WebView loads the dev server over
// the network, not localhost. `tauri ios/android dev` exports the Mac's LAN
// address as TAURI_DEV_HOST; bind Vite (and HMR) to it so the device can
// reach it. Desktop/sim runs leave it unset → `host: false` (localhost only).
const host = process.env.TAURI_DEV_HOST;

// Tauri expects a fixed dev port. The Rust core owns all host I/O, so
// the frontend has no proxy config — it only talks to the core via
// `invoke`/`listen`.
export default defineConfig({
  plugins: [svelte()],
  clearScreen: false,
  server: {
    host: host || false,
    port: 1420,
    strictPort: true,
    hmr: host ? { protocol: "ws", host, port: 1421 } : undefined,
  },
  resolve: {
    alias: {
      // Shared chat render surface, consumed as source (no build step) —
      // same package the desktop app aliases. Mirrored in tsconfig.json.
      "@sovereign/chat-ui": fileURLToPath(
        new URL("../packages/chat-ui/src/index.ts", import.meta.url),
      ),
    },
  },
});
