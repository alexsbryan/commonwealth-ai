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
    host: host || false,
    hmr: host ? { protocol: "ws", host, port: 5174 } : undefined,
    watch: {
      ignored: ["**/src-tauri/**"],
    },
  },
});
