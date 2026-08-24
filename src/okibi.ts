// Tile-demand events, so something outside this worker can warm what people
// actually ask for.
//
// A glb is Overture footprints, a terrain lookup for ground height, and mesh
// generation — seconds of the 30s CPU budget this worker is configured for.
// A renderer bump moves IMPL_VERSION, which is the whole URL space, so every
// tile is cold at once and every first visitor pays a whole render. okibi
// decides which of them to regenerate early, from a record of what was asked
// for. See https://github.com/reearth/okibi, spec/tile-demand.md.
//
// An Overture release is not that kind of change. The cache key is a content
// hash of the MVT inputs, so a release only orphans the tiles whose bytes
// actually moved and the rest stay warm — which is why nothing here reports a
// source epoch: it is not in the key, and an epoch that is not in the key is a
// string okibi could never match an invalidation against.
//
// Nothing in this file may fail a tile response.

import { quadkeyForTile } from "@reearth/okibi";
import { type TileDemand, createWriter, originOf } from "@reearth/okibi/writer";

import epochs from "../okibi.epochs.json";
import type { Env } from "./env";
import { IMPL_VERSION } from "./version";

/**
 * This worker serves one tileset.
 *
 * Named here rather than derived from a request, because it is a fact about
 * the service and not about the tile: every glb it serves is Overture's
 * global buildings, and a second tileset would be a second name in the
 * vocabulary rather than a second value of this one.
 */
const TILESET = "overture-global";

export interface Measured {
  cacheStatus: "hit" | "miss";
  genMs: number;
  bytes: number;
}

/**
 * Write one event for a glb tile, if the binding is there.
 *
 * Optional so that a deployment without Analytics Engine — a preview, a fork,
 * `wrangler dev` — serves tiles exactly as before.
 */
export function writeTileDemand(
  env: Env,
  request: Request,
  coords: { z: number; x: number; y: number },
  measured: Measured,
): void {
  write(env, request, {
    tileset: TILESET,
    kind: "content",
    // The URL after the version segment, which is what warming fetches.
    id: `${coords.z}/${coords.x}/${coords.y}.glb`,
    // Web Mercator, subdivided. The zoom is a size bucket rather than a
    // resolution — z13 means "this much geometry", not "this much ground" —
    // which is why the manifest declares `zoom_semantics: size_bucket` and
    // okibi does not warm a tile's ancestors here.
    qk: quadkeyForTile("web-mercator", coords.z, coords.x, coords.y),
    cacheStatus: measured.cacheStatus,
    // The one part of the cache key that is not this tile's own content, and
    // the one thing whose change costs a whole re-render of everything.
    epoch: { algo: IMPL_VERSION },
    fmt: "glb",
    origin: originOf(request, env.OKIBI_WARM_SECRET),
    genMs: measured.genMs,
    bytes: measured.bytes,
    z: coords.z,
  });
}

/** The same, for a document with no coordinates. */
export function writeMetaDemand(env: Env, request: Request, id: string, measured: Measured): void {
  write(env, request, {
    tileset: TILESET,
    kind: "tileset",
    id,
    cacheStatus: measured.cacheStatus,
    epoch: { algo: IMPL_VERSION },
    fmt: "json",
    origin: originOf(request, env.OKIBI_WARM_SECRET),
    genMs: measured.genMs,
    bytes: measured.bytes,
  });
}

function write(env: Env, request: Request, demand: TileDemand): void {
  if (!env.TILE_DEMAND) return;

  try {
    createWriter({
      dataset: env.TILE_DEMAND,
      epochs,
      onError: (error) => console.warn("okibi:", error),
    }).write(demand);
  } catch (error) {
    // Projection refuses a tile off its grid, which is a bug in a caller
    // rather than a reason to fail the response.
    console.warn("okibi:", error);
  }
}
