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
export const RENDERER_VERSION = "v5";
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
 * The last release this isolate resolved. Only a safety net: if the
 * ListBucket call fails while the archives themselves are fine, serving
 * the release we were already serving beats 503-ing the tile over a
 * listing that will probably work on the next request.
 */
let lastKnownRelease: string | null = null;

/**
 * Resolve the upstream Overture release to fetch from. Auto-discovers via
 * the Workers Cache API on top of an S3 ListBucket call.
 */
export async function currentPmtilesDate(): Promise<string> {
  const cache = caches.default;
  const hit = await cache.match(new Request(LATEST_CACHE_URL));
  if (hit) {
    const cached = await hit.text();
    lastKnownRelease = cached;
    return cached;
  }
  return await resolveAndStore(false);
}

/**
 * Re-resolve, ignoring both caches in front of the answer — the stored
 * release *and* the edge-cached listing it came from.
 *
 * Overture keeps a rolling window of releases and deletes what falls out
 * of it, so a release cached here for a day can be one that no longer
 * exists: its ranges 404 and every tile fails until the entry expires.
 * `withRelease` calls this when that happens, which is the only time
 * paying for an uncached listing is worth it.
 */
export async function refreshPmtilesDate(): Promise<string> {
  return await resolveAndStore(true);
}

async function resolveAndStore(bypassCache: boolean): Promise<string> {
  let fresh: string;
  try {
    fresh = await probeLatestRelease(bypassCache);
  } catch (err) {
    if (lastKnownRelease) {
      console.warn("overture release probe failed; keeping last known", {
        release: lastKnownRelease,
        err: String(err),
      });
      return lastKnownRelease;
    }
    throw err;
  }
  lastKnownRelease = fresh;
  await caches.default.put(
    new Request(LATEST_CACHE_URL),
    new Response(fresh, {
      headers: { "Cache-Control": `max-age=${LATEST_CACHE_TTL_SECONDS}` },
    }),
  );
  return fresh;
}

/**
 * Run `fn` against the current release, and if it fails, once more
 * against a freshly resolved one.
 *
 * The retry is for exactly one situation: the release we had cached has
 * rolled out of Overture's bucket, so every byte we ask for is a 404
 * that no amount of retrying the same URL will fix. When the fresh
 * answer is the same release, the failure was something else and the
 * original error stands.
 */
export async function withRelease<T>(fn: (release: string) => Promise<T>): Promise<T> {
  const release = await currentPmtilesDate();
  try {
    return await fn(release);
  } catch (err) {
    let fresh: string;
    try {
      fresh = await refreshPmtilesDate();
    } catch {
      throw err;
    }
    if (fresh === release) throw err;
    console.warn("overture release rolled over; retrying", {
      was: release,
      now: fresh,
      err: String(err),
    });
    return await fn(fresh);
  }
}

/** Build the absolute upstream URL for a specific Overture release. */
export function upstreamUrl(release: string): string {
  return `${UPSTREAM_BUCKET}/${UPSTREAM_PREFIX}${release}/buildings.pmtiles`;
}

async function probeLatestRelease(bypassCache = false): Promise<string> {
  // S3 ListBucketV2 with delimiter=/ under the tiles/ prefix returns a
  // CommonPrefixes entry for every release directory (`tiles/2026-06-17.0/`).
  const listUrl = `${UPSTREAM_BUCKET}/?list-type=2&delimiter=%2F&prefix=${encodeURIComponent(UPSTREAM_PREFIX)}`;
  // The listing is edge-cached for an hour, which is right for the
  // ordinary path and wrong for the recovery one: re-reading a listing
  // that still names a deleted release just repeats the failure.
  const r = await fetchWithRetry(listUrl, {
    cf: bypassCache
      ? { cacheTtl: 0, cacheEverything: false }
      : { cacheTtl: 3600, cacheEverything: true },
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
  // Date first, then the numeric suffix — `2026-08-19.10` is newer than
  // `2026-08-19.2`, which a lexical sort would get backwards.
  releases.sort(compareReleases);
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

/** Order two `yyyy-mm-dd.n` release names, oldest first. `n` may carry a
 *  `-beta` marker (`0-beta`), which parses to its leading number — the
 *  beta preference is applied separately by the caller. */
function compareReleases(a: string, b: string): number {
  const dateA = a.slice(0, 10);
  const dateB = b.slice(0, 10);
  if (dateA !== dateB) return dateA < dateB ? -1 : 1;
  const nOf = (r: string) => Number.parseInt(r.slice(11), 10) || 0;
  return nOf(a) - nOf(b);
}
