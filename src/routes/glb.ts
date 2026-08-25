import type { Context } from "hono";
import { type Env, cacheDisabled } from "../env";
import { sha1Hex } from "../hash";
import { MAX_Z, MIN_Z, aabbOnlyAt, areaFilterFor, simplifyFor } from "../lod";
import { writeTileDemand } from "../okibi";
import { fetchBuildingsMvt } from "../pmtiles";
import { fetchTerrainWebp } from "../terrain";
import { IMPL_VERSION, withRelease } from "../version";
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

  // Timed from here rather than from around the render, because a Worker's
  // clock only advances after I/O — a Spectre mitigation — and the render is
  // pure CPU. Read either side of it and the answer is zero however long it
  // ran, which okibi would take as "free to regenerate".
  //
  // What this spans is a cold request end to end: fetch the sources, build
  // the mesh, persist it. That is also the number worth having, since it is
  // what somebody waited.
  const startedAt = Date.now();

  // The edge cache, before anything is fetched.
  //
  // Everything below this point costs at least two subrequests: the ETag is a
  // hash of the MVT inputs, so even answering "you already have it" means
  // fetching them. A colo-local hit skips the fetches, the R2 read and the
  // render together, which is the whole of what serving a warm tile costs.
  //
  // Keyed on a URL no client sends, which is the point. An entry stored under
  // the plain request URL is one the edge can find on its own, and then it
  // answers before this worker runs — the tile is served, and the fact that
  // somebody wanted it is never written down. The tiles that would vanish
  // first are the popular ones, which are exactly the ones a warm plan puts
  // first. Both other Re:Earth tile workers key their edge entries this way
  // for the same reason.
  //
  // Not keyed on the content hash, because the hash is not known until after
  // the fetches this is trying to avoid. What that costs is an entry up to
  // `max-age` behind an Overture release, which is what the response already
  // promises every client.
  const edgeCache = caches.default;
  const edgeUrl = new URL(c.req.url);
  edgeUrl.searchParams.set("__v", IMPL_VERSION);
  const edgeKey = new Request(edgeUrl.toString(), { method: "GET" });
  if (!cacheDisabled(c.env)) {
    const edge = await edgeCache.match(edgeKey);
    if (edge) {
      const stored = edge.headers.get("etag");
      if (stored && c.req.header("if-none-match") === stored) {
        writeTileDemand(
          c.env,
          c.req.raw,
          { z, x, y },
          { cacheStatus: "hit", layer: "client", genMs: 0, bytes: 0 },
        );
        return new Response(null, {
          status: 304,
          headers: {
            etag: stored,
            "cache-control": edge.headers.get("cache-control") ?? "",
            "access-control-allow-origin": "*",
          },
        });
      }
      writeTileDemand(
        c.env,
        c.req.raw,
        { z, x, y },
        {
          cacheStatus: "hit",
          layer: "edge",
          genMs: 0,
          bytes: Number(edge.headers.get("content-length") ?? 0),
        },
      );
      return edge;
    }
  }

  // Fetch the single source MVT at the same coord. Overture's
  // buildings.pmtiles ships pre-generalized tiles at every zoom up to
  // MAX_Z, so we let the upstream do the per-zoom thinning instead of
  // aggregating 16 z=14 children into a z=12 output — that fan-in blew
  // past the Workers CPU budget on dense central-Tokyo tiles.
  //
  // Both halves of this can fail: the release probe hits Overture's S3
  // ListBucket, and the tile read is the dominant source of transient
  // flakiness ("Network connection lost.", sporadic 5xx from S3).
  // fetchWithRetry inside the range source already retries once, and
  // withRelease covers the one failure retrying the same URL can't fix —
  // a cached release that has since rolled out of the bucket. If it
  // still fails we surface 503 + Retry-After so loaders (Cesium,
  // MapLibre) retry the tile rather than burning the URL with a 500.
  let mvt: Uint8Array | null;
  try {
    mvt = await withRelease((release) => fetchBuildingsMvt(release, z, x, y));
  } catch (err) {
    console.error("buildings mvt fetch failed", { z, x, y, err: String(err) });
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

  // A 304 is somebody asking for this tile and being told they already have
  // it, which is demand like any other: the tile has to exist for the answer
  // to be that.
  const record = (
    cacheStatus: "hit" | "miss",
    // `client` is a 304: the requester already had the bytes and only asked
    // whether they were still current, so nothing was read anywhere.
    layer: "client" | "edge" | "store" | undefined,
    genMs: number,
    bytes: number,
  ): void => writeTileDemand(c.env, c.req.raw, { z, x, y }, { cacheStatus, layer, genMs, bytes });

  if (!noCache && c.req.header("if-none-match") === etag) {
    // Answered without reading anything: the client already had the bytes and
    // only asked whether they were still current.
    record("hit", "client", 0, 0);
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
      record("hit", "store", 0, cached.size);
      const response = new Response(cached.body, { headers });
      // So the next request for this tile does not read R2 again.
      c.executionCtx.waitUntil(edgeCache.put(edgeKey, response.clone()));
      return response;
    }
    let glb: Uint8Array;
    try {
      glb = renderGlbWasm(sourceTiles, { z, x, y }, filter, simplify, aabbOnly, terrainTile);
    } catch (err) {
      console.error("renderGlbWasm failed", { z, x, y, err: String(err) });
      return retryLater(c, "renderer transient failure");
    }
    // Inputs are dead now; see releaseSources.
    mvt = null;
    releaseSources(sourceTiles, terrainTile);
    // The R2 write happens after the response is dispatched; the runtime
    // sometimes raises "Network connection lost." here when the edge
    // tears down the subrequest. Swallow it so it doesn't pollute the
    // exception log — the next request will regenerate and re-cache.
    c.executionCtx.waitUntil(
      c.env.CACHE.put(r2Key, glb)
        .catch((err) => {
          console.error("R2 put failed", { r2Key, err: String(err) });
        })
        // After the write, because the write is the I/O that lets the clock
        // catch up with the render. Reading it before would be reading the
        // time as of the last fetch.
        .finally(() => record("miss", undefined, Date.now() - startedAt, glb.byteLength)),
    );
    const response = new Response(glb, { headers });
    c.executionCtx.waitUntil(edgeCache.put(edgeKey, response.clone()));
    return response;
  }

  // CACHE_DISABLED: always regenerate, never touch R2.
  try {
    const glb = renderGlbWasm(sourceTiles, { z, x, y }, filter, simplify, aabbOnly, terrainTile);
    // No write here, so nothing makes the clock catch up with the render and
    // this reads as the fetches alone. The var is only set where there is no
    // binding to write to, so in practice this writes nothing — but a path
    // that quietly stopped counting would be worse than one that counts and
    // is never read.
    record("miss", undefined, Date.now() - startedAt, glb.byteLength);
    // Inputs are dead now; see releaseSources.
    mvt = null;
    releaseSources(sourceTiles, terrainTile);
    return new Response(glb, { headers });
  } catch (err) {
    console.error("renderGlbWasm failed (no-cache)", { z, x, y, err: String(err) });
    return retryLater(c, "renderer transient failure");
  }
};

/**
 * Drop the render inputs the moment the glb exists.
 *
 * A dense z=14 Tokyo MVT is ~6 MB and its terrain WebP another ~0.3 MB, and
 * they'd otherwise stay reachable from this handler's locals for the whole
 * tail of the request — the response body and the R2 write both hold the
 * (larger) glb at the same time, inside a 128 MB isolate that is also
 * serving the rest of the viewport concurrently.
 */
function releaseSources(sources: SourceTile[], terrain: { webp: Uint8Array } | null): void {
  sources.length = 0;
  if (terrain) terrain.webp = new Uint8Array(0);
}

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
