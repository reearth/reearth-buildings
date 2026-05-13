import type { KVNamespace, R2Bucket } from "@cloudflare/workers-types";

export interface Env {
  SOURCES: R2Bucket;
  CACHE: R2Bucket;
  STATIC: R2Bucket;
  PMTILES_DIR: KVNamespace;
  PMTILES_VERSION: string;
  PMTILES_SOURCE_URL: string;
}
