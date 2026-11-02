import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

const host = process.env.TAURI_DEV_HOST;

// https://v2.tauri.app/start/frontend/vite/
export default defineConfig({
  plugins: [react()],
  // Tauri expects a fixed port and shows Rust errors itself.
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host ? { protocol: "ws", host, port: 1421 } : undefined,
    // The translations live in the repo's Localization/ folder, shared with the macOS app.
    fs: { allow: [".", "../Localization"] },
    watch: { ignored: ["**/src-tauri/**"] },
  },
  envPrefix: ["VITE_", "TAURI_ENV_*"],
  build: {
    // WebView2 on Windows is Chromium; WebKitGTK on Linux is Safari-like.
    target: process.env.TAURI_ENV_PLATFORM === "windows" ? "chrome105" : "safari13",
    minify: !process.env.TAURI_ENV_DEBUG ? "esbuild" : false,
    sourcemap: !!process.env.TAURI_ENV_DEBUG,
    rollupOptions: {
      output: {
        // The SQL editor is the largest dependency; keep it in its own file.
        manualChunks: (id) =>
          id.includes("node_modules/@codemirror") || id.includes("node_modules/@lezer")
            ? "codemirror"
            : id.includes("node_modules/react")
              ? "react"
              : undefined,
      },
    },
  },
});
