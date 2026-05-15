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
    let bv_feature_id = push_bv_aligned(
        &mut bin,
        &mut buffer_views,
        u64_bytes(&cols.feature_id),
        None,
        8,
    );
    let bv_height = push_bv(&mut bin, &mut buffer_views, f32_bytes(&cols.height), None);
    let bv_source_height = push_bv(
        &mut bin,
        &mut buffer_views,
        f32_bytes(&cols.source_height),
        None,
    );
    let bv_min_height = push_bv(
        &mut bin,
        &mut buffer_views,
        f32_bytes(&cols.min_height),
        None,
    );
    let bv_roof_height = push_bv(
        &mut bin,
        &mut buffer_views,
        f32_bytes(&cols.roof_height),
        None,
    );
    let bv_ground_elev = push_bv(
        &mut bin,
        &mut buffer_views,
        f32_bytes(&cols.ground_elev),
        None,
    );
    let bv_num_floors = push_bv(
        &mut bin,
        &mut buffer_views,
        u16_bytes(&cols.num_floors),
        None,
    );
    let bv_gers_id = push_string_column(&mut bin, &mut buffer_views, &cols.gers_id);
    let bv_name = push_string_column(&mut bin, &mut buffer_views, &cols.name);
    let bv_subtype = push_string_column(&mut bin, &mut buffer_views, &cols.subtype);
    let bv_class = push_string_column(&mut bin, &mut buffer_views, &cols.class);
    let bv_roof_shape = push_string_column(&mut bin, &mut buffer_views, &cols.roof_shape);
    let bv_height_method = push_string_column(&mut bin, &mut buffer_views, &cols.height_method);

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
                                "feature_id":  { "type": "SCALAR", "componentType": "UINT64",  "required": false, "noData": 0 },
                                "gers_id":     { "type": "STRING", "required": false, "noData": "" },
                                "name":        { "type": "STRING", "required": false, "noData": "" },
                                "subtype":     { "type": "STRING", "required": false, "noData": "" },
                                "class":       { "type": "STRING", "required": false, "noData": "" },
                                "height":        { "type": "SCALAR", "componentType": "FLOAT32", "required": true, "description": "Height in metres used for the extrusion. Resolved via height_method." },
                                "source_height": { "type": "SCALAR", "componentType": "FLOAT32", "required": false, "noData": 0.0, "description": "Original Overture height value, if present." },
                                "height_method": { "type": "STRING", "required": false, "noData": "", "description": "How `height` was chosen: explicit | num_floors | class | subtype | footprint." },
                                "min_height":    { "type": "SCALAR", "componentType": "FLOAT32", "required": true },
                                "roof_height":   { "type": "SCALAR", "componentType": "FLOAT32", "required": false, "noData": 0.0 },
                                "ground_elev":   { "type": "SCALAR", "componentType": "FLOAT32", "required": false, "noData": 0.0 },
                                "num_floors":    { "type": "SCALAR", "componentType": "UINT16",  "required": false, "noData": 0 },
                                "roof_shape":    { "type": "STRING", "required": false, "noData": "" }
                            }
                        }
                    }
                },
                "propertyTables": [{
                    "name": "buildings",
                    "class": "building",
                    "count": feat_count,
                    "properties": {
                        "feature_id":  { "values": bv_feature_id },
                        "gers_id":     { "values": bv_gers_id.values, "stringOffsets": bv_gers_id.string_offsets, "stringOffsetType": "UINT32" },
                        "name":        { "values": bv_name.values, "stringOffsets": bv_name.string_offsets, "stringOffsetType": "UINT32" },
                        "subtype":     { "values": bv_subtype.values, "stringOffsets": bv_subtype.string_offsets, "stringOffsetType": "UINT32" },
                        "class":       { "values": bv_class.values, "stringOffsets": bv_class.string_offsets, "stringOffsetType": "UINT32" },
                        "height":        { "values": bv_height },
                        "source_height": { "values": bv_source_height },
                        "height_method": { "values": bv_height_method.values, "stringOffsets": bv_height_method.string_offsets, "stringOffsetType": "UINT32" },
                        "min_height":    { "values": bv_min_height },
                        "roof_height":   { "values": bv_roof_height },
                        "ground_elev":   { "values": bv_ground_elev },
                        "num_floors":    { "values": bv_num_floors },
                        "roof_shape":    { "values": bv_roof_shape.values, "stringOffsets": bv_roof_shape.string_offsets, "stringOffsetType": "UINT32" }
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
    feature_id: Vec<u64>,
    height: Vec<f32>,
    source_height: Vec<f32>,
    min_height: Vec<f32>,
    roof_height: Vec<f32>,
    ground_elev: Vec<f32>,
    num_floors: Vec<u16>,
    gers_id: Vec<String>,
    name: Vec<String>,
    subtype: Vec<String>,
    class: Vec<String>,
    roof_shape: Vec<String>,
    height_method: Vec<String>,
}

fn collect_columns(features: &[FeatureProps]) -> Columns {
    let mut c = Columns::default();
    for f in features {
        c.feature_id.push(f.feature_id.unwrap_or(0));
        c.height.push(f.height_m);
        // 0 doubles as the schema's noData sentinel for "Overture had no
        // height for this building".
        c.source_height.push(f.source_height_m.unwrap_or(0.0));
        c.min_height.push(f.min_height_m);
        c.roof_height.push(f.roof_height_m);
        c.ground_elev.push(f.ground_elev_m);
        c.num_floors.push(f.num_floors);
        c.gers_id.push(f.gers_id.clone().unwrap_or_default());
        c.name.push(f.name.clone().unwrap_or_default());
        c.subtype.push(f.subtype.clone().unwrap_or_default());
        c.class.push(f.class.clone().unwrap_or_default());
        c.roof_shape.push(f.roof_shape.clone().unwrap_or_default());
        c.height_method.push(f.height_method.to_string());
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
fn u64_bytes(v: &[u64]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 8);
    for x in v {
        out.write_u64::<LittleEndian>(*x).unwrap();
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
