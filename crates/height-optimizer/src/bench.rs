//! Speed benchmark for the on-demand height-estimation path.
//!
//! The worker's request budget is what matters: it renders **one source
//! tile per request**, so this benchmark times `build_mesh` per cached
//! source tile with the model Off vs On, then isolates the pieces the
//! model adds — the per-tile `tile_context` pass, the per-building
//! encode+predict against the legacy table lookup, and the once-per-
//! isolate artifact parse. Wall-clock numbers are native (Apple Silicon
//! here), not Workers wasm/V8 — treat the *relative* Off→On deltas as
//! the signal, not the absolute times.

use anyhow::Result;
use buildings_core::height_config::ModelMode;
use buildings_core::height_model::GbtModel;
use buildings_core::mesh::{self, AreaFilter, ExtractedBuilding, HeightCascadeInput, Source};
use buildings_core::{features, HeightConfig};
use mvt_decoder::{decode_buildings, DecodedTile};
use std::hint::black_box;
use std::path::Path;
use std::time::Instant;

use crate::{cities, fetch_overture};

pub fn run(preset: &str, release: &str, cache: &Path, iterations: usize) -> Result<()> {
    let city = cities::get(preset).ok_or_else(|| anyhow::anyhow!("unknown preset: {preset}"))?;
    let raw = fetch_overture::fetch_bbox(release, &city.bbox, &cache.join("overture"))?;
    let tiles: Vec<(u8, u32, u32, DecodedTile)> = raw
        .iter()
        .filter_map(|s| decode_buildings(&s.bytes).ok().map(|d| (s.z, s.x, s.y, d)))
        .collect();
    anyhow::ensure!(!tiles.is_empty(), "no decodable tiles for {preset}");

    let off = HeightConfig::default();
    let on = HeightConfig {
        model: ModelMode::On,
        ..Default::default()
    };

    println!(
        "\n== bench {} ({}) — {iterations} iterations ==",
        city.name, city.note
    );
    println!(
        "{:<18} {:>9} {:>12} {:>12} {:>10} {:>7}",
        "tile", "buildings", "off med(ms)", "on med(ms)", "delta(ms)", "delta%"
    );

    let mut total_off = 0.0;
    let mut total_on = 0.0;
    for (z, x, y, tile) in &tiles {
        let sources = [Source {
            z: *z,
            x: *x,
            y: *y,
            tile,
        }];
        let med_off = time_build(&sources, *z, *x, *y, &off, iterations);
        let med_on = time_build(&sources, *z, *x, *y, &on, iterations);
        total_off += med_off;
        total_on += med_on;
        println!(
            "{:<18} {:>9} {:>12.3} {:>12.3} {:>+10.3} {:>+6.1}%",
            format!("{z}/{x}/{y}"),
            tile.buildings.len(),
            med_off,
            med_on,
            med_on - med_off,
            (med_on - med_off) / med_off * 100.0
        );
    }
    println!(
        "{:<18} {:>9} {:>12.3} {:>12.3} {:>+10.3} {:>+6.1}%",
        "TOTAL",
        tiles.iter().map(|t| t.3.buildings.len()).sum::<usize>(),
        total_off,
        total_on,
        total_on - total_off,
        (total_on - total_off) / total_off * 100.0
    );

    micro_benches(&tiles, &off, iterations);
    parse_bench();
    Ok(())
}

/// Median wall-clock ms of `build_mesh` for one source tile. Two warm-up
/// runs first (JIT-free in Rust, but this pre-faults caches and forces
/// the OnceLock model parse out of the timed region, as in a warm
/// production isolate).
fn time_build(
    sources: &[Source<'_>],
    z: u8,
    x: u32,
    y: u32,
    cfg: &HeightConfig,
    iterations: usize,
) -> f64 {
    for _ in 0..2 {
        black_box(mesh::build_mesh(
            z,
            x,
            y,
            sources,
            AreaFilter::default(),
            false,
            None,
            cfg,
        ));
    }
    let mut samples = Vec::with_capacity(iterations);
    for _ in 0..iterations {
        let t0 = Instant::now();
        black_box(mesh::build_mesh(
            z,
            x,
            y,
            sources,
            AreaFilter::default(),
            false,
            None,
            cfg,
        ));
        samples.push(t0.elapsed().as_secs_f64() * 1e3);
    }
    median(samples)
}

/// Per-building and per-tile component costs, measured on the largest
/// tile (worst case for the tile_context scan and the anchor sort).
fn micro_benches(tiles: &[(u8, u32, u32, DecodedTile)], cfg: &HeightConfig, iterations: usize) {
    let (z, x, y, tile) = tiles
        .iter()
        .max_by_key(|t| t.3.buildings.len())
        .expect("non-empty");
    let sources = [Source {
        z: *z,
        x: *x,
        y: *y,
        tile,
    }];

    // tile_context (pass A) — runs once per tile in BOTH modes.
    let mut samples = Vec::with_capacity(iterations);
    for _ in 0..iterations {
        let t0 = Instant::now();
        black_box(features::tile_context(tile, cfg));
        samples.push(t0.elapsed().as_secs_f64() * 1e6);
    }
    println!(
        "\ntile_context (pass A, largest tile, {} buildings): {:.1} µs/tile",
        tile.buildings.len(),
        median(samples)
    );

    // Model population of that tile, via the production extraction path.
    let extracted = mesh::extract_buildings(&sources, cfg);
    let population: Vec<&ExtractedBuilding> = extracted
        .iter()
        .filter(|e| e.source_height_m.is_none() && e.num_floors.is_none())
        .collect();
    if population.is_empty() {
        println!("no model-population buildings in the largest tile; skipping micro-bench");
        return;
    }

    let avg = tile.buildings.len() as f32;
    let urban = mesh::classify_urban(avg, cfg);
    let model = buildings_core::height_model::builtin();

    // Legacy steps 3-5: table lookups through the shared cascade.
    let legacy_ns = per_building_ns(iterations, &population, |b| {
        let (h, _) = mesh::default_height_meters(&cascade_input(b), urban, cfg);
        h as f32
    });
    // GBT: encode into the stack buffer + walk 48 trees.
    let model_ns = per_building_ns(iterations, &population, |b| {
        model.predict_height_m(&b.feature_input())
    });
    println!(
        "per-building resolve ({} model-population buildings): legacy {legacy_ns:.0} ns | gbt model {model_ns:.0} ns ({:+.1}x)",
        population.len(),
        model_ns / legacy_ns
    );
}

fn cascade_input<'a>(b: &'a ExtractedBuilding) -> HeightCascadeInput<'a> {
    HeightCascadeInput {
        explicit_height_m: b.source_height_m.map(f64::from),
        num_floors: b.num_floors,
        class: b.class.as_deref(),
        subtype: b.subtype.as_deref(),
        footprint_m2: b.footprint_m2,
        perimeter_m: b.perimeter_m,
        has_name: b.has_name,
        has_parts: b.has_parts,
        roof_shape: b.roof_shape.as_deref(),
        min_height_m: b.min_height_m,
        tile: b.tile,
    }
}

fn per_building_ns(
    iterations: usize,
    population: &[&ExtractedBuilding],
    f: impl Fn(&ExtractedBuilding) -> f32,
) -> f64 {
    // Warm-up.
    for b in population {
        black_box(f(b));
    }
    let mut samples = Vec::with_capacity(iterations);
    for _ in 0..iterations {
        let t0 = Instant::now();
        for b in population {
            black_box(f(b));
        }
        samples.push(t0.elapsed().as_secs_f64() * 1e9 / population.len() as f64);
    }
    median(samples)
}

/// Once-per-isolate cost: parse + validate the embedded JSON artifact.
fn parse_bench() {
    let path = "crates/buildings-core/models/height_gbt_v1.json";
    let Ok(json) = std::fs::read_to_string(path) else {
        println!("(artifact parse bench skipped: {path} not readable from CWD)");
        return;
    };
    let mut samples = Vec::with_capacity(30);
    for _ in 0..30 {
        let t0 = Instant::now();
        black_box(GbtModel::from_json(&json).expect("valid artifact"));
        samples.push(t0.elapsed().as_secs_f64() * 1e3);
    }
    println!(
        "artifact parse+validate ({} KB): {:.3} ms (once per isolate)",
        json.len() / 1024,
        median(samples)
    );
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(f64::total_cmp);
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {
        0.5 * (v[n / 2 - 1] + v[n / 2])
    }
}
