import type { Context } from "hono";
import type { Env } from "../env";
import { MAX_Z, MIN_Z, refineFor } from "../lod";
import { IMPL_VERSION } from "../version";

// Two-level explicit tileset:
//   root → z=MIN_Z parents (large buildings only)
//        → z=MAX_Z children (small buildings, additive)
//
// `refine: ADD` keeps parents visible as children stream in, so the
// large-vs-small split between zooms composes into the full set.

const HEIGHT_MIN = 0;
const HEIGHT_MAX = 200;
const GE_ROOT = 500;
const GE_PARENT = 100;
const GE_LEAF = 0;

const DEFAULT_BBOX = { w: 139.65, s: 35.62, e: 139.85, n: 35.78 };

const toRad = (d: number) => (d * Math.PI) / 180;
const lonToX = (lon: number, z: number) => Math.floor(((lon + 180) / 360) * 2 ** z);
const latToY = (lat: number, z: number) => {
  const r = (lat * Math.PI) / 180;
  return Math.floor(((1 - Math.log(Math.tan(r) + 1 / Math.cos(r)) / Math.PI) / 2) * 2 ** z);
};
const xToLon = (x: number, z: number) => (x / 2 ** z) * 360 - 180;
const yToLat = (y: number, z: number) => {
  const n = Math.PI - (2 * Math.PI * y) / 2 ** z;
  return (180 / Math.PI) * Math.atan((Math.exp(n) - Math.exp(-n)) / 2);
};

interface Tile {
  boundingVolume: { region: number[] };
  geometricError: number;
  content?: { uri: string };
  refine?: "ADD" | "REPLACE";
  children?: Tile[];
}

function tileNode(z: number, x: number, y: number, ge: number): Tile {
  const w = xToLon(x, z);
  const e = xToLon(x + 1, z);
  const n = yToLat(y, z);
  const s = yToLat(y + 1, z);
  return {
    boundingVolume: {
      region: [toRad(w), toRad(s), toRad(e), toRad(n), HEIGHT_MIN, HEIGHT_MAX],
    },
    geometricError: ge,
    content: { uri: `/${IMPL_VERSION}/${z}/${x}/${y}.glb` },
  };
}

export const tilesetJson = (c: Context<{ Bindings: Env }>) => {
  const q = c.req.query();
  const bbox = {
    w: q.w ? Number(q.w) : DEFAULT_BBOX.w,
    s: q.s ? Number(q.s) : DEFAULT_BBOX.s,
    e: q.e ? Number(q.e) : DEFAULT_BBOX.e,
    n: q.n ? Number(q.n) : DEFAULT_BBOX.n,
  };

  const px0 = lonToX(bbox.w, MIN_Z);
  const px1 = lonToX(bbox.e, MIN_Z);
  const pyTop = latToY(bbox.n, MIN_Z);
  const pyBot = latToY(bbox.s, MIN_Z);

  const parents: Tile[] = [];
  const factor = 2 ** (MAX_Z - MIN_Z);
  for (let px = px0; px <= px1; px++) {
    for (let py = pyTop; py <= pyBot; py++) {
      const parent = tileNode(MIN_Z, px, py, GE_PARENT);
      // ADD: children stream in on top of parent.
      // REPLACE: parent is hidden once any child is loaded.
      parent.refine = refineFor(MIN_Z);
      const children: Tile[] = [];
      for (let dx = 0; dx < factor; dx++) {
        for (let dy = 0; dy < factor; dy++) {
          children.push(tileNode(MAX_Z, px * factor + dx, py * factor + dy, GE_LEAF));
        }
      }
      parent.children = children;
      parents.push(parent);
    }
  }

  const body = JSON.stringify({
    asset: { version: "1.1", copyright: "© OpenStreetMap contributors" },
    geometricError: GE_ROOT,
    root: {
      boundingVolume: {
        region: [
          toRad(bbox.w),
          toRad(bbox.s),
          toRad(bbox.e),
          toRad(bbox.n),
          HEIGHT_MIN,
          HEIGHT_MAX,
        ],
      },
      geometricError: GE_ROOT,
      // Root is always ADD: multiple top-level parents must coexist.
      refine: "ADD",
      children: parents,
    },
  });

  return new Response(body, {
    headers: {
      "content-type": "application/json",
      "cache-control": "public, max-age=3600",
      etag: `"${IMPL_VERSION}-${bbox.w},${bbox.s},${bbox.e},${bbox.n}"`,
      "access-control-allow-origin": "*",
    },
  });
};
