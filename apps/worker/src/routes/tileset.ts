import type { Context } from "hono";
import type { Env } from "../env";
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
//   /:impl/sub/1/{x}/{y}/tileset.json
//      ↓ … recurse until z = LEAF_PARENT_Z (= MIN_Z - 1)
//   /:impl/sub/{LEAF_PARENT_Z}/{x}/{y}/tileset.json
//      → inline: 4 z=MIN_Z children + their z=MAX_Z grandchildren
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

const COPYRIGHT = "© OpenStreetMap contributors";

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

/**
 * Unversioned root tileset. Tiny, refreshes hourly so a new IMPL_VERSION
 * deploy propagates without waiting on the immutable child tilesets.
 */
export const tilesetJson = (_c: Context<{ Bindings: Env }>) => {
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
  return jsonResponse(body, "public, max-age=3600", `"${IMPL_VERSION}-root"`);
};

/**
 * Versioned sub-tileset at (z, x, y). Immutable for a given IMPL_VERSION
 * because its content depends only on the tile coord and the renderer
 * convention — bumping IMPL_VERSION moves it to a new URL.
 */
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
    // Pure navigation node: 4 quad children, each in its own tileset shard.
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
    // z == LEAF_PARENT_Z: enumerate concrete glb leaves.
    root.children = leafChildren(x, y);
  }

  const body = {
    asset: { version: "1.1", copyright: COPYRIGHT },
    geometricError: geometricErrorFor(z) * 2,
    root,
  };
  return jsonResponse(
    body,
    "public, max-age=2592000, immutable",
    `"${IMPL_VERSION}-sub-${z}-${x}-${y}"`,
  );
};

/**
 * Inline subtree for a (LEAF_PARENT_Z, X, Y) cell: 4 z=MIN_Z children,
 * each carrying 4 z=MAX_Z grandchildren. The whole cluster gets emitted
 * in a single response so Cesium doesn't need yet another round trip
 * before it can start streaming glbs.
 */
function leafChildren(parentX: number, parentY: number): Tile[] {
  const minZ = MIN_Z;
  const maxZ = MAX_Z;
  const out: Tile[] = [];
  for (let dx = 0; dx < 2; dx++) {
    for (let dy = 0; dy < 2; dy++) {
      const mx = 2 * parentX + dx;
      const my = 2 * parentY + dy;
      const grandchildren: Tile[] = [];
      for (let ex = 0; ex < 2; ex++) {
        for (let ey = 0; ey < 2; ey++) {
          const lx = 2 * mx + ex;
          const ly = 2 * my + ey;
          grandchildren.push({
            boundingVolume: { region: tileRegion(maxZ, lx, ly, HEIGHT_MIN_M, HEIGHT_MAX_M) },
            geometricError: geometricErrorFor(maxZ),
            content: { uri: `/${IMPL_VERSION}/${maxZ}/${lx}/${ly}.glb` },
          });
        }
      }
      out.push({
        boundingVolume: { region: tileRegion(minZ, mx, my, HEIGHT_MIN_M, HEIGHT_MAX_M) },
        geometricError: geometricErrorFor(minZ),
        refine: refineFor(minZ),
        content: { uri: `/${IMPL_VERSION}/${minZ}/${mx}/${my}.glb` },
        children: grandchildren,
      });
    }
  }
  return out;
}
