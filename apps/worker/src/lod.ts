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
