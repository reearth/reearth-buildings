//! WASM bridge: JS hands us one-or-more MVT byte blobs + their tile coords,
//! we hand back glb bytes.

use std::sync::Once;
use wasm_bindgen::prelude::*;

// wasm-bindgen's auto-called `#[wasm_bindgen(start)]` hooks are skipped
// by some hosts (workerd, in particular), leaving panics unintelligible.
// Initialise lazily from the first WASM entry point instead.
static INIT: Once = Once::new();

fn ensure_init() {
    INIT.call_once(|| {
        console_error_panic_hook::set_once();
    });
}

// meshoptimizer's C++ allocator pulls in `operator new` / `operator delete`,
// which on wasm32-unknown-unknown have no implementation and would show up
// as `env._Znwm` / `env._ZdlPv` imports — which a bundler-target glb does
// not provide. Backing them with Rust's global allocator keeps everything
// self-contained. We stash the allocation size right before the user
// pointer so the sizeless delete operator can recover a Layout.
#[cfg(target_arch = "wasm32")]
mod cxx_runtime {
    use std::alloc::{alloc, dealloc, Layout};

    const HEADER: usize = std::mem::size_of::<usize>();

    fn layout(size: usize) -> Layout {
        Layout::from_size_align(size + HEADER, std::mem::align_of::<usize>()).unwrap()
    }

    /// `operator new(size_t)`
    #[no_mangle]
    pub unsafe extern "C" fn _Znwm(size: usize) -> *mut u8 {
        let p = alloc(layout(size));
        if p.is_null() {
            return p;
        }
        *(p as *mut usize) = size;
        p.add(HEADER)
    }

    /// `operator delete(void*)`
    #[no_mangle]
    pub unsafe extern "C" fn _ZdlPv(ptr: *mut u8) {
        if ptr.is_null() {
            return;
        }
        let base = ptr.sub(HEADER);
        let size = *(base as *mut usize);
        dealloc(base, layout(size));
    }

    /// `operator delete(void*, size_t)` — sized form. C++14+ may emit this.
    #[no_mangle]
    pub unsafe extern "C" fn _ZdlPvm(ptr: *mut u8, _size: usize) {
        _ZdlPv(ptr);
    }
}

/// Render a glb for the output tile, aggregating geometry from any number
/// of source MVT tiles and filtering by footprint area.
///
/// JS encodes the source list as three parallel arrays:
/// * `mvts_concat`: bytes for every source, concatenated in order.
/// * `mvt_lens`: byte length of each MVT, parallel to `src_tiles`.
/// * `src_tiles`: flat `[z0, x0, y0, z1, x1, y1, …]` for each MVT.
///
/// Setting `max_area_m2` (or `min_area_m2`) to `0` disables that bound.
#[allow(clippy::too_many_arguments)]
#[wasm_bindgen]
pub fn render_glb_lod(
    mvts_concat: &[u8],
    mvt_lens: &[u32],
    src_tiles: &[u32],
    out_z: u8,
    out_x: u32,
    out_y: u32,
    min_area_m2: f32,
    max_area_m2: f32,
    simplify_ratio: f32,
    simplify_target_error_m: f32,
    geoid_offset_m: f32,
    aabb_only: bool,
) -> std::result::Result<Vec<u8>, JsError> {
    ensure_init();
    if mvt_lens.len() * 3 != src_tiles.len() {
        return Err(JsError::new("mvt_lens / src_tiles length mismatch"));
    }
    let mut offset = 0usize;
    let mut inputs: Vec<buildings_core::MvtInput<'_>> = Vec::with_capacity(mvt_lens.len());
    for (i, &len) in mvt_lens.iter().enumerate() {
        let end = offset
            .checked_add(len as usize)
            .ok_or_else(|| JsError::new("mvt offset overflow"))?;
        if end > mvts_concat.len() {
            return Err(JsError::new("mvts_concat truncated relative to mvt_lens"));
        }
        inputs.push(buildings_core::MvtInput {
            z: src_tiles[i * 3] as u8,
            x: src_tiles[i * 3 + 1],
            y: src_tiles[i * 3 + 2],
            bytes: &mvts_concat[offset..end],
        });
        offset = end;
    }
    let filter = buildings_core::AreaFilter {
        min_m2: min_area_m2,
        max_m2: max_area_m2,
    };
    let bytes = buildings_core::render_glb_lod(
        out_z,
        out_x,
        out_y,
        &inputs,
        filter,
        simplify_ratio,
        simplify_target_error_m,
        geoid_offset_m,
        aabb_only,
    )
    .map_err(|e| JsError::new(&e.to_string()))?;
    Ok(bytes.into())
}
