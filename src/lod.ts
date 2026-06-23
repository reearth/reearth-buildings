// LOD configuration shared between the glb route and tileset.json.
//
// Three content zooms with size-based bucketing under refine: ADD —
// every building is rendered at exactly one zoom level, and the three
// layers compose without overlap:
//
//   z=12  mega landmarks         (footprint ≥ MEGA_M2)
//   z=13  large buildings        (THRESHOLD_M2 ≤ footprint < MEGA_M2)
//   z=14  everything smaller     (footprint < THRESHOLD_M2)
//
// REPLACE mode renders all buildings at every zoom, simplified more
// aggressively the further out you go, and the deeper zoom replaces
// the coarser one when it streams in.
//
// Toggle modes via LOD_MODE; IMPL_VERSION encodes it so flipping
// invalidates every cache layer atomically.

export type LodMode = "add" | "replace";
export const LOD_MODE: LodMode = "add";

export const MIN_Z = 12;
export const MAX_Z = 14;

/** Recursive break in the navigation quadtree. */
export const LEAF_PARENT_Z = MIN_Z - 1;

/** Vertical envelope declared in tileset bounding regions. */
export const HEIGHT_MIN_M = 0;
export const HEIGHT_MAX_M = 1000;

/**
 * Footprint cutoffs (m²) for the ADD-mode size buckets. Tuned for
 * downtown Tokyo: most buildings fall in the small bucket; large
 * commercial buildings hit the 2 000 m² tier; only true landmark
 * footprints (station complexes, big-box, large factories) reach 5 000.
 */
export const THRESHOLD_M2 = 2000;
export const MEGA_M2 = 10000;

/**
 * Whether the renderer should collapse each polygon to its axis-aligned
 * bounding box at this zoom.
 *
 * Disabled everywhere. The z=12 "block mesh" look used to AABB every mega
 * landmark, but a tile-axis-aligned box fills in courtyards and circumscribes
 * L-shaped / wing / arcade outlines — exactly the footprint shapes that
 * cluster in dense landmark districts (Marunouchi / Tokyo Station), so those
 * buildings rendered noticeably larger than their true footprint at the
 * coarsest LOD. Mega landmarks are few per tile, so drawing the real outline
 * at z=12 is affordable; keep the flag plumbed so a future LOD can re-enable
 * AABB per zoom if needed.
 */
export function aabbOnlyAt(_z: number): boolean {
  return false;
}

export function refineFor(z: number): "ADD" | "REPLACE" {
  if (LOD_MODE === "add") return "ADD";
  // REPLACE: the parent vanishes when any child streams in. Use REPLACE
  // on every intermediate zoom so the chain MIN_Z → … → MAX_Z replaces
  // top-down. Root tiles stay ADD so multiple top-level parents coexist.
  return z >= MIN_Z && z < MAX_Z ? "REPLACE" : "ADD";
}

export function areaFilterFor(z: number): { minM2: number; maxM2: number } {
  if (LOD_MODE === "add") {
    if (z >= MAX_Z) return { minM2: 0, maxM2: THRESHOLD_M2 };
    if (z === MIN_Z) return { minM2: MEGA_M2, maxM2: 0 };
    // Intermediate (z=13 today): [THRESHOLD, MEGA).
    return { minM2: THRESHOLD_M2, maxM2: MEGA_M2 };
  }
  // REPLACE: every building at every level.
  return { minM2: 0, maxM2: 0 };
}

export function simplifyFor(z: number): { ratio: number; targetErrorM: number } {
  if (LOD_MODE === "add") {
    return { ratio: 1, targetErrorM: 0 };
  }
  // REPLACE: heavier decimation the further out you are.
  if (z === MIN_Z) return { ratio: 0.25, targetErrorM: 12 };
  if (z === MIN_Z + 1) return { ratio: 0.5, targetErrorM: 5 };
  return { ratio: 1, targetErrorM: 0 };
}

/** Geometric error (worst-case deviation in metres) for a tile at zoom z. */
export function geometricErrorFor(z: number): number {
  if (z >= MAX_Z) return 0;
  // Tile width at z divided by 30: z=11 ≈ 650 m, z=12 ≈ 330 m,
  // z=13 ≈ 165 m. With the default 16-px SSE budget Cesium subdivides
  // each tier roughly twice as close as the previous one, which keeps
  // the ADD-mode size buckets streaming in at separate distances.
  return 40_075_017 / 2 ** z / 30;
}
