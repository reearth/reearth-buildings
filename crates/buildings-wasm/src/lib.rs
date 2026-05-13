//! WASM bridge: JS hands us MVT bytes + tile coords, we hand back glb bytes.
//!
//! Everything I/O-related stays in the TypeScript Worker. This module exists
//! purely to keep the hot path (MVT decode → extrusion → glb encode) in
//! native-speed Rust.

use wasm_bindgen::prelude::*;

#[wasm_bindgen(start)]
pub fn _start() {
    console_error_panic_hook::set_once();
}

/// Render a single building tile.
///
/// `mvt` is the (gunzipped) MVT byte slice for the tile at `z/x/y`.
/// Returns a glb (glTF 2.0 binary) byte vector.
#[wasm_bindgen]
pub fn render_glb(mvt: &[u8], z: u8, x: u32, y: u32) -> std::result::Result<Vec<u8>, JsError> {
    let coord = buildings_core::TileCoord { z, x, y };
    let bytes = buildings_core::render_glb(coord, mvt).map_err(|e| JsError::new(&e.to_string()))?;
    Ok(bytes.to_vec())
}
