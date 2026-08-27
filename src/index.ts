import { Hono } from "hono";
import { cors } from "hono/cors";
import type { Env } from "./env";
import { dayBefore, takeDigest } from "./okibi-digest";
import { overturePmtilesProxy } from "./routes/debug";
import { glbTile } from "./routes/glb";
import { subTilesetJson, tilesetJson } from "./routes/tileset";

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

// `/` is served from public/index.html via the [assets] binding in
// wrangler.toml — no Worker route needed.
app.get("/healthz", (c) => c.text("ok"));

// Debug-only: range proxy for the upstream Overture buildings.pmtiles, so
// the viewer's "Compare" split view can render the raw footprints in
// MapLibre via the pmtiles:// protocol. See routes/debug.ts.
app.get("/debug/overture.pmtiles", overturePmtilesProxy);

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

export default {
  fetch: app.fetch,

  /**
   * The daily demand digest.
   *
   * Aggregating a day is not part of serving tiles, and a digest that fails
   * is a digest missing for a day — so it is logged rather than thrown, which
   * would only retry the same failing query on the same finished day.
   */
  async scheduled(controller: ScheduledController, env: Env, ctx: ExecutionContext) {
    ctx.waitUntil(
      takeDigest(env, dayBefore(controller.scheduledTime)).catch((error) => {
        console.warn("okibi: digest failed", error);
      }),
    );
  },
} satisfies ExportedHandler<Env>;
