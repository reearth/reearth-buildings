//! Minimal glTF 2.0 binary (glb) writer for a single building mesh with
//! per-feature metadata via EXT_mesh_features + EXT_structural_metadata
//! and vertex/index compression via EXT_meshopt_compression.
//!
//! glb layout (spec §4.4):
//! ```text
//! [12B header][8B JSON chunk header][JSON][8B BIN chunk header][BIN]
//! ```
//!
//! Buffer layout: BIN holds the actual compressed data plus uncompressed
//! property tables. A second virtual buffer (entry only, no bytes) declares
//! the decompressed sizes that accessors observe.

use crate::mesh::{FeatureProps, Mesh};
use byteorder::{LittleEndian, WriteBytesExt};
use serde_json::{json, Value};
use std::io::Write;

const GLTF_MAGIC: u32 = 0x4654_6C67;
const VERSION: u32 = 2;
const JSON_TYPE: u32 = 0x4E4F_534A;
const BIN_TYPE: u32 = 0x004E_4942;

const BUFFER_REAL: usize = 0;
const BUFFER_VIRTUAL: usize = 1;

/// Write a mesh into a glb. `enu_to_ecef` is column-major 4x4 affine applied
/// at the root node, placing the tile in world ECEF.
pub fn write_glb(mesh: &Mesh, enu_to_ecef: [f64; 16]) -> Vec<u8> {
    let pos_count = mesh.positions.len() / 3;
    let idx_count = mesh.indices.len();
    if pos_count == 0 || idx_count == 0 {
        return write_empty_glb(enu_to_ecef);
    }

    let mut bin: Vec<u8> = Vec::new();
    let mut buffer_views: Vec<Value> = Vec::new();
    let mut virtual_offset: usize = 0;

    // ---- compressed vertex/index buffer views ----
    let pos_typed: Vec<[f32; 3]> = mesh
        .positions
        .chunks_exact(3)
        .map(|c| [c[0], c[1], c[2]])
        .collect();
    let bv_pos = push_compressed_attributes(
        &mut bin,
        &mut buffer_views,
        &mut virtual_offset,
        meshopt::encode_vertex_buffer(&pos_typed).expect("encode pos"),
        12,
        pos_count,
        Some(34962),
        "NONE",
    );

    let nrm_typed: Vec<[f32; 3]> = mesh
        .normals
        .chunks_exact(3)
        .map(|c| [c[0], c[1], c[2]])
        .collect();
    let bv_nrm = push_compressed_attributes(
        &mut bin,
        &mut buffer_views,
        &mut virtual_offset,
        meshopt::encode_vertex_buffer(&nrm_typed).expect("encode nrm"),
        12,
        pos_count,
        Some(34962),
        "NONE",
    );

    let bv_idx = push_compressed_indices(
        &mut bin,
        &mut buffer_views,
        &mut virtual_offset,
        meshopt::encode_index_buffer(&mesh.indices, pos_count).expect("encode idx"),
        idx_count,
        Some(34963),
    );
    let virtual_total = virtual_offset;

    // feature_ids (u16) can't be encoded by meshopt — its vertex codec
    // asserts that stride is a multiple of 4. Leave it as a plain
    // bufferView; the 2-byte savings per vertex aren't worth widening
    // to u32 just to satisfy the encoder.
    let bv_fid = push_bv_aligned(
        &mut bin,
        &mut buffer_views,
        u16_bytes(&mesh.feature_ids),
        Some(34962),
        2,
    );

    // ---- uncompressed property-table buffer views (buffer 0, plain) ----
    let cols = collect_columns(&mesh.features);
    let bv_osm_id = push_bv_aligned(
        &mut bin,
        &mut buffer_views,
        i64_bytes(&cols.osm_id),
        None,
        8,
    );
    let bv_height = push_bv(&mut bin, &mut buffer_views, f32_bytes(&cols.height), None);
    let bv_min_height = push_bv(
        &mut bin,
        &mut buffer_views,
        f32_bytes(&cols.min_height),
        None,
    );
    let bv_levels = push_bv(&mut bin, &mut buffer_views, u16_bytes(&cols.levels), None);
    let bv_name = push_string_column(&mut bin, &mut buffer_views, &cols.name);
    let bv_kind = push_string_column(&mut bin, &mut buffer_views, &cols.kind);
    let bv_building = push_string_column(&mut bin, &mut buffer_views, &cols.building);

    let feat_count = mesh.features.len();
    let bbox = aabb(&mesh.positions);

    let gltf = json!({
        "asset": { "version": "2.0", "generator": "reearth-buildings" },
        "scene": 0,
        "extensionsUsed": ["EXT_mesh_features", "EXT_structural_metadata", "EXT_meshopt_compression"],
        "extensionsRequired": ["EXT_meshopt_compression"],
        "scenes": [{ "nodes": [0] }],
        "nodes": [{ "mesh": 0, "matrix": enu_to_ecef.to_vec() }],
        "meshes": [{
            "primitives": [{
                "attributes": { "POSITION": 0, "NORMAL": 1, "_FEATURE_ID_0": 3 },
                "indices": 2,
                "material": 0,
                "extensions": {
                    "EXT_mesh_features": {
                        "featureIds": [{
                            "featureCount": feat_count,
                            "attribute": 0,
                            "propertyTable": 0
                        }]
                    }
                }
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
                "bufferView": bv_pos, "byteOffset": 0,
                "componentType": 5126, "count": pos_count, "type": "VEC3",
                "min": [bbox.min[0], bbox.min[1], bbox.min[2]],
                "max": [bbox.max[0], bbox.max[1], bbox.max[2]]
            },
            { "bufferView": bv_nrm, "byteOffset": 0,
              "componentType": 5126, "count": pos_count, "type": "VEC3" },
            { "bufferView": bv_idx, "byteOffset": 0,
              "componentType": 5125, "count": idx_count, "type": "SCALAR" },
            { "bufferView": bv_fid, "byteOffset": 0,
              "componentType": 5123, "count": pos_count, "type": "SCALAR" }
        ],
        "bufferViews": buffer_views,
        "buffers": [
            { "byteLength": bin.len() },
            { "byteLength": virtual_total,
              "extensions": { "EXT_meshopt_compression": { "fallback": true } } }
        ],
        "extensions": {
            "EXT_structural_metadata": {
                "schema": {
                    "id": "reearth_buildings",
                    "classes": {
                        "building": {
                            "name": "Building",
                            "properties": {
                                "osm_id":     { "type": "SCALAR", "componentType": "INT64",   "required": false, "noData": 0 },
                                "name":       { "type": "STRING", "required": false, "noData": "" },
                                "kind":       { "type": "STRING", "required": false, "noData": "" },
                                "building":   { "type": "STRING", "required": false, "noData": "" },
                                "height":     { "type": "SCALAR", "componentType": "FLOAT32", "required": true },
                                "min_height": { "type": "SCALAR", "componentType": "FLOAT32", "required": true },
                                "levels":     { "type": "SCALAR", "componentType": "UINT16",  "required": false, "noData": 0 }
                            }
                        }
                    }
                },
                "propertyTables": [{
                    "name": "buildings",
                    "class": "building",
                    "count": feat_count,
                    "properties": {
                        "osm_id":     { "values": bv_osm_id },
                        "name":       { "values": bv_name.values, "stringOffsets": bv_name.string_offsets, "stringOffsetType": "UINT32" },
                        "kind":       { "values": bv_kind.values, "stringOffsets": bv_kind.string_offsets, "stringOffsetType": "UINT32" },
                        "building":   { "values": bv_building.values, "stringOffsets": bv_building.string_offsets, "stringOffsetType": "UINT32" },
                        "height":     { "values": bv_height },
                        "min_height": { "values": bv_min_height },
                        "levels":     { "values": bv_levels }
                    }
                }]
            }
        }
    });

    let mut json_bytes = serde_json::to_vec(&gltf).expect("json serialize");
    while !json_bytes.len().is_multiple_of(4) {
        json_bytes.push(b' ');
    }

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

/// A glb with the asset header and an empty scene. Used when a tile has no
/// buildings — we still want a valid 3D Tiles content payload so clients
/// don't surface an error.
fn write_empty_glb(enu_to_ecef: [f64; 16]) -> Vec<u8> {
    let gltf = json!({
        "asset": { "version": "2.0", "generator": "reearth-buildings" },
        "scene": 0,
        "scenes": [{ "nodes": [0] }],
        "nodes": [{ "matrix": enu_to_ecef.to_vec() }]
    });
    let mut json_bytes = serde_json::to_vec(&gltf).expect("json serialize");
    while !json_bytes.len().is_multiple_of(4) {
        json_bytes.push(b' ');
    }
    let total_len = 12 + 8 + json_bytes.len();
    let mut out = Vec::with_capacity(total_len);
    out.write_u32::<LittleEndian>(GLTF_MAGIC).unwrap();
    out.write_u32::<LittleEndian>(VERSION).unwrap();
    out.write_u32::<LittleEndian>(total_len as u32).unwrap();
    out.write_u32::<LittleEndian>(json_bytes.len() as u32)
        .unwrap();
    out.write_u32::<LittleEndian>(JSON_TYPE).unwrap();
    out.write_all(&json_bytes).unwrap();
    out
}

// ---------------- meshopt-compressed bufferView helpers ----------------

#[allow(clippy::too_many_arguments)]
fn push_compressed_attributes(
    bin: &mut Vec<u8>,
    views: &mut Vec<Value>,
    virtual_offset: &mut usize,
    compressed: Vec<u8>,
    byte_stride: usize,
    count: usize,
    target: Option<u32>,
    filter: &str,
) -> usize {
    let real_offset = bin.len();
    let real_len = compressed.len();
    bin.extend_from_slice(&compressed);
    pad_to(bin, 4);

    let uncompressed_len = byte_stride * count;
    let virtual_pos = *virtual_offset;
    *virtual_offset += round_up(uncompressed_len, 4);

    let mut v = json!({
        "buffer": BUFFER_VIRTUAL,
        "byteOffset": virtual_pos,
        "byteLength": uncompressed_len,
        "byteStride": byte_stride,
        "extensions": {
            "EXT_meshopt_compression": {
                "buffer": BUFFER_REAL,
                "byteOffset": real_offset,
                "byteLength": real_len,
                "byteStride": byte_stride,
                "count": count,
                "mode": "ATTRIBUTES",
                "filter": filter
            }
        }
    });
    if let Some(t) = target {
        v["target"] = json!(t);
    }
    let idx = views.len();
    views.push(v);
    idx
}

fn push_compressed_indices(
    bin: &mut Vec<u8>,
    views: &mut Vec<Value>,
    virtual_offset: &mut usize,
    compressed: Vec<u8>,
    count: usize,
    target: Option<u32>,
) -> usize {
    let real_offset = bin.len();
    let real_len = compressed.len();
    bin.extend_from_slice(&compressed);
    pad_to(bin, 4);

    // u32 indices → byteStride 4
    let byte_stride = 4usize;
    let uncompressed_len = byte_stride * count;
    let virtual_pos = *virtual_offset;
    *virtual_offset += round_up(uncompressed_len, 4);

    let mut v = json!({
        "buffer": BUFFER_VIRTUAL,
        "byteOffset": virtual_pos,
        "byteLength": uncompressed_len,
        "extensions": {
            "EXT_meshopt_compression": {
                "buffer": BUFFER_REAL,
                "byteOffset": real_offset,
                "byteLength": real_len,
                "byteStride": byte_stride,
                "count": count,
                "mode": "TRIANGLES"
            }
        }
    });
    if let Some(t) = target {
        v["target"] = json!(t);
    }
    let idx = views.len();
    views.push(v);
    idx
}

fn round_up(n: usize, align: usize) -> usize {
    n.div_ceil(align) * align
}

// ---------------- column collection ----------------

#[derive(Default)]
struct Columns {
    osm_id: Vec<i64>,
    height: Vec<f32>,
    min_height: Vec<f32>,
    levels: Vec<u16>,
    name: Vec<String>,
    kind: Vec<String>,
    building: Vec<String>,
}

fn collect_columns(features: &[FeatureProps]) -> Columns {
    let mut c = Columns::default();
    for f in features {
        c.osm_id.push(f.osm_id.unwrap_or(0));
        c.height.push(f.height_m);
        c.min_height.push(f.min_height_m);
        c.levels.push(f.levels);
        c.name.push(f.name.clone().unwrap_or_default());
        c.kind.push(f.kind.clone().unwrap_or_default());
        c.building.push(f.building.clone().unwrap_or_default());
    }
    c
}

// ---------------- bin packers ----------------

struct StringBv {
    values: usize,
    string_offsets: usize,
}

fn push_bv(
    bin: &mut Vec<u8>,
    views: &mut Vec<Value>,
    bytes: Vec<u8>,
    target: Option<u32>,
) -> usize {
    push_bv_aligned(bin, views, bytes, target, 4)
}

fn push_bv_aligned(
    bin: &mut Vec<u8>,
    views: &mut Vec<Value>,
    bytes: Vec<u8>,
    target: Option<u32>,
    align: usize,
) -> usize {
    pad_to(bin, align);
    let offset = bin.len();
    let len = bytes.len();
    bin.extend_from_slice(&bytes);
    pad_to(bin, 4);
    let mut v = json!({
        "buffer": BUFFER_REAL,
        "byteOffset": offset,
        "byteLength": len,
    });
    if let Some(t) = target {
        v["target"] = json!(t);
    }
    let idx = views.len();
    views.push(v);
    idx
}

fn push_string_column(bin: &mut Vec<u8>, views: &mut Vec<Value>, strings: &[String]) -> StringBv {
    let mut values_bytes: Vec<u8> = Vec::new();
    let mut offsets: Vec<u32> = Vec::with_capacity(strings.len() + 1);
    offsets.push(0);
    for s in strings {
        values_bytes.extend_from_slice(s.as_bytes());
        offsets.push(values_bytes.len() as u32);
    }
    let values_idx = push_bv(bin, views, values_bytes, None);
    let offsets_idx = push_bv(bin, views, u32_bytes(&offsets), None);
    StringBv {
        values: values_idx,
        string_offsets: offsets_idx,
    }
}

fn f32_bytes(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 4);
    for x in v {
        out.write_f32::<LittleEndian>(*x).unwrap();
    }
    out
}
fn u32_bytes(v: &[u32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 4);
    for x in v {
        out.write_u32::<LittleEndian>(*x).unwrap();
    }
    out
}
fn u16_bytes(v: &[u16]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 2);
    for x in v {
        out.write_u16::<LittleEndian>(*x).unwrap();
    }
    out
}
fn i64_bytes(v: &[i64]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 8);
    for x in v {
        out.write_i64::<LittleEndian>(*x).unwrap();
    }
    out
}

fn pad_to(buf: &mut Vec<u8>, align: usize) {
    while !buf.len().is_multiple_of(align) {
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
