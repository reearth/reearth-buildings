import type { Context } from "hono";
import { type Env, cacheDisabled } from "../env";
import { sha1Hex } from "../hash";
import { MAX_Z, MIN_Z, aabbOnlyAt, areaFilterFor, simplifyFor } from "../lod";
import { fetchBuildingsMvt } from "../pmtiles";
import { fetchTerrainWebp } from "../terrain";
import { IMPL_VERSION, currentPmtilesDate } from "../version";
import { type SourceTile, renderGlbWasm } from "../wasm";

/**
 * Content-addressable, LOD-aware tile delivery.
 *
 * z=12 keeps mega landmarks (footprint ≥ MEGA_M2). z=13 aggregates the
 * z=14 children and keeps mid-sized buildings. z=14 keeps everything
 * smaller. With refine:ADD in tileset.json the layers compose without
 * overlap.
 *
 * The ETag is a hash of all MVT inputs concatenated with the LOD filter
 * range and the terrain tile content — so any upstream change in any
 * contributing tile invalidates only the affected output.
 */
export const glbTile = async (c: Context<{ Bindings: Env }>) => {
  if (c.req.param("impl") !== IMPL_VERSION) {
    return c.text("unknown impl version — re-fetch /tileset.json", 410);
  }

  const yRaw = c.req.param("y") ?? "";
  if (!yRaw.endsWith(".glb")) return c.text("not found", 404);
  const z = Number(c.req.param("z"));
  const x = Number(c.req.param("x"));
  const y = Number(yRaw.slice(0, -4));
  if (!Number.isFinite(z) || !Number.isFinite(x) || !Number.isFinite(y)) {
    return c.text("bad tile coord", 400);
  }
  if (z < MIN_Z || z > MAX_Z) {
    return c.text(`only z=${MIN_Z}..${MAX_Z} is served`, 404);
  }

  const release = await currentPmtilesDate();

  // Fetch the single source MVT at the same coord. Overture's
  // buildings.pmtiles ships pre-generalized tiles at every zoom up to
  // MAX_Z, so we let the upstream do the per-zoom thinning instead of
  // aggregating 16 z=14 children into a z=12 output — that fan-in blew
  // past the Workers CPU budget on dense central-Tokyo tiles.
  // Upstream PMTiles is the dominant source of transient flakiness
  // ("Network connection lost.", sporadic 5xx from S3). fetchWithRetry
  // inside the range source already retries once; if it still fails we
  // surface 503 + Retry-After so loaders (Cesium, MapLibre) retry the
  // tile rather than burning the URL with a 500.
  let mvt: Uint8Array | null;
  try {
    mvt = await fetchBuildingsMvt(release, z, x, y);
  } catch (err) {
    console.error("fetchBuildingsMvt failed", { z, x, y, err: String(err) });
    return retryLater(c, "buildings upstream unavailable");
  }
  if (!mvt) {
    return c.text("tile out of source coverage", 404);
  }
  const sourceTiles: SourceTile[] = [{ mvt, z, x, y }];

  const filter = areaFilterFor(z);
  const simplify = simplifyFor(z);
  const aabbOnly = aabbOnlyAt(z);

  // Terrain for the same (z, x, y). One WebP covers the whole output
  // tile; build_mesh samples per-building at the centroid. Failure is
  // soft: fall back to ellipsoid-anchored output, the worst case being
  // buildings sitting at h=0 instead of on the ground.
  let terrainTile: { webp: Uint8Array; z: number; x: number; y: number } | null = null;
  try {
    const webp = await fetchTerrainWebp(z, x, y);
    if (webp) terrainTile = { webp, z, x, y };
  } catch {
    terrainTile = null;
  }

  const hash = await hashSources(
    sourceTiles,
    z,
    x,
    y,
    filter,
    simplify,
    aabbOnly,
    terrainTile?.webp,
  );
  const etag = `"${hash}"`;
  const noCache = cacheDisabled(c.env);
  const headers = {
    "content-type": "model/gltf-binary",
    "cache-control": noCache ? "no-store" : "public, max-age=300, must-revalidate",
    etag,
    "access-control-allow-origin": "*",
  } as const;

  if (!noCache && c.req.header("if-none-match") === etag) {
    return new Response(null, { status: 304, headers });
  }

  if (!noCache) {
    const r2Key = `cache/buildings/${IMPL_VERSION}/${hash}.glb`;
    let cached: R2ObjectBody | null = null;
    try {
      cached = await c.env.CACHE.get(r2Key);
    } catch (err) {
      // R2 read errors are recoverable: fall through to regeneration.
      console.error("R2 get failed", { r2Key, err: String(err) });
    }
    if (cached) {
      return new Response(cached.body, { headers });
    }
    let glb: Uint8Array;
    try {
      glb = renderGlbWasm(sourceTiles, { z, x, y }, filter, simplify, aabbOnly, terrainTile);
    } catch (err) {
      console.error("renderGlbWasm failed", { z, x, y, err: String(err) });
      return retryLater(c, "renderer transient failure");
    }
    // The R2 write happens after the response is dispatched; the runtime
    // sometimes raises "Network connection lost." here when the edge
    // tears down the subrequest. Swallow it so it doesn't pollute the
    // exception log — the next request will regenerate and re-cache.
    c.executionCtx.waitUntil(
      c.env.CACHE.put(r2Key, glb).catch((err) => {
        console.error("R2 put failed", { r2Key, err: String(err) });
      }),
    );
    return new Response(glb, { headers });
  }

  // CACHE_DISABLED: always regenerate, never touch R2.
  try {
    const glb = renderGlbWasm(sourceTiles, { z, x, y }, filter, simplify, aabbOnly, terrainTile);
    return new Response(glb, { headers });
  } catch (err) {
    console.error("renderGlbWasm failed (no-cache)", { z, x, y, err: String(err) });
    return retryLater(c, "renderer transient failure");
  }
};

function retryLater(c: Context<{ Bindings: Env }>, msg: string): Response {
  return new Response(msg, {
    status: 503,
    headers: {
      "content-type": "text/plain; charset=utf-8",
      "retry-after": "2",
      "cache-control": "no-store",
      "access-control-allow-origin": "*",
    },
  });
}

/** Stable fingerprint covering inputs + filter + output coord + terrain. */
async function hashSources(
  sources: SourceTile[],
  outZ: number,
  outX: number,
  outY: number,
  filter: { minM2: number; maxM2: number },
  simplify: { ratio: number; targetErrorM: number },
  aabbOnly: boolean,
  terrain: Uint8Array | undefined,
): Promise<string> {
  const terrainTag = terrain ? await sha1Hex(terrain, 12) : "no-terrain";
  const header = new TextEncoder().encode(
    `${outZ}/${outX}/${outY};f=${filter.minM2},${filter.maxM2};s=${simplify.ratio},${simplify.targetErrorM};a=${aabbOnly ? 1 : 0};t=${terrainTag};n=${sources.length};`,
  );
  const per: Uint8Array[] = [header];
  for (const s of sources) {
    const h = await sha1Hex(s.mvt, 16);
    per.push(new TextEncoder().encode(`${s.z}/${s.x}/${s.y}:${h};`));
  }
  const total = per.reduce((n, b) => n + b.length, 0);
  const concat = new Uint8Array(total);
  let off = 0;
  for (const b of per) {
    concat.set(b, off);
    off += b.length;
  }
  return sha1Hex(concat, 12);
}
