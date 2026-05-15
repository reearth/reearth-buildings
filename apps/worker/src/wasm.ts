// Wrangler imports `.wasm` files as a compiled `WebAssembly.Module`. We
// pass that into wasm-bindgen's synchronous `initSync` once at module
// load and then re-export the typed wrapper.

import { initSync, render_glb_lod } from "../wasm/buildings_wasm";
import wasmModule from "../wasm/buildings_wasm_bg.wasm";

initSync({ module: wasmModule });

export interface SourceTile {
  mvt: Uint8Array;
  z: number;
  x: number;
  y: number;
}

export interface AreaFilter {
  /** 0 to disable. */
  minM2: number;
  /** 0 to disable. */
  maxM2: number;
}

export interface SimplifyOptions {
  /** 1.0 keeps every triangle; lower drops them. */
  ratio: number;
  /** Max allowed geometric deviation in metres. Ignored when ratio = 1. */
  targetErrorM: number;
}

export interface TerrainTile {
  /** Mapzen Terrarium-encoded WebP bytes. Empty array = skip terrain. */
  webp: Uint8Array;
  z: number;
  x: number;
  y: number;
}

export function renderGlbWasm(
  sources: SourceTile[],
  out: { z: number; x: number; y: number },
  filter: AreaFilter,
  simplify: SimplifyOptions = { ratio: 1, targetErrorM: 0 },
  aabbOnly = false,
  terrain: TerrainTile | null = null,
): Uint8Array {
  const totalLen = sources.reduce((s, src) => s + src.mvt.length, 0);
  const concat = new Uint8Array(totalLen);
  const lens = new Uint32Array(sources.length);
  const tiles = new Uint32Array(sources.length * 3);
  let off = 0;
  for (let i = 0; i < sources.length; i++) {
    const s = sources[i]!;
    concat.set(s.mvt, off);
    off += s.mvt.length;
    lens[i] = s.mvt.length;
    tiles[i * 3] = s.z;
    tiles[i * 3 + 1] = s.x;
    tiles[i * 3 + 2] = s.y;
  }
  const terrainBytes = terrain?.webp ?? new Uint8Array(0);
  const terrainZ = terrain?.z ?? 0;
  const terrainX = terrain?.x ?? 0;
  const terrainY = terrain?.y ?? 0;
  return render_glb_lod(
    concat,
    lens,
    tiles,
    out.z,
    out.x,
    out.y,
    filter.minM2,
    filter.maxM2,
    simplify.ratio,
    simplify.targetErrorM,
    aabbOnly,
    terrainBytes,
    terrainZ,
    terrainX,
    terrainY,
  );
}
