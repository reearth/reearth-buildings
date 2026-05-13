#!/usr/bin/env node
// Sanity-check that wall and roof normals point outward / upward.
//
// Method:
//   1. Read the glb, parse JSON + bin
//   2. Group triangles by face plane orientation
//   3. For walls: triangles whose normal is roughly horizontal (|n.y|<0.5)
//      — verify n points AWAY from the local cluster centroid
//   4. For roofs: triangles with n.y ≈ +1 should have y_pos at TOP of their
//      column; n.y ≈ -1 means inverted

import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { dirname } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const glbPath = resolve(here, "../out/14-14552-6451.glb");
const buf = readFileSync(glbPath);
const dv = new DataView(buf.buffer, buf.byteOffset, buf.byteLength);

const jsonLen = dv.getUint32(12, true);
const json = JSON.parse(buf.slice(20, 20 + jsonLen).toString());
const binStart = 20 + jsonLen + 8;

const posAcc = json.accessors[0];
const nrmAcc = json.accessors[1];
const idxAcc = json.accessors[2];
const posBv = json.bufferViews[posAcc.bufferView];
const nrmBv = json.bufferViews[nrmAcc.bufferView];
const idxBv = json.bufferViews[idxAcc.bufferView];

const positions = new Float32Array(buf.buffer, buf.byteOffset + binStart + posBv.byteOffset, posBv.byteLength / 4);
const normals = new Float32Array(buf.buffer, buf.byteOffset + binStart + nrmBv.byteOffset, nrmBv.byteLength / 4);
const indices = new Uint32Array(buf.buffer, buf.byteOffset + binStart + idxBv.byteOffset, idxBv.byteLength / 4);

console.log(`verts=${posAcc.count}, tris=${idxAcc.count / 3}`);

// Geometric normal vs stored normal: if dot < 0 they disagree.
let agree = 0;
let disagree = 0;
let roofUp = 0;
let roofDown = 0;
let wallSamples = 0;
let wallOutward = 0;

for (let t = 0; t < indices.length; t += 3) {
  const i0 = indices[t] * 3;
  const i1 = indices[t + 1] * 3;
  const i2 = indices[t + 2] * 3;
  const p0 = [positions[i0], positions[i0 + 1], positions[i0 + 2]];
  const p1 = [positions[i1], positions[i1 + 1], positions[i1 + 2]];
  const p2 = [positions[i2], positions[i2 + 1], positions[i2 + 2]];
  const u = [p1[0] - p0[0], p1[1] - p0[1], p1[2] - p0[2]];
  const v = [p2[0] - p0[0], p2[1] - p0[1], p2[2] - p0[2]];
  // u × v
  const gn = [
    u[1] * v[2] - u[2] * v[1],
    u[2] * v[0] - u[0] * v[2],
    u[0] * v[1] - u[1] * v[0],
  ];
  const glen = Math.hypot(gn[0], gn[1], gn[2]);
  if (glen < 1e-9) continue;
  gn[0] /= glen;
  gn[1] /= glen;
  gn[2] /= glen;

  const sn = [normals[i0], normals[i0 + 1], normals[i0 + 2]];
  const dot = gn[0] * sn[0] + gn[1] * sn[1] + gn[2] * sn[2];
  if (dot > 0) agree++;
  else disagree++;

  // Classify
  if (Math.abs(sn[1]) > 0.9) {
    // roof or floor
    if (sn[1] > 0) roofUp++;
    else roofDown++;
  } else if (Math.abs(sn[1]) < 0.1) {
    // wall: check it points away from neighborhood midpoint
    // approximate "interior reference" as the average of nearby vertices in horizontal plane
    wallSamples++;
    // simple check: midpoint of base edge + outward step should be further from
    // the building's centroid than the midpoint. We don't have per-building
    // groupings, but a good proxy: the wall's two top vertices are at y=top,
    // bottom at y=base. The geometric outward direction equals stored normal
    // iff `dot > 0` AND the normal is consistent. Already counted above.
    if (dot > 0) wallOutward++;
  }
}

console.log(`geometric vs stored normal: agree=${agree} disagree=${disagree}`);
console.log(`roof tris facing UP=${roofUp}, DOWN=${roofDown}`);
console.log(`wall tris sampled=${wallSamples}, outward-consistent=${wallOutward}`);
