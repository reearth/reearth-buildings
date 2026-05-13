//! Minimal Mapbox Vector Tile (MVT) decoder, scoped to extracting the
//! `buildings` layer from Protomaps planet tiles.
//!
//! We hand-roll a tiny varint protobuf reader instead of pulling in
//! `prost`/`protobuf` codegen. The MVT proto is only a handful of messages
//! and we only care about a small subset (one layer, one geom type).

use flate2::read::GzDecoder;
use std::io::Read;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid mvt: {0}")]
    Invalid(&'static str),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, Default)]
pub struct BuildingFeature {
    /// One or more closed rings in tile-local coordinates (origin at NW of
    /// the tile, x→E, y→S). Each ring's first and last vertex are equal.
    pub rings: Vec<Vec<[i32; 2]>>,
    pub height: Option<f64>,
    pub min_height: Option<f64>,
    pub levels: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct DecodedTile {
    /// Tile extent (units across the tile, default 4096 per MVT spec).
    pub extent: u32,
    pub buildings: Vec<BuildingFeature>,
}

/// Decode an MVT tile, optionally gzipped. Returns features from the
/// `buildings` layer only (other layers are skipped without parsing).
pub fn decode_buildings(tile_bytes: &[u8]) -> Result<DecodedTile> {
    let bytes = maybe_gunzip(tile_bytes)?;
    let mut p = Parser::new(&bytes);

    let mut extent = 4096u32;
    let mut buildings = Vec::new();

    while !p.is_eof() {
        let (field, wt) = p.read_tag()?;
        if field == 3 && wt == 2 {
            let layer_bytes = p.read_bytes()?;
            if let Some((layer_extent, feats)) = decode_layer(layer_bytes)? {
                extent = layer_extent;
                buildings = feats;
            }
        } else {
            p.skip(wt)?;
        }
    }

    Ok(DecodedTile { extent, buildings })
}

fn maybe_gunzip(bytes: &[u8]) -> Result<Vec<u8>> {
    // gzip magic: 1f 8b
    if bytes.len() >= 2 && bytes[0] == 0x1f && bytes[1] == 0x8b {
        let mut out = Vec::with_capacity(bytes.len() * 4);
        GzDecoder::new(bytes).read_to_end(&mut out)?;
        Ok(out)
    } else {
        Ok(bytes.to_vec())
    }
}

/// Returns `Some` if this layer is `buildings`.
fn decode_layer(layer_bytes: &[u8]) -> Result<Option<(u32, Vec<BuildingFeature>)>> {
    let mut p = Parser::new(layer_bytes);
    let mut name: Option<String> = None;
    let mut keys: Vec<String> = Vec::new();
    let mut values: Vec<Value> = Vec::new();
    let mut extent: u32 = 4096;
    let mut feature_blobs: Vec<&[u8]> = Vec::new();

    while !p.is_eof() {
        let (field, wt) = p.read_tag()?;
        match (field, wt) {
            (1, 2) => name = Some(String::from_utf8_lossy(p.read_bytes()?).into_owned()),
            (2, 2) => feature_blobs.push(p.read_bytes()?),
            (3, 2) => keys.push(String::from_utf8_lossy(p.read_bytes()?).into_owned()),
            (4, 2) => values.push(decode_value(p.read_bytes()?)?),
            (5, 0) => extent = p.read_varint()? as u32,
            _ => p.skip(wt)?,
        }
    }

    if name.as_deref() != Some("buildings") {
        return Ok(None);
    }

    let mut out = Vec::with_capacity(feature_blobs.len());
    for fb in feature_blobs {
        if let Some(feat) = decode_feature(fb, &keys, &values)? {
            out.push(feat);
        }
    }
    Ok(Some((extent, out)))
}

fn decode_feature(
    bytes: &[u8],
    keys: &[String],
    values: &[Value],
) -> Result<Option<BuildingFeature>> {
    let mut p = Parser::new(bytes);
    let mut geom_type: u32 = 0;
    let mut tag_pairs: Vec<u32> = Vec::new();
    let mut geometry: Vec<u32> = Vec::new();

    while !p.is_eof() {
        let (field, wt) = p.read_tag()?;
        match (field, wt) {
            (1, 0) => {
                let _ = p.read_varint()?;
            }
            (2, 2) => {
                let raw = p.read_bytes()?;
                let mut q = Parser::new(raw);
                while !q.is_eof() {
                    tag_pairs.push(q.read_varint()? as u32);
                }
            }
            (3, 0) => geom_type = p.read_varint()? as u32,
            (4, 2) => {
                let raw = p.read_bytes()?;
                let mut q = Parser::new(raw);
                while !q.is_eof() {
                    geometry.push(q.read_varint()? as u32);
                }
            }
            _ => p.skip(wt)?,
        }
    }

    // POLYGON = 3
    if geom_type != 3 {
        return Ok(None);
    }
    let rings = decode_polygon_rings(&geometry)?;
    if rings.is_empty() {
        return Ok(None);
    }

    let mut feat = BuildingFeature {
        rings,
        ..Default::default()
    };

    let mut i = 0;
    while i + 1 < tag_pairs.len() {
        let k_idx = tag_pairs[i] as usize;
        let v_idx = tag_pairs[i + 1] as usize;
        i += 2;
        let Some(key) = keys.get(k_idx) else { continue };
        let Some(value) = values.get(v_idx) else {
            continue;
        };
        match key.as_str() {
            "height" => feat.height = value.as_f64(),
            "min_height" => feat.min_height = value.as_f64(),
            "building:levels" | "levels" => feat.levels = value.as_f64(),
            _ => {}
        }
    }
    Ok(Some(feat))
}

fn decode_polygon_rings(cmds: &[u32]) -> Result<Vec<Vec<[i32; 2]>>> {
    let mut rings: Vec<Vec<[i32; 2]>> = Vec::new();
    let mut current: Vec<[i32; 2]> = Vec::new();
    let mut cx: i32 = 0;
    let mut cy: i32 = 0;
    let mut i = 0;
    while i < cmds.len() {
        let head = cmds[i];
        i += 1;
        let id = head & 0x7;
        let count = (head >> 3) as usize;
        match id {
            1 => {
                // MoveTo
                if !current.is_empty() {
                    rings.push(std::mem::take(&mut current));
                }
                for _ in 0..count {
                    let dx = zigzag(cmds.get(i).copied().ok_or(Error::Invalid("geom"))?);
                    let dy = zigzag(cmds.get(i + 1).copied().ok_or(Error::Invalid("geom"))?);
                    i += 2;
                    cx = cx.wrapping_add(dx);
                    cy = cy.wrapping_add(dy);
                    current.push([cx, cy]);
                }
            }
            2 => {
                // LineTo
                for _ in 0..count {
                    let dx = zigzag(cmds.get(i).copied().ok_or(Error::Invalid("geom"))?);
                    let dy = zigzag(cmds.get(i + 1).copied().ok_or(Error::Invalid("geom"))?);
                    i += 2;
                    cx = cx.wrapping_add(dx);
                    cy = cy.wrapping_add(dy);
                    current.push([cx, cy]);
                }
            }
            7 => {
                // ClosePath
                if let Some(&first) = current.first() {
                    if current.last() != Some(&first) {
                        current.push(first);
                    }
                }
                rings.push(std::mem::take(&mut current));
            }
            _ => return Err(Error::Invalid("unknown geom command")),
        }
    }
    if !current.is_empty() {
        rings.push(current);
    }
    Ok(rings)
}

#[derive(Debug, Clone)]
enum Value {
    Str(String),
    F32(f32),
    F64(f64),
    I64(i64),
    U64(u64),
    Bool(bool),
    None,
}

impl Value {
    fn as_f64(&self) -> Option<f64> {
        match self {
            Value::F32(v) => Some(*v as f64),
            Value::F64(v) => Some(*v),
            Value::I64(v) => Some(*v as f64),
            Value::U64(v) => Some(*v as f64),
            Value::Str(s) => s.parse().ok(),
            _ => None,
        }
    }
}

fn decode_value(bytes: &[u8]) -> Result<Value> {
    let mut p = Parser::new(bytes);
    let mut out = Value::None;
    while !p.is_eof() {
        let (field, wt) = p.read_tag()?;
        match (field, wt) {
            (1, 2) => out = Value::Str(String::from_utf8_lossy(p.read_bytes()?).into_owned()),
            (2, 5) => out = Value::F32(f32::from_le_bytes(p.read_fixed32()?)),
            (3, 1) => out = Value::F64(f64::from_le_bytes(p.read_fixed64()?)),
            (4, 0) => out = Value::I64(p.read_varint()? as i64),
            (5, 0) => out = Value::U64(p.read_varint()?),
            (6, 0) => {
                let raw = p.read_varint()?;
                out = Value::I64(zigzag64(raw));
            }
            (7, 0) => out = Value::Bool(p.read_varint()? != 0),
            _ => p.skip(wt)?,
        }
    }
    Ok(out)
}

// -------- minimal protobuf reader --------

struct Parser<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }
    fn is_eof(&self) -> bool {
        self.pos >= self.buf.len()
    }
    fn read_byte(&mut self) -> Result<u8> {
        let b = *self.buf.get(self.pos).ok_or(Error::Invalid("eof"))?;
        self.pos += 1;
        Ok(b)
    }
    fn read_varint(&mut self) -> Result<u64> {
        let mut x: u64 = 0;
        let mut shift = 0;
        loop {
            let b = self.read_byte()?;
            x |= ((b & 0x7f) as u64) << shift;
            if b & 0x80 == 0 {
                return Ok(x);
            }
            shift += 7;
            if shift >= 64 {
                return Err(Error::Invalid("varint overflow"));
            }
        }
    }
    fn read_tag(&mut self) -> Result<(u32, u32)> {
        let v = self.read_varint()?;
        Ok(((v >> 3) as u32, (v & 0x7) as u32))
    }
    fn read_bytes(&mut self) -> Result<&'a [u8]> {
        let len = self.read_varint()? as usize;
        let end = self
            .pos
            .checked_add(len)
            .ok_or(Error::Invalid("length overflow"))?;
        if end > self.buf.len() {
            return Err(Error::Invalid("truncated"));
        }
        let s = &self.buf[self.pos..end];
        self.pos = end;
        Ok(s)
    }
    fn read_fixed32(&mut self) -> Result<[u8; 4]> {
        if self.pos + 4 > self.buf.len() {
            return Err(Error::Invalid("fixed32 truncated"));
        }
        let mut out = [0u8; 4];
        out.copy_from_slice(&self.buf[self.pos..self.pos + 4]);
        self.pos += 4;
        Ok(out)
    }
    fn read_fixed64(&mut self) -> Result<[u8; 8]> {
        if self.pos + 8 > self.buf.len() {
            return Err(Error::Invalid("fixed64 truncated"));
        }
        let mut out = [0u8; 8];
        out.copy_from_slice(&self.buf[self.pos..self.pos + 8]);
        self.pos += 8;
        Ok(out)
    }
    fn skip(&mut self, wire_type: u32) -> Result<()> {
        match wire_type {
            0 => {
                self.read_varint()?;
            }
            1 => {
                let _ = self.read_fixed64()?;
            }
            2 => {
                let _ = self.read_bytes()?;
            }
            5 => {
                let _ = self.read_fixed32()?;
            }
            _ => return Err(Error::Invalid("unknown wire type")),
        }
        Ok(())
    }
}

fn zigzag(v: u32) -> i32 {
    ((v >> 1) as i32) ^ -((v & 1) as i32)
}
fn zigzag64(v: u64) -> i64 {
    ((v >> 1) as i64) ^ -((v & 1) as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zigzag_roundtrip() {
        assert_eq!(zigzag(0), 0);
        assert_eq!(zigzag(1), -1);
        assert_eq!(zigzag(2), 1);
        assert_eq!(zigzag(3), -2);
    }

    #[test]
    fn varint_decode() {
        // 300 -> [0xac, 0x02]
        let mut p = Parser::new(&[0xac, 0x02]);
        assert_eq!(p.read_varint().unwrap(), 300);
    }

    #[test]
    fn empty_input_ok() {
        let t = decode_buildings(&[]).unwrap();
        assert_eq!(t.buildings.len(), 0);
    }
}
