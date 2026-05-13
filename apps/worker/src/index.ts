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

app.get(
  "/:z{[0-9]+}/:x{[0-9]+}/:y{[0-9]+}.glb",
  cache({ cacheName: "glb", cacheControl: "public, max-age=2592000, immutable" }),
  glbTile,
);

export default app;
