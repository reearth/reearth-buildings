import { LOD_MODE } from "./lod";
import { fetchWithRetry } from "./retry";

/**
 * Bump when the renderer / glb schema changes. Lives in the URL prefix so
 * a bump alone invalidates every cache layer atomically.
 *
 * Monthly upstream updates are *not* propagated through this constant.
 * Instead, the glb route uses a content hash of the MVT bytes for the
 * ETag and R2 cache key — unchanged tiles stay deduplicated across
 * Overture releases, so only tiles whose source content actually
 * changed get re-rendered.
 *
 * The LOD mode is appended too: switching ADD ↔ REPLACE changes the
 * meaning of the geometry at every zoom, so a flip must invalidate the
 * whole URL space.
 */
// v5: drop explicitly-flagged underground structures (Overture
// `is_underground` / negative `level`) instead of extruding them above
// ground. Renderer-only change with no new hashed parameter (the
// underground policy isn't part of the per-tile ETag), so the version bump
// is what invalidates existing caches.
// v4: Lambert (roughness=1) building material. Visual change in the glb
// payload, so existing v3 caches are no longer correct.
// v3: Overture Maps Buildings + Re:Earth Terrain ground placement.
// Earlier (v2) URL paths used Protomaps OSM and EGM2008 anchoring and
// no longer make sense.
const RENDERER_VERSION = "v5";
export const IMPL_VERSION = `${RENDERER_VERSION}-${LOD_MODE}`;

/**
 * Where Overture Maps publishes the buildings PMTiles. Each release lives
 * under an immutable `tiles/<release>/` prefix (e.g. `tiles/2026-06-17.0/`).
 * The former `overturemaps-tiles-us-west-2-beta` bucket was shut off
 * (403 AllAccessDisabled) in July 2026.
 */
const UPSTREAM_BUCKET = "https://overturemaps-extras-us-west-2.s3.us-west-2.amazonaws.com";
const UPSTREAM_PREFIX = "tiles/";

// v2: cache key bumped with the bucket move so a release string cached
// against the old bucket layout can't poison the first day on the new one.
const LATEST_CACHE_URL = "https://cache.local/overture-latest-v2";
const LATEST_CACHE_TTL_SECONDS = 86400;

/**
 * Resolve the upstream Overture release to fetch from. Auto-discovers via
 * the Workers Cache API on top of an S3 ListBucket call.
 */
export async function currentPmtilesDate(): Promise<string> {
  const cacheKey = new Request(LATEST_CACHE_URL);
  const cache = caches.default;
  const hit = await cache.match(cacheKey);
  if (hit) return await hit.text();
  const fresh = await probeLatestRelease();
  await cache.put(
    cacheKey,
    new Response(fresh, {
      headers: { "Cache-Control": `max-age=${LATEST_CACHE_TTL_SECONDS}` },
    }),
  );
  return fresh;
}

/** Build the absolute upstream URL for a specific Overture release. */
export function upstreamUrl(release: string): string {
  return `${UPSTREAM_BUCKET}/${UPSTREAM_PREFIX}${release}/buildings.pmtiles`;
}

async function probeLatestRelease(): Promise<string> {
  // S3 ListBucketV2 with delimiter=/ under the tiles/ prefix returns a
  // CommonPrefixes entry for every release directory (`tiles/2026-06-17.0/`).
  // They sort lexically into chronological order (yyyy-mm-dd.n).
  const listUrl = `${UPSTREAM_BUCKET}/?list-type=2&delimiter=%2F&prefix=${encodeURIComponent(UPSTREAM_PREFIX)}`;
  const r = await fetchWithRetry(listUrl, {
    cf: { cacheTtl: 3600, cacheEverything: true },
  } as RequestInit);
  if (!r.ok) {
    throw new Error(`overture ListBucket failed: ${r.status}`);
  }
  const xml = await r.text();
  const releases: string[] = [];
  const re = /<Prefix>([^<]+?)\/<\/Prefix>/g;
  for (const m of xml.matchAll(re)) {
    const p = (m[1] ?? "").slice(UPSTREAM_PREFIX.length);
    // Releases are yyyy-mm-dd.n. Anything that doesn't start with a
    // 4-digit date is some other kind of prefix and should be ignored.
    if (/^\d{4}-\d{2}-\d{2}/.test(p)) releases.push(p);
  }
  if (releases.length === 0) {
    throw new Error("no Overture releases found in bucket listing");
  }
  // Lexical sort = chronological order for these prefixes.
  releases.sort();
  const dateOf = (r: string) => r.slice(0, 10);
  const latest = releases[releases.length - 1] ?? "";
  const latestDate = dateOf(latest);
  // Prefer a non-beta release for the latest date if both forms exist
  // (a historical quirk of the old bucket; harmless to keep).
  for (let i = releases.length - 1; i >= 0; i--) {
    const r = releases[i] ?? "";
    if (dateOf(r) !== latestDate) break;
    if (!r.endsWith("-beta")) return r;
  }
  return latest;
}
