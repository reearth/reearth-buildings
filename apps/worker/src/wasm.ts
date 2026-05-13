// The Rust crate `buildings-wasm` is built via `wasm-pack --target bundler`
// and emits an ES module at `../wasm/`. wrangler / esbuild bundles the .wasm
// alongside the worker bundle.
import { render_glb } from "../wasm/buildings_wasm";

export function renderGlbWasm(mvt: Uint8Array, z: number, x: number, y: number): Uint8Array {
  return render_glb(mvt, z, x, y);
}
