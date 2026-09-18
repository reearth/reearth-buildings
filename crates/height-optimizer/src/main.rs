//! Calibration CLI for the buildings-core height estimator.

mod bbox;
mod bench;
mod cities;
mod dataset;
mod fetch_dutch_3dbag;
mod fetch_overture;
mod fetch_plateau;
mod gbt;
mod matcher;
mod metrics;
mod rd;
mod report;
mod truth;

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
    /// Dump per-building truth as csv (lon,lat,h_m) from the preset's
    /// configured source (PLATEAU or 3D BAG).
    #[command(alias = "dump-plateau")]
    DumpTruth {
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
    /// Benchmark on-demand height estimation speed: per-tile build_mesh
    /// with the model Off vs On, plus per-building and per-isolate
    /// component costs.
    Bench {
        /// Comma-separated preset names.
        #[arg(long, default_value = "chiyoda,setagaya,iiyama")]
        presets: String,
        /// Timed runs per measurement (medians reported).
        #[arg(long, default_value_t = 20)]
        iterations: usize,
        #[arg(long)]
        cache: Option<PathBuf>,
        #[arg(long)]
        release: Option<String>,
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
    /// Train the offline GBT height model against PLATEAU truth and write
    /// the JSON artifact consumed by `buildings-core`.
    Train {
        #[arg(
            long,
            default_value = "chiyoda,setagaya,nishi-yokohama,iiyama,hachioji"
        )]
        train_presets: String,
        #[arg(long, default_value = "tsukuba,kanazawa,takamatsu")]
        holdout_presets: String,
        #[arg(long, default_value_t = 48)]
        n_trees: usize,
        #[arg(long, default_value_t = 4)]
        max_depth: usize,
        #[arg(long, default_value_t = 0.1)]
        learning_rate: f32,
        #[arg(long, default_value_t = 20)]
        min_samples_leaf: usize,
        #[arg(long, default_value_t = 16)]
        top_k_class: usize,
        #[arg(long, default_value_t = 8)]
        top_k_subtype: usize,
        #[arg(long, default_value_t = 42)]
        seed: u64,
        #[arg(
            long,
            default_value = "crates/buildings-core/models/height_gbt_v1.json"
        )]
        out: PathBuf,
        #[arg(long)]
        cache: Option<PathBuf>,
        #[arg(long)]
        release: Option<String>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Cities => {
            for c in cities::all() {
                println!(
                    "{:18} truth={:14} bbox=[{:.4},{:.4},{:.4},{:.4}] -- {}",
                    c.name,
                    c.truth.label(),
                    c.bbox.west,
                    c.bbox.south,
                    c.bbox.east,
                    c.bbox.north,
                    c.note
                );
            }
        }
        Cmd::DumpTruth { preset, cache } => {
            let city =
                cities::get(&preset).ok_or_else(|| anyhow::anyhow!("unknown preset: {preset}"))?;
            let cache_dir = cache.unwrap_or_else(default_cache_dir);
            let buildings = fetch_truth(city, &cache_dir)?;
            eprintln!("fetched {} truth buildings", buildings.len());
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
        Cmd::Bench {
            presets,
            iterations,
            cache,
            release,
        } => {
            let cache_dir = cache.unwrap_or_else(default_cache_dir);
            let release = resolve_release(release)?;
            for name in presets.split(',').map(|s| s.trim()) {
                bench::run(name, &release, &cache_dir, iterations)?;
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
        Cmd::Train {
            train_presets,
            holdout_presets,
            n_trees,
            max_depth,
            learning_rate,
            min_samples_leaf,
            top_k_class,
            top_k_subtype,
            seed,
            out,
            cache,
            release,
        } => {
            let cache_dir = cache.unwrap_or_else(default_cache_dir);
            let release = resolve_release(release)?;
            let params = gbt::TrainParams {
                n_trees,
                max_depth,
                learning_rate,
                min_samples_leaf,
                seed,
                ..Default::default()
            };
            run_train(
                &train_presets,
                &holdout_presets,
                top_k_class,
                top_k_subtype,
                &params,
                &out,
                &cache_dir,
                &release,
            )?;
        }
    }
    Ok(())
}

/// Collect owned pairs for a comma-separated preset list, always with the
/// production-default config and `ModelMode::Off` so the estimates' height
/// and `height_method` reflect the *legacy* cascade the model replaces.
fn collect_preset_set(
    presets: &str,
    release: &str,
    cache: &std::path::Path,
) -> Result<Vec<(String, Vec<dataset::OwnedPair>)>> {
    let cfg = HeightConfig::default();
    let mut out = Vec::new();
    for name in presets
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
    {
        eprintln!("\n############ collect {name} ############");
        let pairs = dataset::collect_pairs(name, release, cache, &cfg)?;
        out.push((name.to_string(), pairs));
    }
    Ok(out)
}

#[allow(clippy::too_many_arguments)]
fn run_train(
    train_presets: &str,
    holdout_presets: &str,
    top_k_class: usize,
    top_k_subtype: usize,
    params: &gbt::TrainParams,
    out: &std::path::Path,
    cache: &std::path::Path,
    release: &str,
) -> Result<()> {
    let train_pairs = collect_preset_set(train_presets, release, cache)?;
    let holdout_pairs = collect_preset_set(holdout_presets, release, cache)?;

    let (train_ds, holdout_ds) =
        dataset::build_datasets(&train_pairs, &holdout_pairs, top_k_class, top_k_subtype);
    let clamp = (dataset::CLAMP_MIN_M, dataset::CLAMP_MAX_M);

    eprintln!(
        "\ntraining: {} train rows, {} holdout rows, {} features",
        train_ds.rows.len(),
        holdout_ds.rows.len(),
        train_ds.encoder.len()
    );
    let report = gbt::train(&train_ds, &holdout_ds, params, clamp, "height_gbt_v1");
    let model = &report.model;

    // Minified artifact.
    let json = serde_json::to_string(model)?;
    std::fs::write(out, &json).with_context(|| format!("write {}", out.display()))?;
    let artifact_bytes = json.len();

    // ---------------- report ----------------
    println!("\n===== GBT training report =====");
    println!("out: {}", out.display());
    println!(
        "params: n_trees={} max_depth={} lr={} min_samples_leaf={} l2={} seed={}",
        params.n_trees,
        params.max_depth,
        params.learning_rate,
        params.min_samples_leaf,
        params.l2_lambda,
        params.seed,
    );

    println!("\n-- rows per preset (kept = model population; filtered = has explicit height or num_floors) --");
    println!(
        "{:<18} {:>8} {:>8} {:>8} {:>6}",
        "preset", "matched", "kept", "filtered", "role"
    );
    let mut kept_per_preset: std::collections::BTreeMap<&str, usize> =
        std::collections::BTreeMap::new();
    for row in train_ds.rows.iter().chain(holdout_ds.rows.iter()) {
        *kept_per_preset.entry(row.preset.as_str()).or_insert(0) += 1;
    }
    for (role, sets) in [("train", &train_pairs), ("holdout", &holdout_pairs)] {
        for (name, pairs) in sets {
            let matched = pairs.len();
            let kept = kept_per_preset.get(name.as_str()).copied().unwrap_or(0);
            println!(
                "{:<18} {:>8} {:>8} {:>8} {:>6}",
                name,
                matched,
                kept,
                matched - kept,
                role
            );
        }
    }

    println!("\n-- vocab (train population, top-k by frequency) --");
    println!(
        "class  ({:>2}): {}",
        model.encoder.class_vocab.len(),
        model.encoder.class_vocab.join(", ")
    );
    println!(
        "subtype({:>2}): {}",
        model.encoder.subtype_vocab.len(),
        model.encoder.subtype_vocab.join(", ")
    );

    println!("\n-- training curve (every 8 trees; RMSE in log space, MAE in metres) --");
    println!(
        "{:>6} {:>14} {:>16} {:>14}",
        "tree", "train_rmse_log", "holdout_rmse_log", "holdout_mae_m"
    );
    for it in &report.iters {
        let last = it.tree_idx + 1 == report.iters.len();
        if (it.tree_idx + 1) % 8 == 0 || last || it.tree_idx == 0 {
            println!(
                "{:>6} {:>14.5} {:>16.5} {:>14.3}",
                it.tree_idx + 1,
                it.train_rmse_log,
                it.holdout_rmse_log,
                it.holdout_mae_m
            );
        }
    }

    println!("\n-- final per-preset metrics (model population only): MODEL vs legacy steps 3-5 baseline --");
    print_metrics_header();
    for (role, sets) in [("TRAIN", &train_pairs), ("HOLDOUT", &holdout_pairs)] {
        println!("[{role}]");
        for (name, pairs) in sets {
            let mut model_pt: Vec<(f32, f32)> = Vec::new();
            let mut legacy_pt: Vec<(f32, f32)> = Vec::new();
            for p in pairs {
                if !dataset::is_model_population(&p.estimate) {
                    continue;
                }
                let pred = model.predict_height_m(&p.estimate.feature_input());
                model_pt.push((pred, p.truth_height_m));
                legacy_pt.push((p.estimate.height_m, p.truth_height_m));
            }
            let model_stats = metrics::Stats::from_pred_truth(&model_pt);
            let legacy_stats = metrics::Stats::from_pred_truth(&legacy_pt);
            print_metrics_row(&format!("{name} model"), &model_stats);
            print_metrics_row(&format!("{name} legacy"), &legacy_stats);
        }
    }

    println!("\nartifact size: {artifact_bytes} bytes");
    Ok(())
}

fn print_metrics_header() {
    println!(
        "{:<24} {:>6} {:>7} {:>7} {:>7} {:>7} {:>7} {:>7} {:>7}",
        "slice", "n", "MAE", "RMSE", "bias", "P50", "P90", "<20%", "<50%"
    );
}

fn print_metrics_row(name: &str, s: &metrics::Stats) {
    println!(
        "{:<24} {:>6} {:>7.2} {:>7.2} {:>+7.2} {:>7.2} {:>7.2} {:>6.0}% {:>6.0}%",
        name,
        s.n,
        s.mae,
        s.rmse,
        s.bias,
        s.p50_abs,
        s.p90_abs,
        s.within_20pct * 100.0,
        s.within_50pct * 100.0
    );
}

struct PipelineResult {
    city_name: String,
    city_note: String,
    report: metrics::Report,
}

/// Owned truth + estimate data for one preset, before matching. Shared by
/// [`run_pipeline`] (which matches + scores) and the trainer's
/// `collect_pairs` (which matches + owns the paired estimates).
pub(crate) struct MatchedData {
    pub city_name: String,
    pub city_note: String,
    pub truths: Vec<truth::Building>,
    /// Overture estimates whose centroid falls inside the preset bbox.
    pub estimates: Vec<buildings_core::ExtractedBuilding>,
}

/// Fetch ground-truth buildings for a preset from its configured source.
/// `cache` is the cache root; each source memoises under its own subdir.
fn fetch_truth(city: &cities::City, cache: &std::path::Path) -> Result<Vec<truth::Building>> {
    match &city.truth {
        cities::TruthSource::Plateau { city_code } => {
            fetch_plateau::fetch_lod1(city_code, &city.bbox, &cache.join("plateau"))
        }
        cities::TruthSource::Bag3d => {
            fetch_dutch_3dbag::fetch_truth(&city.bbox, &cache.join("3dbag"))
        }
    }
}

/// Fetch + decode + extract + bbox-filter one preset with the given config.
/// The single source of the pipeline guts — both scoring and training run
/// through here so they see identical extracted buildings.
pub(crate) fn matched_data(
    preset: &str,
    release: &str,
    cache: &std::path::Path,
    cfg: &HeightConfig,
) -> Result<MatchedData> {
    let city = cities::get(preset).ok_or_else(|| anyhow::anyhow!("unknown preset: {preset}"))?;

    eprintln!("== truth [{}] ({}) ==", city.truth.label(), city.name);
    let truths = fetch_truth(city, cache)?;
    eprintln!("truth buildings (in bbox, height>0): {}", truths.len());

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

    Ok(MatchedData {
        city_name: city.name.to_string(),
        city_note: city.note.to_string(),
        truths,
        estimates: in_bbox,
    })
}

fn run_pipeline(
    preset: &str,
    release: &str,
    cache: &std::path::Path,
    cfg: &HeightConfig,
) -> Result<PipelineResult> {
    let data = matched_data(preset, release, cache, cfg)?;
    let m = matcher::match_buildings(&data.truths, &data.estimates);
    let report = metrics::build_report(
        &m.pairs,
        m.unmatched_truth,
        data.truths.len(),
        data.estimates.len(),
    );
    Ok(PipelineResult {
        city_name: data.city_name,
        city_note: data.city_note,
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
