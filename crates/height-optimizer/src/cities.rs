//! City presets with bounding boxes + PLATEAU city codes.
//!
//! Bboxes are intentionally small (~1–2 km square) — the goal is to
//! get a representative sample of building stock, not blanket the
//! whole city. PLATEAU LOD1 also runs into the hundreds of MB at full
//! ward scope.

use crate::bbox::BBox;

pub struct City {
    pub name: &'static str,
    /// JIS X 0402 5-digit municipal code used in PLATEAU 3D Tiles URLs.
    pub city_code: &'static str,
    pub bbox: BBox,
    pub note: &'static str,
}

pub fn all() -> &'static [City] {
    &[
        City {
            name: "chiyoda",
            city_code: "13101",
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
            city_code: "13112",
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
            city_code: "14100",
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
            city_code: "08220",
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
            city_code: "20213",
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
            city_code: "13201",
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
            city_code: "17201",
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
            city_code: "37201",
            // Central arcades (Marugamemachi) + station-front CBD.
            bbox: BBox {
                west: 134.043,
                south: 34.338,
                east: 134.058,
                north: 34.351,
            },
            note: "regional CBD",
        },
    ]
}

pub fn get(name: &str) -> Option<&'static City> {
    all().iter().find(|c| c.name == name)
}
