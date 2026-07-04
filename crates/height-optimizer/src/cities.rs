//! City presets with bounding boxes + their ground-truth source.
//!
//! Bboxes are intentionally small (~1–2 km square) — the goal is to
//! get a representative sample of building stock, not blanket the
//! whole city. PLATEAU LOD1 also runs into the hundreds of MB at full
//! ward scope.
//!
//! Japanese presets draw truth from PLATEAU LOD1; the Dutch presets draw
//! from 3D BAG. The Dutch presets exist only to *measure* cross-country
//! generalization of the height model — they are not part of any default
//! train/holdout split.

use crate::bbox::BBox;

/// Where a preset's ground-truth building heights come from.
pub enum TruthSource {
    /// PLATEAU LOD1 (Japan), keyed by JIS X 0402 5-digit municipal code.
    Plateau { city_code: &'static str },
    /// 3D BAG (Netherlands), nationwide FlatCityBuf index queried by bbox.
    Bag3d,
}

impl TruthSource {
    /// Short label for listings/reports, e.g. `plateau:13101` or `3dbag`.
    pub fn label(&self) -> String {
        match self {
            TruthSource::Plateau { city_code } => format!("plateau:{city_code}"),
            TruthSource::Bag3d => "3dbag".to_string(),
        }
    }
}

pub struct City {
    pub name: &'static str,
    pub truth: TruthSource,
    pub bbox: BBox,
    pub note: &'static str,
}

pub fn all() -> &'static [City] {
    &[
        City {
            name: "chiyoda",
            truth: TruthSource::Plateau { city_code: "13101" },
            // Marunouchi / Otemachi — high-rise office core.
            bbox: BBox {
                west: 139.760,
                south: 35.675,
                east: 139.775,
                north: 35.687,
            },
            note: "high-rise office core",
        },
        City {
            name: "setagaya",
            truth: TruthSource::Plateau { city_code: "13112" },
            // Sangenjaya — dense single-family + small apartments.
            bbox: BBox {
                west: 139.665,
                south: 35.640,
                east: 139.680,
                north: 35.650,
            },
            note: "dense low-rise residential",
        },
        City {
            name: "nishi-yokohama",
            // PLATEAU serves Yokohama at the parent city code (14100),
            // not per-ward (14103 etc).
            truth: TruthSource::Plateau { city_code: "14100" },
            // Minato Mirai — tower apartments + waterfront warehouses.
            bbox: BBox {
                west: 139.625,
                south: 35.450,
                east: 139.640,
                north: 35.465,
            },
            note: "tower apartments + warehouses",
        },
        City {
            name: "tsukuba",
            truth: TruthSource::Plateau { city_code: "08220" },
            // Tsukuba Center — planned low/mid-rise + research facilities.
            bbox: BBox {
                west: 140.105,
                south: 36.075,
                east: 140.120,
                north: 36.090,
            },
            note: "planned low/mid-rise + research",
        },
        City {
            // 飯山市 (Iiyama, Nagano). PLATEAU has no Takayama; Iiyama
            // covers the same "small mountain town + rural mix" niche.
            name: "iiyama",
            truth: TruthSource::Plateau { city_code: "20213" },
            // Iiyama Station / castle area — low-rise townscape.
            bbox: BBox {
                west: 138.360,
                south: 36.845,
                east: 138.380,
                north: 36.860,
            },
            note: "small mountain town, low-rise",
        },
        City {
            // 八王子市 (Hachioji, Tokyo) — suburban terminal station.
            name: "hachioji",
            truth: TruthSource::Plateau { city_code: "13201" },
            // North side of Hachioji Station — mid-rise commercial +
            // dense low-rise mix around the terminal.
            bbox: BBox {
                west: 139.332,
                south: 35.655,
                east: 139.348,
                north: 35.668,
            },
            note: "suburban terminal station",
        },
        City {
            // 金沢市 (Kanazawa, Ishikawa) — regional core.
            name: "kanazawa",
            truth: TruthSource::Plateau { city_code: "17201" },
            // Kanazawa Station to Korinbo — regional CBD spine.
            bbox: BBox {
                west: 136.647,
                south: 36.560,
                east: 136.663,
                north: 36.573,
            },
            note: "regional core",
        },
        City {
            // 高松市 (Takamatsu, Kagawa) — regional CBD.
            name: "takamatsu",
            truth: TruthSource::Plateau { city_code: "37201" },
            // Central arcades (Marugamemachi) + station-front CBD.
            bbox: BBox {
                west: 134.043,
                south: 34.338,
                east: 134.058,
                north: 34.351,
            },
            note: "regional CBD",
        },
        // ---- Netherlands (3D BAG) — cross-country generalization checks ----
        City {
            name: "delft",
            truth: TruthSource::Bag3d,
            // Historic centre around the Markt — dense low-rise.
            bbox: BBox {
                west: 4.352,
                south: 51.995,
                east: 4.368,
                north: 52.008,
            },
            note: "NL historic center, low-rise",
        },
        City {
            name: "amsterdam",
            truth: TruthSource::Bag3d,
            // Canal belt (grachtengordel) — mid-rise townhouses.
            bbox: BBox {
                west: 4.880,
                south: 52.360,
                east: 4.900,
                north: 52.375,
            },
            note: "NL canal belt, mid-rise",
        },
        City {
            name: "rotterdam",
            truth: TruthSource::Bag3d,
            // CBD around the Coolsingel — the Dutch high-rise regime.
            bbox: BBox {
                west: 4.465,
                south: 51.915,
                east: 4.485,
                north: 51.928,
            },
            note: "NL CBD high-rise",
        },
        City {
            name: "groningen",
            truth: TruthSource::Bag3d,
            // City centre around the Grote Markt — regional center.
            bbox: BBox {
                west: 6.558,
                south: 53.212,
                east: 6.578,
                north: 53.225,
            },
            note: "NL regional center",
        },
        City {
            name: "maastricht",
            truth: TruthSource::Bag3d,
            // Historic south; ground sits ~45–50 m above NAP, so this is the
            // canary for a broken NAP subtraction.
            bbox: BBox {
                west: 5.685,
                south: 50.843,
                east: 5.705,
                north: 50.855,
            },
            note: "NL historic south",
        },
    ]
}

pub fn get(name: &str) -> Option<&'static City> {
    all().iter().find(|c| c.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preset_names_are_unique() {
        let mut names: Vec<&str> = all().iter().map(|c| c.name).collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(before, names.len(), "duplicate preset name");
    }

    #[test]
    fn dutch_presets_use_bag3d_and_japanese_use_plateau() {
        for c in all() {
            let is_nl = matches!(
                c.name,
                "delft" | "amsterdam" | "rotterdam" | "groningen" | "maastricht"
            );
            match (&c.truth, is_nl) {
                (TruthSource::Bag3d, true) | (TruthSource::Plateau { .. }, false) => {}
                _ => panic!("{} has wrong truth source {:?}", c.name, c.truth.label()),
            }
        }
        assert!(matches!(
            get("delft").expect("delft preset exists").truth,
            TruthSource::Bag3d
        ));
    }

    #[test]
    fn truth_labels_are_stable() {
        assert_eq!(get("chiyoda").unwrap().truth.label(), "plateau:13101");
        assert_eq!(get("rotterdam").unwrap().truth.label(), "3dbag");
    }
}
