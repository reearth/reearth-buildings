//! Calibration CLI for the buildings-core height estimator.
//!
//! See README at the crate root for the design / methodology. Short
//! version: fetch PLATEAU LOD1 buildings as truth (centroid +
//! measuredHeight), fetch the same area's Overture buildings, run our
//! `default_height_meters` cascade against each Overture building,
//! match Overture↔PLATEAU by point-in-polygon, summarise the
//! residuals.

mod bbox;
mod cities;
mod fetch_overture;
mod fetch_plateau;
mod matcher;
mod metrics;

use anyhow::{Context, Result};
use buildings_core::{mesh, HeightConfig};
use clap::{Parser, Subcommand};
use mvt_decoder::decode_buildings;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "height-optimizer", about, version)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// List the bundled city presets.
    Cities,
    /// Dump per-building PLATEAU LOD1 truth as csv (lon,lat,h_m).
    DumpPlateau {
        #[arg(long)]
        preset: String,
        #[arg(long)]
        cache: Option<PathBuf>,
    },
    /// Run the full eval: PLATEAU truth ↔ Overture estimate, print
    /// sliced residual stats. Uses `HeightConfig::default()` unless
    /// `--config` is given.
    Eval {
        #[arg(long)]
        preset: String,
        /// PLATEAU/Overture cache root. Default:
        /// `target/height-optimizer-cache`.
        #[arg(long)]
        cache: Option<PathBuf>,
        /// Override Overture release (yyyy-mm-dd-…). Default: probe
        /// the upstream bucket for the latest non-beta release.
        #[arg(long)]
        release: Option<String>,
        /// Path to a TOML HeightConfig override. If omitted, the
        /// production defaults are used.
        #[arg(long)]
        config: Option<PathBuf>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Cities => {
            for c in cities::all() {
                println!(
                    "{:18} city={} bbox=[{:.4},{:.4},{:.4},{:.4}] -- {}",
                    c.name, c.city_code, c.bbox.west, c.bbox.south, c.bbox.east, c.bbox.north,
                    c.note
                );
            }
        }
        Cmd::DumpPlateau { preset, cache } => {
            let city = cities::get(&preset)
                .ok_or_else(|| anyhow::anyhow!("unknown preset: {preset}"))?;
            let cache_dir = cache.unwrap_or_else(default_cache_dir);
            let buildings =
                fetch_plateau::fetch_lod1(city.city_code, &city.bbox, &cache_dir)?;
            eprintln!("fetched {} PLATEAU buildings", buildings.len());
            for b in &buildings {
                println!(
                    "{:.7},{:.7},{:.2}",
                    b.centroid.lon_deg, b.centroid.lat_deg, b.measured_height_m
                );
            }
        }
        Cmd::Eval {
            preset,
            cache,
            release,
            config,
        } => run_eval(&preset, cache, release, config)?,
    }
    Ok(())
}

fn run_eval(
    preset: &str,
    cache: Option<PathBuf>,
    release: Option<String>,
    config: Option<PathBuf>,
) -> Result<()> {
    let city = cities::get(preset)
        .ok_or_else(|| anyhow::anyhow!("unknown preset: {preset}"))?;
    let cache_dir = cache.unwrap_or_else(default_cache_dir);
    std::fs::create_dir_all(&cache_dir).ok();

    let cfg: HeightConfig = match config {
        Some(p) => {
            let s = std::fs::read_to_string(&p)
                .with_context(|| format!("read {}", p.display()))?;
            toml::from_str(&s).with_context(|| format!("parse {}", p.display()))?
        }
        None => HeightConfig::default(),
    };

    eprintln!("== PLATEAU truth ==");
    let truths =
        fetch_plateau::fetch_lod1(city.city_code, &city.bbox, &cache_dir.join("plateau"))?;
    eprintln!("PLATEAU buildings (height>0, in bbox): {}", truths.len());

    eprintln!("\n== Overture release ==");
    let release = match release {
        Some(r) => r,
        None => {
            let r = fetch_overture::latest_release()?;
            eprintln!("auto-discovered release: {r}");
            r
        }
    };
    let raw_sources =
        fetch_overture::fetch_bbox(&release, &city.bbox, &cache_dir.join("overture"))?;
    eprintln!("downloaded {} Overture tiles", raw_sources.len());

    let decoded: Vec<(u8, u32, u32, mvt_decoder::DecodedTile)> = raw_sources
        .iter()
        .filter_map(|s| match decode_buildings(&s.bytes) {
            Ok(d) => Some((s.z, s.x, s.y, d)),
            Err(e) => {
                eprintln!("warn: skip {}/{}/{}: {e}", s.z, s.x, s.y);
                None
            }
        })
        .collect();
    let mesh_sources: Vec<mesh::Source<'_>> = decoded
        .iter()
        .map(|(z, x, y, t)| mesh::Source { z: *z, x: *x, y: *y, tile: t })
        .collect();
    let estimates = mesh::extract_buildings(&mesh_sources, &cfg);
    let in_bbox: Vec<_> = estimates
        .into_iter()
        .filter(|e| city.bbox.contains_lonlat(e.centroid.lon_deg, e.centroid.lat_deg))
        .collect();
    eprintln!("Overture buildings (in bbox): {}", in_bbox.len());

    eprintln!("\n== Match + score ==");
    let m = matcher::match_buildings(&truths, &in_bbox);
    let report =
        metrics::build_report(&m.pairs, m.unmatched_truth, truths.len(), in_bbox.len());
    metrics::print_report(&format!("{} ({})", city.name, city.note), &report);
    Ok(())
}

fn default_cache_dir() -> PathBuf {
    PathBuf::from("target/height-optimizer-cache")
}
