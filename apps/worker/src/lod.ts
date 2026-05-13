// LOD configuration shared between the glb route and tileset.json.
//
// We use refine: ADD between z=13 and z=14 so geometry never duplicates:
//   z=13 tile contains only buildings with footprint ≥ THRESHOLD_M2.
//   z=14 tile contains only buildings with footprint <  THRESHOLD_M2.
// Cesium leaves the parent visible and additively streams in the
// children, ending up with the full set at full zoom.

export const MIN_Z = 13;
export const MAX_Z = 14;
// Tuning knob: footprint cutoff between z=13 and z=14. Buildings ≥ this go
// to z=13 (landmark layer), the rest to z=14. 2000 m² (~45m × 45m) keeps
// the z=13 size manageable in dense downtown areas while still surfacing
// most named landmarks.
export const THRESHOLD_M2 = 2000;
