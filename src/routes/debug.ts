import type { Context } from "hono";
import type { Env } from "../env";
import { fetchWithRetry } from "../retry";
import { upstreamUrl, withRelease } from "../version";

/**
 * Range proxy for the upstream Overture `buildings.pmtiles`.
 *
 * Debug-only. The bundled viewer's "Compare" split view renders the raw
 * Overture footprints in MapLibre via the `pmtiles://` protocol, side by
 * side with our 3D Tiles output, so you can eyeball footprint drift (e.g.
 * the Tokyo-Station AABB inflation). The browser can't hit the Overture
 * S3 bucket directly — it isn't guaranteed to send CORS headers, and the
 * release date is resolved server-side — so we proxy here: resolve the
 * current release, forward the client's `Range`, and return the bytes with
 * permissive CORS. Mirrors the caching used by the PMTiles range source in
 * pmtiles.ts (Cloudflare caches the full object and serves sub-ranges).
 */
export const overturePmtilesProxy = async (c: Context<{ Bindings: Env }>) => {
  const range = c.req.header("range");
  const init: RequestInit = {
    headers: range ? { Range: range } : {},
    cf: { cacheTtl: 86400, cacheEverything: true },
  } as RequestInit;

  // Same recovery as the glb route: a release cached from before Overture
  // rolled it out of the bucket answers every range with a 404, and the
  // only cure is to resolve the release again. Throwing on a bad status
  // is what lets `withRelease` see that and retry.
  let proxied: { upstream: Response; release: string };
  try {
    proxied = await withRelease(async (rel) => {
      const resp = await fetchWithRetry(upstreamUrl(rel), init);
      if (!resp.ok && resp.status !== 206) {
        throw new Error(`overture upstream ${resp.status}`);
      }
      return { upstream: resp, release: rel };
    });
  } catch (err) {
    console.error("overture pmtiles proxy fetch failed", { err: String(err) });
    return c.text("overture upstream unavailable", 502);
  }
  const { upstream, release } = proxied;

  const headers = new Headers();
  headers.set("access-control-allow-origin", "*");
  headers.set(
    "access-control-expose-headers",
    "Content-Range, Content-Length, ETag, Accept-Ranges",
  );
  headers.set("accept-ranges", "bytes");
  headers.set("content-type", upstream.headers.get("content-type") ?? "application/octet-stream");
  // The Overture release date is immutable per URL, so let the browser keep
  // the header/directory ranges around.
  headers.set("cache-control", "public, max-age=86400");
  headers.set("x-overture-release", release);
  for (const h of ["content-range", "content-length", "etag"]) {
    const v = upstream.headers.get(h);
    if (v) headers.set(h, v);
  }

  return new Response(upstream.body, { status: upstream.status, headers });
};
