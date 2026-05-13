#!/usr/bin/env node
// Decode the meshopt-compressed bufferViews and verify the resulting vertex
// data is sane (positions roughly within tile extents, normals unit length,
// index range valid).

import { readFileSync } from "node:fs";
import { resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { MeshoptDecoder } from "meshoptimizer";

const here = dirname(fileURLToPath(import.meta.url));
const glbPath = resolve(here, "../out/14-14552-6451.glb");
const buf = readFileSync(glbPath);
const dv = new DataView(buf.buffer, buf.byteOffset, buf.byteLength);

const jsonLen = dv.getUint32(12, true);
const json = JSON.parse(buf.slice(20, 20 + jsonLen).toString());
const binStart = 20 + jsonLen + 8;
const binEnd = buf.byteLength;
const bin = new Uint8Array(buf.buffer, buf.byteOffset + binStart, binEnd - binStart);

await MeshoptDecoder.ready;

function decodeView(bv) {
  const ext = bv.extensions?.EXT_meshopt_compression;
  if (!ext) {
    return new Uint8Array(bin.buffer, bin.byteOffset + bv.byteOffset, bv.byteLength);
  }
  const src = new Uint8Array(bin.buffer, bin.byteOffset + ext.byteOffset, ext.byteLength);
  const out = new Uint8Array(ext.count * ext.byteStride);
  const filter = ext.filter ?? "NONE";
  MeshoptDecoder.decodeGltfBuffer(out, ext.count, ext.byteStride, src, ext.mode, filter);
  return out;
}

// positions
const posBytes = decodeView(json.bufferViews[0]);
const positions = new Float32Array(posBytes.buffer, posBytes.byteOffset, posBytes.byteLength / 4);
const nrmBytes = decodeView(json.bufferViews[1]);
const normals = new Float32Array(nrmBytes.buffer, nrmBytes.byteOffset, nrmBytes.byteLength / 4);
const idxBytes = decodeView(json.bufferViews[2]);
const indices = new Uint32Array(idxBytes.buffer, idxBytes.byteOffset, idxBytes.byteLength / 4);

let pmin = [Infinity, Infinity, Infinity];
let pmax = [-Infinity, -Infinity, -Infinity];
for (let i = 0; i < positions.length; i += 3) {
  for (let k = 0; k < 3; k++) {
    if (positions[i + k] < pmin[k]) pmin[k] = positions[i + k];
    if (positions[i + k] > pmax[k]) pmax[k] = positions[i + k];
  }
}

let normalLenError = 0;
for (let i = 0; i < normals.length; i += 3) {
  const len = Math.hypot(normals[i], normals[i + 1], normals[i + 2]);
  if (Math.abs(len - 1) > 0.01) normalLenError++;
}

let idxMax = 0;
for (let i = 0; i < indices.length; i++) if (indices[i] > idxMax) idxMax = indices[i];

const vertCount = positions.length / 3;
console.log("decoded vertices:", vertCount);
console.log("decoded indices:", indices.length);
console.log("position bbox:", pmin.map((v) => v.toFixed(1)), "-", pmax.map((v) => v.toFixed(1)));
console.log("normals with |n|≠1:", normalLenError, "/", normals.length / 3);
console.log("max index:", idxMax, "(should be <", vertCount, ")");
console.log("index in range:", idxMax < vertCount ? "yes" : "NO");
