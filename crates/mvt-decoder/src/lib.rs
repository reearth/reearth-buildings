//! Mapbox Vector Tile (MVT) decoder, scoped to extracting the `buildings`
//! layer from Protomaps planet tiles.

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid mvt: {0}")]
    Invalid(&'static str),
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, Default)]
pub struct BuildingFeature {
    pub rings: Vec<Vec<[f64; 2]>>,
    pub height: Option<f64>,
    pub min_height: Option<f64>,
}

pub fn decode_buildings(_tile_bytes: &[u8]) -> Result<Vec<BuildingFeature>> {
    Ok(Vec::new())
}
