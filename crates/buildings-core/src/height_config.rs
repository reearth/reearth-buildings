//! Tunable parameters for the height-estimation cascade.
//!
//! Extracted from `mesh.rs` so the height-optimizer crate can sweep
//! parameters and measure error against PLATEAU LOD1 truth without
//! forking the production code.
//!
//! [`HeightConfig::default`] reproduces the legacy hard-coded values
//! exactly, so callers that don't care about tuning can continue to
//! ignore this module.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Sentinel "catch-all" upper bound for the last [`FootprintBucket`] in
/// each [`FootprintCurve`]. f32::INFINITY would be cleaner but TOML
/// rejects non-finite floats; 1e9 m² is comfortably larger than any
/// real building footprint while staying serialisable.
pub const SENTINEL_OPEN: f32 = 1.0e9;

/// Top-level config for [`crate::mesh::default_height_meters`] and
/// [`crate::mesh::classify_urban`]. Cheap to clone (a few hundred bytes
/// of strings); pass by reference into the mesh builder.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeightConfig {
    /// Multiplier applied to Overture `num_floors` when no explicit
    /// height is present. Default 3.0 m/floor.
    pub meters_per_floor: f64,
    /// Class → height (metres). Lookup key is the Overture `class` enum.
    pub class_height_m: HashMap<String, f64>,
    /// Subtype → height (metres). Coarser fallback after class.
    pub subtype_height_m: HashMap<String, f64>,
    /// Density classifier thresholds (avg buildings per source MVT).
    pub urban_thresholds: UrbanThresholds,
    /// Footprint heuristic table, applied per [`UrbanLevel`].
    pub footprint: FootprintTable,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct UrbanThresholds {
    pub dense_urban_min: f32,
    pub urban_min: f32,
    pub suburban_min: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FootprintTable {
    pub dense_urban: FootprintCurve,
    pub urban: FootprintCurve,
    pub suburban: FootprintCurve,
    pub rural: FootprintCurve,
}

/// Piecewise-constant `area_m2 → height_m` function. Buckets must be
/// sorted ascending by `max_area_m2` (exclusive upper bound). The last
/// entry should set `max_area_m2 = f32::INFINITY` to act as the
/// catch-all.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FootprintCurve {
    pub buckets: Vec<FootprintBucket>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct FootprintBucket {
    pub max_area_m2: f32,
    pub height_m: f64,
}

impl FootprintCurve {
    /// Lookup the bucket whose `max_area_m2` first exceeds `area`. Falls
    /// back to the last bucket if none match (covers the empty-curve
    /// degenerate case with a safe small height).
    pub fn lookup(&self, area_m2: f32) -> f64 {
        for b in &self.buckets {
            if area_m2 < b.max_area_m2 {
                return b.height_m;
            }
        }
        self.buckets.last().map(|b| b.height_m).unwrap_or(3.0)
    }
}

impl Default for HeightConfig {
    fn default() -> Self {
        Self {
            // 3.3 m/floor: chosen to fit Overture commercial buildings
            // without blowing up residential. Residential walk-ups are
            // ~2.7-3.0 m, commercial offices ~3.5-4.0 m; 3.3 is the
            // compromise the height-optimizer settled on across 5 cities.
            meters_per_floor: 3.3,
            class_height_m: default_class_table(),
            subtype_height_m: default_subtype_table(),
            urban_thresholds: UrbanThresholds {
                dense_urban_min: 1500.0,
                urban_min: 800.0,
                suburban_min: 200.0,
            },
            footprint: FootprintTable {
                dense_urban: FootprintCurve {
                    buckets: vec![
                        FootprintBucket { max_area_m2: 60.0, height_m: 12.0 },
                        FootprintBucket { max_area_m2: 200.0, height_m: 15.0 },
                        FootprintBucket { max_area_m2: 800.0, height_m: 18.0 },
                        // Catch-all bumped 15 → 24: large CBD lots
                        // (>800 m²) are typically full-block office or
                        // retail towers, not "big-box at street level"
                        // as the original 15 m comment assumed. Iiyama
                        // / Tsukuba town centres also classify
                        // DenseUrban but rarely have features in this
                        // bucket, so the bump targets actual CBDs.
                        FootprintBucket { max_area_m2: SENTINEL_OPEN, height_m: 24.0 },
                    ],
                },
                urban: FootprintCurve {
                    buckets: vec![
                        FootprintBucket { max_area_m2: 60.0, height_m: 9.0 },
                        FootprintBucket { max_area_m2: 200.0, height_m: 9.0 },
                        FootprintBucket { max_area_m2: 800.0, height_m: 12.0 },
                        FootprintBucket { max_area_m2: SENTINEL_OPEN, height_m: 12.0 },
                    ],
                },
                suburban: FootprintCurve {
                    buckets: vec![
                        FootprintBucket { max_area_m2: 60.0, height_m: 6.0 },
                        FootprintBucket { max_area_m2: 200.0, height_m: 6.0 },
                        FootprintBucket { max_area_m2: 800.0, height_m: 9.0 },
                        FootprintBucket { max_area_m2: SENTINEL_OPEN, height_m: 10.0 },
                    ],
                },
                rural: FootprintCurve {
                    buckets: vec![
                        FootprintBucket { max_area_m2: 60.0, height_m: 4.0 },
                        FootprintBucket { max_area_m2: 200.0, height_m: 6.0 },
                        FootprintBucket { max_area_m2: 800.0, height_m: 9.0 },
                        FootprintBucket { max_area_m2: SENTINEL_OPEN, height_m: 10.0 },
                    ],
                },
            },
        }
    }
}

fn entries(pairs: &[(&[&str], f64)]) -> HashMap<String, f64> {
    let mut m = HashMap::new();
    for (keys, h) in pairs {
        for k in *keys {
            m.insert((*k).to_string(), *h);
        }
    }
    m
}

fn default_class_table() -> HashMap<String, f64> {
    entries(&[
        (&["house", "detached", "semidetached_house", "terrace", "bungalow",
           "static_caravan", "houseboat"], 6.0),
        (&["apartments", "residential", "dormitory"], 25.0),
        // Hotel split out of the apartments group: chiyoda CBD hotels
        // averaged 60 m of truth vs the 25 m default (-36 m bias);
        // 35 m halves the gap without overshooting suburban hotels.
        (&["hotel"], 35.0),
        // Office bumped 30 → 40: Marunouchi towers were -35 m biased
        // at 30, while suburban offices (Yokohama Nishi-ku) were +26 m
        // over. 40 splits the difference better than either extreme.
        // Commercial held at 30 — bimodal between strip-mall retail
        // and CBD high-rise; bumping it caused regressions elsewhere.
        (&["office"], 40.0),
        (&["commercial"], 30.0),
        (&["retail", "supermarket", "shop", "kiosk", "mall"], 6.0),
        (&["school", "kindergarten", "university", "college", "civic",
           "government", "public", "library", "museum", "theatre"], 12.0),
        (&["hospital", "clinic"], 18.0),
        (&["industrial", "warehouse", "factory", "manufacture"], 10.0),
        (&["garage", "garages", "carport", "parking"], 4.0),
        (&["shed", "hut", "container", "cabin", "tent", "service"], 3.0),
        (&["barn", "farm", "farm_auxiliary", "cowshed", "stable", "sty",
           "greenhouse", "silo"], 5.0),
        (&["church", "chapel", "cathedral", "mosque", "temple", "synagogue",
           "shrine", "religious", "monastery"], 12.0),
        (&["stadium", "sports_hall", "sports_centre", "grandstand"], 15.0),
        (&["train_station", "bus_station", "terminal", "transportation"], 12.0),
    ])
}

fn default_subtype_table() -> HashMap<String, f64> {
    entries(&[
        (&["residential"], 8.0),
        (&["commercial"], 15.0),
        (&["industrial"], 10.0),
        (&["agricultural", "outbuilding", "service"], 5.0),
        (&["civic", "education", "entertainment", "religious"], 12.0),
        (&["medical"], 18.0),
        (&["military", "transportation"], 8.0),
    ])
}
