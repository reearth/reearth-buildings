import { PMTiles, type RangeResponse, type Source } from "pmtiles";
import { upstreamUrl } from "./version";

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

// Per-isolate cache so a busy edge keeps its PMTiles header/leaf-directory
// state hot across requests. Keyed by date so a pmtiles version bump
// doesn't reuse a stale instance.
const instances = new Map<string, PMTiles>();

function pmtilesFor(date: string): PMTiles {
  let inst = instances.get(date);
  if (!inst) {
    inst = new PMTiles(new RangeSource(upstreamUrl(date)));
    instances.set(date, inst);
  }
  return inst;
}

/**
 * Fetch the (decompressed) MVT bytes for a tile from a specific upstream
 * date. Returns null if the tile is absent (e.g. ocean, out-of-coverage).
 */
export async function fetchBuildingsMvt(
  date: string,
  z: number,
  x: number,
  y: number,
): Promise<Uint8Array | null> {
  const tile = await pmtilesFor(date).getZxy(z, x, y);
  if (!tile) return null;
  return new Uint8Array(tile.data);
}
