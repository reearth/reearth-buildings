// LOD configuration shared between the glb route and tileset.json.
//
// Two modes, switched by LOD_MODE:
//
//   "add"     — refine: ADD. z=13 holds only large buildings, z=14 only
//               small ones. The two layers compose at full zoom without
//               overlap. Best when you want every building rendered at
//               its original geometry detail.
//
//   "replace" — refine: REPLACE. z=13 holds every building, simplified
//               for distance. z=14 holds every building at full detail
//               and replaces the parent when loaded. Best when you want
//               the parent layer to be light at the cost of redundant
//               data on the wire.
//
// Switching modes silently would mix tileset.json contents (refine value)
// with the corresponding glb payloads (different filter / simplify). To
// keep caches honest we suffix IMPL_VERSION with the mode (see
// version.ts) — flipping LOD_MODE atomically invalidates every layer.

export type LodMode = "add" | "replace";
export const LOD_MODE: LodMode = "add";

export const MIN_Z = 13;
export const MAX_Z = 14;

/**
 * Recursion break in the quadtree of external sub-tilesets. Sub-tilesets
 * at z < LEAF_PARENT_Z list four external children at z+1; sub-tilesets
 * at z = LEAF_PARENT_Z enumerate the concrete z=MIN_Z and z=MAX_Z glb
 * tiles inline. Set to MIN_Z - 1 so the deepest navigation level sits
 * one above the real glb leaves.
 */
export const LEAF_PARENT_Z = MIN_Z - 1;

/**
 * Vertical extent declared in every tile's bounding region. We err on the
 * generous side (1 000 m) so even the tallest extrusions stay inside the
 * Cesium-side culling volume — actual roof heights come from the OSM
 * `height` tag and rarely exceed 350 m, but a 200 m clamp was tight
 * enough that downtown landmark tiles got clipped during frustum tests.
 */
export const HEIGHT_MIN_M = 0;
export const HEIGHT_MAX_M = 1000;

/** Geometric error (worst-case deviation in metres) for a tile at zoom z. */
export function geometricErrorFor(z: number): number {
  if (z >= MAX_Z) return 0;
  // 40 075 017 m ≈ Earth circumference at the equator → tile width at z.
  // Dividing further by 100 produces values matching common Cesium defaults
  // (root ~400 km, z=12 ~100 m, z=13 ~50 m).
  return 40_075_017 / 2 ** z / 100;
}

/** Footprint cutoff between z=13 and z=14 in ADD mode. Ignored in REPLACE. */
export const THRESHOLD_M2 = 2000;

/** meshopt simplification applied at z=13 in REPLACE mode. */
export const SIMPLIFY = {
  ratio: 0.5,
  targetErrorM: 5,
};

export function refineFor(z: number): "ADD" | "REPLACE" {
  if (LOD_MODE === "add") return "ADD";
  // In REPLACE mode the parent is hidden when children load; root uses
  // ADD so multiple top-level parents can coexist.
  return z === MIN_Z ? "REPLACE" : "ADD";
}

export function areaFilterFor(z: number): { minM2: number; maxM2: number } {
  if (LOD_MODE === "add") {
    return z === MAX_Z ? { minM2: 0, maxM2: THRESHOLD_M2 } : { minM2: THRESHOLD_M2, maxM2: 0 };
  }
  // REPLACE: every building at every level.
  return { minM2: 0, maxM2: 0 };
}

export function simplifyFor(z: number): { ratio: number; targetErrorM: number } {
  if (LOD_MODE === "replace" && z === MIN_Z) {
    return { ratio: SIMPLIFY.ratio, targetErrorM: SIMPLIFY.targetErrorM };
  }
  return { ratio: 1, targetErrorM: 0 };
}
