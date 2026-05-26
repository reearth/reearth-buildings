import { PMTiles, type RangeResponse, type Source } from "pmtiles";
import { fetchWithRetry } from "./retry";
import { upstreamUrl } from "./version";

class RangeSource implements Source {
  constructor(private readonly url: string) {}

  getKey() {
    return this.url;
  }

  async getBytes(offset: number, length: number): Promise<RangeResponse> {
    // Cloudflare's subrequest pipeline occasionally surfaces transient
    // "Network connection lost." / 5xx from upstream S3. One retry with a
    // tiny backoff turns the dominant cause of random 500s on this worker
    // into a silent recovery.
    const resp = await fetchWithRetry(this.url, {
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
// state hot across requests. Keyed by Overture release so a release bump
// doesn't reuse a stale instance.
const instances = new Map<string, PMTiles>();

function pmtilesFor(release: string): PMTiles {
  let inst = instances.get(release);
  if (!inst) {
    inst = new PMTiles(new RangeSource(upstreamUrl(release)));
    instances.set(release, inst);
  }
  return inst;
}

/**
 * Fetch the (decompressed) MVT bytes for a tile from a specific Overture
 * release. Returns null if the tile is absent (e.g. ocean, out-of-coverage).
 */
export async function fetchBuildingsMvt(
  release: string,
  z: number,
  x: number,
  y: number,
): Promise<Uint8Array | null> {
  const tile = await pmtilesFor(release).getZxy(z, x, y);
  if (!tile) return null;
  return new Uint8Array(tile.data);
}
