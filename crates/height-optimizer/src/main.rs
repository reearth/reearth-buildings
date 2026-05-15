//! Calibration CLI for the buildings-core height estimator.

mod bbox;
mod cities;
mod fetch_overture;
mod fetch_plateau;
mod matcher;
mod metrics;
mod report;

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
    /// Print the production-default HeightConfig as TOML. Useful as a
    /// starting point for hand-tuning.
    DumpConfig {
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Evaluate one preset against a single config (default if --config
    /// is omitted).
    Eval {
        #[arg(long)]
        preset: String,
        #[arg(long)]
        cache: Option<PathBuf>,
        #[arg(long)]
        release: Option<String>,
        #[arg(long)]
        config: Option<PathBuf>,
    },
    /// Run baseline (default if --baseline omitted) + candidate
    /// across one or more presets and emit a side-by-side Markdown
    /// report.
    Compare {
        /// Comma-separated preset names (e.g. `chiyoda,setagaya`).
        #[arg(long)]
        presets: String,
        #[arg(long)]
        baseline: Option<PathBuf>,
        #[arg(long)]
        candidate: PathBuf,
        #[arg(long)]
        cache: Option<PathBuf>,
        #[arg(long)]
        release: Option<String>,
        /// Path to write the Markdown report. Default: stdout.
        #[arg(long)]
        out: Option<PathBuf>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Cities => {
            for c in cities::all() {
                println!(
                    "{:18} city={} bbox=[{:.4},{:.4},{:.4},{:.4}] -- {}",
                    c.name,
                    c.city_code,
                    c.bbox.west,
                    c.bbox.south,
                    c.bbox.east,
                    c.bbox.north,
                    c.note
                );
            }
        }
        Cmd::DumpPlateau { preset, cache } => {
            let city =
                cities::get(&preset).ok_or_else(|| anyhow::anyhow!("unknown preset: {preset}"))?;
            let cache_dir = cache.unwrap_or_else(default_cache_dir);
            let buildings =
                fetch_plateau::fetch_lod1(city.city_code, &city.bbox, &cache_dir.join("plateau"))?;
            eprintln!("fetched {} PLATEAU buildings", buildings.len());
            for b in &buildings {
                println!(
                    "{:.7},{:.7},{:.2}",
                    b.centroid.lon_deg, b.centroid.lat_deg, b.measured_height_m
                );
            }
        }
        Cmd::DumpConfig { out } => {
            let cfg = HeightConfig::default();
            let toml_str = toml::to_string_pretty(&cfg)?;
            match out {
                Some(p) => std::fs::write(&p, toml_str)
                    .with_context(|| format!("write {}", p.display()))?,
                None => print!("{toml_str}"),
            }
        }
        Cmd::Eval {
            preset,
            cache,
            release,
            config,
        } => {
            let cache_dir = cache.unwrap_or_else(default_cache_dir);
            let cfg = load_config(config.as_deref())?;
            let release = resolve_release(release)?;
            let result = run_pipeline(&preset, &release, &cache_dir, &cfg)?;
            metrics::print_report(
                &format!("{} ({})", result.city_name, result.city_note),
                &result.report,
            );
        }
        Cmd::Compare {
            presets,
            baseline,
            candidate,
            cache,
            release,
            out,
        } => {
            let cache_dir = cache.unwrap_or_else(default_cache_dir);
            let baseline_cfg = load_config(baseline.as_deref())?;
            let candidate_cfg = load_config(Some(&candidate))?;
            let release = resolve_release(release)?;
            let names: Vec<&str> = presets.split(',').map(|s| s.trim()).collect();

            let mut sections: Vec<report::ComparisonSection> = Vec::new();
            for name in &names {
                eprintln!("\n############ {name} ############");
                let base = run_pipeline(name, &release, &cache_dir, &baseline_cfg)?;
                let cand = run_pipeline(name, &release, &cache_dir, &candidate_cfg)?;
                sections.push(report::ComparisonSection {
                    city_name: base.city_name,
                    city_note: base.city_note,
                    baseline: base.report,
                    candidate: cand.report,
                });
            }

            let md = report::render_markdown(&sections, baseline.as_deref(), &candidate, &release);
            match out {
                Some(p) => {
                    std::fs::write(&p, md)?;
                    eprintln!("wrote {}", p.display());
                }
                None => println!("{md}"),
            }
        }
    }
    Ok(())
}

struct PipelineResult {
    city_name: String,
    city_note: String,
    report: metrics::Report,
}

fn run_pipeline(
    preset: &str,
    release: &str,
    cache: &std::path::Path,
    cfg: &HeightConfig,
) -> Result<PipelineResult> {
    let city = cities::get(preset).ok_or_else(|| anyhow::anyhow!("unknown preset: {preset}"))?;

    eprintln!("== PLATEAU truth ({}) ==", city.name);
    let truths = fetch_plateau::fetch_lod1(city.city_code, &city.bbox, &cache.join("plateau"))?;
    eprintln!("PLATEAU buildings (in bbox, height>0): {}", truths.len());

    eprintln!("== Overture estimate ({}) ==", city.name);
    let raw = fetch_overture::fetch_bbox(release, &city.bbox, &cache.join("overture"))?;
    let decoded: Vec<(u8, u32, u32, mvt_decoder::DecodedTile)> = raw
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
        .map(|(z, x, y, t)| mesh::Source {
            z: *z,
            x: *x,
            y: *y,
            tile: t,
        })
        .collect();
    let estimates = mesh::extract_buildings(&mesh_sources, cfg);
    let in_bbox: Vec<_> = estimates
        .into_iter()
        .filter(|e| {
            city.bbox
                .contains_lonlat(e.centroid.lon_deg, e.centroid.lat_deg)
        })
        .collect();
    eprintln!("Overture buildings (in bbox): {}", in_bbox.len());

    let m = matcher::match_buildings(&truths, &in_bbox);
    let report = metrics::build_report(&m.pairs, m.unmatched_truth, truths.len(), in_bbox.len());
    Ok(PipelineResult {
        city_name: city.name.to_string(),
        city_note: city.note.to_string(),
        report,
    })
}

fn load_config(path: Option<&std::path::Path>) -> Result<HeightConfig> {
    match path {
        Some(p) => {
            let s = std::fs::read_to_string(p).with_context(|| format!("read {}", p.display()))?;
            toml::from_str(&s).with_context(|| format!("parse {}", p.display()))
        }
        None => Ok(HeightConfig::default()),
    }
}

fn resolve_release(release: Option<String>) -> Result<String> {
    if let Some(r) = release {
        return Ok(r);
    }
    let r = fetch_overture::latest_release()?;
    eprintln!("auto-discovered Overture release: {r}");
    Ok(r)
}

fn default_cache_dir() -> PathBuf {
    PathBuf::from("target/height-optimizer-cache")
}
