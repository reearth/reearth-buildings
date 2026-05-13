#!/usr/bin/env node
// Fetch source MVTs from Protomaps for an output tile (z=13 or z=14), run
// the WASM pipeline, and write the resulting glb to disk. The script
// mirrors what the worker does so we can validate LOD output locally.
//
// Usage:
//   node scripts/fetch-and-render.mjs [z x y] [pmtiles-url]
// Defaults to z=14 over Tokyo Station.

import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { FetchSource, PMTiles } from "pmtiles";

const MAX_Z = 14;
const MIN_Z = 13;
const THRESHOLD_M2 = 2000;

const here = dirname(fileURLToPath(import.meta.url));
const [, , zArg, xArg, yArg, urlArg] = process.argv;
const z = Number(zArg ?? 14);
const x = Number(xArg ?? 14552);
const y = Number(yArg ?? 6451);
const url = urlArg ?? "https://build.protomaps.com/20260513.pmtiles";

if (z < MIN_Z || z > MAX_Z) {
  console.error(`only z=${MIN_Z}..${MAX_Z} is supported`);
  process.exit(2);
}

console.log(`fetching ${url} for output z=${z} x=${x} y=${y}`);
const pm = new PMTiles(new FetchSource(url));

const factor = 2 ** (MAX_Z - z);
const sourceCoords = [];
for (let dx = 0; dx < factor; dx++) {
  for (let dy = 0; dy < factor; dy++) {
    sourceCoords.push({ z: MAX_Z, x: x * factor + dx, y: y * factor + dy });
  }
}

const sources = [];
for (const sc of sourceCoords) {
  const tile = await pm.getZxy(sc.z, sc.x, sc.y);
  if (!tile) continue;
  sources.push({ mvt: new Uint8Array(tile.data), ...sc });
}
console.log(`source tiles: ${sources.length}/${sourceCoords.length}`);
if (sources.length === 0) {
  console.error("no source tiles available");
  process.exit(3);
}

const wasmModule = await import(resolve(here, "../wasm/buildings_wasm.js"));
const { render_glb_lod } = wasmModule;

const totalLen = sources.reduce((s, t) => s + t.mvt.length, 0);
const concat = new Uint8Array(totalLen);
const lens = new Uint32Array(sources.length);
const tiles = new Uint32Array(sources.length * 3);
let off = 0;
for (let i = 0; i < sources.length; i++) {
  const s = sources[i];
  concat.set(s.mvt, off);
  off += s.mvt.length;
  lens[i] = s.mvt.length;
  tiles[i * 3] = s.z;
  tiles[i * 3 + 1] = s.x;
  tiles[i * 3 + 2] = s.y;
}

const filter = z === MAX_Z ? { min: 0, max: THRESHOLD_M2 } : { min: THRESHOLD_M2, max: 0 };
console.log(`filter min=${filter.min} max=${filter.max} m²`);

// Simplify off by default; pass --simplify <ratio> <errorM> to try it.
const simplifyArgs = process.argv.slice(6);
const simplifyRatio = simplifyArgs[0] ? Number(simplifyArgs[0]) : 1;
const simplifyError = simplifyArgs[1] ? Number(simplifyArgs[1]) : 0;

const t0 = performance.now();
const glb = render_glb_lod(
  concat,
  lens,
  tiles,
  z,
  x,
  y,
  filter.min,
  filter.max,
  simplifyRatio,
  simplifyError,
);
const ms = (performance.now() - t0).toFixed(1);
console.log(`glb bytes: ${glb.byteLength} (${ms} ms)`);

const outDir = resolve(here, "../out");
mkdirSync(outDir, { recursive: true });
const outPath = resolve(outDir, `${z}-${x}-${y}.glb`);
writeFileSync(outPath, glb);
console.log(`wrote ${outPath}`);
