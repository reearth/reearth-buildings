//! Minimal glTF 2.0 binary (glb) writer for a single mesh primitive.
//!
//! Layout (glTF 2.0 spec §4.4):
//! ```text
//! [12B header][8B JSON chunk header][JSON][8B BIN chunk header][BIN]
//! ```
//! All chunks are padded to 4 bytes.

use crate::mesh::Mesh;
use byteorder::{LittleEndian, WriteBytesExt};
use serde_json::json;
use std::io::Write;

const GLTF_MAGIC: u32 = 0x4654_6C67; // "glTF"
const VERSION: u32 = 2;
const JSON_TYPE: u32 = 0x4E4F_534A; // "JSON"
const BIN_TYPE: u32 = 0x004E_4942; // "BIN\0"

/// Write a mesh into a glb. `enu_to_ecef` is column-major 4x4 affine
/// applied at the root node, placing the tile in world ECEF.
pub fn write_glb(mesh: &Mesh, enu_to_ecef: [f64; 16]) -> Vec<u8> {
    // ----- BIN buffer: positions, normals, indices -----
    let mut bin: Vec<u8> = Vec::new();

    // positions (vec3 f32)
    let pos_offset = bin.len();
    for v in &mesh.positions {
        bin.write_f32::<LittleEndian>(*v).unwrap();
    }
    let pos_len = bin.len() - pos_offset;
    pad_to_4(&mut bin);

    // normals (vec3 f32)
    let nrm_offset = bin.len();
    for v in &mesh.normals {
        bin.write_f32::<LittleEndian>(*v).unwrap();
    }
    let nrm_len = bin.len() - nrm_offset;
    pad_to_4(&mut bin);

    // indices (u32)
    let idx_offset = bin.len();
    for v in &mesh.indices {
        bin.write_u32::<LittleEndian>(*v).unwrap();
    }
    let idx_len = bin.len() - idx_offset;
    pad_to_4(&mut bin);

    let bbox = aabb(&mesh.positions);

    // ----- JSON chunk -----
    let pos_count = mesh.positions.len() / 3;
    let idx_count = mesh.indices.len();
    let gltf = json!({
        "asset": { "version": "2.0", "generator": "reearth-buildings" },
        "scene": 0,
        "scenes": [{ "nodes": [0] }],
        "nodes": [{ "mesh": 0, "matrix": enu_to_ecef.to_vec() }],
        "meshes": [{
            "primitives": [{
                "attributes": { "POSITION": 0, "NORMAL": 1 },
                "indices": 2,
                "material": 0
            }]
        }],
        "materials": [{
            "name": "buildings",
            "doubleSided": false,
            "pbrMetallicRoughness": {
                "baseColorFactor": [0.78, 0.78, 0.78, 1.0],
                "metallicFactor": 0.0,
                "roughnessFactor": 0.9
            }
        }],
        "accessors": [
            {
                "bufferView": 0, "byteOffset": 0,
                "componentType": 5126, "count": pos_count, "type": "VEC3",
                "min": [bbox.min[0], bbox.min[1], bbox.min[2]],
                "max": [bbox.max[0], bbox.max[1], bbox.max[2]]
            },
            {
                "bufferView": 1, "byteOffset": 0,
                "componentType": 5126, "count": pos_count, "type": "VEC3"
            },
            {
                "bufferView": 2, "byteOffset": 0,
                "componentType": 5125, "count": idx_count, "type": "SCALAR"
            }
        ],
        "bufferViews": [
            { "buffer": 0, "byteOffset": pos_offset, "byteLength": pos_len, "target": 34962 },
            { "buffer": 0, "byteOffset": nrm_offset, "byteLength": nrm_len, "target": 34962 },
            { "buffer": 0, "byteOffset": idx_offset, "byteLength": idx_len, "target": 34963 }
        ],
        "buffers": [{ "byteLength": bin.len() }]
    });
    let mut json_bytes = serde_json::to_vec(&gltf).expect("json serialize");
    while json_bytes.len() % 4 != 0 {
        json_bytes.push(b' ');
    }

    // ----- assemble -----
    let total_len = 12 + 8 + json_bytes.len() + 8 + bin.len();
    let mut out = Vec::with_capacity(total_len);
    out.write_u32::<LittleEndian>(GLTF_MAGIC).unwrap();
    out.write_u32::<LittleEndian>(VERSION).unwrap();
    out.write_u32::<LittleEndian>(total_len as u32).unwrap();
    out.write_u32::<LittleEndian>(json_bytes.len() as u32)
        .unwrap();
    out.write_u32::<LittleEndian>(JSON_TYPE).unwrap();
    out.write_all(&json_bytes).unwrap();
    out.write_u32::<LittleEndian>(bin.len() as u32).unwrap();
    out.write_u32::<LittleEndian>(BIN_TYPE).unwrap();
    out.write_all(&bin).unwrap();
    out
}

fn pad_to_4(buf: &mut Vec<u8>) {
    while buf.len() % 4 != 0 {
        buf.push(0);
    }
}

struct Aabb {
    min: [f32; 3],
    max: [f32; 3],
}

fn aabb(positions: &[f32]) -> Aabb {
    if positions.is_empty() {
        return Aabb {
            min: [0.0; 3],
            max: [0.0; 3],
        };
    }
    let mut min = [f32::INFINITY; 3];
    let mut max = [f32::NEG_INFINITY; 3];
    for v in positions.chunks_exact(3) {
        for i in 0..3 {
            if v[i] < min[i] {
                min[i] = v[i];
            }
            if v[i] > max[i] {
                max[i] = v[i];
            }
        }
    }
    Aabb { min, max }
}
