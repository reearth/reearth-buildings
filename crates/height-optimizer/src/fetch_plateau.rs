//! PLATEAU LOD1 building truth fetcher.
//!
//! Pipeline (uses the public CityGML JSON APIs — avoids parsing
//! Draco-compressed b3dm or raw CityGML XML):
//!
//!   1. `GET /datacatalog/citygml/{cityCode}` → list of per-mesh
//!      bldg.gml asset URLs.
//!   2. For each gml URL: `GET /citygml/features?sid=0/0/0/0&url=...`
//!      to harvest every `gml:id` in the file (`sid=0/0/0/0`, the
//!      whole-world spatial ID, returns all features regardless of
//!      mesh location).
//!   3. Batch: `GET /citygml/attributes?url=...&id=csv` → records
//!      with `_bbox.center.{lng,lat}` and `bldg:measuredHeight`.
//!   4. Filter by bbox + drop `measuredHeight == -9999` (PLATEAU's
//!      no-data sentinel).

use crate::bbox::BBox;
use anyhow::{anyhow, Context, Result};
use buildings_core::coord::LonLat;
use rayon::prelude::*;
use serde_json::Value;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

const API_BASE: &str = "https://api.plateauview.mlit.go.jp";
/// IDs per `/citygml/attributes` request. Each ID is ~40 chars; 200
/// keeps the URL under ~10 KB and the response under ~300 KB.
const ATTRIBUTES_BATCH: usize = 200;
/// Whole-world spatial ID — returns every feature in the file regardless
/// of geographic location.
const WORLD_SID: &str = "0/0/0/0";

#[derive(Debug, Clone)]
pub struct Building {
    pub centroid: LonLat,
    pub measured_height_m: f32,
    pub gml_id: String,
}

pub fn fetch_lod1(city_code: &str, bbox: &BBox, cache: &Path) -> Result<Vec<Building>> {
    std::fs::create_dir_all(cache).ok();
    let all_urls = list_bldg_gml_urls(city_code, cache)
        .with_context(|| format!("list bldg gml for city {city_code}"))?;
    // Pre-filter by Japanese standard mesh code → bbox so we skip
    // gml files that don't even touch our area. The catalog returns
    // every file in the city's mesh footprint, which for Setagaya
    // (78 files) or Tsukuba (315 files) would otherwise mean
    // hundreds of pointless feature-list calls.
    let bldg_urls: Vec<String> = all_urls
        .into_iter()
        .filter(|u| match mesh_bbox_from_url(u) {
            Some(mb) => mb.intersects(bbox),
            None => true, // unknown filename shape — keep, don't drop
        })
        .collect();
    eprintln!(
        "city {city_code}: {} bldg gml files (after mesh-bbox prefilter)",
        bldg_urls.len()
    );

    let done = AtomicUsize::new(0);
    let total = bldg_urls.len();

    // Outer parallelism: process gml files concurrently. Inner
    // parallelism: per gml, fan attribute batches out across the same
    // pool. rayon nests cooperatively, so the global thread count
    // stays bounded.
    let per_gml: Vec<Result<Vec<Building>>> = bldg_urls
        .par_iter()
        .map(|url| -> Result<Vec<Building>> {
            let ids = list_feature_ids(url, cache)?;
            let batches: Vec<&[String]> = ids.chunks(ATTRIBUTES_BATCH).collect();
            let nested: Result<Vec<Vec<Building>>> = batches
                .par_iter()
                .map(|chunk| -> Result<Vec<Building>> {
                    let records = fetch_attributes(url, chunk, cache)?;
                    let mut local: Vec<Building> = Vec::new();
                    for r in records {
                        if let Some(b) = building_from_record(&r) {
                            if bbox.contains_lonlat(b.centroid.lon_deg, b.centroid.lat_deg) {
                                local.push(b);
                            }
                        }
                    }
                    Ok(local)
                })
                .collect();
            let done_n = done.fetch_add(1, Ordering::Relaxed) + 1;
            eprintln!(
                "[{done_n}/{total}] {}  features={} → in_bbox={}",
                short_url(url),
                ids.len(),
                nested.as_ref().map(|v| v.iter().map(|x| x.len()).sum::<usize>()).unwrap_or(0)
            );
            Ok(nested?.into_iter().flatten().collect())
        })
        .collect();

    let mut out: Vec<Building> = Vec::new();
    for r in per_gml {
        out.extend(r?);
    }
    Ok(out)
}

fn building_from_record(r: &Value) -> Option<Building> {
    let h = r.get("bldg:measuredHeight").and_then(|v| v.as_f64())?;
    if h <= 0.0 || (h - -9999.0).abs() < 0.5 {
        return None;
    }
    let center = r.get("_bbox").and_then(|b| b.get("center"))?;
    let lng = center.get("lng").and_then(|v| v.as_f64())?;
    let lat = center.get("lat").and_then(|v| v.as_f64())?;
    let id = r
        .get("gml:id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    Some(Building {
        centroid: LonLat { lon_deg: lng, lat_deg: lat },
        measured_height_m: h as f32,
        gml_id: id,
    })
}

fn list_bldg_gml_urls(city_code: &str, cache: &Path) -> Result<Vec<String>> {
    let url = format!("{API_BASE}/datacatalog/citygml/{city_code}");
    let bytes = fetch_with_cache(&url, cache, "json")?;
    let doc: Value = serde_json::from_slice(&bytes)
        .with_context(|| format!("parse {url}"))?;
    let cities = doc
        .get("cities")
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow!("catalog response missing `cities`"))?;
    let mut urls: Vec<String> = Vec::new();
    for city in cities {
        // Latest entry per city — the API returns a single most-recent
        // entry, but iterate defensively.
        let bldg = city
            .pointer("/files/bldg")
            .and_then(|v| v.as_array())
            .ok_or_else(|| anyhow!("city has no files.bldg"))?;
        for f in bldg {
            if let Some(u) = f.get("url").and_then(|v| v.as_str()) {
                urls.push(u.to_string());
            }
        }
    }
    Ok(urls)
}

fn list_feature_ids(gml_url: &str, cache: &Path) -> Result<Vec<String>> {
    let url = format!(
        "{API_BASE}/citygml/features?sid={WORLD_SID}&url={}",
        urlencode(gml_url)
    );
    let bytes = fetch_with_cache(&url, cache, "json")?;
    let doc: Value = serde_json::from_slice(&bytes)?;
    let ids = doc
        .get("featureIds")
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow!("features response missing featureIds"))?;
    Ok(ids
        .iter()
        .filter_map(|v| v.as_str().map(|s| s.to_string()))
        .collect())
}

fn fetch_attributes(gml_url: &str, ids: &[String], cache: &Path) -> Result<Vec<Value>> {
    let id_csv = ids.join(",");
    let url = format!(
        "{API_BASE}/citygml/attributes?url={}&id={}&skip_code_list_fetch=true",
        urlencode(gml_url),
        urlencode(&id_csv),
    );
    let bytes = fetch_with_cache(&url, cache, "json")?;
    let arr: Vec<Value> = serde_json::from_slice(&bytes)
        .with_context(|| format!("parse attributes batch ({} ids)", ids.len()))?;
    Ok(arr)
}

// ---------------- HTTP + cache ----------------

fn cache_path(cache: &Path, url: &str, ext: &str) -> PathBuf {
    cache.join(format!("{:016x}.{ext}", fnv1a(url)))
}

fn fnv1a(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

fn fetch_with_cache(url: &str, cache: &Path, ext: &str) -> Result<Vec<u8>> {
    let path = cache_path(cache, url, ext);
    if let Ok(bytes) = std::fs::read(&path) {
        return Ok(bytes);
    }
    // Retry transient network errors. PLATEAU's attributes endpoint
    // occasionally times out under load; a second attempt 2s later
    // almost always succeeds.
    const MAX_ATTEMPTS: u32 = 4;
    let mut last_err: Option<anyhow::Error> = None;
    for attempt in 0..MAX_ATTEMPTS {
        if attempt > 0 {
            std::thread::sleep(std::time::Duration::from_millis(
                500u64 * (1u64 << attempt),
            ));
        }
        match ureq::get(url)
            .timeout(std::time::Duration::from_secs(180))
            .call()
        {
            Ok(resp) => {
                let mut bytes: Vec<u8> = Vec::new();
                if let Err(e) = resp.into_reader().read_to_end(&mut bytes) {
                    last_err = Some(anyhow::anyhow!("read body: {e}"));
                    continue;
                }
                std::fs::write(&path, &bytes).ok();
                return Ok(bytes);
            }
            Err(e) => {
                last_err = Some(anyhow::Error::new(e));
            }
        }
    }
    Err(last_err
        .unwrap_or_else(|| anyhow!("unknown fetch error"))
        .context(format!("GET {}", short_url(url))))
}

fn urlencode(s: &str) -> String {
    // RFC 3986 unreserved: A-Z a-z 0-9 - _ . ~
    let mut out = String::with_capacity(s.len());
    for b in s.as_bytes() {
        let c = *b as char;
        if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~') {
            out.push(c);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}

/// Japanese standard 3rd-order mesh code → degree-space bbox.
/// `xxxxxxxx` = first-order(4) + second-order(2) + third-order(2).
/// 3rd-order mesh is ~1 km square (30" lat × 45" lon).
fn mesh_to_bbox(code: &str) -> Option<BBox> {
    if code.len() < 8 || !code.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let p1: f64 = code[0..2].parse().ok()?;
    let p2: f64 = code[2..4].parse().ok()?;
    let s1: f64 = code[4..5].parse().ok()?;
    let s2: f64 = code[5..6].parse().ok()?;
    let t1: f64 = code[6..7].parse().ok()?;
    let t2: f64 = code[7..8].parse().ok()?;
    let lat0 = p1 / 1.5 + s1 * (5.0 / 60.0) + t1 * (30.0 / 3600.0);
    let lon0 = p2 + 100.0 + s2 * (7.5 / 60.0) + t2 * (45.0 / 3600.0);
    let dlat = 30.0 / 3600.0;
    let dlon = 45.0 / 3600.0;
    Some(BBox::new(lon0, lat0, lon0 + dlon, lat0 + dlat))
}

/// Extract the mesh code from a PLATEAU bldg gml URL.
/// `.../udx/bldg/53394509_bldg_6697_op.gml` → `Some("53394509")`.
fn mesh_bbox_from_url(url: &str) -> Option<BBox> {
    let last = url.rsplit('/').next()?;
    let code = last.split('_').next()?;
    mesh_to_bbox(code)
}

fn short_url(url: &str) -> String {
    if url.len() > 100 {
        format!("{}…{}", &url[..60], &url[url.len() - 36..])
    } else {
        url.to_string()
    }
}
