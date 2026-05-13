import type { KVNamespace, R2Bucket } from "@cloudflare/workers-types";

export interface Env {
  SOURCES: R2Bucket;
  CACHE: R2Bucket;
  PMTILES_DIR: KVNamespace;
  /**
   * Optional explicit pin (e.g. "20260513"). When set, overrides upstream
   * probing — useful for dev / staging snapshots and reproducible repros.
   * When absent, the worker resolves the latest available date via KV
   * (see `latestPmtilesDate` in version.ts).
   */
  PMTILES_VERSION?: string;
  /**
   * Set to "1" (or any truthy string) in dev to bypass the R2 cache and
   * issue `Cache-Control: no-store` on every response. KV caches for
   * upstream date / geoid stay populated since they don't affect
   * render-iteration feedback. See `cacheDisabled()` below.
   */
  CACHE_DISABLED?: string;
}

/** True when the worker should bypass its own caches and tell clients
 *  not to cache responses. */
export function cacheDisabled(env: Env): boolean {
  const v = env.CACHE_DISABLED;
  return v != null && v !== "" && v !== "0" && v.toLowerCase() !== "false";
}
