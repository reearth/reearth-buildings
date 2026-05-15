//! Extrude Overture MVT building polygons (possibly drawn from several
//! source tiles) into a flat-shaded triangle mesh anchored at an output
//! tile.
//!
//! Output frame: tile-local ENU metres (east/north/up), centred on the
//! output tile's centre. y is up in the glb-friendly remapping below.
//! Per-building ground elevation comes from a Re:Earth Terrain tile sampled
//! at each building's centroid.

use crate::coord::{self, LonLat};
use crate::height_config::HeightConfig;
use mvt_decoder::{BuildingFeature, DecodedTile};
use std::collections::HashMap;
use terrain_decoder::TerrainTile;

/// Per-building metadata, indexed by FEATURE_ID written into the mesh.
#[derive(Debug, Clone, Default)]
pub struct FeatureProps {
    /// MVT feature id (Planetiler-hashed Overture id). Stable for dedup
    /// across source tiles within one PMTiles release.
    pub feature_id: Option<u64>,
    /// GERS feature id (string). Stable across releases.
    pub gers_id: Option<String>,
    pub name: Option<String>,
    pub subtype: Option<String>,
    pub class: Option<String>,
    /// Resolved height used for the extrusion. Always positive.
    pub height_m: f32,
    /// Original Overture `height` value, if present. `None` when the
    /// upstream property was missing or 0; surfaces in glb as 0 with
    /// `noData=0`.
    pub source_height_m: Option<f32>,
    /// Tag identifying which fallback path produced [`height_m`]. One of
    /// `explicit` / `num_floors` / `class` / `subtype` / `footprint`.
    pub height_method: &'static str,
    pub min_height_m: f32,
    pub roof_height_m: f32,
    pub roof_shape: Option<String>,
    pub roof_material: Option<String>,
    pub roof_color: Option<String>,
    pub facade_material: Option<String>,
    pub facade_color: Option<String>,
    pub num_floors: u16,
    /// Ground elevation (ellipsoidal metres) sampled at the centroid.
    pub ground_elev_m: f32,
    pub footprint_m2: f32,
}

pub struct Mesh {
    pub positions: Vec<f32>,
    pub normals: Vec<f32>,
    pub feature_ids: Vec<u16>,
    pub indices: Vec<u32>,
    pub features: Vec<FeatureProps>,
}

pub struct Source<'a> {
    pub z: u8,
    pub x: u32,
    pub y: u32,
    pub tile: &'a DecodedTile,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct AreaFilter {
    pub min_m2: f32,
    pub max_m2: f32,
}

impl AreaFilter {
    pub fn accepts(&self, area_m2: f32) -> bool {
        if self.min_m2 > 0.0 && area_m2 < self.min_m2 {
            return false;
        }
        if self.max_m2 > 0.0 && area_m2 >= self.max_m2 {
            return false;
        }
        true
    }
}

/// Coarse "how built-up is this neighbourhood" classifier derived from
/// the source-tile building count. Drives a minimum height for buildings
/// that fall through to the footprint heuristic — the small "pencil
/// buildings" of central Tokyo would otherwise look like sheds.
///
/// Only applied to `footprint`-method heights; explicit / num_floors /
/// class / subtype values are trusted as-is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UrbanLevel {
    Rural,
    Suburban,
    Urban,
    DenseUrban,
}

impl UrbanLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            UrbanLevel::Rural => "rural",
            UrbanLevel::Suburban => "suburban",
            UrbanLevel::Urban => "urban",
            UrbanLevel::DenseUrban => "dense_urban",
        }
    }
}

/// Classify by average building count per source MVT (z=14). Thresholds
/// are tuned against Japanese urban patterns; rural Hokkaido sits well
/// below 200, Tokyo 23-ku averages 800–2000, Marunouchi-class blocks
/// push past 1500.
pub fn classify_urban(avg_buildings_per_source: f32, cfg: &HeightConfig) -> UrbanLevel {
    let t = &cfg.urban_thresholds;
    if avg_buildings_per_source >= t.dense_urban_min {
        UrbanLevel::DenseUrban
    } else if avg_buildings_per_source >= t.urban_min {
        UrbanLevel::Urban
    } else if avg_buildings_per_source >= t.suburban_min {
        UrbanLevel::Suburban
    } else {
        UrbanLevel::Rural
    }
}

/// Pick the building height in metres via a 5-step fallback cascade:
///
///   1. explicit Overture `height` (best — usually OSM-tagged)
///   2. `num_floors × 3 m`
///   3. class lookup (`apartments`, `house`, `industrial`, …)
///   4. subtype lookup (coarser: `residential`, `commercial`, …)
///   5. footprint area heuristic, optionally boosted by neighbourhood
///      density (last resort for ML-derived footprints with no metadata)
///
/// Returns `(height_m, method)` where `method` is a stable tag the glb
/// surfaces as a property so downstream styling can distinguish
/// "trust this" from "we guessed".
pub fn default_height_meters(
    feat: &BuildingFeature,
    area_m2: f32,
    urban: UrbanLevel,
    cfg: &HeightConfig,
) -> (f64, &'static str) {
    if let Some(h) = feat.height {
        if h > 0.0 {
            return (h, "explicit");
        }
    }
    if let Some(l) = feat.num_floors {
        if l > 0 {
            return ((l as f64) * cfg.meters_per_floor, "num_floors");
        }
    }
    if let Some(cls) = feat.class.as_deref() {
        if let Some(h) = cfg.class_height_m.get(cls) {
            return (*h, "class");
        }
    }
    if let Some(st) = feat.subtype.as_deref() {
        if let Some(h) = cfg.subtype_height_m.get(st) {
            return (*h, "subtype");
        }
    }
    let curve = match urban {
        UrbanLevel::DenseUrban => &cfg.footprint.dense_urban,
        UrbanLevel::Urban => &cfg.footprint.urban,
        UrbanLevel::Suburban => &cfg.footprint.suburban,
        UrbanLevel::Rural => &cfg.footprint.rural,
    };
    let h = curve.lookup(area_m2);
    let method = if urban == UrbanLevel::Rural {
        "footprint"
    } else {
        // Density-boosted: distinguishable in the glb so users can
        // filter / restyle the guessed-tall buildings.
        "density"
    };
    (h, method)
}

/// One clipped polygon belonging to some (possibly multi-fragment) building.
struct Fragment {
    /// Index into the parent `sources` slice. Lets us look up
    /// (src_z, src_x, src_y, extent) later without copying them around.
    source_idx: usize,
    polygon: Polygon,
}

struct PendingFeature {
    props: FeatureProps,
    total_area_m2: f32,
    fragments: Vec<Fragment>,
    /// Lon/lat sum and weight for the area-weighted centroid (used to
    /// sample terrain). Centroid is computed at emit time so that
    /// multi-fragment buildings get a single ground elevation.
    centroid_lon_weighted: f64,
    centroid_lat_weighted: f64,
    centroid_weight: f64,
}

pub fn build_mesh(
    out_z: u8,
    out_x: u32,
    out_y: u32,
    sources: &[Source<'_>],
    filter: AreaFilter,
    aabb_only: bool,
    terrain: Option<&TerrainTile>,
    height_config: &HeightConfig,
) -> Mesh {
    let anchor = coord::tile_center(out_z, out_x, out_y);

    // Density classifier from raw building counts (pre-filter).
    // Captures the "this is downtown" signal so the footprint heuristic
    // doesn't render Marunouchi as a row of huts.
    let total_buildings: usize = sources.iter().map(|s| s.tile.buildings.len()).sum();
    let avg_per_source =
        total_buildings as f32 / (sources.len().max(1) as f32);
    let urban = classify_urban(avg_per_source, height_config);

    // ---- 1. collect all fragments, grouping by feature id ----
    let mut by_id: HashMap<u64, usize> = HashMap::new();
    let mut pending: Vec<PendingFeature> = Vec::new();

    for (src_idx, source) in sources.iter().enumerate() {
        for feat in &source.tile.buildings {
            let polygons = group_polygons(&feat.rings);
            if polygons.is_empty() {
                continue;
            }
            for polygon in polygons {
                let area = polygon_area_m2(&polygon, source, source.tile.extent) as f32;
                let centroid_ll =
                    polygon_centroid_lonlat(&polygon, source, source.tile.extent);
                let polygon = if aabb_only {
                    polygon_to_aabb(&polygon)
                } else {
                    polygon
                };
                let fragment = Fragment {
                    source_idx: src_idx,
                    polygon,
                };
                let w = area as f64;
                let entry_idx = match feat.id {
                    Some(fid) => match by_id.get(&fid).copied() {
                        Some(idx) => {
                            pending[idx].fragments.push(fragment);
                            pending[idx].total_area_m2 += area;
                            idx
                        }
                        None => {
                            let idx = pending.len();
                            by_id.insert(fid, idx);
                            pending.push(PendingFeature {
                                props: feature_props(feat, area, urban, height_config),
                                total_area_m2: area,
                                fragments: vec![fragment],
                                centroid_lon_weighted: 0.0,
                                centroid_lat_weighted: 0.0,
                                centroid_weight: 0.0,
                            });
                            idx
                        }
                    },
                    None => {
                        // No stable id → can't dedup; each unidentified
                        // feature counts as its own building.
                        let idx = pending.len();
                        pending.push(PendingFeature {
                            props: feature_props(feat, area, urban, height_config),
                            total_area_m2: area,
                            fragments: vec![fragment],
                            centroid_lon_weighted: 0.0,
                            centroid_lat_weighted: 0.0,
                            centroid_weight: 0.0,
                        });
                        idx
                    }
                };
                pending[entry_idx].centroid_lon_weighted += centroid_ll.lon_deg * w;
                pending[entry_idx].centroid_lat_weighted += centroid_ll.lat_deg * w;
                pending[entry_idx].centroid_weight += w;
            }
        }
    }

    // ---- 2. emit features that pass the filter ----
    let mut positions: Vec<f32> = Vec::new();
    let mut normals: Vec<f32> = Vec::new();
    let mut feature_ids: Vec<u16> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();
    let mut features: Vec<FeatureProps> = Vec::new();

    for mut building in pending {
        if !filter.accepts(building.total_area_m2) {
            continue;
        }
        building.props.footprint_m2 = building.total_area_m2;

        // Centroid for terrain sampling — area-weighted so multi-fragment
        // buildings settle on the right ground elevation.
        let ground_elev = if let Some(t) = terrain {
            let (lon, lat) = if building.centroid_weight > 0.0 {
                (
                    building.centroid_lon_weighted / building.centroid_weight,
                    building.centroid_lat_weighted / building.centroid_weight,
                )
            } else {
                (anchor.lon_deg, anchor.lat_deg)
            };
            t.sample(lon, lat)
        } else {
            0.0
        };
        building.props.ground_elev_m = ground_elev;

        let base_h = (ground_elev as f64) + (building.props.min_height_m as f64);
        let top_h = (ground_elev as f64) + (building.props.height_m as f64);
        let fid = features.len().min(u16::MAX as usize - 1) as u16;
        features.push(building.props);

        for fragment in building.fragments {
            let source = &sources[fragment.source_idx];
            extrude_polygon(
                source.z,
                source.x,
                source.y,
                source.tile.extent,
                &fragment.polygon,
                anchor,
                base_h,
                top_h,
                fid,
                &mut positions,
                &mut normals,
                &mut feature_ids,
                &mut indices,
            );
        }
    }
    Mesh {
        positions,
        normals,
        feature_ids,
        indices,
        features,
    }
}

fn feature_props(
    feat: &BuildingFeature,
    area_m2: f32,
    urban: UrbanLevel,
    cfg: &HeightConfig,
) -> FeatureProps {
    let (height_m, height_method) = default_height_meters(feat, area_m2, urban, cfg);
    let source_height_m = feat
        .height
        .filter(|h| *h > 0.0)
        .map(|h| h as f32);
    FeatureProps {
        feature_id: feat.id,
        gers_id: feat.gers_id.clone(),
        name: feat.name.clone(),
        subtype: feat.subtype.clone(),
        class: feat.class.clone(),
        height_m: height_m as f32,
        source_height_m,
        height_method,
        min_height_m: feat.min_height.unwrap_or(0.0) as f32,
        roof_height_m: feat.roof_height.unwrap_or(0.0) as f32,
        roof_shape: feat.roof_shape.clone(),
        roof_material: feat.roof_material.clone(),
        roof_color: feat.roof_color.clone(),
        facade_material: feat.facade_material.clone(),
        facade_color: feat.facade_color.clone(),
        num_floors: feat
            .num_floors
            .unwrap_or(0)
            .min(u16::MAX as u32) as u16,
        ground_elev_m: 0.0,
        footprint_m2: area_m2,
    }
}

/// One outer ring with zero or more holes. All vertex coords are in tile
/// units (with the closing duplicate vertex stripped).
struct Polygon {
    outer: Vec<[i32; 2]>,
    holes: Vec<Vec<[i32; 2]>>,
}

/// Group MVT rings into polygons (spec §4.3.4.4): tile-space A > 0 starts
/// a new outer; A < 0 is a hole attached to the most recent outer.
fn group_polygons(rings: &[Vec<[i32; 2]>]) -> Vec<Polygon> {
    let mut out: Vec<Polygon> = Vec::new();
    for raw in rings {
        let r = strip_close(raw);
        if r.len() < 3 {
            continue;
        }
        let area = signed_area_tile(&r);
        if area > 0.0 {
            out.push(Polygon {
                outer: r,
                holes: Vec::new(),
            });
        } else if let Some(last) = out.last_mut() {
            last.holes.push(r);
        }
    }
    out
}

/// Replace a polygon with its axis-aligned bounding rectangle. The
/// resulting block silhouette has 4 vertices and no holes, which keeps
/// the coarsest LOD level cheap to render (≈12 triangles per building).
fn polygon_to_aabb(polygon: &Polygon) -> Polygon {
    let mut min_x = i32::MAX;
    let mut max_x = i32::MIN;
    let mut min_y = i32::MAX;
    let mut max_y = i32::MIN;
    for p in &polygon.outer {
        if p[0] < min_x {
            min_x = p[0];
        }
        if p[0] > max_x {
            max_x = p[0];
        }
        if p[1] < min_y {
            min_y = p[1];
        }
        if p[1] > max_y {
            max_y = p[1];
        }
    }
    Polygon {
        outer: vec![
            [min_x, min_y],
            [max_x, min_y],
            [max_x, max_y],
            [min_x, max_y],
        ],
        holes: vec![],
    }
}

fn strip_close(ring: &[[i32; 2]]) -> Vec<[i32; 2]> {
    if ring.len() >= 2 && ring.first() == ring.last() {
        ring[..ring.len() - 1].to_vec()
    } else {
        ring.to_vec()
    }
}

fn signed_area_tile(ring: &[[i32; 2]]) -> f64 {
    if ring.len() < 3 {
        return 0.0;
    }
    let mut a: f64 = 0.0;
    for i in 0..ring.len() {
        let p = ring[i];
        let q = ring[(i + 1) % ring.len()];
        a += (p[0] as f64) * (q[1] as f64) - (q[0] as f64) * (p[1] as f64);
    }
    0.5 * a
}

fn polygon_area_m2(polygon: &Polygon, source: &Source<'_>, extent: u32) -> f64 {
    let src_center = coord::tile_center(source.z, source.x, source.y);
    let outer_area = ring_area_m2(&polygon.outer, source, extent, src_center).abs();
    let hole_area: f64 = polygon
        .holes
        .iter()
        .map(|h| ring_area_m2(h, source, extent, src_center).abs())
        .sum();
    (outer_area - hole_area).max(0.0)
}

fn ring_area_m2(ring: &[[i32; 2]], source: &Source<'_>, extent: u32, anchor: LonLat) -> f64 {
    if ring.len() < 3 {
        return 0.0;
    }
    let enu: Vec<[f64; 2]> = ring
        .iter()
        .map(|p| coord::tile_xy_to_enu_at(source.z, source.x, source.y, extent, p[0], p[1], anchor))
        .collect();
    let mut a = 0.0;
    for i in 0..enu.len() {
        let p = enu[i];
        let q = enu[(i + 1) % enu.len()];
        a += p[0] * q[1] - q[0] * p[1];
    }
    0.5 * a
}

/// Lon/lat centroid of a polygon (outer ring only). Sample-quality
/// centroid for terrain lookup — we don't need geometric accuracy here.
fn polygon_centroid_lonlat(polygon: &Polygon, source: &Source<'_>, extent: u32) -> LonLat {
    let mut sum_lon = 0.0f64;
    let mut sum_lat = 0.0f64;
    let n = polygon.outer.len();
    if n == 0 {
        return coord::tile_center(source.z, source.x, source.y);
    }
    for p in &polygon.outer {
        let ll =
            coord::tile_xy_to_lonlat(source.z, source.x, source.y, extent, p[0], p[1]);
        sum_lon += ll.lon_deg;
        sum_lat += ll.lat_deg;
    }
    LonLat {
        lon_deg: sum_lon / n as f64,
        lat_deg: sum_lat / n as f64,
    }
}

#[allow(clippy::too_many_arguments)]
fn extrude_polygon(
    src_z: u8,
    src_x: u32,
    src_y: u32,
    extent: u32,
    polygon: &Polygon,
    anchor: LonLat,
    base_height: f64,
    top_height: f64,
    fid: u16,
    positions: &mut Vec<f32>,
    normals: &mut Vec<f32>,
    feature_ids: &mut Vec<u16>,
    indices: &mut Vec<u32>,
) {
    let outer_enu: Vec<[f64; 2]> = polygon
        .outer
        .iter()
        .map(|p| coord::tile_xy_to_enu_at(src_z, src_x, src_y, extent, p[0], p[1], anchor))
        .collect();
    let hole_enus: Vec<Vec<[f64; 2]>> = polygon
        .holes
        .iter()
        .map(|h| {
            h.iter()
                .map(|p| coord::tile_xy_to_enu_at(src_z, src_x, src_y, extent, p[0], p[1], anchor))
                .collect()
        })
        .collect();
    // ----- roof -----
    let mut flat: Vec<f64> = Vec::new();
    for p in &outer_enu {
        flat.push(p[0]);
        flat.push(p[1]);
    }
    let mut hole_indices: Vec<usize> = Vec::with_capacity(hole_enus.len());
    let mut running = outer_enu.len();
    for h in &hole_enus {
        if h.len() < 3 {
            continue;
        }
        hole_indices.push(running);
        for p in h {
            flat.push(p[0]);
            flat.push(p[1]);
        }
        running += h.len();
    }
    let roof_tris = earcutr::earcut(&flat, &hole_indices, 2).unwrap_or_default();

    if !roof_tris.is_empty() {
        let base = positions.len() as u32 / 3;
        let mut all_pts: Vec<[f64; 2]> = Vec::with_capacity(running);
        all_pts.extend_from_slice(&outer_enu);
        for h in &hole_enus {
            all_pts.extend_from_slice(h);
        }
        for p in &all_pts {
            positions.extend_from_slice(&[p[0] as f32, top_height as f32, -p[1] as f32]);
            normals.extend_from_slice(&[0.0, 1.0, 0.0]);
            feature_ids.push(fid);
        }
        for tri in roof_tris.chunks_exact(3) {
            indices.push(base + tri[0] as u32);
            indices.push(base + tri[1] as u32);
            indices.push(base + tri[2] as u32);
        }
    }

    // ----- outer walls -----
    extrude_ring_walls(
        &outer_enu,
        None,
        base_height,
        top_height,
        fid,
        positions,
        normals,
        feature_ids,
        indices,
    );
    for h in &hole_enus {
        extrude_ring_walls(
            h,
            None,
            base_height,
            top_height,
            fid,
            positions,
            normals,
            feature_ids,
            indices,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn extrude_ring_walls(
    enu: &[[f64; 2]],
    skip_edges: Option<&[bool]>,
    base_height: f64,
    top_height: f64,
    fid: u16,
    positions: &mut Vec<f32>,
    normals: &mut Vec<f32>,
    feature_ids: &mut Vec<u16>,
    indices: &mut Vec<u32>,
) {
    if enu.len() < 3 {
        return;
    }
    let mut a_enu = 0.0;
    for i in 0..enu.len() {
        let p = enu[i];
        let q = enu[(i + 1) % enu.len()];
        a_enu += p[0] * q[1] - q[0] * p[1];
    }
    let reverse = a_enu > 0.0;
    let n = enu.len();
    for i in 0..n {
        if let Some(skips) = skip_edges {
            let original_idx = if reverse { (n - 1 - i) % n } else { i };
            if skips[original_idx] {
                continue;
            }
        }
        let ai = if reverse { n - 1 - i } else { i };
        let bi = if reverse {
            (n + n - 2 - i) % n
        } else {
            (i + 1) % n
        };
        let a = enu[ai];
        let b = enu[bi];
        let dx = b[0] - a[0];
        let dz_n = b[1] - a[1];
        let len = (dx * dx + dz_n * dz_n).sqrt().max(1e-9);
        let nx = dz_n / len;
        let nz = -dx / len;
        let base = positions.len() as u32 / 3;
        let verts = [
            [a[0] as f32, base_height as f32, -a[1] as f32],
            [b[0] as f32, base_height as f32, -b[1] as f32],
            [b[0] as f32, top_height as f32, -b[1] as f32],
            [a[0] as f32, top_height as f32, -a[1] as f32],
        ];
        let n_vec = [-nx as f32, 0.0, nz as f32];
        for v in &verts {
            positions.extend_from_slice(v);
            normals.extend_from_slice(&n_vec);
            feature_ids.push(fid);
        }
        indices.extend_from_slice(&[base, base + 2, base + 1, base, base + 3, base + 2]);
    }
}

/// In-place mesh decimation via meshopt. See lib.rs caller for rationale.
pub fn simplify_mesh(mesh: &mut Mesh, ratio: f32, target_error_m: f32) {
    if ratio >= 1.0 || mesh.indices.len() < 6 {
        return;
    }
    let target = ((mesh.indices.len() as f32 * ratio) as usize).max(3) / 3 * 3;
    let pos_bytes: &[u8] = bytemuck_slice(&mesh.positions);
    let adapter = match meshopt::VertexDataAdapter::new(pos_bytes, 12, 0) {
        Ok(a) => a,
        Err(_) => return,
    };
    let new_indices =
        meshopt::simplify_sloppy(&mesh.indices, &adapter, target, target_error_m, None);
    if !new_indices.is_empty() {
        mesh.indices = new_indices;
    }
}

fn bytemuck_slice(v: &[f32]) -> &[u8] {
    unsafe { std::slice::from_raw_parts(v.as_ptr() as *const u8, std::mem::size_of_val(v)) }
}
