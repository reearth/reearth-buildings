import type { Context } from "hono";
import type { Env } from "../env";
import { fetchBuildingsMvt } from "../pmtiles";
import { renderGlbWasm } from "../wasm";

export const glbTile = async (c: Context<{ Bindings: Env }>) => {
  const z = Number(c.req.param("z"));
  const x = Number(c.req.param("x"));
  const y = Number(c.req.param("y"));

  if (z !== 14) return c.text("only z=14 is served in M1", 404);

  const cacheKey = `cache/buildings/v${c.env.PMTILES_VERSION}/${z}/${x}/${y}.glb`;

  // L2: R2 cache lookup
  const cached = await c.env.CACHE.get(cacheKey);
  if (cached) {
    return new Response(cached.body, {
      headers: { "content-type": "model/gltf-binary" },
    });
  }

  // L3: regenerate
  const mvt = await fetchBuildingsMvt(c.env, z, x, y);
  if (!mvt) return c.text("tile out of source coverage", 404);

  const glb = renderGlbWasm(mvt, z, x, y);

  // async write-back, do not block response
  c.executionCtx.waitUntil(c.env.CACHE.put(cacheKey, glb));

  return new Response(glb, {
    headers: { "content-type": "model/gltf-binary" },
  });
};
