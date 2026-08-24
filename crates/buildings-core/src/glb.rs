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
///
/// Takes the mesh by value on purpose: a dense z=14 Tokyo tile carries
/// ~15 MB of geometry and ~15 MB of per-feature metadata, and the worker
/// runs inside a 128 MB isolate. Each input vector is released the moment
/// it has been packed into `bin`, so the peak is the output buffer plus
/// whatever is still waiting to be packed — not everything at once.
pub fn write_glb(mut mesh: Mesh, enu_to_ecef: [f64; 16]) -> Vec<u8> {
    let pos_count = mesh.positions.len() / 3;
    let idx_count = mesh.indices.len();
    if pos_count == 0 || idx_count == 0 {
        return write_empty_glb(enu_to_ecef);
    }

    let feat_count = mesh.features.len();
    let bbox = aabb(&mesh.positions);

    // `bin` starts with a reserved gap that the glb header + JSON chunk is
    // written into at the end (see `assemble`), so the finished file is the
    // one buffer we filled here rather than a second full-size copy of it.
    // Every recorded byteOffset is relative to `PREFIX_GAP`.
    let mut bin: Vec<u8> = vec![0u8; PREFIX_GAP];
    let mut buffer_views: Vec<Value> = Vec::new();
    let mut virtual_offset: usize = 0;

    // ---- compressed vertex/index buffer views ----
    let bv_pos = push_compressed_attributes(
        &mut bin,
        &mut buffer_views,
        &mut virtual_offset,
        meshopt::encode_vertex_buffer(mesh.positions.as_chunks::<3>().0).expect("encode pos"),
        12,
        pos_count,
        Some(34962),
        "NONE",
    );
    mesh.positions = Vec::new();

    let bv_nrm = push_compressed_attributes(
        &mut bin,
        &mut buffer_views,
        &mut virtual_offset,
        meshopt::encode_vertex_buffer(mesh.normals.as_chunks::<3>().0).expect("encode nrm"),
        12,
        pos_count,
        Some(34962),
        "NONE",
    );
    mesh.normals = Vec::new();

    let bv_idx = push_compressed_indices(
        &mut bin,
        &mut buffer_views,
        &mut virtual_offset,
        meshopt::encode_index_buffer(&mesh.indices, pos_count).expect("encode idx"),
        idx_count,
        Some(34963),
    );
    mesh.indices = Vec::new();
    let virtual_total = virtual_offset;

    // feature_ids (u16) can't be encoded by meshopt — its vertex codec
    // asserts that stride is a multiple of 4. Leave it as a plain
    // bufferView; the 2-byte savings per vertex aren't worth widening
    // to u32 just to satisfy the encoder.
    let feature_ids = std::mem::take(&mut mesh.feature_ids);
    let bv_fid = push_bv_with(&mut bin, &mut buffer_views, Some(34962), 2, |out| {
        for v in &feature_ids {
            out.write_u16::<LittleEndian>(*v).unwrap();
        }
    });
    drop(feature_ids);

    // ---- uncompressed property-table buffer views (buffer 0, plain) ----
    //
    // Each column is written straight into `bin` from `mesh.features`.
    // Materialising them as `Vec<T>` first (and `Vec<String>` for the text
    // ones) used to duplicate the whole feature table before a single byte
    // was packed.
    let feats = &mesh.features;
    let bv_feature_id = push_bv_with(&mut bin, &mut buffer_views, None, 8, |out| {
        for f in feats {
            out.write_u64::<LittleEndian>(f.feature_id.unwrap_or(0))
                .unwrap();
        }
    });
    let bv_height = push_f32_column(&mut bin, &mut buffer_views, feats, |f| f.height_m);
    // 0 doubles as the schema's noData sentinel for "Overture had no
    // height for this building".
    let bv_source_height = push_f32_column(&mut bin, &mut buffer_views, feats, |f| {
        f.source_height_m.unwrap_or(0.0)
    });
    let bv_min_height = push_f32_column(&mut bin, &mut buffer_views, feats, |f| f.min_height_m);
    let bv_roof_height = push_f32_column(&mut bin, &mut buffer_views, feats, |f| f.roof_height_m);
    let bv_ground_elev = push_f32_column(&mut bin, &mut buffer_views, feats, |f| f.ground_elev_m);
    let bv_num_floors = push_bv_with(&mut bin, &mut buffer_views, None, 4, |out| {
        for f in feats {
            out.write_u16::<LittleEndian>(f.num_floors).unwrap();
        }
    });
    let bv_gers_id = push_string_column(&mut bin, &mut buffer_views, feats, |f| opt(&f.gers_id));
    let bv_name = push_string_column(&mut bin, &mut buffer_views, feats, |f| opt(&f.name));
    let bv_subtype = push_string_column(&mut bin, &mut buffer_views, feats, |f| opt(&f.subtype));
    let bv_class = push_string_column(&mut bin, &mut buffer_views, feats, |f| opt(&f.class));
    let bv_roof_shape =
        push_string_column(&mut bin, &mut buffer_views, feats, |f| opt(&f.roof_shape));
    let bv_height_method =
        push_string_column(&mut bin, &mut buffer_views, feats, |f| f.height_method);
    mesh.features = Vec::new();

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
            // Pure Lambert: metallicFactor=0 + roughnessFactor=1 collapses
            // glTF's GGX specular term to zero, so shading reduces to
            // baseColor × max(0, dot(N, L)) — no specular highlight on
            // facade edges. Avoids the "shiny mall" look the previous
            // 0.9 roughness produced under sharp midday sun.
            "pbrMetallicRoughness": {
                "baseColorFactor": [0.92, 0.92, 0.92, 1.0],
                "metallicFactor": 0.0,
                "roughnessFactor": 1.0
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
            // Payload only — `bin` also carries the reserved prefix gap.
            { "byteLength": bin.len() - PREFIX_GAP },
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

    let json_bytes = serde_json::to_vec(&gltf).expect("json serialize");
    assemble(bin, json_bytes)
}

/// Bytes reserved at the front of `bin` for `[glb header][JSON chunk]`.
///
/// The JSON is a fixed schema plus a fixed 23 bufferView entries — ~5.3 KB,
/// and it doesn't grow with the building count — so 8 KB always fits.
/// Nothing downstream depends on the gap: bufferView byteOffsets are
/// relative to the BIN chunk, which is why the prefix can be sized
/// independently of the payload.
const PREFIX_GAP: usize = 8 * 1024;

/// Payload size from which filling the gap beats copying the file.
///
/// Using the gap means padding the JSON chunk out to fill it exactly
/// (glTF §4.4.2: JSON chunks are space-padded, any length is valid), which
/// costs ~2.7 KB of trailing spaces. That's noise next to a multi-MB
/// central-Tokyo tile whose copy we're avoiding, and not worth paying on a
/// sparse tile that a second buffer would hold comfortably anyway.
const GAP_MIN_PAYLOAD: usize = 1024 * 1024;

/// Write the glb header + JSON chunk into the gap `write_glb` left at the
/// front of `bin`, and return the whole thing as the finished file.
fn assemble(mut bin: Vec<u8>, mut json_bytes: Vec<u8>) -> Vec<u8> {
    let bin_len = bin.len() - PREFIX_GAP;
    // 12 B glb header + 8 B JSON chunk header + JSON + 8 B BIN chunk header.
    let json_len = PREFIX_GAP.saturating_sub(12 + 8 + 8);
    if json_bytes.len() > json_len || bin_len < GAP_MIN_PAYLOAD {
        return assemble_copied(&bin[PREFIX_GAP..], json_bytes);
    }
    json_bytes.resize(json_len, b' ');

    let total_len = PREFIX_GAP + bin_len;
    let mut head = &mut bin[..PREFIX_GAP];
    head.write_u32::<LittleEndian>(GLTF_MAGIC).unwrap();
    head.write_u32::<LittleEndian>(VERSION).unwrap();
    head.write_u32::<LittleEndian>(total_len as u32).unwrap();
    head.write_u32::<LittleEndian>(json_len as u32).unwrap();
    head.write_u32::<LittleEndian>(JSON_TYPE).unwrap();
    head.write_all(&json_bytes).unwrap();
    head.write_u32::<LittleEndian>(bin_len as u32).unwrap();
    head.write_u32::<LittleEndian>(BIN_TYPE).unwrap();
    debug_assert!(head.is_empty());
    bin
}

/// Build the file as a second buffer: the payload is small enough that the
/// copy is cheap (and a tight JSON chunk is nicer), or — never seen in
/// practice — the JSON outgrew the reserved gap.
fn assemble_copied(bin: &[u8], mut json_bytes: Vec<u8>) -> Vec<u8> {
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
    out.write_all(bin).unwrap();
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
    let real_offset = bin.len() - PREFIX_GAP;
    let real_len = compressed.len();
    bin.extend_from_slice(&compressed);
    drop(compressed);
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
    let real_offset = bin.len() - PREFIX_GAP;
    let real_len = compressed.len();
    bin.extend_from_slice(&compressed);
    drop(compressed);
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

// ---------------- bin packers ----------------

struct StringBv {
    values: usize,
    string_offsets: usize,
}

/// `""` for an absent optional string — the schema's noData sentinel.
fn opt(s: &Option<String>) -> &str {
    s.as_deref().unwrap_or("")
}

/// Append a bufferView by writing its bytes directly into `bin`.
///
/// The `fill` closure appends the payload; the bufferView entry is derived
/// from how far `bin` grew. Writing in place is what keeps a column from
/// existing twice (once as `Vec<T>`, once as its little-endian bytes).
fn push_bv_with(
    bin: &mut Vec<u8>,
    views: &mut Vec<Value>,
    target: Option<u32>,
    align: usize,
    fill: impl FnOnce(&mut Vec<u8>),
) -> usize {
    pad_to(bin, align);
    let offset = bin.len() - PREFIX_GAP;
    let start = bin.len();
    fill(bin);
    let len = bin.len() - start;
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

fn push_f32_column(
    bin: &mut Vec<u8>,
    views: &mut Vec<Value>,
    features: &[FeatureProps],
    get: impl Fn(&FeatureProps) -> f32,
) -> usize {
    push_bv_with(bin, views, None, 4, |out| {
        for f in features {
            out.write_f32::<LittleEndian>(get(f)).unwrap();
        }
    })
}

fn push_string_column(
    bin: &mut Vec<u8>,
    views: &mut Vec<Value>,
    features: &[FeatureProps],
    get: impl Fn(&FeatureProps) -> &str,
) -> StringBv {
    let mut offsets: Vec<u32> = Vec::with_capacity(features.len() + 1);
    offsets.push(0);
    let values_idx = push_bv_with(bin, views, None, 4, |out| {
        let start = out.len();
        for f in features {
            out.extend_from_slice(get(f).as_bytes());
            offsets.push((out.len() - start) as u32);
        }
    });
    let offsets_idx = push_bv_with(bin, views, None, 4, |out| {
        for o in &offsets {
            out.write_u32::<LittleEndian>(*o).unwrap();
        }
    });
    StringBv {
        values: values_idx,
        string_offsets: offsets_idx,
    }
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
    for v in positions.as_chunks::<3>().0 {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::FeatureProps;

    /// Split a glb into its JSON and BIN chunks, checking the framing.
    fn chunks(glb: &[u8]) -> (Value, &[u8]) {
        let u32_at = |o: usize| u32::from_le_bytes(glb[o..o + 4].try_into().unwrap()) as usize;
        assert_eq!(&glb[0..4], b"glTF");
        assert_eq!(u32_at(4), VERSION as usize);
        assert_eq!(u32_at(8), glb.len(), "header total length");
        let json_len = u32_at(12);
        assert_eq!(u32_at(16), JSON_TYPE as usize);
        assert!(json_len.is_multiple_of(4), "JSON chunk must be 4-aligned");
        let json: Value = serde_json::from_slice(&glb[20..20 + json_len]).expect("JSON chunk");
        let bin_off = 20 + json_len;
        let bin_len = u32_at(bin_off);
        assert_eq!(u32_at(bin_off + 4), BIN_TYPE as usize);
        assert_eq!(bin_off + 8 + bin_len, glb.len(), "BIN chunk fills the file");
        (json, &glb[bin_off + 8..])
    }

    /// A unit cube's worth of geometry, `feats` features deep.
    fn mesh_with(verts: usize, feats: usize) -> Mesh {
        Mesh {
            positions: (0..verts * 3).map(|i| i as f32).collect(),
            normals: (0..verts * 3).map(|_| 0.0).collect(),
            feature_ids: (0..verts).map(|i| (i % feats.max(1)) as u16).collect(),
            indices: (0..verts as u32).collect(),
            features: (0..feats)
                .map(|i| FeatureProps {
                    feature_id: Some(i as u64),
                    gers_id: Some(format!("gers-{i}")),
                    height_m: 10.0,
                    height_method: "explicit",
                    ..Default::default()
                })
                .collect(),
        }
    }

    /// The declared buffer length has to match the BIN chunk actually
    /// written — they're computed in different places, and a mismatch is
    /// invisible until a client tries to read past the end of a bufferView.
    #[test]
    fn declared_buffer_matches_bin_chunk() {
        for (verts, feats) in [(24, 2), (300_000, 4_000)] {
            let glb = write_glb(mesh_with(verts, feats), [0.0; 16]);
            let (json, bin) = chunks(&glb);
            assert_eq!(
                json["buffers"][0]["byteLength"].as_u64().unwrap() as usize,
                bin.len(),
                "verts={verts}"
            );
            for v in json["bufferViews"].as_array().unwrap() {
                if v["buffer"] == json!(BUFFER_REAL) {
                    let end = v["byteOffset"].as_u64().unwrap() + v["byteLength"].as_u64().unwrap();
                    assert!(end as usize <= bin.len(), "bufferView past BIN end: {v}");
                }
            }
        }
    }

    /// Both assembly paths — the padded in-place one for big payloads and
    /// the copying one for everything else — must produce the same framing.
    #[test]
    fn assemble_paths_agree() {
        let json = br#"{"asset":{"version":"2.0"}}"#.to_vec();
        for payload in [16usize, GAP_MIN_PAYLOAD + 4] {
            let mut bin = vec![0u8; PREFIX_GAP];
            bin.extend((0..payload).map(|i| i as u8));
            let glb = assemble(bin, json.clone());
            let (parsed, chunk) = chunks(&glb);
            assert_eq!(parsed["asset"]["version"], json!("2.0"));
            assert_eq!(chunk.len(), payload);
            assert!(chunk.iter().enumerate().all(|(i, b)| *b == i as u8));
        }
    }
}
