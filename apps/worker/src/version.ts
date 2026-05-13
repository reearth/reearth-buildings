import type { Env } from "./env";

/**
 * Bump when the renderer or schema changes. The value lands in every glb
 * URL so edge caches, browser caches, and the R2 cache shard are all
 * invalidated atomically on deploy.
 *
 * Conventions:
 *   v<N> — renderer / glTF schema bump (this constant).
 *   d<YYYYMMDD> — upstream PMTiles build date (PMTILES_VERSION env var).
 *
 * Combined version key = `${IMPL_VERSION}-d${PMTILES_VERSION}`.
 */
export const IMPL_VERSION = "v1";

export function buildVersionKey(env: Pick<Env, "PMTILES_VERSION">): string {
  return `${IMPL_VERSION}-d${env.PMTILES_VERSION}`;
}
