//! PMTiles v3 reader with a pluggable byte-range backend.
//!
//! The reader is agnostic to where the bytes come from: a Cloudflare R2
//! binding, an HTTP `Range:` request, or local fs. Backends implement
//! [`ByteRangeReader`].

use async_trait::async_trait;
use bytes::Bytes;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("backend error: {0}")]
    Backend(String),
    #[error("invalid pmtiles: {0}")]
    Invalid(&'static str),
}

pub type Result<T> = std::result::Result<T, Error>;

#[async_trait(?Send)]
pub trait ByteRangeReader {
    async fn read_range(&self, start: u64, length: u64) -> Result<Bytes>;
}

pub struct PmTiles<R: ByteRangeReader> {
    _reader: R,
}

impl<R: ByteRangeReader> PmTiles<R> {
    pub fn new(reader: R) -> Self {
        Self { _reader: reader }
    }
}
