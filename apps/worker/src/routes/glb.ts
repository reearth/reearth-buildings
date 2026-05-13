import type { Context } from "hono";
import { geoidUndulation } from "../egm";
import { type Env, cacheDisabled } from "../env";
import { sha1Hex } from "../hash";
import { MAX_Z, MIN_Z, aabbOnlyAt, areaFilterFor, simplifyFor } from "../lod";
import { fetchBuildingsMvt } from "../pmtiles";
import { tileCenter } from "../tile";
import { IMPL_VERSION, currentPmtilesDate } from "../version";
import { type SourceTile, renderGlbWasm } from "../wasm";

/**
 * Content-addressable, LOD-aware tile delivery.
 *
 * z=13 aggregates the four z=14 children's MVTs and keeps only buildings
 * with footprint ≥ THRESHOLD_M2 (large landmarks). z=14 keeps only
 * buildings smaller than that. Together with refine:ADD in tileset.json
 * the two layers compose without overlap.
 *
 * The ETag is a hash of all MVT inputs concatenated with the filter
 * range — so any upstream change in any of the contributing tiles
 * invalidates only the affected output.
 */
export const glbTile = async (c: Context<{ Bindings: Env }>) => {
  if (c.req.param("impl") !== IMPL_VERSION) {
    return c.text("unknown impl version — re-fetch /tileset.json", 410);
  }

  // Route is `/:impl/:z/:x/:y` (no regex constraint to keep Hono happy
  // alongside the sub-tileset pattern), so `y` arrives with the trailing
  // ".glb" attached. Strip it here.
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

  const pmtilesDate = await currentPmtilesDate(c.env);

  // Fetch the source MVTs that contribute to this output tile.
  const sourceCoords = childCoordsAtZ(z, x, y, MAX_Z);
  const sourceTiles: SourceTile[] = [];
  for (const sc of sourceCoords) {
    const mvt = await fetchBuildingsMvt(pmtilesDate, sc.z, sc.x, sc.y);
    if (mvt) sourceTiles.push({ mvt, z: sc.z, x: sc.x, y: sc.y });
  }
  if (sourceTiles.length === 0) {
    return c.text("tile out of source coverage", 404);
  }

  // Filter / simplify derive from LOD_MODE so a mode flip changes the
  // payload semantics. IMPL_VERSION (URL prefix) also encodes the mode,
  // keeping the URL space cache-safe across flips.
  const filter = areaFilterFor(z);
  const simplify = simplifyFor(z);
  const aabbOnly = aabbOnlyAt(z);
  // EGM2008 undulation at the output tile centre; anchors building bases
  // on the geoid so the tileset slots into Cesium terrain at the right
  // altitude. Fail soft to 0 m when the geoid COG isn't provisioned —
  // produces ellipsoid-anchored output, the previous behaviour.
  let geoidOffsetM = 0;
  const center = tileCenter(z, x, y);
  try {
    geoidOffsetM = await geoidUndulation(c.env, center.lonDeg, center.latDeg);
  } catch {
    geoidOffsetM = 0;
  }
  const hash = await hashSources(sourceTiles, z, x, y, filter, simplify, geoidOffsetM, aabbOnly);
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
    const cached = await c.env.CACHE.get(r2Key);
    if (cached) {
      return new Response(cached.body, { headers });
    }
    const glb = renderGlbWasm(sourceTiles, { z, x, y }, filter, simplify, geoidOffsetM, aabbOnly);
    c.executionCtx.waitUntil(c.env.CACHE.put(r2Key, glb));
    return new Response(glb, { headers });
  }

  // CACHE_DISABLED: always regenerate, never touch R2.
  const glb = renderGlbWasm(sourceTiles, { z, x, y }, filter, simplify, geoidOffsetM, aabbOnly);
  return new Response(glb, { headers });
};

/** Tile coords of the descendants at zoom `targetZ` that fully cover (z, x, y). */
function childCoordsAtZ(z: number, x: number, y: number, targetZ: number) {
  if (targetZ < z) return [];
  const f = 2 ** (targetZ - z);
  const result: Array<{ z: number; x: number; y: number }> = [];
  for (let dx = 0; dx < f; dx++) {
    for (let dy = 0; dy < f; dy++) {
      result.push({ z: targetZ, x: x * f + dx, y: y * f + dy });
    }
  }
  return result;
}

/** Stable fingerprint covering inputs + filter + output coord. */
async function hashSources(
  sources: SourceTile[],
  outZ: number,
  outX: number,
  outY: number,
  filter: { minM2: number; maxM2: number },
  simplify: { ratio: number; targetErrorM: number },
  geoidOffsetM: number,
  aabbOnly: boolean,
): Promise<string> {
  const header = new TextEncoder().encode(
    `${outZ}/${outX}/${outY};f=${filter.minM2},${filter.maxM2};s=${simplify.ratio},${simplify.targetErrorM};g=${geoidOffsetM.toFixed(2)};a=${aabbOnly ? 1 : 0};n=${sources.length};`,
  );
  // Hash each MVT first (cheap on cache hit, ~1ms), then mix into a final
  // hash so input order matters but per-tile recomputation is avoided
  // across versions of the filter / output coord.
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
