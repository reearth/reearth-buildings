//! Extrude MVT building polygons into a flat-shaded triangle mesh.
//!
//! Output frame: tile-local ENU metres (east/north/up), centred on the
//! tile centre. y is up.

use crate::coord;
use mvt_decoder::{BuildingFeature, DecodedTile};

pub struct Mesh {
    /// Flattened [x,y,z, x,y,z, ...] in metres (ENU; y=up).
    pub positions: Vec<f32>,
    /// Per-vertex normal, parallel to `positions`.
    pub normals: Vec<f32>,
    /// Triangle indices into the position array.
    pub indices: Vec<u32>,
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
    let mut indices: Vec<u32> = Vec::new();

    for feat in &tile.buildings {
        let height = default_height_meters(feat);
        let min_h = feat.min_height.unwrap_or(0.0);
        for ring in &feat.rings {
            extrude_ring(
                z,
                x,
                y,
                tile.extent,
                ring,
                min_h,
                height,
                &mut positions,
                &mut normals,
                &mut indices,
            );
        }
    }
    Mesh {
        positions,
        normals,
        indices,
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
    positions: &mut Vec<f32>,
    normals: &mut Vec<f32>,
    indices: &mut Vec<u32>,
) {
    if ring.len() < 4 {
        return; // not a closed ring
    }
    // Drop the explicit closing duplicate vertex if present.
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
            // ENU has +Y=north, +Z=up. We map to glTF (+Y=up, -Z=north) so
            // viewers display "up" correctly. The outer node matrix corrects
            // back to ECEF.
            normals.extend_from_slice(&[0.0, 1.0, 0.0]);
        }
        // earcutr returns CCW indices in the input (east, north) plane.
        // Mapping (e, n) → (x=e, y=top, z=-n) flips the visual winding
        // when projected to (x, z), so triangles end up CCW as seen from
        // +Y (above) with the original order — the geometric normal
        // already points up. Keep the order.
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
        // outward normal: rotate edge by -90deg in xz plane (right-hand)
        let dx = b[0] - a[0];
        let dz_n = b[1] - a[1];
        // edge length
        let len = (dx * dx + dz_n * dz_n).sqrt().max(1e-9);
        let nx = (dz_n) / len;
        let nz = (-dx) / len;
        let base = positions.len() as u32 / 3;
        // 4 verts: a_bot, b_bot, b_top, a_top (glTF: y=up, z=south)
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
        }
        indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }
}
