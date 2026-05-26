// Re:Earth Terrain (Mapzen Terrarium / WebP) tile fetcher. We pull the
// raw WebP for the same (z, x, y) as the output buildings tile and hand
// it to the WASM bridge, which decodes and bilinearly samples the
// per-building ground elevation.
//
// We use the `ellipsoid` variant so the per-pixel value is already a
// WGS84 ellipsoidal height (EGM2008 blended in upstream) — no separate
// geoid lookup needed on our side.

import { fetchWithRetry } from "./retry";

const TERRAIN_BASE = "https://terrain.reearth.land/terrarium/ellipsoid";

/**
 * Fetch the terrain WebP for tile (z, x, y). Returns null when the tile
 * is missing (e.g. ocean / out-of-coverage); the caller treats that as
 * "no ground correction" rather than failing the build.
 *
 * Cloudflare's edge cache is used opportunistically; the upstream sets
 * `Cache-Control: public, max-age=2592000`.
 */
export async function fetchTerrainWebp(
  z: number,
  x: number,
  y: number,
): Promise<Uint8Array | null> {
  const url = `${TERRAIN_BASE}/${z}/${x}/${y}.webp`;
  const r = await fetchWithRetry(url, {
    cf: { cacheTtl: 86400, cacheEverything: true },
  } as RequestInit);
  if (r.status === 404) return null;
  if (!r.ok) {
    throw new Error(`terrain fetch failed: ${r.status} ${url}`);
  }
  const buf = await r.arrayBuffer();
  return new Uint8Array(buf);
}
