//! Ground-truth building model, shared across truth sources.
//!
//! Both PLATEAU (LOD1, Japan) and 3D BAG (Netherlands) reduce to the same
//! minimal record — a centroid and a height-above-ground in metres — so the
//! matcher and metrics stay source-agnostic.

use buildings_core::coord::LonLat;

/// One truth building: its plan-view centroid plus the measured
/// height-above-ground in metres. For PLATEAU this is `bldg:measuredHeight`;
/// for 3D BAG it is `b3_h_dak_50p − b3_h_maaiveld` (both metres above NAP).
#[derive(Debug, Clone)]
pub struct Building {
    pub centroid: LonLat,
    pub measured_height_m: f32,
}
