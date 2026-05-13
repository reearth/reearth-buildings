//! End-to-end glb tile builder.
//!
//! Pipeline: MVT buildings layer (gzipped or not) →
//!   triangulated, extruded mesh in tile-local ENU →
//!   glb (glTF 2.0 binary) with a root-node ENU→ECEF matrix.

use bytes::Bytes;

pub mod coord;
pub mod glb;
pub mod mesh;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Mvt(#[from] mvt_decoder::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

pub struct TileCoord {
    pub z: u8,
    pub x: u32,
    pub y: u32,
}

pub fn render_glb(coord: TileCoord, mvt: &[u8]) -> Result<Bytes> {
    let tile = mvt_decoder::decode_buildings(mvt)?;
    let mesh = mesh::build_mesh(coord.z, coord.x, coord.y, &tile);

    let center = self::coord::tile_center(coord.z, coord.x, coord.y);
    // glTF node matrix expects a "y-up local frame", but our mesh writes its
    // local positions as (east, up, -north) so that y=up matches viewers. We
    // therefore need a matrix that maps that frame back to ECEF: rebuild it
    // by swapping the basis columns relative to enu_to_ecef.
    let m = self::coord::enu_to_ecef_matrix(center);
    let transform = remap_yup_to_enu(m);

    let bytes = glb::write_glb(&mesh, transform);
    Ok(Bytes::from(bytes))
}

/// Our mesh uses (x=east, y=up, z=-north). Convert the ENU→ECEF matrix
/// (which maps (east,north,up) columns) to one that takes (east,up,-north).
fn remap_yup_to_enu(enu_to_ecef: [f64; 16]) -> [f64; 16] {
    // Original columns (column-major): col0=east, col1=north, col2=up, col3=translation.
    // Target columns: col0=east, col1=up, col2=-north, col3=translation.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_mvt_produces_valid_glb() {
        let glb = render_glb(
            TileCoord {
                z: 14,
                x: 14552,
                y: 6451,
            },
            &[],
        )
        .unwrap();
        assert!(glb.len() > 28); // header + at least both chunk headers
        assert_eq!(&glb[0..4], b"glTF");
    }
}
