//! Overture buildings PMTiles fetcher.
//!
//! Pulls the z=14 MVT tiles overlapping a bbox from the upstream
//! Overture S3 bucket via PMTiles range reads. Tiles are cached on
//! disk by `(release, z, x, y)`.

use crate::bbox::BBox;
use anyhow::{anyhow, Context, Result};
use futures::stream::{FuturesUnordered, StreamExt};
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Overture's PMTiles live here. The former
/// `overturemaps-tiles-us-west-2-beta` bucket was shut off (403
/// AllAccessDisabled) in July 2026; the replacement also nests every
/// release under a `tiles/` prefix, which the old one did not.
const BUCKET: &str = "https://overturemaps-extras-us-west-2.s3.us-west-2.amazonaws.com";
const PREFIX: &str = "tiles/";
/// Source tile zoom for Overture buildings PMTiles (matches the
/// production worker — see `src/lod.ts`).
pub const SOURCE_Z: u8 = 14;

pub struct OvertureSource {
    pub z: u8,
    pub x: u32,
    pub y: u32,
    pub bytes: Vec<u8>,
}

pub fn fetch_release_url(release: &str) -> String {
    format!("{BUCKET}/{PREFIX}{release}/buildings.pmtiles")
}

/// Discover the most recent yyyy-mm-dd release under the bucket's
/// `tiles/` prefix. Mirrors `src/version.ts::probeLatestRelease` — same
/// ordering, same preference for a non-beta release on the same date.
///
/// The listing has to be scoped to `tiles/`: the bucket's root holds
/// several unrelated prefixes (`stac/`, `maproulette/`, …) and no
/// releases at all.
pub fn latest_release() -> Result<String> {
    let xml = ureq::get(&format!(
        "{BUCKET}/?list-type=2&delimiter=%2F&prefix={PREFIX}"
    ))
    .timeout(std::time::Duration::from_secs(30))
    .call()?
    .into_string()?;
    let mut releases: Vec<String> = Vec::new();
    for cap in xml.split("<Prefix>").skip(1) {
        if let Some(end) = cap.find("</Prefix>") {
            let p = &cap[..end];
            let p = p.trim_end_matches('/');
            // Each CommonPrefixes entry is `tiles/<release>`; the echoed
            // request prefix is `tiles/` alone and drops out of the date
            // check below.
            let p = p.strip_prefix(PREFIX).unwrap_or(p);
            if p.len() >= 10 && p.as_bytes()[4] == b'-' && p.as_bytes()[7] == b'-' {
                releases.push(p.to_string());
            }
        }
    }
    releases.sort_by(|a, b| compare_releases(a, b));
    let latest = releases
        .last()
        .cloned()
        .ok_or_else(|| anyhow!("no Overture releases found"))?;
    let date = &latest[..10];
    for r in releases.iter().rev() {
        if &r[..10] != date {
            break;
        }
        if !r.ends_with("-beta") {
            return Ok(r.clone());
        }
    }
    Ok(latest)
}

/// Order two `yyyy-mm-dd.n` release names, oldest first. Date first,
/// then the numeric suffix — `2026-08-19.10` is newer than
/// `2026-08-19.2`, which a plain lexical sort gets backwards. The `n`
/// may carry a `-beta` marker, which parses to its leading number; the
/// beta preference is applied by the caller.
fn compare_releases(a: &str, b: &str) -> std::cmp::Ordering {
    let suffix = |r: &str| {
        r.get(11..)
            .map(|s| {
                s.split(|c: char| !c.is_ascii_digit())
                    .next()
                    .unwrap_or("")
                    .parse::<u32>()
                    .unwrap_or(0)
            })
            .unwrap_or(0)
    };
    a[..10]
        .cmp(&b[..10])
        .then_with(|| suffix(a).cmp(&suffix(b)))
}

/// Fetch every z=14 MVT tile overlapping `bbox` for `release`. Returns
/// raw (decompressed) MVT bytes per tile.
pub fn fetch_bbox(release: &str, bbox: &BBox, cache: &Path) -> Result<Vec<OvertureSource>> {
    let (x0, y0) = lonlat_to_tile(bbox.west, bbox.north, SOURCE_Z);
    let (x1, y1) = lonlat_to_tile(bbox.east, bbox.south, SOURCE_Z);
    let mut tiles: Vec<(u32, u32)> = Vec::new();
    for x in x0..=x1 {
        for y in y0..=y1 {
            tiles.push((x, y));
        }
    }
    eprintln!("overture: {} tiles at z={SOURCE_Z}", tiles.len());

    std::fs::create_dir_all(cache).ok();
    let url = fetch_release_url(release);

    // Single-threaded async runtime — pmtiles reader is async-only.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let reader = Arc::new(rt.block_on(open_reader(&url))?);
    let cache = cache.to_path_buf();
    let release_owned = release.to_string();

    let results: Vec<Result<Option<OvertureSource>>> = rt.block_on(async {
        let mut futs = FuturesUnordered::new();
        for (x, y) in tiles {
            let reader = Arc::clone(&reader);
            let cache = cache.clone();
            let release = release_owned.clone();
            futs.push(async move {
                let path = cache_path(&cache, &release, SOURCE_Z, x, y);
                let bytes = if let Ok(b) = std::fs::read(&path) {
                    b
                } else {
                    let raw = fetch_one(&reader, SOURCE_Z, x, y).await?;
                    match raw {
                        Some(b) => {
                            let decoded = maybe_gunzip(&b)?;
                            std::fs::write(&path, &decoded).ok();
                            decoded
                        }
                        None => {
                            std::fs::write(&path, b"").ok();
                            Vec::new()
                        }
                    }
                };
                if bytes.is_empty() {
                    Ok::<_, anyhow::Error>(None)
                } else {
                    Ok(Some(OvertureSource {
                        z: SOURCE_Z,
                        x,
                        y,
                        bytes,
                    }))
                }
            });
        }
        let mut out = Vec::new();
        while let Some(r) = futs.next().await {
            out.push(r);
        }
        out
    });

    let mut sources: Vec<OvertureSource> = Vec::new();
    for r in results {
        if let Some(s) = r? {
            sources.push(s);
        }
    }
    Ok(sources)
}

async fn open_reader(
    url: &str,
) -> Result<pmtiles::AsyncPmTilesReader<pmtiles::HttpBackend, pmtiles::NoCache>> {
    let client = pmtiles::reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()?;
    pmtiles::AsyncPmTilesReader::new_with_url(client, url)
        .await
        .context("open PMTiles reader")
}

async fn fetch_one(
    reader: &pmtiles::AsyncPmTilesReader<pmtiles::HttpBackend, pmtiles::NoCache>,
    z: u8,
    x: u32,
    y: u32,
) -> Result<Option<Vec<u8>>> {
    let coord =
        pmtiles::TileCoord::new(z, x, y).map_err(|e| anyhow!("invalid tile {z}/{x}/{y}: {e}"))?;
    let res: Option<bytes::Bytes> = reader.get_tile(coord).await?;
    Ok(res.map(|b| b.to_vec()))
}

fn maybe_gunzip(b: &[u8]) -> Result<Vec<u8>> {
    if b.len() >= 2 && b[0] == 0x1f && b[1] == 0x8b {
        let mut d = flate2::read::GzDecoder::new(b);
        let mut out = Vec::new();
        d.read_to_end(&mut out)?;
        Ok(out)
    } else {
        Ok(b.to_vec())
    }
}

fn cache_path(cache: &Path, release: &str, z: u8, x: u32, y: u32) -> PathBuf {
    cache.join(format!("ov-{release}-{z}-{x}-{y}.mvt"))
}

fn lonlat_to_tile(lon: f64, lat: f64, z: u8) -> (u32, u32) {
    let n = (1u64 << z) as f64;
    let x = ((lon + 180.0) / 360.0 * n).floor().max(0.0) as u32;
    let lat_rad = lat.to_radians();
    let y = ((1.0 - lat_rad.tan().asinh() / std::f64::consts::PI) * 0.5 * n)
        .floor()
        .max(0.0) as u32;
    let max = (1u32 << z) - 1;
    (x.min(max), y.min(max))
}
