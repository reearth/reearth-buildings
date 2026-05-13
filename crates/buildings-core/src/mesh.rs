//! Extrude MVT building polygons (possibly drawn from several source
//! tiles) into a flat-shaded triangle mesh anchored at an output tile.
//!
//! Output frame: tile-local ENU metres (east/north/up), centred on the
//! output tile's centre. y is up in the glb-friendly remapping below.

use crate::coord::{self, LonLat};
use mvt_decoder::{BuildingFeature, DecodedTile};

/// Per-building metadata, indexed by FEATURE_ID written into the mesh.
#[derive(Debug, Clone, Default)]
pub struct FeatureProps {
    pub osm_id: Option<i64>,
    pub name: Option<String>,
    pub kind: Option<String>,
    pub building: Option<String>,
    pub height_m: f32,
    pub min_height_m: f32,
    pub levels: u16,
    pub footprint_m2: f32,
}

pub struct Mesh {
    /// Flattened [x,y,z, x,y,z, ...] in metres (ENU; y=up).
    pub positions: Vec<f32>,
    /// Per-vertex normal, parallel to `positions`.
    pub normals: Vec<f32>,
    /// Per-vertex FEATURE_ID_0 (u16). Index into `features`.
    pub feature_ids: Vec<u16>,
    /// Triangle indices into the position array.
    pub indices: Vec<u32>,
    /// One entry per building feature.
    pub features: Vec<FeatureProps>,
}

/// A source MVT tile contributing geometry. For z=14 single-tile renders
/// you pass one. For an aggregated parent (e.g. z=13) you pass the four
/// z=14 children whose union covers the parent.
pub struct Source<'a> {
    pub z: u8,
    pub x: u32,
    pub y: u32,
    pub tile: &'a DecodedTile,
}

/// Inclusive [`min_area_m2`, `max_area_m2`) filter. Either bound set to
/// `0.0` disables that end of the range.
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

pub fn default_height_meters(feat: &BuildingFeature) -> f64 {
    match (feat.height, feat.levels) {
        (Some(h), _) if h > 0.0 => h,
        (_, Some(l)) if l > 0.0 => l * 3.0,
        _ => 6.0,
    }
}

pub fn build_mesh(
    out_z: u8,
    out_x: u32,
    out_y: u32,
    sources: &[Source<'_>],
    filter: AreaFilter,
) -> Mesh {
    let anchor = coord::tile_center(out_z, out_x, out_y);

    let mut positions: Vec<f32> = Vec::new();
    let mut normals: Vec<f32> = Vec::new();
    let mut feature_ids: Vec<u16> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();
    let mut features: Vec<FeatureProps> = Vec::new();

    for source in sources {
        for feat in &source.tile.buildings {
            let polygons = group_polygons(&feat.rings);
            if polygons.is_empty() {
                continue;
            }
            let area = polygons
                .iter()
                .map(|p| polygon_area_m2(p, source, source.tile.extent))
                .sum::<f64>() as f32;
            if !filter.accepts(area) {
                continue;
            }

            let height = default_height_meters(feat);
            let min_h = feat.min_height.unwrap_or(0.0);
            let fid = features.len().min(u16::MAX as usize - 1) as u16;
            features.push(FeatureProps {
                osm_id: feat.id.map(|v| v as i64),
                name: feat.name.clone(),
                kind: feat.kind.clone(),
                building: feat.building.clone(),
                height_m: height as f32,
                min_height_m: min_h as f32,
                levels: feat
                    .levels
                    .unwrap_or(0.0)
                    .round()
                    .clamp(0.0, u16::MAX as f64) as u16,
                footprint_m2: area,
            });

            for polygon in &polygons {
                extrude_polygon(
                    source.z,
                    source.x,
                    source.y,
                    source.tile.extent,
                    polygon,
                    anchor,
                    min_h,
                    height,
                    fid,
                    &mut positions,
                    &mut normals,
                    &mut feature_ids,
                    &mut indices,
                );
            }
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

/// One outer ring with zero or more holes. All vertex coords are in tile
/// units; the closing duplicate vertex (when present) has been stripped.
struct Polygon {
    outer: Vec<[i32; 2]>,
    holes: Vec<Vec<[i32; 2]>>,
}

/// Group MVT rings into polygons using the signed-area sign convention
/// (spec §4.3.4.4): A > 0 in tile coords starts a new outer ring; A < 0
/// is a hole attached to the most recent outer.
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

/// Footprint area in m². Computes in the source tile's own ENU frame
/// (anchor-independent), subtracting hole areas.
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

    extrude_ring_walls(
        &outer_enu,
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
    let ring: Vec<[f64; 2]> = if a_enu > 0.0 {
        enu.iter().rev().copied().collect()
    } else {
        enu.to_vec()
    };
    let enu = &ring[..];
    for i in 0..enu.len() {
        let a = enu[i];
        let b = enu[(i + 1) % enu.len()];
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
        let n = [nx as f32, 0.0, -nz as f32];
        for v in &verts {
            positions.extend_from_slice(v);
            normals.extend_from_slice(&n);
            feature_ids.push(fid);
        }
        indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }
}
