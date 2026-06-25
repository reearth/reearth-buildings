import { LOD_MODE } from "./lod";

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
 * under an immutable date prefix (e.g. `2026-01-21/`).
 */
const UPSTREAM_BUCKET = "https://overturemaps-tiles-us-west-2-beta.s3.amazonaws.com";

const LATEST_CACHE_URL = "https://cache.local/overture-latest";
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
  return `${UPSTREAM_BUCKET}/${release}/buildings.pmtiles`;
}

async function probeLatestRelease(): Promise<string> {
  // S3 ListBucketV2 with delimiter=/ returns a CommonPrefixes entry for
  // every release directory at the bucket root. They sort lexically into
  // chronological order (yyyy-mm-dd...).
  const listUrl = `${UPSTREAM_BUCKET}/?list-type=2&delimiter=%2F`;
  const r = await fetch(listUrl, {
    cf: { cacheTtl: 3600, cacheEverything: true },
  } as RequestInit);
  if (!r.ok) {
    throw new Error(`overture ListBucket failed: ${r.status}`);
  }
  const xml = await r.text();
  const releases: string[] = [];
  const re = /<Prefix>([^<]+?)\/<\/Prefix>/g;
  for (const m of xml.matchAll(re)) {
    const p = m[1] ?? "";
    // Releases are yyyy-mm-dd (with optional `-beta` suffix for the very
    // first one). Anything that doesn't start with a 4-digit year is some
    // other kind of prefix and should be ignored.
    if (/^\d{4}-\d{2}-\d{2}/.test(p)) releases.push(p);
  }
  if (releases.length === 0) {
    throw new Error("no Overture releases found in bucket listing");
  }
  // Lexical sort = chronological order for these prefixes; -beta sorts
  // first under 2024-06-13 vs 2024-06-13-beta which is what we want
  // (prefer the non-beta form when both exist on the same day).
  releases.sort();
  // Filter out any beta-only entries that share a date with a stable one.
  const dateOf = (r: string) => r.slice(0, 10);
  const latest = releases[releases.length - 1] ?? "";
  const latestDate = dateOf(latest);
  // Prefer the non-beta release for the latest date if both exist.
  for (let i = releases.length - 1; i >= 0; i--) {
    const r = releases[i] ?? "";
    if (dateOf(r) !== latestDate) break;
    if (!r.endsWith("-beta")) return r;
  }
  return latest;
}
