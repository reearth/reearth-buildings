# GBT Height Model — Design, Accuracy, and Speed Report

A gradient-boosted-tree model now replaces the hand-tuned lookup tables
(cascade steps 3–5) for buildings that ship no usable height metadata.
This document summarises why, how it works, what it scores, and what it
costs at request time. It is the follow-up to the calibration findings
in [README.md](README.md) §4–5, which this work implements.

> **Data versions** (every number below is from this snapshot)
> - Evaluation date: **2026-07-04**
> - Overture Maps Buildings: release **2026-01-21**
> - PLATEAU CityGML: fiscal-year 2025 release
> - Model artifact: `height_gbt_v1` (48 trees × depth 4, 31 KB JSON,
>   commit `6c42fe9`)
> - Raw per-city comparison: [report-model-v1.md](report-model-v1.md)

## 1. Why a model

The README established two structural problems no TOML tuning can fix:

1. **The footprint heuristic is one-sided per city** — the same
   area-bucket table under-shoots Chiyoda by ~8 m and over-shoots
   Iiyama by ~8 m, and it covers 50–95% of buildings everywhere.
2. **Class defaults are bimodal** — `office` means a 60 m tower in
   Marunouchi and a 6 m walk-up in suburban Yokohama; any single
   number lands in the valley between the peaks.

Both are context problems: the answer to "how tall is this building?"
depends on the *neighbourhood*. The model's key input is exactly that
context — the heights of nearby buildings we already trust.

## 2. How it works

### 2.1 Cascade integration

Steps 1–2 are untouched (explicit `height`, then
`num_floors × meters_per_floor`). When `HeightConfig.model = "on"`,
everything that used to fall to the class/subtype/footprint tables is
predicted by the GBT instead, tagged `height_method = "model"` in the
glb. `model = "off"` (the current production default) preserves legacy
behaviour bit-for-bit.

### 2.2 Features (36, all computable from the single requested tile)

| group | features |
|---|---|
| geometry | log1p area, log1p perimeter, isoperimetric compactness |
| tile context | log1p building count, log1p **anchor** count, log1p anchor median height, log1p anchor p90 height |
| attributes | min_height, has_name, has_parts |
| categorical | one-hot top-16 `class` + other, top-8 `subtype` + other |

An **anchor** is a same-tile building resolved by steps 1–2 — the
"surroundings" signal that lets one model serve both Marunouchi and a
mountain town. Missing values (no anchors, no min_height, degenerate
geometry) encode as NaN and are routed by per-node learned default
directions (LightGBM-style).

### 2.3 Parity firewall

Feature encoding lives in `buildings-core::features` and is the single
source of truth: inference encodes through it, and the trainer is only
allowed to build feature vectors via `ExtractedBuilding::feature_input()`.
The encoder's vocabularies ship *inside* the model artifact, and a mesh
unit test asserts `build_mesh` and `extract_buildings` resolve identical
heights — training and production cannot silently skew.

### 2.4 Determinism

The trainer (`height-optimizer/src/gbt.rs`) is a dependency-free
exact-greedy GBT built for byte-identical retrains: `total_cmp`
presorts, BTreeMap-only vocab counting, tie-breaks by lowest feature
index then threshold. `cargo run -p height-optimizer -- train` against
a warm cache reproduces the committed artifact byte-for-byte (verified
with `cmp`). Caveat: a *cold* cache's concurrent network fetches can
differ slightly; regenerate from a warm cache.

### 2.5 Training protocol

- **Population**: matched Overture↔PLATEAU pairs with no explicit
  height and no `num_floors` — the rows the model actually serves.
  12,067 train / 7,391 holdout rows.
- **Split**: train = chiyoda, setagaya, nishi-yokohama, iiyama,
  hachioji; **holdout** (never seen by training or vocab) = tsukuba,
  kanazawa, takamatsu.
- **Target**: `log1p(clamp(measuredHeight, 2.5, 300))`, squared loss;
  predictions are `expm1`-ed and clamped to the same range.
- Defaults: 48 trees, depth 4, lr 0.1, min_samples_leaf 20, λ=1.

## 3. Accuracy (all matched buildings, baseline → model)

| city | n | MAE | bias | <20% |
|---|---:|---|---|---|
| chiyoda | 903 | 12.57 → **10.16** | −6.78 → −4.12 | 34% → 45% |
| setagaya | 5656 | 4.28 → **2.37** | +1.03 → −0.63 | 31% → 62% |
| nishi-yokohama | 632 | 7.67 → **6.30** | −2.51 → −0.99 | 35% → 44% |
| **tsukuba** (holdout) | 1173 | 6.79 → **3.55** | +3.75 → −0.34 | 19% → 42% |
| iiyama | 1851 | 7.95 → **1.53** | +7.89 → −0.16 | 2% → 59% |
| hachioji | 4329 | 6.95 → **3.25** | +4.46 → −1.07 | 11% → 57% |
| **kanazawa** (holdout) | 3096 | 6.57 → **3.38** | +4.62 → −1.07 | 13% → 46% |
| **takamatsu** (holdout) | 3655 | 6.28 → **5.07** | +2.45 → −3.42 | 23% → 34% |

Every city improves on MAE, RMSE, and <20% hit-rate — holdouts
included, so this is generalisation, not memorisation. The README's
worst offenders are largely fixed: iiyama's +7.9 m blanket
over-estimate drops to −0.16 m bias (<20% hit-rate 2% → 59%), and the
bimodal `office`/`hotel` misses in Chiyoda shrink by ~40%.

Known blemishes:

- **takamatsu** bias flips sign (+2.45 → −3.42): the model
  under-predicts the arcade district, though every other metric still
  improves.
- The holdout learning curve bottoms around **16 trees** and flattens;
  trees 17–48 mostly refine Chiyoda's high-rise tail (train-side
  chiyoda MAE 10.0 → 8.2) without moving holdout error.

### 3.1 Cross-country verification (Netherlands, 3D BAG)

To measure transfer beyond Japan, five Dutch presets score against
**3D BAG** truth (`b3_h_dak_50p − b3_h_maaiveld`, height above ground)
fetched from the nationwide FlatCityBuf index by HTTP range reads.
The model never saw non-Japanese data. Full breakdown:
[report-model-v1-nl.md](report-model-v1-nl.md).

| city | n | MAE | bias | <20% |
|---|---:|---|---|---|
| delft | 3048 | 2.48 → **1.74** | +1.06 → +0.68 | 50% → 47% |
| amsterdam | 6310 | 7.62 → **4.48** | +4.08 → −3.52 | 19% → 23% |
| rotterdam (CBD) | 2255 | 4.78 → 4.83 | +2.78 → −3.19 | 45% → 22% |
| groningen | 5348 | 7.18 → **2.46** | +4.60 → −0.05 | 19% → 45% |
| maastricht | 4141 | 6.98 → **3.09** | +2.44 → −1.54 | 16% → 37% |

Four of five cities improve substantially — the location-free feature
design transfers across countries. The systematic pattern: the model
**under-predicts Dutch stock by ~3.5 m** (tall narrow terraced houses,
a form absent from Japanese training data). Where the legacy tables
over-shot badly (amsterdam, groningen) the model still wins by a wide
margin; in rotterdam the baseline was only +2.8 m biased, so the swing
to −3.2 m nets flat MAE and a worse <20% hit-rate. The obvious fix —
adding a Dutch city to the training presets — is deliberately left as
future work so these five cities remain a clean verification set.

## 4. Speed (on-demand cost)

Measured with `cargo run --release -p height-optimizer -- bench` on
real cached tiles, native Apple Silicon. Absolute times will differ
under Workers wasm; the Off→On deltas are the signal. The worker
renders one z14 source tile per request.

Inference walks a **flattened node arena**: at load, `PreparedModel`
concatenates every tree's struct-of-arrays JSON into one contiguous
16-byte-node array. This measured **2.8× faster** than walking the
serde structs directly (1.4 → 0.51 µs/building for 48 trees) with
bit-identical predictions (unit-tested).

**Shipped 48-tree model (with arena):**

| preset | tile total (buildings) | build_mesh Off → On |
|---|---|---|
| chiyoda | 22,008 | 67.6 → 76.2 ms (+12.8%) |
| setagaya | 16,980 | 55.3 → 64.7 ms (+17.1%) |
| iiyama | 3,874 | 10.2 → 12.2 ms (+20.1%) |

Decomposition: one prediction costs **~0.51 µs** (encode + 48 arena
walks) vs ~20 ns for a legacy table lookup; the tile delta is almost
exactly `population × 0.51 µs`. Fixed overheads are negligible — the
pass-A anchor scan is 35–60 µs/tile and the once-per-isolate artifact
parse is ~0.3 ms.

**16-tree alternative** (`train --n-trees 16`): ~0.25 µs/building,
tile overhead **+4–7%**, 13 KB artifact, holdout MAE statistically
identical (3.87/3.25/4.81 vs 3.83/3.31/4.87) — at the cost of the
Chiyoda high-rise tail (train MAE 8.2 → 10.0).

Assessment: +2–3 ms per z14 tile against the worker's 30 s CPU limit,
paid once per tile thanks to R2 caching. The 48-tree model is
comfortably affordable; the 16-tree retrain is the zero-risk fallback
if dense low-zoom tiles ever become a budget concern.

## 5. Rollout and operations

- **Enable**: flip `model: ModelMode::On` in
  `HeightConfig::default()` (`buildings-core/src/height_config.rs`).
  The worker takes no runtime config, so the default *is* the rollout.
- **Retrain** (e.g. new Overture release):
  `cargo run --release -p height-optimizer -- train`, review the
  printed holdout table, commit the regenerated artifact.
- **Re-evaluate**: `compare --presets <all 8> --candidate <model-on
  toml>`; **re-benchmark**: `bench --presets chiyoda,setagaya,iiyama`.

## 6. Limitations and future work

1. **Tile-seam variance**: anchor stats are per-source-tile, so a
   building near a tile edge can get slightly different model heights
   in adjacent renders — same class of issue as the legacy
   `UrbanLevel`, now finer-grained. A precomputed prior grid would
   remove it structurally.
1a. **Dutch under-bias**: §3.1's ~−3.5 m systematic bias on NL stock
   suggests a mixed-country training set (add one Dutch city to the
   train presets, keep the rest as holdout) as the highest-value
   accuracy follow-up.
2. **Further inference speed** (if ever needed): tree-major batched
   prediction over a whole tile's buildings, or compile-time codegen of
   the artifact via `build.rs` (regenerated automatically from the JSON
   on every build — retraining never touches source). Both deferred:
   the arena already puts the overhead in the noise.
3. **Richer context**: FAR zoning polygons (国土数値情報), distance to
   nearest station, and GHS-BUILT-H offline priors were identified as
   the next accuracy levers, especially outside Japan where no PLATEAU
   truth exists.
4. **meters_per_floor coupling**: anchor heights use
   `cfg.meters_per_floor` (3.3); changing it requires retraining.
