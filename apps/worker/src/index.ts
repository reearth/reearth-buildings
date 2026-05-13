import { Hono } from "hono";
import { cache } from "hono/cache";
import type { Env } from "./env";
import { glbTile } from "./routes/glb";
import { subTilesetJson, tilesetJson } from "./routes/tileset";
import { viewerHtml } from "./routes/viewer";

const app = new Hono<{ Bindings: Env }>();

app.get("/", viewerHtml);
app.get("/healthz", (c) => c.text("ok"));

// Unversioned entry tileset; small, short cache so a deploy can change
// which IMPL_VERSION-prefixed sub-tileset it points at.
app.get(
  "/tileset.json",
  cache({ cacheName: "tileset-root", cacheControl: "public, max-age=3600" }),
  tilesetJson,
);

// Versioned navigation tilesets — immutable for a given IMPL_VERSION, so
// edge cache can hold them aggressively. Hono's cache middleware keys by
// URL, which is what we want.
app.get(
  "/:impl/sub/:z/:x/:y/tileset.json",
  cache({ cacheName: "tileset-sub", cacheControl: "public, max-age=2592000, immutable" }),
  subTilesetJson,
);

// Versioned glb URL. See src/routes/glb.ts for the content-addressable
// dedup logic; the per-tile ETag covers per-MVT hashes and the LOD
// filter / simplify parameters.
// The trailing `.glb` is part of `:y` (Hono's RegExpRouter crashes on
// regex-constrained params when mixed with our sub-tileset route); the
// handler strips it before parsing.
app.get("/:impl/:z/:x/:y", glbTile);

export default app;
