//! Peak-heap profiler for a single tile render.
//!
//! The worker runs the renderer inside a 128 MB Workers isolate, where the
//! wasm heap only ever grows, so what matters is the *peak* a tile costs —
//! not what it ends up holding. This walks one tile through the same phases
//! `render_glb_lod` does (decode → terrain → mesh → glb) behind a tracking
//! allocator, printing live and peak bytes per phase so a regression can be
//! attributed to a phase instead of guessed at.
//!
//! Grab the inputs the worker would fetch:
//!
//! ```text
//! # MVT: any tile out of Overture's buildings.pmtiles for the release in
//! #      use (see src/version.ts); easiest via the pmtiles CLI or the
//! #      worker's own /debug/overture.pmtiles range proxy.
//! curl -o t.webp https://terrain.reearth.land/terrarium/ellipsoid/14/14549/6451.webp
//! cargo run --release --example memprof -- 14/14549/6451 t.mvt t.webp
//! ```
//!
//! ```text
//! usage: memprof <z>/<x>/<y> <mvt> [terrain.webp] [options]
//!   --min <m2>            footprint filter floor    (default: per-zoom, see lod.ts)
//!   --max <m2>            footprint filter ceiling  (default: per-zoom; 0 = none)
//!   --simplify <r[,err]>  meshopt decimation ratio + target error in metres
//!   --aabb                collapse footprints to their bounding box
//!   --out <file.glb>      also write the rendered tile out
//! ```
//!
//! Not part of the worker build — `examples/` is only compiled by
//! `cargo test` / `cargo run --example`.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PHASE_PEAK: AtomicUsize = AtomicUsize::new(0);
static RUN_PEAK: AtomicUsize = AtomicUsize::new(0);

/// Wraps the system allocator to track live and peak bytes. Counts what the
/// program asked for, not allocator overhead — the same basis wasm's linear
/// memory grows on.
struct Track;

impl Track {
    fn record(n: usize) {
        PHASE_PEAK.fetch_max(n, Relaxed);
        RUN_PEAK.fetch_max(n, Relaxed);
    }
}

unsafe impl GlobalAlloc for Track {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(l) };
        if !p.is_null() {
            Self::record(LIVE.fetch_add(l.size(), Relaxed) + l.size());
        }
        p
    }

    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        LIVE.fetch_sub(l.size(), Relaxed);
        unsafe { System.dealloc(p, l) }
    }

    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        let q = unsafe { System.realloc(p, l, new) };
        if !q.is_null() {
            let live = if new >= l.size() {
                LIVE.fetch_add(new - l.size(), Relaxed) + (new - l.size())
            } else {
                LIVE.fetch_sub(l.size() - new, Relaxed) - (l.size() - new)
            };
            Self::record(live);
        }
        q
    }
}

#[global_allocator]
static ALLOC: Track = Track;

fn mb(n: usize) -> f64 {
    n as f64 / 1048576.0
}

/// Print live + peak since the last phase, then start a new phase.
fn phase(label: &str) {
    println!(
        "{label:<26} live={:>7.2} MB  peak={:>7.2} MB",
        mb(LIVE.load(Relaxed)),
        mb(PHASE_PEAK.load(Relaxed))
    );
    PHASE_PEAK.store(LIVE.load(Relaxed), Relaxed);
}

struct Args {
    z: u8,
    x: u32,
    y: u32,
    mvt: String,
    webp: Option<String>,
    filter: buildings_core::AreaFilter,
    simplify: (f32, f32),
    aabb: bool,
    out: Option<String>,
}

/// Footprint bucket for a zoom, mirroring `areaFilterFor` in src/lod.ts so
/// profiling a coordinate reflects what the worker would actually render.
fn default_filter(z: u8) -> buildings_core::AreaFilter {
    match z {
        0..=12 => buildings_core::AreaFilter {
            min_m2: 10_000.0,
            max_m2: 0.0,
        },
        13 => buildings_core::AreaFilter {
            min_m2: 2_000.0,
            max_m2: 10_000.0,
        },
        _ => buildings_core::AreaFilter {
            min_m2: 5.0,
            max_m2: 2_000.0,
        },
    }
}

fn parse_args() -> Result<Args, String> {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut positional: Vec<String> = Vec::new();
    let mut filter: Option<buildings_core::AreaFilter> = None;
    let mut min: Option<f32> = None;
    let mut max: Option<f32> = None;
    let mut simplify = (1.0f32, 0.0f32);
    let mut aabb = false;
    let mut out = None;

    let mut i = 0;
    while i < argv.len() {
        let arg = argv[i].as_str();
        let mut value = || {
            i += 1;
            argv.get(i)
                .cloned()
                .ok_or_else(|| format!("{arg} needs a value"))
        };
        match arg {
            "--min" => min = Some(value()?.parse().map_err(|e| format!("--min: {e}"))?),
            "--max" => max = Some(value()?.parse().map_err(|e| format!("--max: {e}"))?),
            "--simplify" => {
                let v = value()?;
                let (r, err) = v.split_once(',').unwrap_or((v.as_str(), "0"));
                simplify = (
                    r.parse().map_err(|e| format!("--simplify ratio: {e}"))?,
                    err.parse().map_err(|e| format!("--simplify error: {e}"))?,
                );
            }
            "--aabb" => aabb = true,
            "--out" => out = Some(value()?),
            "-h" | "--help" => return Err("usage: see the module docs".into()),
            other if other.starts_with('-') => return Err(format!("unknown option {other}")),
            other => positional.push(other.to_string()),
        }
        i += 1;
    }

    if positional.len() < 2 {
        return Err("usage: memprof <z>/<x>/<y> <mvt> [terrain.webp] [options]".into());
    }
    let coord: Vec<&str> = positional[0].split('/').collect();
    if coord.len() != 3 {
        return Err(format!("bad tile coord {:?}, want z/x/y", positional[0]));
    }
    let z: u8 = coord[0].parse().map_err(|e| format!("z: {e}"))?;
    if min.is_some() || max.is_some() {
        let d = default_filter(z);
        filter = Some(buildings_core::AreaFilter {
            min_m2: min.unwrap_or(d.min_m2),
            max_m2: max.unwrap_or(d.max_m2),
        });
    }

    Ok(Args {
        z,
        x: coord[1].parse().map_err(|e| format!("x: {e}"))?,
        y: coord[2].parse().map_err(|e| format!("y: {e}"))?,
        mvt: positional[1].clone(),
        webp: positional.get(2).cloned(),
        filter: filter.unwrap_or_else(|| default_filter(z)),
        simplify,
        aabb,
        out,
    })
}

fn main() {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    let (z, x, y) = (args.z, args.x, args.y);

    let mvt = std::fs::read(&args.mvt).expect("read mvt");
    let webp = args
        .webp
        .as_ref()
        .map(|p| std::fs::read(p).expect("read webp"));
    println!(
        "{z}/{x}/{y}  mvt={:.2} MB  terrain={:.2} MB  filter=[{}, {})  simplify={:?}  aabb={}",
        mb(mvt.len()),
        mb(webp.as_ref().map_or(0, Vec::len)),
        args.filter.min_m2,
        args.filter.max_m2,
        args.simplify,
        args.aabb,
    );
    phase("inputs read");

    let decoded = mvt_decoder::decode_buildings(&mvt).expect("decode mvt");
    phase("decode_buildings");
    println!("  buildings in tile = {}", decoded.buildings.len());

    let terrain = webp
        .as_ref()
        .map(|w| terrain_decoder::decode_terrarium_webp(z, x, y, w).expect("decode terrain"));
    phase("terrain decode");

    let sources = vec![buildings_core::mesh::Source {
        z,
        x,
        y,
        tile: &decoded,
    }];
    let cfg = buildings_core::HeightConfig::default();
    let mut mesh = buildings_core::mesh::build_mesh(
        z,
        x,
        y,
        &sources,
        args.filter,
        args.aabb,
        terrain.as_ref(),
        &cfg,
    );
    if args.simplify.0 > 0.0 && args.simplify.0 < 1.0 {
        buildings_core::mesh::simplify_mesh(&mut mesh, args.simplify.0, args.simplify.1);
    }
    phase("build_mesh");
    let props = std::mem::size_of::<buildings_core::mesh::FeatureProps>();
    println!(
        "  verts={} tris={} feats={}",
        mesh.positions.len() / 3,
        mesh.indices.len() / 3,
        mesh.features.len()
    );
    println!(
        "  geometry={:.2} MB (capacity {:.2} MB)  features={:.2} MB ({props} B each)",
        mb(mesh.positions.len() * 4
            + mesh.normals.len() * 4
            + mesh.indices.len() * 4
            + mesh.feature_ids.len() * 2),
        mb(mesh.positions.capacity() * 4
            + mesh.normals.capacity() * 4
            + mesh.indices.capacity() * 4
            + mesh.feature_ids.capacity() * 2),
        mb(props * mesh.features.len()),
    );

    // Mirrors render_glb_lod, which frees the decoded inputs before writing.
    drop(sources);
    drop(decoded);
    drop(terrain);
    phase("drop decoded inputs");

    let glb = buildings_core::glb::write_glb(mesh, [0.0; 16]);
    phase("write_glb");

    println!(
        "\nglb = {:.2} MB     run peak = {:.2} MB",
        mb(glb.len()),
        mb(RUN_PEAK.load(Relaxed))
    );
    if let Some(path) = &args.out {
        std::fs::write(path, &glb).expect("write glb");
        println!("wrote {path}");
    }
}
