import type { Env } from "./env";
import { LOD_MODE } from "./lod";

/**
 * Bump when the renderer / glb schema changes. Lives in the URL prefix so
 * a bump alone invalidates every cache layer atomically.
 *
 * Daily upstream updates are *not* propagated through this constant.
 * Instead, the glb route uses a content hash of the MVT bytes for the
 * ETag and R2 cache key — unchanged tiles stay deduplicated across
 * pmtiles builds, so only tiles whose source content actually changed
 * get re-rendered.
 *
 * The LOD mode is appended too: switching ADD ↔ REPLACE changes the
 * meaning of the geometry at every zoom, so a flip must invalidate the
 * whole URL space.
 */
// v2: introduced z=12 mega-landmark layer, restructured leaf subtree
// (LEAF_PARENT_Z = 11). All earlier URL paths no longer make sense.
const RENDERER_VERSION = "v2";
export const IMPL_VERSION = `${RENDERER_VERSION}-${LOD_MODE}`;

/**
 * Where Protomaps publishes daily planet builds. Each `${YYYYMMDD}.pmtiles`
 * URL is immutable.
 */
const UPSTREAM_BASE = "https://build.protomaps.com";

/** Reasonable upper bound when walking back to find the most recent build. */
const MAX_PROBE_DAYS = 7;

const KV_LATEST_KEY = "pmtiles:latest";
const KV_LATEST_TTL_SECONDS = 3600;

/**
 * Resolve the upstream PMTiles date to fetch from. Always auto-discovers
 * via a KV-cached HEAD probe; for reproducible repros, pin the date by
 * pre-populating the KV namespace under "pmtiles:latest".
 */
export async function currentPmtilesDate(env: Env): Promise<string> {
  const cached = await env.PMTILES_DIR.get(KV_LATEST_KEY);
  if (cached) return cached;
  const fresh = await probeLatestDate();
  await env.PMTILES_DIR.put(KV_LATEST_KEY, fresh, {
    expirationTtl: KV_LATEST_TTL_SECONDS,
  });
  return fresh;
}

/** Build the absolute upstream URL for a specific PMTiles date. */
export function upstreamUrl(date: string): string {
  return `${UPSTREAM_BASE}/${date}.pmtiles`;
}

async function probeLatestDate(): Promise<string> {
  const now = Date.now();
  for (let i = 0; i < MAX_PROBE_DAYS; i++) {
    const ts = now - i * 86_400_000;
    const date = formatUtcDate(new Date(ts));
    const r = await fetch(upstreamUrl(date), {
      method: "HEAD",
      cf: { cacheTtl: 60, cacheEverything: true },
    } as RequestInit);
    if (r.ok) return date;
  }
  throw new Error(
    `no recent PMTiles build found at ${UPSTREAM_BASE} within ${MAX_PROBE_DAYS} days`,
  );
}

function formatUtcDate(d: Date): string {
  const y = d.getUTCFullYear();
  const m = `${d.getUTCMonth() + 1}`.padStart(2, "0");
  const day = `${d.getUTCDate()}`.padStart(2, "0");
  return `${y}${m}${day}`;
}
