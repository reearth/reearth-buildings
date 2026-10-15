//! 3D BAG (Netherlands) building truth fetcher.
//!
//! Reads the nationwide FlatCityBuf cloud-optimized CityJSON index over
//! HTTP range requests (`fcb_core`), querying by bbox in RD New metres
//! (EPSG:28992) and converting results back to WGS84 for the shared
//! [`crate::truth::Building`] model.
//!
//! Height semantics (verified against the live file, Phase 0 probe): each
//! `Building` object carries `b3_h_dak_50p` (median roof, m above NAP) and
//! `b3_h_maaiveld` (ground, m above NAP). Height-above-ground —
//! comparable to PLATEAU `bldg:measuredHeight` — is their difference. The
//! per-feature `vertices` are quantized `i64`; the global header transform
//! (`scale`, `translate`) dequantizes them into RD metres.

use crate::bbox::BBox;
use crate::fetch_plateau::fnv1a;
use crate::rd::{rd_to_wgs84, wgs84_to_rd};
use crate::truth::Building;
use anyhow::{Context, Result};
use buildings_core::coord::LonLat;
use cjseq2::CityJSONFeature;
use fcb_core::packed_rtree::Query;
use fcb_core::HttpFcbReader;
use std::path::Path;

const FCB_URL: &str = "https://storage.googleapis.com/flatcitybuf/3dbag_all_index.fcb";

/// Bumped whenever the parse (attribute names, height formula, filters)
/// changes, so stale result caches are ignored.
const CACHE_VERSION: u32 = 1;

/// RD New is rotated relative to lon/lat, so transforming the four WGS84
/// corners under-covers the true RD envelope near the edges. Pad the query
/// box; the real per-building `bbox.contains_lonlat` filter trims the excess.
const QUERY_PAD_M: f64 = 50.0;

/// Fetch 3D BAG truth buildings whose centroid falls inside `bbox`.
/// `cache` is the per-source cache directory (the caller passes
/// `.../3dbag`); results are memoised there as JSON.
pub fn fetch_truth(bbox: &BBox, cache: &Path) -> Result<Vec<Building>> {
    std::fs::create_dir_all(cache).ok();

    let key = fnv1a(&format!(
        "{FCB_URL}|v{CACHE_VERSION}|{:.6},{:.6},{:.6},{:.6}",
        bbox.west, bbox.south, bbox.east, bbox.north
    ));
    let cache_file = cache.join(format!("{key:016x}.json"));
    if let Ok(bytes) = std::fs::read(&cache_file) {
        if let Ok(cached) = serde_json::from_slice::<Vec<CachedBuilding>>(&bytes) {
            eprintln!("3dbag: cache hit ({} buildings)", cached.len());
            return Ok(cached
                .into_iter()
                .map(CachedBuilding::into_building)
                .collect());
        }
    }

    let (minx, miny, maxx, maxy) = rd_query_bbox(bbox);
    eprintln!(
        "3dbag: RD query bbox [{minx:.1},{miny:.1},{maxx:.1},{maxy:.1}] (padded {QUERY_PAD_M} m)"
    );

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("build tokio runtime")?;

    let buildings = rt.block_on(async {
        let reader = HttpFcbReader::open(FCB_URL)
            .await
            .context("open 3DBAG FlatCityBuf reader")?;

        // Read the global transform before select_query consumes the reader.
        let (scale, translate) = {
            let header = reader.header();
            let t = header
                .transform()
                .context("3DBAG header has no transform")?;
            (
                [t.scale().x(), t.scale().y(), t.scale().z()],
                [t.translate().x(), t.translate().y(), t.translate().z()],
            )
        };

        let mut iter = reader
            .select_query(Query::BBox(minx, miny, maxx, maxy))
            .await
            .context("select_query by bbox")?;

        let mut out: Vec<Building> = Vec::new();
        let mut total = 0usize;
        let mut skipped_no_height = 0usize;
        let mut skipped_nonpositive = 0usize;
        let mut skipped_out_of_bbox = 0usize;
        let mut low_quality = 0usize;
        while iter.next().await.context("read next feature")?.is_some() {
            let cj = iter.cur_cj_feature().context("decode cj feature")?;
            total += 1;
            match building_from_feature(&cj, &scale, &translate, bbox) {
                FeatureOutcome::Kept(b) => out.push(b),
                FeatureOutcome::NoHeight => skipped_no_height += 1,
                FeatureOutcome::NonPositive => skipped_nonpositive += 1,
                FeatureOutcome::OutOfBbox => skipped_out_of_bbox += 1,
            }
            if feature_low_quality(&cj) {
                low_quality += 1;
            }
        }
        eprintln!(
            "3dbag: features={total} kept={} skipped(no_height={skipped_no_height}, h<=0={skipped_nonpositive}, out_of_bbox={skipped_out_of_bbox}) pw_insufficient={low_quality}",
            out.len()
        );
        Ok::<_, anyhow::Error>(out)
    })?;

    // Best-effort cache write.
    if let Ok(json) = serde_json::to_vec(
        &buildings
            .iter()
            .map(CachedBuilding::from_building)
            .collect::<Vec<_>>(),
    ) {
        std::fs::write(&cache_file, json).ok();
    }

    Ok(buildings)
}

enum FeatureOutcome {
    Kept(Building),
    NoHeight,
    NonPositive,
    OutOfBbox,
}

/// Build a truth building from one CityJSON feature, or classify why it was
/// dropped. Height = `b3_h_dak_50p − b3_h_maaiveld` (both m above NAP);
/// centroid = mean of the feature's dequantized RD vertices, reprojected to
/// WGS84 and filtered against the real (unpadded) `bbox`.
fn building_from_feature(
    cj: &CityJSONFeature,
    scale: &[f64; 3],
    translate: &[f64; 3],
    bbox: &BBox,
) -> FeatureOutcome {
    let Some(attrs) = cj
        .city_objects
        .values()
        .find(|co| co.thetype == "Building")
        .and_then(|co| co.attributes.as_ref())
        .and_then(|v| v.as_object())
    else {
        return FeatureOutcome::NoHeight;
    };

    let roof = attrs.get("b3_h_dak_50p").and_then(|v| v.as_f64());
    let ground = attrs.get("b3_h_maaiveld").and_then(|v| v.as_f64());
    let (Some(roof), Some(ground)) = (roof, ground) else {
        return FeatureOutcome::NoHeight;
    };
    let h = roof - ground;
    if h <= 0.0 {
        return FeatureOutcome::NonPositive;
    }

    if cj.vertices.is_empty() {
        return FeatureOutcome::NoHeight;
    }
    let mut sx = 0.0f64;
    let mut sy = 0.0f64;
    for v in &cj.vertices {
        sx += v[0] as f64 * scale[0] + translate[0];
        sy += v[1] as f64 * scale[1] + translate[1];
    }
    let n = cj.vertices.len() as f64;
    let (lon, lat) = rd_to_wgs84(sx / n, sy / n);

    if !bbox.contains_lonlat(lon, lat) {
        return FeatureOutcome::OutOfBbox;
    }

    FeatureOutcome::Kept(Building {
        centroid: LonLat {
            lon_deg: lon,
            lat_deg: lat,
        },
        measured_height_m: h as f32,
    })
}

/// Diagnostic-only quality proxy. 3DBAG has no single `kwaliteitsindicator`
/// field (Phase 0 probe); the most height-relevant per-building signal is the
/// boolean `b3_pw_onvoldoende` ("point cloud insufficient"), which flags
/// buildings whose lidar coverage was too sparse to reconstruct a reliable
/// roof height. Tallied, not filtered, in v1.
fn feature_low_quality(cj: &CityJSONFeature) -> bool {
    cj.city_objects
        .values()
        .find(|co| co.thetype == "Building")
        .and_then(|co| co.attributes.as_ref())
        .and_then(|v| v.as_object())
        .and_then(|m| m.get("b3_pw_onvoldoende"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

/// RD-metre query envelope covering `bbox`: transform all four WGS84 corners
/// (RD is rotated, so corners alone under-cover), take the min/max, and pad.
fn rd_query_bbox(bbox: &BBox) -> (f64, f64, f64, f64) {
    let corners = [
        (bbox.west, bbox.south),
        (bbox.west, bbox.north),
        (bbox.east, bbox.south),
        (bbox.east, bbox.north),
    ];
    let mut minx = f64::INFINITY;
    let mut miny = f64::INFINITY;
    let mut maxx = f64::NEG_INFINITY;
    let mut maxy = f64::NEG_INFINITY;
    for (lon, lat) in corners {
        let (x, y) = wgs84_to_rd(lon, lat);
        minx = minx.min(x);
        miny = miny.min(y);
        maxx = maxx.max(x);
        maxy = maxy.max(y);
    }
    (
        minx - QUERY_PAD_M,
        miny - QUERY_PAD_M,
        maxx + QUERY_PAD_M,
        maxy + QUERY_PAD_M,
    )
}

/// On-disk cache record. [`LonLat`] has no serde derives, so we flatten the
/// truth building to primitive fields.
#[derive(serde::Serialize, serde::Deserialize)]
struct CachedBuilding {
    lon: f64,
    lat: f64,
    h: f32,
}

impl CachedBuilding {
    fn from_building(b: &Building) -> Self {
        Self {
            lon: b.centroid.lon_deg,
            lat: b.centroid.lat_deg,
            h: b.measured_height_m,
        }
    }
    fn into_building(self) -> Building {
        Building {
            centroid: LonLat {
                lon_deg: self.lon,
                lat_deg: self.lat,
            },
            measured_height_m: self.h,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Transform matching the live 3DBAG file (Phase 0 probe).
    const SCALE: [f64; 3] = [0.001, 0.001, 0.001];
    const TRANSLATE: [f64; 3] = [171_800.0, 472_700.0, 0.0];

    /// Quantize an RD coordinate into the fixture's vertex space.
    fn q(x_rd: f64, y_rd: f64) -> [i64; 3] {
        [
            ((x_rd - TRANSLATE[0]) / SCALE[0]).round() as i64,
            ((y_rd - TRANSLATE[1]) / SCALE[1]).round() as i64,
            0,
        ]
    }

    /// Minimal CityJSONFeature fixture: one Building carrying `attrs`,
    /// one attribute-less BuildingPart (as in the live file), and the
    /// given quantized vertices.
    fn feature(attrs: serde_json::Value, vertices: Vec<[i64; 3]>) -> CityJSONFeature {
        serde_json::from_value(serde_json::json!({
            "type": "CityJSONFeature",
            "id": "fixture",
            "CityObjects": {
                "b1": { "type": "Building", "attributes": attrs, "children": ["p1"] },
                "p1": { "type": "BuildingPart", "parents": ["b1"] }
            },
            "vertices": vertices
        }))
        .expect("fixture must deserialize as CityJSONFeature")
    }

    /// Westertoren, Amsterdam — the rd.rs landmark, reused so the test
    /// covers dequantize -> rd_to_wgs84 end to end.
    const WESTER_RD: (f64, f64) = (120_700.723, 487_525.501);
    const WESTER_WGS: (f64, f64) = (4.883_525_59, 52.374_532_53);

    fn wester_bbox() -> BBox {
        BBox::new(4.87, 52.37, 4.89, 52.38)
    }

    #[test]
    fn building_from_feature_happy_path() {
        let f = feature(
            serde_json::json!({ "b3_h_dak_50p": 12.5, "b3_h_maaiveld": 2.5 }),
            vec![q(WESTER_RD.0, WESTER_RD.1)],
        );
        match building_from_feature(&f, &SCALE, &TRANSLATE, &wester_bbox()) {
            FeatureOutcome::Kept(b) => {
                assert!((b.measured_height_m - 10.0).abs() < 1e-4);
                assert!((b.centroid.lon_deg - WESTER_WGS.0).abs() < 2e-5);
                assert!((b.centroid.lat_deg - WESTER_WGS.1).abs() < 2e-5);
            }
            _ => panic!("expected Kept"),
        }
    }

    /// The NAP canary in unit-test form: both attributes are ABSOLUTE
    /// elevations, so a building on 49 m NAP ground with a 60 m NAP roof
    /// is 11 m tall — not 60. (Maastricht-class terrain.)
    #[test]
    fn height_is_roof_minus_ground_not_absolute() {
        let f = feature(
            serde_json::json!({ "b3_h_dak_50p": 60.0, "b3_h_maaiveld": 49.0 }),
            vec![q(WESTER_RD.0, WESTER_RD.1)],
        );
        match building_from_feature(&f, &SCALE, &TRANSLATE, &wester_bbox()) {
            FeatureOutcome::Kept(b) => assert!((b.measured_height_m - 11.0).abs() < 1e-4),
            _ => panic!("expected Kept"),
        }
    }

    #[test]
    fn missing_attributes_are_no_height() {
        let missing_roof = feature(
            serde_json::json!({ "b3_h_maaiveld": 2.5 }),
            vec![q(WESTER_RD.0, WESTER_RD.1)],
        );
        assert!(matches!(
            building_from_feature(&missing_roof, &SCALE, &TRANSLATE, &wester_bbox()),
            FeatureOutcome::NoHeight
        ));

        let missing_ground = feature(
            serde_json::json!({ "b3_h_dak_50p": 12.5 }),
            vec![q(WESTER_RD.0, WESTER_RD.1)],
        );
        assert!(matches!(
            building_from_feature(&missing_ground, &SCALE, &TRANSLATE, &wester_bbox()),
            FeatureOutcome::NoHeight
        ));

        let no_vertices = feature(
            serde_json::json!({ "b3_h_dak_50p": 12.5, "b3_h_maaiveld": 2.5 }),
            vec![],
        );
        assert!(matches!(
            building_from_feature(&no_vertices, &SCALE, &TRANSLATE, &wester_bbox()),
            FeatureOutcome::NoHeight
        ));
    }

    #[test]
    fn non_positive_height_is_dropped() {
        let f = feature(
            serde_json::json!({ "b3_h_dak_50p": 2.0, "b3_h_maaiveld": 2.5 }),
            vec![q(WESTER_RD.0, WESTER_RD.1)],
        );
        assert!(matches!(
            building_from_feature(&f, &SCALE, &TRANSLATE, &wester_bbox()),
            FeatureOutcome::NonPositive
        ));
    }

    #[test]
    fn padded_query_excess_is_filtered_by_real_bbox() {
        // Valid building whose centroid sits outside the requested bbox
        // (as padded RD queries will return).
        let f = feature(
            serde_json::json!({ "b3_h_dak_50p": 12.5, "b3_h_maaiveld": 2.5 }),
            vec![q(WESTER_RD.0 + 5_000.0, WESTER_RD.1)],
        );
        assert!(matches!(
            building_from_feature(&f, &SCALE, &TRANSLATE, &wester_bbox()),
            FeatureOutcome::OutOfBbox
        ));
    }

    #[test]
    fn low_quality_flag_is_read_from_building_object() {
        let flagged = feature(
            serde_json::json!({ "b3_pw_onvoldoende": true }),
            vec![q(WESTER_RD.0, WESTER_RD.1)],
        );
        assert!(feature_low_quality(&flagged));

        let absent = feature(serde_json::json!({}), vec![q(WESTER_RD.0, WESTER_RD.1)]);
        assert!(!feature_low_quality(&absent));
    }

    #[test]
    fn rd_query_bbox_covers_all_corners_with_padding() {
        let bbox = wester_bbox();
        let (minx, miny, maxx, maxy) = rd_query_bbox(&bbox);
        for (lon, lat) in [
            (bbox.west, bbox.south),
            (bbox.west, bbox.north),
            (bbox.east, bbox.south),
            (bbox.east, bbox.north),
        ] {
            let (x, y) = wgs84_to_rd(lon, lat);
            assert!(
                x >= minx + QUERY_PAD_M - 1e-6 && x <= maxx - QUERY_PAD_M + 1e-6,
                "corner x {x} outside padded envelope"
            );
            assert!(
                y >= miny + QUERY_PAD_M - 1e-6 && y <= maxy - QUERY_PAD_M + 1e-6,
                "corner y {y} outside padded envelope"
            );
        }
        assert!(maxx - minx > 0.0 && maxy - miny > 0.0);
    }

    #[test]
    fn cached_building_round_trips() {
        let b = Building {
            centroid: LonLat {
                lon_deg: 4.8835,
                lat_deg: 52.3745,
            },
            measured_height_m: 10.5,
        };
        let json = serde_json::to_vec(&[CachedBuilding::from_building(&b)]).unwrap();
        let back: Vec<CachedBuilding> = serde_json::from_slice(&json).unwrap();
        let rb = back.into_iter().next().unwrap().into_building();
        assert!((rb.centroid.lon_deg - b.centroid.lon_deg).abs() < 1e-12);
        assert!((rb.centroid.lat_deg - b.centroid.lat_deg).abs() < 1e-12);
        assert!((rb.measured_height_m - b.measured_height_m).abs() < 1e-6);
    }
}

#[cfg(test)]
mod probe {
    //! Phase 0 live probe. Network-gated (`#[ignore]`); run with
    //! `cargo test -p height-optimizer bag3d_live_probe -- --ignored --nocapture`.
    //! Resolves the schema questions the fetcher body depends on: transform
    //! semantics, vertex quantization, city-object types, and the exact
    //! `b3_*` attribute names in the live file.
    use super::FCB_URL;
    use fcb_core::packed_rtree::Query;
    use fcb_core::HttpFcbReader;

    #[test]
    #[ignore = "network: hits the live 3DBAG FlatCityBuf index"]
    fn bag3d_live_probe() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let reader = HttpFcbReader::open(FCB_URL).await.expect("open reader");

            let (scale, translate) = {
                let header = reader.header();
                let t = header.transform().expect("header has transform");
                (
                    [t.scale().x(), t.scale().y(), t.scale().z()],
                    [t.translate().x(), t.translate().y(), t.translate().z()],
                )
            };
            let total_features = reader.header().features_count();
            eprintln!("=== header ===");
            eprintln!("total features_count = {total_features}");
            eprintln!("transform.scale     = {scale:?}");
            eprintln!("transform.translate = {translate:?}");

            // Delft, RD New metres.
            let (minx, miny, maxx, maxy) = (84000.0, 446000.0, 86000.0, 448000.0);
            let mut iter = reader
                .select_query(Query::BBox(minx, miny, maxx, maxy))
                .await
                .expect("select_query");
            eprintln!("selected features_count = {:?}", iter.features_count());

            let mut seen = 0usize;
            let mut co_types: std::collections::BTreeSet<String> = Default::default();
            while iter.next().await.expect("next").is_some() {
                let cj = iter.cur_cj_feature().expect("cj_feature");
                seen += 1;
                for (id, co) in &cj.city_objects {
                    co_types.insert(co.thetype.clone());
                    if seen <= 3 {
                        eprintln!("\n--- feature #{seen} object {id} type={} ---", co.thetype);
                        if let Some(map) = co.attributes.as_ref().and_then(|a| a.as_object()) {
                            let mut keys: Vec<&String> = map.keys().collect();
                            keys.sort();
                            eprintln!("attribute keys ({}): {keys:?}", keys.len());
                            for k in [
                                "b3_h_dak_50p",
                                "b3_h_maaiveld",
                                "b3_val3dity_lod22",
                                "b3_pw_onvoldoende",
                                "b3_nodata_fractie_ahn5",
                            ] {
                                if let Some(v) = map.get(k) {
                                    eprintln!("  {k} = {v}");
                                }
                            }
                        }
                    }
                }
                if seen <= 3 {
                    for (vi, v) in cj.vertices.iter().take(2).enumerate() {
                        let raw = (v[0], v[1], v[2]);
                        let deq = (
                            v[0] as f64 * scale[0] + translate[0],
                            v[1] as f64 * scale[1] + translate[1],
                            v[2] as f64 * scale[2] + translate[2],
                        );
                        eprintln!("  vertex[{vi}] raw={raw:?} dequantized={deq:?}");
                    }
                }
                if seen >= 500 {
                    break;
                }
            }
            eprintln!("\n=== summary ===");
            eprintln!("iterated {seen} features (capped at 500)");
            eprintln!("city-object types seen: {co_types:?}");
        });
    }
}
