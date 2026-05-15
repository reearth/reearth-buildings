//! Pair PLATEAU truth buildings to Overture estimates by point-in-polygon.
//!
//! Each PLATEAU building has a centroid (lon/lat); each Overture
//! building has one or more outer rings (also lon/lat). We pick the
//! first Overture polygon whose AABB contains the PLATEAU centroid AND
//! whose outer ring contains it.

use crate::fetch_plateau::Building as PlateauBuilding;
use buildings_core::{coord::LonLat, ExtractedBuilding};
use rstar::{primitives::GeomWithData, primitives::Rectangle, RTree, RTreeObject, AABB};

/// Wraps a polygon's bounding box; the `usize` payload is the index
/// into the `estimates` slice.
type Leaf = GeomWithData<Rectangle<[f64; 2]>, usize>;

pub struct Pair<'a> {
    pub truth: &'a PlateauBuilding,
    pub estimate: &'a ExtractedBuilding,
    pub residual_m: f32,
}

pub struct MatchResult<'a> {
    pub pairs: Vec<Pair<'a>>,
    pub unmatched_truth: usize,
}

pub fn match_buildings<'a>(
    truths: &'a [PlateauBuilding],
    estimates: &'a [ExtractedBuilding],
) -> MatchResult<'a> {
    // Build R-tree over Overture polygon AABBs. Each leaf carries the
    // estimate index — we resolve the precise polygon-contains test on
    // candidates only.
    let mut leaves: Vec<Leaf> = Vec::new();
    for (i, est) in estimates.iter().enumerate() {
        for ring in &est.outer_rings_lonlat {
            if let Some(rect) = ring_rect(ring) {
                leaves.push(GeomWithData::new(rect, i));
            }
        }
    }
    let tree: RTree<Leaf> = RTree::bulk_load(leaves);

    let mut pairs: Vec<Pair<'a>> = Vec::with_capacity(truths.len());
    let mut unmatched = 0usize;
    for t in truths {
        let p = [t.centroid.lon_deg, t.centroid.lat_deg];
        let pt = AABB::from_point(p);
        let mut hit: Option<usize> = None;
        for cand in tree.locate_in_envelope_intersecting(&pt) {
            let est_idx = cand.data;
            let est = &estimates[est_idx];
            if est
                .outer_rings_lonlat
                .iter()
                .any(|ring| ring_contains(ring, t.centroid))
            {
                hit = Some(est_idx);
                break;
            }
        }
        match hit {
            Some(i) => {
                let est = &estimates[i];
                pairs.push(Pair {
                    truth: t,
                    estimate: est,
                    residual_m: est.height_m - t.measured_height_m,
                });
            }
            None => unmatched += 1,
        }
    }
    MatchResult {
        pairs,
        unmatched_truth: unmatched,
    }
}

fn ring_rect(ring: &[LonLat]) -> Option<Rectangle<[f64; 2]>> {
    if ring.is_empty() {
        return None;
    }
    let mut min = [f64::INFINITY; 2];
    let mut max = [f64::NEG_INFINITY; 2];
    for p in ring {
        if p.lon_deg < min[0] {
            min[0] = p.lon_deg;
        }
        if p.lat_deg < min[1] {
            min[1] = p.lat_deg;
        }
        if p.lon_deg > max[0] {
            max[0] = p.lon_deg;
        }
        if p.lat_deg > max[1] {
            max[1] = p.lat_deg;
        }
    }
    Some(Rectangle::from_corners(min, max))
}

// Silence unused-import warnings on RTreeObject (imported for trait
// resolution on Rectangle).
#[allow(dead_code)]
fn _rtree_obj_marker(_r: &Rectangle<[f64; 2]>) {
    fn assert_obj<T: RTreeObject>() {}
    assert_obj::<Rectangle<[f64; 2]>>();
}

/// Standard ray-casting point-in-polygon test on a closed ring.
fn ring_contains(ring: &[LonLat], p: LonLat) -> bool {
    if ring.len() < 3 {
        return false;
    }
    let (px, py) = (p.lon_deg, p.lat_deg);
    let mut inside = false;
    let n = ring.len();
    let mut j = n - 1;
    for i in 0..n {
        let xi = ring[i].lon_deg;
        let yi = ring[i].lat_deg;
        let xj = ring[j].lon_deg;
        let yj = ring[j].lat_deg;
        let intersects =
            ((yi > py) != (yj > py)) && (px < (xj - xi) * (py - yi) / (yj - yi + 1e-30) + xi);
        if intersects {
            inside = !inside;
        }
        j = i;
    }
    inside
}
