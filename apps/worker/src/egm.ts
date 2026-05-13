// EGM2008 geoid undulation sampler.
//
// Reads the PROJ-distributed `us_nga_egm08_25.tif` (25′ Float32 COG) from
// the SOURCES R2 bucket and returns N — the height of the geoid (mean
// sea level) above the WGS84 ellipsoid — at a given lon/lat. We use it
// to anchor every building's base on the geoid instead of the bare
// ellipsoid, which is what Cesium World Terrain (and viewers expecting
// real-world altitudes) assume.
//
// Per-tile values are cached in KV indefinitely; geoid data is
// effectively immutable for our purposes.

import { openCog } from "./cog";
import type { Env } from "./env";

const R2_KEY = "geoid/egm08_cog.tif";
const KV_PREFIX = "geoid:egm08:";

/** Returns N in metres at (lon, lat), interpolated bilinearly. */
export async function geoidUndulation(env: Env, lonDeg: number, latDeg: number): Promise<number> {
  // Round to 0.01° (~1 km) for caching so neighbouring tile centres
  // share the same KV entry.
  const lonKey = Math.round(lonDeg * 100) / 100;
  const latKey = Math.round(latDeg * 100) / 100;
  const cacheKey = `${KV_PREFIX}${lonKey},${latKey}`;
  const cached = await env.PMTILES_DIR.get(cacheKey);
  if (cached) {
    const n = Number(cached);
    if (Number.isFinite(n)) return n;
  }

  const n = await sampleCog(env.SOURCES, lonDeg, latDeg);
  // Best-effort KV write; failures (e.g. dev local without KV) are non-fatal.
  await env.PMTILES_DIR.put(cacheKey, n.toFixed(2)).catch(() => {});
  return n;
}

async function sampleCog(bucket: R2Bucket, lonDeg: number, latDeg: number): Promise<number> {
  const { image } = await openCog(bucket, R2_KEY);
  const w = image.getWidth();
  const h = image.getHeight();
  const bbox = image.getBoundingBox(); // [minX, minY, maxX, maxY] = [west, south, east, north]
  if (!bbox || bbox.length < 4) return 0;
  const [west, south, east, north] = bbox as [number, number, number, number];

  // Normalize lon to [west, east). The EGM08 grid covers [-180, 180] but
  // geotiff.js may expose [0, 360]; handle both.
  let lon = lonDeg;
  if (lon < west) lon += 360;
  if (lon >= east) lon -= 360;

  const fx = ((lon - west) / (east - west)) * w;
  const fy = ((north - latDeg) / (north - south)) * h;

  // Read a 2x2 window covering the lookup point so we can bilinearly
  // interpolate without grabbing the whole image strip.
  const x0 = Math.max(0, Math.min(w - 2, Math.floor(fx)));
  const y0 = Math.max(0, Math.min(h - 2, Math.floor(fy)));
  const rasters = await image.readRasters({
    window: [x0, y0, x0 + 2, y0 + 2],
    samples: [0],
    interleave: false,
  });
  const band = (Array.isArray(rasters) ? rasters[0] : rasters) as ArrayLike<number> | undefined;
  if (!band || band.length < 4) return 0;

  const dx = fx - x0;
  const dy = fy - y0;
  const v00 = band[0] ?? 0;
  const v10 = band[1] ?? 0;
  const v01 = band[2] ?? 0;
  const v11 = band[3] ?? 0;
  return (v00 * (1 - dx) + v10 * dx) * (1 - dy) + (v01 * (1 - dx) + v11 * dx) * dy;
}
