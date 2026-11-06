import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// The Rust server serves `dist/`; during development Vite proxies the API.
export default defineConfig({
  plugins: [react()],
  server: {
    port: 5173,
    proxy: {
      "/api": { target: "http://127.0.0.1:7788", changeOrigin: true },
      "/healthz": { target: "http://127.0.0.1:7788" },
    },
  },
  build: { outDir: "dist", emptyOutDir: true },
});
