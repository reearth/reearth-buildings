//! End-to-end glb tile builder for Overture Maps building MVTs, with
//! per-building ground elevation pulled from a Re:Earth Terrain
//! Terrarium WebP tile.

use bytes::Bytes;

pub mod coord;
pub mod glb;
pub mod height_config;
pub mod mesh;

pub use height_config::{
    FootprintBucket, FootprintCurve, FootprintTable, HeightConfig, UrbanThresholds,
};
pub use mesh::AreaFilter;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Mvt(#[from] mvt_decoder::Error),
    #[error(transparent)]
    Terrain(#[from] terrain_decoder::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

pub struct MvtInput<'a> {
    pub z: u8,
    pub x: u32,
    pub y: u32,
    pub bytes: &'a [u8],
}

/// Optional terrain raster covering the output tile.
pub struct TerrainInput<'a> {
    pub z: u8,
    pub x: u32,
    pub y: u32,
    /// Mapzen Terrarium-encoded WebP bytes from
    /// `terrain.reearth.land/terrarium/ellipsoid/{z}/{x}/{y}.webp`.
    pub webp: &'a [u8],
}

/// Render an output tile from one or more source MVT tiles. Buildings
/// are placed on the ground using `terrain` (sample at each building's
/// centroid). Pass `None` to anchor everything at ellipsoidal h=0.
///
/// `simplify_ratio < 1.0` hands the mesh to `meshopt::simplify` to drop
/// triangles while staying within `simplify_target_error_m` metres of
/// the original surface; useful for parent (z<MAX_Z) tiles.
#[allow(clippy::too_many_arguments)]
pub fn render_glb_lod(
    out_z: u8,
    out_x: u32,
    out_y: u32,
    sources: &[MvtInput<'_>],
    filter: AreaFilter,
    simplify_ratio: f32,
    simplify_target_error_m: f32,
    // When true, every polygon is collapsed to its axis-aligned bounding
    // rectangle before extrusion. Produces a "block mesh" silhouette for
    // the coarsest LOD level without invoking meshopt simplify.
    aabb_only: bool,
    terrain: Option<TerrainInput<'_>>,
) -> Result<Bytes> {
    let decoded: Vec<(u8, u32, u32, mvt_decoder::DecodedTile)> = sources
        .iter()
        .map(|s| Ok((s.z, s.x, s.y, mvt_decoder::decode_buildings(s.bytes)?)))
        .collect::<Result<_>>()?;
    let mesh_sources: Vec<mesh::Source<'_>> = decoded
        .iter()
        .map(|(z, x, y, t)| mesh::Source {
            z: *z,
            x: *x,
            y: *y,
            tile: t,
        })
        .collect();

    let terrain_tile = match terrain {
        Some(t) => Some(terrain_decoder::decode_terrarium_webp(
            t.z, t.x, t.y, t.webp,
        )?),
        None => None,
    };

    let height_config = HeightConfig::default();
    let mut mesh = mesh::build_mesh(
        out_z,
        out_x,
        out_y,
        &mesh_sources,
        filter,
        aabb_only,
        terrain_tile.as_ref(),
        &height_config,
    );

    if simplify_ratio > 0.0 && simplify_ratio < 1.0 {
        mesh::simplify_mesh(&mut mesh, simplify_ratio, simplify_target_error_m);
    }

    let center = coord::tile_center(out_z, out_x, out_y);
    // Origin sits on the ellipsoid (h=0). Ground elevation per building
    // is already baked into vertex positions during build_mesh.
    let m = coord::enu_to_ecef_matrix(center);
    let local_yup = remap_yup_to_enu(m);
    let transform = apply_zup_to_yup_to_output(local_yup);

    let bytes = glb::write_glb(&mesh, transform);
    Ok(Bytes::from(bytes))
}

fn remap_yup_to_enu(enu_to_ecef: [f64; 16]) -> [f64; 16] {
    let col = |c: usize| {
        [
            enu_to_ecef[c * 4],
            enu_to_ecef[c * 4 + 1],
            enu_to_ecef[c * 4 + 2],
            enu_to_ecef[c * 4 + 3],
        ]
    };
    let east = col(0);
    let north = col(1);
    let up = col(2);
    let tr = col(3);
    let neg_north = [-north[0], -north[1], -north[2], north[3]];
    let mut out = [0.0f64; 16];
    out[0..4].copy_from_slice(&east);
    out[4..8].copy_from_slice(&up);
    out[8..12].copy_from_slice(&neg_north);
    out[12..16].copy_from_slice(&tr);
    out
}

fn apply_zup_to_yup_to_output(m: [f64; 16]) -> [f64; 16] {
    let mut out = [0.0f64; 16];
    for i in 0..4 {
        out[i * 4] = m[i * 4];
        out[i * 4 + 1] = m[i * 4 + 2];
        out[i * 4 + 2] = -m[i * 4 + 1];
        out[i * 4 + 3] = m[i * 4 + 3];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_mvt_produces_valid_glb() {
        let glb = render_glb_lod(
            14,
            14552,
            6451,
            &[MvtInput {
                z: 14,
                x: 14552,
                y: 6451,
                bytes: &[],
            }],
            AreaFilter::default(),
            1.0,
            0.0,
            false,
            None,
        )
        .unwrap();
        assert!(glb.len() > 20);
        assert_eq!(&glb[0..4], b"glTF");
    }
}
