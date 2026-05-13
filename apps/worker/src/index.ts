import { Hono } from "hono";
import { cache } from "hono/cache";
import type { Env } from "./env";
import { glbTile } from "./routes/glb";
import { tilesetJson } from "./routes/tileset";

const app = new Hono<{ Bindings: Env }>();

app.get("/", (c) => c.text("reearth-buildings — see /tileset.json"));
app.get("/healthz", (c) => c.text("ok"));

app.get(
  "/tileset.json",
  cache({ cacheName: "tileset", cacheControl: "public, max-age=3600" }),
  tilesetJson,
);

// Versioned glb URL: /{version}/{z}/{x}/{y}.glb. The version is
// `${IMPL_VERSION}-d${PMTILES_VERSION}`; see src/version.ts. Including it
// in the path lets us bump the constant or env var to atomically
// invalidate every cache layer (edge Cache API, browser, R2) — the URL
// itself is the cache key.
app.get(
  "/:version/:z{[0-9]+}/:x{[0-9]+}/:y{[0-9]+}.glb",
  cache({ cacheName: "glb", cacheControl: "public, max-age=2592000, immutable" }),
  glbTile,
);

export default app;
