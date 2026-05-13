import { PMTiles, type RangeResponse, type Source } from "pmtiles";
import type { Env } from "./env";

class RangeSource implements Source {
  constructor(private readonly url: string) {}

  getKey() {
    return this.url;
  }

  async getBytes(offset: number, length: number): Promise<RangeResponse> {
    const resp = await fetch(this.url, {
      headers: { Range: `bytes=${offset}-${offset + length - 1}` },
      cf: { cacheTtl: 86400, cacheEverything: true },
    } as RequestInit);
    if (!resp.ok && resp.status !== 206) {
      throw new Error(`PMTiles range fetch failed: ${resp.status}`);
    }
    const data = await resp.arrayBuffer();
    const out: RangeResponse = { data };
    const etag = resp.headers.get("etag");
    if (etag) out.etag = etag;
    const expires = resp.headers.get("expires");
    if (expires) out.expires = expires;
    const cacheControl = resp.headers.get("cache-control");
    if (cacheControl) out.cacheControl = cacheControl;
    return out;
  }
}

let pmtilesInstance: PMTiles | undefined;

function pmtilesFor(env: Env): PMTiles {
  if (!pmtilesInstance) {
    pmtilesInstance = new PMTiles(new RangeSource(env.PMTILES_SOURCE_URL));
  }
  return pmtilesInstance;
}

/**
 * Fetch the MVT bytes for a tile at (z, x, y) from the upstream PMTiles file.
 * Returns null when the tile is absent (sea, out of coverage, etc).
 */
export async function fetchBuildingsMvt(
  env: Env,
  z: number,
  x: number,
  y: number,
): Promise<Uint8Array | null> {
  const tile = await pmtilesFor(env).getZxy(z, x, y);
  if (!tile) return null;
  return new Uint8Array(tile.data);
}
