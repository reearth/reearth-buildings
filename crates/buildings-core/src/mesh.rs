//! Extrude MVT building polygons into a flat-shaded triangle mesh.
//!
//! Output frame: tile-local ENU metres (east/north/up), centred on the
//! tile centre. y is up.

use crate::coord;
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

pub fn default_height_meters(feat: &BuildingFeature) -> f64 {
    match (feat.height, feat.levels) {
        (Some(h), _) if h > 0.0 => h,
        (_, Some(l)) if l > 0.0 => l * 3.0,
        _ => 6.0,
    }
}

pub fn build_mesh(z: u8, x: u32, y: u32, tile: &DecodedTile) -> Mesh {
    let mut positions: Vec<f32> = Vec::new();
    let mut normals: Vec<f32> = Vec::new();
    let mut feature_ids: Vec<u16> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();
    let mut features: Vec<FeatureProps> = Vec::new();

    for feat in &tile.buildings {
        let height = default_height_meters(feat);
        let min_h = feat.min_height.unwrap_or(0.0);
        // Cap feature count at u16::MAX − 1; spill silently into id=u16::MAX
        // (a "miscellaneous" bucket). At z=14 a single tile holds far fewer
        // than 65 535 buildings in practice.
        let fid = features.len().min(u16::MAX as usize - 1) as u16;
        let props = FeatureProps {
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
        };
        features.push(props);

        for ring in &feat.rings {
            extrude_ring(
                z,
                x,
                y,
                tile.extent,
                ring,
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
    Mesh {
        positions,
        normals,
        feature_ids,
        indices,
        features,
    }
}

#[allow(clippy::too_many_arguments)]
fn extrude_ring(
    z: u8,
    tx: u32,
    ty: u32,
    extent: u32,
    ring: &[[i32; 2]],
    base_height: f64,
    top_height: f64,
    fid: u16,
    positions: &mut Vec<f32>,
    normals: &mut Vec<f32>,
    feature_ids: &mut Vec<u16>,
    indices: &mut Vec<u32>,
) {
    if ring.len() < 4 {
        return;
    }
    let ring_no_close = if ring.first() == ring.last() {
        &ring[..ring.len() - 1]
    } else {
        ring
    };
    if ring_no_close.len() < 3 {
        return;
    }

    let enu: Vec<[f64; 2]> = ring_no_close
        .iter()
        .map(|p| coord::tile_xy_to_enu(z, tx, ty, extent, p[0], p[1]))
        .collect();

    // ----- roof (top cap) -----
    let mut flat: Vec<f64> = Vec::with_capacity(enu.len() * 2);
    for p in &enu {
        flat.push(p[0]);
        flat.push(p[1]);
    }
    let roof_tris = earcutr::earcut(&flat, &[], 2).unwrap_or_default();
    if !roof_tris.is_empty() {
        let base = positions.len() as u32 / 3;
        for p in &enu {
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

    // ----- walls -----
    for i in 0..enu.len() {
        let a = enu[i];
        let b = enu[(i + 1) % enu.len()];
        let dx = b[0] - a[0];
        let dz_n = b[1] - a[1];
        let len = (dx * dx + dz_n * dz_n).sqrt().max(1e-9);
        let nx = (dz_n) / len;
        let nz = (-dx) / len;
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
