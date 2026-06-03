import { defineConfig } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";
import { fileURLToPath, URL } from "node:url";

// Tauri expects a fixed dev port. The Rust core owns all host I/O, so
// the frontend has no proxy config — it only talks to the core via
// `invoke`/`listen`.
export default defineConfig({
  plugins: [svelte()],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
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
