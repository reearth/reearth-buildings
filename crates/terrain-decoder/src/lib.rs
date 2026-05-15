//! Mapzen Terrarium tile decoder for the Re:Earth Terrain ellipsoid
//! variant (`terrain.reearth.land/terrarium/ellipsoid/{z}/{x}/{y}.webp`).
//!
//! Tile encoding (per pixel, RGB):
//!   elevation_m = (R * 256 + G + B / 256) - 32768
//!
//! The `ellipsoid` variant returns WGS84 ellipsoidal heights — the geoid
//! (EGM2008) is already blended in upstream — so values are directly
//! usable as the `h` argument to the WGS84 → ECEF transform.

use image_webp::WebPDecoder;
use std::io::Cursor;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("webp decode: {0}")]
    Webp(#[from] image_webp::DecodingError),
    #[error("unexpected pixel format")]
    PixelFormat,
}

pub type Result<T> = std::result::Result<T, Error>;

/// Decoded raster of one terrain tile, kept as elevation metres
/// (ellipsoidal). Stored in row-major order, top-row (north) first.
pub struct TerrainTile {
    pub width: u32,
    pub height: u32,
    /// Slippy tile coords this raster came from. The sampler uses these
    /// to map (lon, lat) → pixel coords.
    pub z: u8,
    pub x: u32,
    pub y: u32,
    /// Per-pixel elevation in metres. `width * height` entries.
    pub elev: Vec<f32>,
}

/// Decode a Mapzen Terrarium WebP tile into per-pixel elevations.
pub fn decode_terrarium_webp(z: u8, x: u32, y: u32, bytes: &[u8]) -> Result<TerrainTile> {
    let mut decoder = WebPDecoder::new(Cursor::new(bytes))?;
    let (w, h) = decoder.dimensions();
    // Lossless WebP from the Re:Earth Terrain pipeline always carries an
    // alpha channel (RGBA), but be tolerant of plain RGB too.
    let has_alpha = decoder.has_alpha();
    let bpp: usize = if has_alpha { 4 } else { 3 };
    let mut buf = vec![0u8; (w as usize) * (h as usize) * bpp];
    decoder.read_image(&mut buf)?;

    let pixels = (w as usize) * (h as usize);
    let mut elev = Vec::with_capacity(pixels);
    for i in 0..pixels {
        let base = i * bpp;
        let r = buf[base] as f32;
        let g = buf[base + 1] as f32;
        let b = buf[base + 2] as f32;
        // Mapzen Terrarium decoding.
        elev.push((r * 256.0 + g + b / 256.0) - 32768.0);
    }

    Ok(TerrainTile {
        width: w,
        height: h,
        z,
        x,
        y,
        elev,
    })
}

impl TerrainTile {
    /// Bilinearly sample the elevation at (lon_deg, lat_deg). Coordinates
    /// outside the tile are clamped to the nearest edge pixel — callers
    /// should pick a tile that actually contains the lookup point.
    pub fn sample(&self, lon_deg: f64, lat_deg: f64) -> f32 {
        let n = (1u64 << self.z) as f64;
        // Pixel coord (fractional) within this tile.
        let world_x = (lon_deg + 180.0) / 360.0 * n;
        let lat_rad = lat_deg.to_radians();
        let world_y =
            (1.0 - (lat_rad.tan() + 1.0 / lat_rad.cos()).ln() / std::f64::consts::PI) / 2.0 * n;
        let fx = (world_x - self.x as f64) * self.width as f64;
        let fy = (world_y - self.y as f64) * self.height as f64;

        let w = self.width as i64;
        let h = self.height as i64;
        let x0 = (fx.floor() as i64).clamp(0, w - 1);
        let y0 = (fy.floor() as i64).clamp(0, h - 1);
        let x1 = (x0 + 1).min(w - 1);
        let y1 = (y0 + 1).min(h - 1);
        let dx = (fx - x0 as f64).clamp(0.0, 1.0) as f32;
        let dy = (fy - y0 as f64).clamp(0.0, 1.0) as f32;

        let idx = |xi: i64, yi: i64| (yi * w + xi) as usize;
        let v00 = self.elev[idx(x0, y0)];
        let v10 = self.elev[idx(x1, y0)];
        let v01 = self.elev[idx(x0, y1)];
        let v11 = self.elev[idx(x1, y1)];
        let top = v00 * (1.0 - dx) + v10 * dx;
        let bot = v01 * (1.0 - dx) + v11 * dx;
        top * (1.0 - dy) + bot * dy
    }
}
