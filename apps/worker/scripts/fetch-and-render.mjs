#!/usr/bin/env node
// Fetch one MVT tile from Protomaps, run it through the WASM pipeline, and
// write the resulting glb to disk. Used for local validation before
// wrangler dev / R2 setup.
//
// Usage: node scripts/fetch-and-render.mjs [z x y] [pmtiles-url]
// Defaults to a tile over Tokyo Station.

import { PMTiles, FetchSource } from "pmtiles";
import { writeFileSync, mkdirSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const [, , zArg, xArg, yArg, urlArg] = process.argv;
const z = Number(zArg ?? 14);
const x = Number(xArg ?? 14552);
const y = Number(yArg ?? 6451);
const url = urlArg ?? "https://build.protomaps.com/20250101.pmtiles";

console.log(`fetching ${url} @ z=${z} x=${x} y=${y}`);
const pm = new PMTiles(new FetchSource(url));
const tile = await pm.getZxy(z, x, y);
if (!tile) {
  console.error("tile not found (out of coverage?)");
  process.exit(2);
}
console.log(`mvt bytes: ${tile.data.byteLength}`);

const wasmModule = await import(resolve(here, "../wasm/buildings_wasm.js"));
const { render_glb } = wasmModule;

const mvt = new Uint8Array(tile.data);
const t0 = performance.now();
const glb = render_glb(mvt, z, x, y);
const ms = (performance.now() - t0).toFixed(1);
console.log(`glb bytes: ${glb.byteLength} (${ms} ms)`);

const outDir = resolve(here, "../out");
mkdirSync(outDir, { recursive: true });
const outPath = resolve(outDir, `${z}-${x}-${y}.glb`);
writeFileSync(outPath, glb);
console.log(`wrote ${outPath}`);
