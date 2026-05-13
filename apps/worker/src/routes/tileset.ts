import type { Context } from "hono";
import type { Env } from "../env";
import { buildVersionKey } from "../version";

// Explicit tileset for the M3 thin slice. Implicit tiling (with .subtree
// binary files) is deferred; until then we emit one child per z=14 tile
// inside the served bbox. Default bbox = Tokyo central wards.
//
// Override via query params: ?w=...&s=...&e=...&n=...

const Z = 14;
const HEIGHT_MIN = 0;
const HEIGHT_MAX = 200;
const GE_LEAF = 0;
const GE_ROOT = 200;

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

export const tilesetJson = (c: Context<{ Bindings: Env }>) => {
  const q = c.req.query();
  const bbox = {
    w: q.w ? Number(q.w) : DEFAULT_BBOX.w,
    s: q.s ? Number(q.s) : DEFAULT_BBOX.s,
    e: q.e ? Number(q.e) : DEFAULT_BBOX.e,
    n: q.n ? Number(q.n) : DEFAULT_BBOX.n,
  };

  const version = buildVersionKey(c.env);

  const x0 = lonToX(bbox.w, Z);
  const x1 = lonToX(bbox.e, Z);
  const yTop = latToY(bbox.n, Z);
  const yBot = latToY(bbox.s, Z);

  const children: unknown[] = [];
  for (let x = x0; x <= x1; x++) {
    for (let y = yTop; y <= yBot; y++) {
      const w = xToLon(x, Z);
      const e = xToLon(x + 1, Z);
      const n = yToLat(y, Z);
      const s = yToLat(y + 1, Z);
      children.push({
        boundingVolume: {
          region: [toRad(w), toRad(s), toRad(e), toRad(n), HEIGHT_MIN, HEIGHT_MAX],
        },
        geometricError: GE_LEAF,
        content: { uri: `/${version}/${Z}/${x}/${y}.glb` },
      });
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
      refine: "REPLACE",
      children,
    },
  });

  return new Response(body, {
    headers: {
      "content-type": "application/json",
      "cache-control": "public, max-age=3600",
      etag: `"${version}-${bbox.w},${bbox.s},${bbox.e},${bbox.n}"`,
      "access-control-allow-origin": "*",
    },
  });
};
