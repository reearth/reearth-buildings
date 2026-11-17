import { defineConfig } from "vite";

// Builds the bundled viewer (viewer/index.html + viewer/main.ts) into
// build/viewer, which wrangler serves as static assets. Navara loads its
// workers and WASM from URLs next to its own module, and a browser refuses a
// cross-origin worker, so the viewer has to be bundled and served from this
// origin rather than imported from a CDN.
//
// `pnpm dev:viewer` runs Vite's dev server against a `wrangler dev` on :8787
// for the tileset and tiles.
const WORKER = "http://localhost:8787";

export default defineConfig({
  root: "viewer",
  publicDir: "../public",
  build: {
    outDir: "../build/viewer",
    emptyOutDir: true,
    target: "es2022",
  },
  server: {
    proxy: {
      "/tileset.json": WORKER,
      "/debug": WORKER,
      "/healthz": WORKER,
      // Content and subtree URIs: /{impl}/{z}/{x}/{y}.glb and
      // /{impl}/sub/{z}/{x}/{y}/tileset.json.
      "^/v[^/]*/": WORKER,
    },
  },
});
