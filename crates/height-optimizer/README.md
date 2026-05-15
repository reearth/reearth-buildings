# height-optimizer

An offline calibration tool that scores `buildings-core`'s **building
height estimator** against PLATEAU's high-precision ground-truth
heights, so we can tune the estimator's parameters with numbers
instead of hand-waving.

> **Data versions used in this report** (every number below is from
> this snapshot)
> - Evaluation date: **2026-05-15**
> - Overture Maps Buildings: release **2026-01-21**
>   (`overturemaps-tiles-us-west-2-beta` bucket)
> - PLATEAU CityGML: **fiscal-year 2025 release**
>   (`api.plateauview.mlit.go.jp/datacatalog/citygml/{cityCode}`)

## 1. Background and Problem

### 1.1 What this system does

This repository as a whole runs a pipeline that **takes the building
data Overture Maps publishes and extrudes it into 3D for browser
display.** `buildings-core` is the heart of it: given a building's
*footprint* (the polygon outline of its base, i.e. its floor plan in
plan view), it extrudes that footprint vertically by **some estimated
height** to produce a 3D mesh.

### 1.2 How the height is decided

The interesting part is "**some estimated height**." Overture's
buildings don't always carry a height. The explicit `height` attribute
is only filled in for a fraction of features; the rest have to be
guessed.

`buildings-core` uses a **5-step cascade**, defined in
[mesh.rs::default_height_meters](../buildings-core/src/mesh.rs).
Each step is tried in order; the first one that yields a value wins:

1. Overture's explicit `height` attribute (metres).
2. `num_floors` (floor count) × `meters_per_floor` (metres per floor).
3. Overture `class` attribute (`apartments`, `office`, `house`, ...)
   looked up in a height table (e.g. `apartments=25 m`).
4. Overture `subtype` attribute (`residential`, `commercial`, ... — a
   coarser classification) looked up in a similar table.
5. **footprint heuristic**: combine the footprint area with the
   "how built-up is this neighbourhood" classification (one of
   `DenseUrban` / `Urban` / `Suburban` / `Rural` — `UrbanLevel`,
   derived from per-tile building count) to pick a height.

### 1.3 What's wrong

The numbers that appear in this cascade — the per-`class` heights, the
per-`subtype` heights, the area-bucket heights, the `UrbanLevel`
thresholds, `meters_per_floor` — were all picked by gut feel during
development. There is no objective basis for them.

When the renderer runs, the Marunouchi office-tower district collapses
into squat boxes and rural detached houses balloon up unrealistically,
making it **obvious that something is off in some neighbourhoods**.
But nobody had ever quantified *where, how much, in which direction*
the estimator was wrong — so there was no path to improving it.

## 2. Objectives

This crate exists for three reasons:

1. **Quantify the error.** Compute the residual (= estimated minus
   truth) for the 5-step cascade and slice it across
   `height_method × class × subtype × city` so we can answer
   "which knob, in which direction, by how much?" with numbers.
2. **Build an A/B framework.** Externalise the parameters into TOML
   files so we can stand multiple candidates next to each other and
   compare. The body of `buildings-core` doesn't need to be touched
   to explore.
3. **Feed back into production.** Take whatever the search finds and
   bake it into `HeightConfig::default()`, so the worker (the
   Cloudflare Workers service that does the live rendering) picks up
   the new values automatically.

## 3. Methods

### 3.1 Ground-truth source: PLATEAU LOD1

The Japanese MLIT's **PLATEAU** project publishes 3D city models for
all major Japanese cities. Buildings are available at multiple levels
of detail (**LOD = Level of Detail**); the coarsest, **LOD1**,
represents each building as a rectangular prism formed by extruding
its footprint by a *measured* height. We use that measured height
(`bldg:measuredHeight`, in metres) as ground truth.

#### 3.1.1 How we fetch it

PLATEAU exposes a public API at `api.plateauview.mlit.go.jp`. There
are several endpoints; this tool calls three of them in sequence:

1. `GET /datacatalog/citygml/{cityCode}` — for one municipality,
   returns the URL list of its **CityGML files** (XML-encoded city
   model files, distributed split per geographic mesh).
2. `GET /citygml/features?sid={spatialId}&url={gmlUrl}` — for one
   CityGML file, returns the list of building IDs (`gml:id`) it
   contains. Pass the whole-world spatial ID `0/0/0/0` and you get
   every feature in the file regardless of mesh location.
3. `GET /citygml/attributes?url={gmlUrl}&id={ids}` — given a list of
   IDs, returns each building's attributes (`bldg:measuredHeight` and
   many others) plus `_bbox.center.{lng,lat}` (the geometric centroid
   in lon/lat). We batch by 200 IDs per request.

Buildings whose `bldg:measuredHeight` is `-9999` (PLATEAU's "no data"
sentinel) or ≤ 0 are dropped.

#### 3.1.2 Path we did NOT take: 3D Tiles (b3dm)

The original plan was to fetch the same data via PLATEAU's **3D Tiles**
endpoint (the OGC standard for streaming 3D geospatial data) and
decode the tile-level **b3dm** (Batched 3D Model) payloads. PLATEAU's
b3dm tiles, however, embed mesh data compressed with **Draco** (a
Google mesh-compression format), and there is no production-grade
pure-Rust Draco decoder. The CityGML JSON path needs neither Draco
nor b3dm parsing and gives us exactly the attributes we want.

### 3.2 Estimate source: Overture + buildings-core

The estimate comes from running `buildings-core`'s height cascade
against the same Overture Maps **PMTiles** the worker uses in
production. PMTiles is a single-file pyramid format that packs all
tiles into one archive readable via HTTP range requests.

#### 3.2.1 Tile fetching

Because PMTiles supports HTTP range reads, we don't have to download
the whole archive (tens of GB). Using the `pmtiles` crate, we pull
just the **z=14** tiles (slippy-map zoom 14, ≈ 2.4 km × 2.4 km per
tile at 35° latitude) that overlap our target bbox.

#### 3.2.2 Running the estimator

`buildings-core::mesh::extract_buildings` returns a vector of
`ExtractedBuilding`, each carrying `(centroid, footprint_polygon,
footprint_m2, height_m, height_method, class, subtype, ...)`. This
function is the production `build_mesh`'s aggregation pass with the
extrusion step stripped out — same height resolution path, same
multi-tile fragment dedup.

### 3.3 Matching

We need to pair each PLATEAU building (one centroid + one height)
with one Overture building (one or more outer-ring polygons + an
estimated height).

The simple approach is **point-in-polygon**: a PLATEAU building is
considered to match an Overture building when the PLATEAU centroid
falls inside one of the Overture outer rings.

For candidate pruning we use an **R-tree** (a spatial index that
makes "find rectangles overlapping a query" cheap). We bulk-load the
**AABB** (Axis-Aligned Bounding Box) of every Overture polygon into
the tree, query the centroid as a point, and only run ray-casting
point-in-polygon on the survivors.

PLATEAU buildings that match nothing are counted under
`unmatched_truth` but otherwise ignored. Stricter **IoU**-based
matching (Intersection over Union — the area of the polygon overlap
divided by the area of the union) is left for future work.

### 3.4 Metrics

`residual = (Overture estimated height) − (PLATEAU truth height)`,
in metres. Reported metrics:

| metric        | meaning |
|---------------|---------|
| n             | matched buildings under this slice |
| MAE           | **Mean Absolute Error** (m). Average error magnitude |
| RMSE          | **Root Mean Squared Error** (m). Penalises large outliers |
| bias          | Signed mean (m). Positive = over-estimating, negative = under |
| P50 / P90     | Absolute-residual **percentiles** (P50 = median) |
| <20% / <50%   | Fraction of buildings whose relative error is within 20% / 50% of truth |

Slices: overall, by `height_method` (which cascade step resolved the
height), by `class`, by `subtype`, by city preset.

### 3.5 City presets

We picked five PLATEAU-LOD1-covered districts of distinctly different
characters, each as a roughly **1 km square** bbox. Mixing extremes
exposes where the estimator breaks down.

| preset           | municipality          | code     | character |
|------------------|------------------------|----------|-----------|
| `chiyoda`        | Chiyoda-ku, Tokyo      | 13101    | High-rise office CBD (Marunouchi / Otemachi). 30–200 m towers |
| `setagaya`       | Setagaya-ku, Tokyo     | 13112    | Dense detached houses + small apartments (around Sangenjaya). 2–4 storeys |
| `nishi-yokohama` | Yokohama City          | 14100    | Minato Mirai. Tower apartments + waterfront warehouses |
| `tsukuba`        | Tsukuba City           | 08220    | Planned-city centre. Low/mid-rise + research facilities |
| `iiyama`         | Iiyama City, Nagano    | 20213    | Small mountain town with traditional wooden buildings. Mostly 1–2 storeys |

> The bldg gml catalogue for a municipality returns gml files for
> **adjacent meshes** (1 km square blocks neighbouring the city),
> not just files strictly inside it. Pulling dozens of files for a
> 1 km bbox is wasteful, so we extract the **JIS X 0410 standard
> mesh code** (Japan's eight-digit ~1 km grid identifier) from each
> gml filename, compute that mesh's bbox, and only process files
> whose mesh intersects the target bbox. This cuts setagaya from
> 78 → 4 files and tsukuba from 315 → 6.

## 4. Results and Discussion

### 4.1 Baseline scores (the defaults shipping before this work)

| city            | n     | MAE   | RMSE  | bias   | <20% |
|-----------------|------:|------:|------:|-------:|-----:|
| chiyoda (CBD)   |   903 | 13.47 | 26.63 |  -8.03 |  29% |
| setagaya (low-rise) |  5656 |  4.29 |  6.10 |  +0.89 |  30% |
| n-yokohama      |   632 |  7.66 | 17.77 |  -3.13 |  33% |
| tsukuba (suburb) |  1173 |  6.82 |  8.48 |  +3.17 |  13% |
| iiyama (mountain) |  1851 |  7.66 |  8.05 |  +7.58 |   2% |

Setagaya, the densest preset, sits at MAE ~4 m; chiyoda, the hardest,
at ~13 m. The bias signs are opposite per city (chiyoda under,
iiyama over), which already tells us **no single uniform correction
will fix this**.

### 4.2 Structural biases that emerged

#### (a) The footprint heuristic is one-sided per city

For buildings resolved by step 5 (`height_method = density` or
`footprint`), bias is **−8.6 m** for chiyoda and **+7.8 m** for
iiyama. The same area-bucket table is wrong in opposite directions.
This path covers **50–95% of buildings in every city**, so its error
dominates the OVERALL row.

#### (b) `classify_urban` mis-bins small-town centres

The "how built-up" classifier looks at the average building count
per z=14 tile (under 200 = Rural, 200–800 = Suburban, 800–1500 =
Urban, ≥ 1500 = DenseUrban). But **Iiyama's centre tile also has
1000+ buildings**, so it ends up classified as DenseUrban. Yet the
actual buildings there are 1–2 storeys, so the DenseUrban table
(12–18 m) inevitably over-shoots them. **Per-tile building count
doesn't capture "city scale"**.

#### (c) Several class defaults are bimodal

The default of `class=office=30 m` gives **bias −35 m** in Chiyoda
(real CBD towers, average ≈ 60 m) and **bias +26 m** in Yokohama
Nishi-ku (low/mid-rise offices, average ≈ 4 m). The same value
produces opposite errors on the same attribute.

This happens because the actual height distribution of buildings
labelled `class=office` is **bimodal** (two peaks). Most attributes
have a single-peak distribution that clusters around a typical
value, but office heights split into two modes:

```
count
 │       ▲              ▲
 │     ▲ ▲ ▲          ▲ ▲ ▲
 │   ▲ ▲ ▲ ▲ ▲      ▲ ▲ ▲ ▲ ▲
 │ ▲ ▲ ▲ ▲ ▲ ▲ ▲  ▲ ▲ ▲ ▲ ▲ ▲ ▲
 └─────────────────────────────── height (m)
   ↑ low/mid-rise suburban       ↑ CBD high-rises
   (peaks around 3–10 m)         (peaks around 40–100 m)
```

Picking 30 m (the mean) lands in the **valley between the peaks**,
where almost no actual buildings live, so we miss badly from either
side. `apartments` shows the same pattern (Chiyoda 5–8-storey
condos vs Setagaya 3-storey walk-ups).

A TOML config can only carry one value per key, so the real fix
needs **context-aware tables** (a code change). For example,
`class=office` would have to return 60 m in `DenseUrban_CBD` and
6 m in `Suburban` — different values per neighbourhood context.

#### (d) `num_floors × 3.0 m` runs systematically low for commercial

Real commercial floor-to-floor heights are typically 3.5–4.0 m,
versus 2.7–3.0 m for residential. Pinning it at 3.0 makes
commercial buildings come out uniformly short. Lifting to 3.3
reduces the `num_floors`-slice bias without inflating residential.

### 4.3 Candidate parameter sets — A/B trials

Four candidates were tried. See [presets/README.md](presets/README.md)
for the catalog and the TOML files in the same directory.

| candidate                     | strategy                                                                  | result |
|-------------------------------|---------------------------------------------------------------------------|--------|
| `candidate-tall-cbd.toml`     | aggressive: office/commercial 30→60, m/floor→3.5                          | Chiyoda improves dramatically, but Setagaya / Tsukuba regress significantly (the 3.5 m/floor bump pushes apartments and similar above truth) |
| `candidate-conservative.toml` | mild bumps: office 30→40, hotel 25→35, m/floor→3.3                        | Safe but the gains are small |
| `candidate-tuned-v2.toml`     | Bump the entire dense_urban table + raise suburban_min 200→500            | **Regression**: the centres of Iiyama and Tsukuba also classify as DenseUrban, so the table bump pushes their bias further out |
| `candidate-tuned-v3.toml`     | Only the dense_urban **>800 m² catch-all bucket** + class.office 40 + m/floor 3.3 | **Net win** (next subsection) |

### 4.4 Effect of the adopted v3

Baseline → v3:

| city            | MAE base→v3        | bias base→v3       | <20% base→v3 |
|-----------------|--------------------|--------------------|--------------|
| chiyoda         | 13.47 → **12.57** ✅ | -8.03 → **-6.78** ✅ | 29% → **34%** ✅ |
| setagaya        |  4.29 →   4.28      | +0.89 →  +1.03      | 30% →   31%   |
| n-yokohama      |  7.66 →   7.67      | -3.13 →  -2.51 ✅   | 33% →   35% ✅ |
| tsukuba         |  6.82 →   6.78      | +3.17 →  +3.74      | 13% →   19% ✅ |
| iiyama          |  7.66 →   7.95      | +7.58 →  +7.89      |  2% →    2%   |

Concrete improvement at Chiyoda CBD (MAE down ~7%, within-20% up
+5pt); the other four cities are within ±0.4 m, neutral. Iiyama
regresses by 0.3 m (a small side-effect of the m/floor bump
nudging some `num_floors`-resolved buildings up), judged
acceptable.

## 5. Conclusions and Future Work

### 5.1 Conclusions

- We have, for the first time, a quantitative breakdown of the
  5-step cascade's residuals across 5 cities.
- The **footprint heuristic** and the **bimodal class defaults** are
  the dominant error sources.
- The TOML-only improvements we found (v3) are now baked into
  `HeightConfig::default()` (commit `66384ab`). Chiyoda CBD's MAE
  drops by about 7%.

### 5.2 Proposed buildings-core changes

TOML configuration alone can't fix the bimodal problem, so the next
wins require code changes. In priority order:

1. **Add a city-context axis to `classify_urban`.**
   Combine per-tile building density with broader-area density (e.g.
   z=10 tile counts) or municipality area, so we can tell "Marunouchi
   (CBD towers)" apart from "Iiyama centre (small-town main street)."
   `UrbanLevel` would expand to a 2-dimensional `(density_tier,
   scale_tier)` enum.

2. **Make the class height table context-dependent.**
   For example, `class=office` would map not to a single number but
   to `{DenseUrban_CBD: 60, Suburban: 15, ...}`. `HeightConfig`'s
   `class_height_m` would change from `HashMap<String, f64>` to
   `HashMap<String, ContextualHeight>`.

3. **Use Overture's `is_underground` / `has_parts` / `min_height`.**
   Handling underground structures and parent-child building parts
   on separate paths could rescue some of the unmatched truth
   (PLATEAU buildings with no Overture counterpart). Currently
   282/1185 (24%) of Chiyoda's PLATEAU buildings are unmatched.

4. **Promote ground-truth matching to IoU.**
   The current centroid-in-polygon scheme is good enough for
   practical scoring, but for buildings whose footprints differ
   significantly between PLATEAU (precise) and Overture (sometimes
   ML-derived and rough), an IoU threshold would denoise the
   residual aggregates.

### 5.3 Extending the evaluation framework itself

- **More city presets**: Sapporo, Fukuoka, Osaka CBD, mountain
  villages, etc.
- **Automated search**: today the candidates are hand-tuned, but
  since the score is numeric we could plug in grid search or
  Bayesian optimisation.
- **CI integration**: run the report on a schedule and use it for
  **regression detection** when Overture publishes new releases
  (currently monthly).

## Usage

```bash
# List the city presets
cargo run -p height-optimizer -- cities

# Single-config eval (uses the production HeightConfig)
cargo run -p height-optimizer -- eval --preset chiyoda

# Compare a candidate against the baseline (defaults)
cargo run -p height-optimizer -- compare \
  --presets chiyoda,setagaya,nishi-yokohama,tsukuba,iiyama \
  --candidate crates/height-optimizer/presets/candidate-tuned-v3.toml \
  --out report.md

# Dump the current default values to TOML (handy as a tuning starting point)
cargo run -p height-optimizer -- dump-config --out my-config.toml
```

PLATEAU and Overture fetches are cached to disk under
`target/height-optimizer-cache/`. First run takes a few minutes
(HTTP-bound); subsequent runs take tens of seconds (local read +
compute only).
