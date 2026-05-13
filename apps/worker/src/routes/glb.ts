import type { Context } from "hono";
import type { Env } from "../env";
import { fetchBuildingsMvt } from "../pmtiles";
import { buildVersionKey } from "../version";
import { renderGlbWasm } from "../wasm";

export const glbTile = async (c: Context<{ Bindings: Env }>) => {
  const version = c.req.param("version") ?? "";
  const expected = buildVersionKey(c.env);
  // Reject stale URLs so clients holding a pre-bump tileset.json refresh
  // it instead of letting us regenerate under a wrong cache key.
  if (version !== expected) {
    return c.text("unknown tileset version — re-fetch /tileset.json", 410);
  }

  const z = Number(c.req.param("z"));
  const x = Number(c.req.param("x"));
  const y = Number(c.req.param("y"));
  if (z !== 14) return c.text("only z=14 is served", 404);

  const r2Key = `cache/buildings/${version}/${z}/${x}/${y}.glb`;
  const etag = `"${version}-${z}-${x}-${y}"`;

  const headers = {
    "content-type": "model/gltf-binary",
    "cache-control": "public, max-age=2592000, immutable",
    etag,
    "access-control-allow-origin": "*",
  } as const;

  // Handle conditional requests cheaply.
  if (c.req.header("if-none-match") === etag) {
    return new Response(null, { status: 304, headers });
  }

  // L2: R2 cache
  const cached = await c.env.CACHE.get(r2Key);
  if (cached) {
    return new Response(cached.body, { headers });
  }

  // L3: regenerate
  const mvt = await fetchBuildingsMvt(c.env, z, x, y);
  if (!mvt) return c.text("tile out of source coverage", 404);

  const glb = renderGlbWasm(mvt, z, x, y);

  // async write-back, do not block response
  c.executionCtx.waitUntil(c.env.CACHE.put(r2Key, glb));

  return new Response(glb, { headers });
};
