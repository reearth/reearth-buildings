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
}
