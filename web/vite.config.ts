import { defineConfig } from "vite";
import solid from "vite-plugin-solid";
import { readFileSync } from "node:fs";

const pkg = JSON.parse(readFileSync(new URL("./package.json", import.meta.url), "utf8")) as { version: string };

// The dev server proxies the API to a local Bridge service so the site runs same-origin in
// development. `BRIDGE_API_PROXY` picks the target: the Rust service on 8480 by default, or the
// mock on 8490 (`pnpm dev:mock`).
const target = process.env.BRIDGE_API_PROXY || "http://127.0.0.1:8480";

export default defineConfig({
  plugins: [solid()],
  define: { __APP_VERSION__: JSON.stringify(pkg.version) },
  build: { target: "es2022" },
  server: {
    port: Number(process.env.PORT || 5190),
    proxy: {
      "/api": { target, changeOrigin: false },
      "/__mock": { target, changeOrigin: false },
    },
  },
  preview: {
    proxy: {
      "/api": { target, changeOrigin: false },
      "/__mock": { target, changeOrigin: false },
    },
  },
  test: {
    environment: "jsdom",
    include: ["tests/unit/**/*.test.ts"],
  },
} as never);
