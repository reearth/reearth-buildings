# Calibration presets

`HeightConfig` TOML files. Pass via `height-optimizer compare --candidate <file>`.

`baseline.toml` is a verbatim dump of `HeightConfig::default()` — the
production values currently shipping in `buildings-core`. The other
files are tuning experiments. Findings below are from the 5-city
baseline eval (chiyoda / setagaya / nishi-yokohama / tsukuba / iiyama)
against PLATEAU LOD1 `bldg:measuredHeight`.

## Baseline numbers (reference)

| city            | n     | MAE   | bias   | <20% |
|-----------------|------:|------:|-------:|-----:|
| chiyoda (CBD)   |   903 | 13.47 |  -8.03 |  29% |
| setagaya (低層)  |  5656 |  4.29 |  +0.89 |  30% |
| n-yokohama       |   632 |  7.66 |  -3.13 |  33% |
| tsukuba (郊外)   |  1173 |  6.82 |  +3.17 |  13% |
| iiyama (山間)    |  1851 |  7.66 |  +7.58 |   2% |

## Cross-city findings

- **Footprint heuristic is the dominant slice** (50–95% of features fall
  through to it). Its bias swings from –9 m in Marunouchi to +8 m in
  Iiyama: same lookup table, opposite errors.
- **`classify_urban` is mis-binning small towns.** Iiyama's central tile
  has 1000+ buildings → DenseUrban. But its buildings are 1–2 floors,
  not 10. Tile-density doesn't capture "city size" context. Without
  code changes (e.g., factoring in regional density), the global
  footprint table can't be both right for Marunouchi and right for
  Iiyama.
- **Several class defaults are bimodal.** `class=office=30 m` undershoots
  Chiyoda CBD by 35 m AND overshoots Yokohama Nishi-ku by 26 m. Same
  for `class=apartments=25` (right for Chiyoda, +7 over for Setagaya
  walk-ups). TOML can only express one value per key; class lookup
  needs density-context to fix properly.
- **`num_floors × 3.0 m` is consistently low** for commercial-floored
  buildings; 3.3 helps without hurting residential.

## Candidate files

| file                              | strategy                                                 | net effect (vs baseline)                                                            |
|-----------------------------------|----------------------------------------------------------|-------------------------------------------------------------------------------------|
| `candidate-tall-cbd.toml`         | aggressive: office/commercial 30→60, m/floor 3.5         | helps Chiyoda office (–11 MAE) but blows up apartments and tsukuba                  |
| `candidate-conservative.toml`     | small bumps: office 30→40, hotel 25→35, m/floor 3.3      | safe but minor wins                                                                 |
| `candidate-tuned-v2.toml`         | bump entire dense_urban table + raise suburban_min       | **regresses** iiyama / tsukuba — their dense centres also classify DenseUrban       |
| `candidate-tuned-v3.toml`         | dense_urban catch-all only (>800 m²) + class.office 40   | **net win**: chiyoda MAE 13.47→12.57, others within ±0.4 m, iiyama –0.3 m only loss |

## Recommendation

`candidate-tuned-v3.toml` is the best TOML-only tuning we found. **As
of the current commit, `HeightConfig::default()` IS the v3 values** —
the worker now ships with these. `baseline.toml` is regenerated from
`Default` so the two are byte-identical; `candidate-tuned-v3.toml` is
kept as the historical record of the change.

To go further, we'd need code changes — e.g. a "city-context"
classifier that distinguishes Marunouchi (Tokyo CBD, hundreds of
high-rises) from Iiyama centre (small town, one main street). The
`density` slice can then have its own table per context tier.
