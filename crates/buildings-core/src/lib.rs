//! buildings-core
//!
//! Pipeline: MVT buildings layer → extruded triangle mesh → glb (glTF 2.0
//! binary), emitted as a 3D Tiles 1.1 content payload.

use bytes::Bytes;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Mvt(#[from] mvt_decoder::Error),
    #[error(transparent)]
    PmTiles(#[from] pmtiles_reader::Error),
    #[error("encode: {0}")]
    Encode(&'static str),
}

pub type Result<T> = std::result::Result<T, Error>;

pub struct TileCoord {
    pub z: u8,
    pub x: u32,
    pub y: u32,
}

pub fn default_height_meters(levels: Option<f64>, raw_height: Option<f64>) -> f64 {
    match (raw_height, levels) {
        (Some(h), _) => h,
        (None, Some(l)) => l * 3.0,
        (None, None) => 6.0,
    }
}

pub async fn render_glb(_coord: TileCoord, _mvt: &[u8]) -> Result<Bytes> {
    Err(Error::Encode("render_glb not implemented yet"))
}
