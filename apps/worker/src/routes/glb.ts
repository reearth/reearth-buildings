import type { Context } from "hono";
import type { Env } from "../env";
import { sha1Hex } from "../hash";
import { fetchBuildingsMvt } from "../pmtiles";
import { IMPL_VERSION, currentPmtilesDate } from "../version";
import { renderGlbWasm } from "../wasm";

/**
 * Content-addressable tile delivery.
 *
 * Flow:
 *   1. Resolve the current upstream PMTiles date (KV-cached, 1h TTL).
 *   2. Fetch the MVT bytes via PMTiles Range (Cloudflare caches the Range
 *      response, so this is cheap on repeats).
 *   3. Hash the MVT → ETag. If the client already has it, return 304
 *      without doing any further work.
 *   4. Look up R2 by hash. If the same MVT was ever rendered before, the
 *      glb is already there and we reuse it — even if the pmtiles build
 *      date changed, content-unchanged tiles never re-render.
 *   5. Only on R2 miss do we run the WASM pipeline.
 */
export const glbTile = async (c: Context<{ Bindings: Env }>) => {
  if (c.req.param("impl") !== IMPL_VERSION) {
    return c.text("unknown impl version — re-fetch /tileset.json", 410);
  }

  const z = Number(c.req.param("z"));
  const x = Number(c.req.param("x"));
  const y = Number(c.req.param("y"));
  if (z !== 14) return c.text("only z=14 is served", 404);

  const pmtilesDate = await currentPmtilesDate(c.env);
  const mvt = await fetchBuildingsMvt(pmtilesDate, z, x, y);
  if (!mvt) return c.text("tile out of source coverage", 404);

  const hash = await sha1Hex(mvt);
  const etag = `"${hash}"`;
  const headers = {
    "content-type": "model/gltf-binary",
    // must-revalidate: clients can reuse for a few minutes, then ask us
    // again so we can serve a 304 (cheap) when content hasn't changed.
    "cache-control": "public, max-age=300, must-revalidate",
    etag,
    "access-control-allow-origin": "*",
  } as const;

  if (c.req.header("if-none-match") === etag) {
    return new Response(null, { status: 304, headers });
  }

  const r2Key = `cache/buildings/${IMPL_VERSION}/${hash}.glb`;
  const cached = await c.env.CACHE.get(r2Key);
  if (cached) {
    return new Response(cached.body, { headers });
  }

  const glb = renderGlbWasm(mvt, z, x, y);
  c.executionCtx.waitUntil(c.env.CACHE.put(r2Key, glb));
  return new Response(glb, { headers });
};
