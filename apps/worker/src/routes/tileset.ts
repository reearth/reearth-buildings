import type { Context } from "hono";
import { type Env, cacheDisabled } from "../env";
import {
  HEIGHT_MAX_M,
  HEIGHT_MIN_M,
  LEAF_PARENT_Z,
  MAX_Z,
  MIN_Z,
  geometricErrorFor,
  refineFor,
} from "../lod";
import { tileRegion, toRad } from "../tile";
import { IMPL_VERSION } from "../version";

// World coverage via a lazy quadtree of external tilesets:
//
//   /tileset.json (root, world bbox, unversioned)
//      ↓ single external child
//   /:impl/sub/0/0/0/tileset.json (covers world)
//      ↓ 4 external children at z=1
//   /:impl/sub/{Z}/{X}/{Y}/tileset.json
//      ↓ … recurse until z = LEAF_PARENT_Z (= MIN_Z - 1)
//   /:impl/sub/{LEAF_PARENT_Z}/{X}/{Y}/tileset.json
//      → inline: z=MIN_Z..MAX_Z content tiles, refined via the LOD_MODE.
//
// Cesium fetches sub-tilesets lazily based on view, so a streetside view
// pays for at most LEAF_PARENT_Z + 1 sequential tileset fetches; an edge
// cache hit makes that ~ms each.

const WORLD_REGION = [
  toRad(-180),
  toRad(-85.0511287798),
  toRad(180),
  toRad(85.0511287798),
  HEIGHT_MIN_M,
  HEIGHT_MAX_M,
];

const COPYRIGHT = "© OpenStreetMap contributors (ODbL) · tiles by Protomaps";

interface Tile {
  boundingVolume: { region: number[] };
  geometricError: number;
  refine?: "ADD" | "REPLACE";
  content?: { uri: string };
  children?: Tile[];
}

const jsonResponse = (body: unknown, cacheControl: string, etag: string) =>
  new Response(JSON.stringify(body), {
    headers: {
      "content-type": "application/json",
      "cache-control": cacheControl,
      etag,
      "access-control-allow-origin": "*",
    },
  });

/** Unversioned root tileset. */
export const tilesetJson = (c: Context<{ Bindings: Env }>) => {
  const noCache = cacheDisabled(c.env);
  const etag = `"${IMPL_VERSION}-root"`;
  const cc = noCache ? "no-store" : "public, max-age=60, must-revalidate";
  if (!noCache && c.req.header("if-none-match") === etag) {
    return new Response(null, {
      status: 304,
      headers: { etag, "cache-control": cc },
    });
  }
  const body = {
    asset: { version: "1.1", copyright: COPYRIGHT },
    geometricError: geometricErrorFor(0) * 2,
    root: {
      boundingVolume: { region: WORLD_REGION },
      geometricError: geometricErrorFor(0),
      refine: "ADD",
      content: { uri: `/${IMPL_VERSION}/sub/0/0/0/tileset.json` },
    },
  };
  // Short freshness + must-revalidate: clients still hit the worker
  // every minute or so, but ETag means an unchanged IMPL_VERSION costs
  // them only a 304. When IMPL_VERSION moves the URL contents change,
  // the ETag changes, and clients pick up the new sub-tileset prefix
  // without needing a manual hard reload.
  return jsonResponse(body, cc, etag);
};

/** Versioned sub-tileset at (z, x, y). */
export const subTilesetJson = (c: Context<{ Bindings: Env }>) => {
  if (c.req.param("impl") !== IMPL_VERSION) {
    return c.text("unknown impl version — re-fetch /tileset.json", 410);
  }
  const z = Number(c.req.param("z"));
  const x = Number(c.req.param("x"));
  const y = Number(c.req.param("y"));
  if (!Number.isFinite(z) || !Number.isFinite(x) || !Number.isFinite(y)) {
    return c.text("bad tile coord", 400);
  }
  if (z < 0 || z > LEAF_PARENT_Z) {
    return c.text(`only z=0..${LEAF_PARENT_Z} is served`, 404);
  }

  const root: Tile = {
    boundingVolume: { region: tileRegion(z, x, y, HEIGHT_MIN_M, HEIGHT_MAX_M) },
    geometricError: geometricErrorFor(z),
    refine: "ADD",
  };

  if (z < LEAF_PARENT_Z) {
    // Pure navigation: 4 quad children, each in its own external shard.
    root.children = [
      [2 * x, 2 * y],
      [2 * x + 1, 2 * y],
      [2 * x, 2 * y + 1],
      [2 * x + 1, 2 * y + 1],
    ].map(([cx, cy]) => ({
      boundingVolume: { region: tileRegion(z + 1, cx!, cy!, HEIGHT_MIN_M, HEIGHT_MAX_M) },
      geometricError: geometricErrorFor(z + 1),
      content: { uri: `/${IMPL_VERSION}/sub/${z + 1}/${cx}/${cy}/tileset.json` },
    }));
  } else {
    // z == LEAF_PARENT_Z: expand the MIN_Z..MAX_Z subtree inline.
    root.children = [
      [2 * x, 2 * y],
      [2 * x + 1, 2 * y],
      [2 * x, 2 * y + 1],
      [2 * x + 1, 2 * y + 1],
    ].map(([cx, cy]) => buildContentSubtree(MIN_Z, cx!, cy!));
  }

  const body = {
    asset: { version: "1.1", copyright: COPYRIGHT },
    geometricError: geometricErrorFor(z) * 2,
    root,
  };
  return jsonResponse(
    body,
    cacheDisabled(c.env) ? "no-store" : "public, max-age=2592000, immutable",
    `"${IMPL_VERSION}-sub-${z}-${x}-${y}"`,
  );
};

/**
 * Inline tile node at zoom `z` covering (x, y). Recurses through every
 * content zoom from MIN_Z down to MAX_Z so a single LEAF_PARENT_Z tileset
 * carries the whole content subtree it needs.
 */
function buildContentSubtree(z: number, x: number, y: number): Tile {
  const tile: Tile = {
    boundingVolume: { region: tileRegion(z, x, y, HEIGHT_MIN_M, HEIGHT_MAX_M) },
    geometricError: geometricErrorFor(z),
    content: { uri: `/${IMPL_VERSION}/${z}/${x}/${y}.glb` },
  };
  if (z >= MAX_Z) return tile; // leaf — no children
  tile.refine = refineFor(z);
  tile.children = [
    [2 * x, 2 * y],
    [2 * x + 1, 2 * y],
    [2 * x, 2 * y + 1],
    [2 * x + 1, 2 * y + 1],
  ].map(([cx, cy]) => buildContentSubtree(z + 1, cx!, cy!));
  return tile;
}
