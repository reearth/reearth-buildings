import { Hono } from "hono";
import { cors } from "hono/cors";
import type { Env } from "./env";
import { glbTile } from "./routes/glb";
import { subTilesetJson, tilesetJson } from "./routes/tileset";
import { viewerHtml } from "./routes/viewer";

const app = new Hono<{ Bindings: Env }>();

// Public dataset — every endpoint is GET-only, but Cesium / MapLibre /
// three.js loaders sometimes preflight (e.g. when headers like
// `If-None-Match` are added by intermediaries), so wire up a permissive
// CORS layer that handles OPTIONS up front. Individual route handlers
// still set their own `Access-Control-Allow-Origin: *` for clarity, but
// the middleware fills in `Allow-Methods` / `Allow-Headers` and 204s
// the preflight.
app.use(
  "*",
  cors({
    origin: "*",
    allowMethods: ["GET", "HEAD", "OPTIONS"],
    allowHeaders: ["*"],
    exposeHeaders: ["ETag", "Cache-Control", "Content-Type"],
    maxAge: 86400,
  }),
);

app.get("/", viewerHtml);
app.get("/healthz", (c) => c.text("ok"));

// Unversioned entry tileset. Skip the edge cache middleware so a deploy
// that changes IMPL_VERSION takes effect immediately for browsers
// revalidating against the worker. The route hands out its own short
// `Cache-Control` with `must-revalidate` (see routes/tileset.ts).
app.get("/tileset.json", tilesetJson);

// Versioned navigation tilesets. We deliberately skip the Hono cache
// middleware: the handler itself emits the right Cache-Control header
// (immutable + URL versioning means browsers/CDNs cache effectively),
// and CACHE_DISABLED in dev can flip those headers to `no-store`.
app.get("/:impl/sub/:z/:x/:y/tileset.json", subTilesetJson);

// Versioned glb URL. See src/routes/glb.ts for the content-addressable
// dedup logic; the per-tile ETag covers per-MVT hashes and the LOD
// filter / simplify parameters.
// The trailing `.glb` is part of `:y` (Hono's RegExpRouter crashes on
// regex-constrained params when mixed with our sub-tileset route); the
// handler strips it before parsing.
app.get("/:impl/:z/:x/:y", glbTile);

export default app;
