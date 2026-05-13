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

// Versioned glb URL: /{impl}/{z}/{x}/{y}.glb. The {impl} segment is the
// IMPL_VERSION constant; bumping it invalidates the URL space. Upstream
// PMTiles updates are handled inside the handler via a content hash on
// the MVT bytes (ETag) — see src/routes/glb.ts.
app.get("/:impl/:z{[0-9]+}/:x{[0-9]+}/:y{[0-9]+}.glb", glbTile);

export default app;
