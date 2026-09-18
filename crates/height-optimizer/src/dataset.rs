//! Build the training/holdout matrices for the offline GBT trainer.
//!
//! The pipeline is: [`collect_pairs`] fetches one preset and matches its
//! Overture estimates to PLATEAU truth, then [`build_datasets`] filters to
//! the model's production population, learns the categorical vocabularies
//! from the training pairs, and encodes every row through the shared
//! [`buildings_core::FeatureEncoder`]. Feature vectors are *only* ever
//! produced via [`ExtractedBuilding::feature_input`] — never hand-assembled
//! — which is the train/predict parity firewall.

use anyhow::Result;
use buildings_core::{mesh::ExtractedBuilding, FeatureEncoder, HeightConfig};
use std::collections::BTreeMap;
use std::path::Path;

/// One matched building: the extracted Overture estimate plus its PLATEAU
/// truth height in metres. Owns the estimate so it survives past the
/// borrowed [`crate::matcher::Pair`].
pub struct OwnedPair {
    pub estimate: ExtractedBuilding,
    pub truth_height_m: f32,
}

/// Fetch + decode + extract + match one preset, returning owned pairs.
/// Delegates the pipeline to [`crate::matched_data`] so training sees the
/// exact buildings the scoring path does.
pub fn collect_pairs(
    preset: &str,
    release: &str,
    cache: &Path,
    cfg: &HeightConfig,
) -> Result<Vec<OwnedPair>> {
    let data = crate::matched_data(preset, release, cache, cfg)?;
    let m = crate::matcher::match_buildings(&data.truths, &data.estimates);
    Ok(m.pairs
        .iter()
        .map(|p| OwnedPair {
            estimate: p.estimate.clone(),
            truth_height_m: p.truth.measured_height_m,
        })
        .collect())
}

/// One encoded training example.
pub struct Row {
    pub preset: String,
    pub features: Vec<f32>,
    pub target_log1p: f32,
}

/// A fully-encoded matrix plus the encoder that produced it (so predictions
/// re-use the identical layout).
pub struct Dataset {
    pub encoder: FeatureEncoder,
    pub rows: Vec<Row>,
}

/// Clamp bounds applied to truth heights before the `log1p` target
/// transform. Mirrors the model's prediction clamp so the trainer never
/// chases physically-impossible extremes.
pub const CLAMP_MIN_M: f32 = 2.5;
pub const CLAMP_MAX_M: f32 = 300.0;

/// The model's production population: buildings the cascade would resolve
/// with steps 3-5 (no explicit height, no `num_floors`). Everything else is
/// served by steps 1-2 and never reaches the model.
pub fn is_model_population(e: &ExtractedBuilding) -> bool {
    e.source_height_m.is_none() && e.num_floors.is_none()
}

/// Learn vocabularies from the *training* population, then encode both sets
/// with the same encoder.
///
/// Vocab selection: count each `class` / `subtype` value over the
/// population-filtered training pairs, keep the `top_k` most frequent, ties
/// broken alphabetically. A [`BTreeMap`] keyed by value gives alphabetical
/// order for free, and a *stable* sort by descending count then preserves
/// that order among equal-frequency values — so the vocab is fully
/// deterministic with no hash-map iteration.
pub fn build_datasets(
    train: &[(String, Vec<OwnedPair>)],
    holdout: &[(String, Vec<OwnedPair>)],
    top_k_class: usize,
    top_k_subtype: usize,
) -> (Dataset, Dataset) {
    let mut class_counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut subtype_counts: BTreeMap<String, usize> = BTreeMap::new();
    for (_, pairs) in train {
        for p in pairs {
            if !is_model_population(&p.estimate) {
                continue;
            }
            if let Some(c) = &p.estimate.class {
                *class_counts.entry(c.clone()).or_insert(0) += 1;
            }
            if let Some(s) = &p.estimate.subtype {
                *subtype_counts.entry(s.clone()).or_insert(0) += 1;
            }
        }
    }

    let encoder = FeatureEncoder {
        class_vocab: top_k_by_freq(&class_counts, top_k_class),
        subtype_vocab: top_k_by_freq(&subtype_counts, top_k_subtype),
    };

    let encode_set = |sets: &[(String, Vec<OwnedPair>)]| -> Vec<Row> {
        let mut rows = Vec::new();
        for (preset, pairs) in sets {
            for p in pairs {
                if !is_model_population(&p.estimate) {
                    continue;
                }
                let mut features = vec![0.0f32; encoder.len()];
                encoder.encode(&p.estimate.feature_input(), &mut features);
                let target_log1p = p.truth_height_m.clamp(CLAMP_MIN_M, CLAMP_MAX_M).ln_1p();
                rows.push(Row {
                    preset: preset.clone(),
                    features,
                    target_log1p,
                });
            }
        }
        rows
    };

    let train_rows = encode_set(train);
    let holdout_rows = encode_set(holdout);
    let train_ds = Dataset {
        encoder: encoder.clone(),
        rows: train_rows,
    };
    let holdout_ds = Dataset {
        encoder,
        rows: holdout_rows,
    };
    (train_ds, holdout_ds)
}

/// Top-`k` keys of a count map by descending frequency, alphabetical on
/// ties. Relies on [`BTreeMap`]'s alphabetical iteration + a stable sort.
fn top_k_by_freq(counts: &BTreeMap<String, usize>, k: usize) -> Vec<String> {
    let mut v: Vec<(String, usize)> = counts.iter().map(|(s, c)| (s.clone(), *c)).collect();
    v.sort_by(|a, b| b.1.cmp(&a.1)); // stable: equal counts keep alphabetical order
    v.into_iter().take(k).map(|(s, _)| s).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use buildings_core::coord::LonLat;
    use buildings_core::TileContext;

    fn building(
        class: Option<&str>,
        source_height_m: Option<f32>,
        num_floors: Option<u32>,
    ) -> ExtractedBuilding {
        ExtractedBuilding {
            feature_id: Some(1),
            gers_id: None,
            class: class.map(str::to_string),
            subtype: Some("residential".to_string()),
            footprint_m2: 100.0,
            height_m: 8.0,
            height_method: "footprint",
            source_height_m,
            num_floors,
            centroid: LonLat {
                lon_deg: 139.7,
                lat_deg: 35.7,
            },
            outer_rings_lonlat: vec![],
            perimeter_m: 40.0,
            has_name: false,
            has_parts: false,
            roof_shape: None,
            min_height_m: None,
            tile: TileContext {
                buildings_in_tile: 100,
                anchor_count: 0,
                anchor_median_height_m: f32::NAN,
                anchor_p90_height_m: f32::NAN,
            },
        }
    }

    fn pair(class: Option<&str>, h: Option<f32>, floors: Option<u32>) -> OwnedPair {
        OwnedPair {
            estimate: building(class, h, floors),
            truth_height_m: 10.0,
        }
    }

    #[test]
    fn population_filter_excludes_step_1_and_2_buildings() {
        assert!(is_model_population(&building(None, None, None)));
        assert!(!is_model_population(&building(None, Some(12.0), None)));
        assert!(!is_model_population(&building(None, None, Some(3))));

        let train = vec![(
            "t".to_string(),
            vec![
                pair(Some("house"), None, None),
                pair(Some("house"), Some(12.0), None), // explicit → excluded
                pair(Some("house"), None, Some(3)),    // floors → excluded
            ],
        )];
        let (ds, ho) = build_datasets(&train, &[], 4, 4);
        assert_eq!(ds.rows.len(), 1, "only the metadata-less row survives");
        assert!(ho.rows.is_empty());
    }

    #[test]
    fn vocab_top_k_by_frequency_with_alphabetical_ties() {
        // Frequencies: b=3, a=2, c=2, d=1. top_k=2 → [b, a] (a beats c
        // alphabetically at equal count).
        let mut pairs = Vec::new();
        for _ in 0..3 {
            pairs.push(pair(Some("b"), None, None));
        }
        for _ in 0..2 {
            pairs.push(pair(Some("a"), None, None));
            pairs.push(pair(Some("c"), None, None));
        }
        pairs.push(pair(Some("d"), None, None));
        let train = vec![("t".to_string(), pairs)];

        let (ds, _) = build_datasets(&train, &[], 2, 8);
        assert_eq!(ds.encoder.class_vocab, vec!["b", "a"]);
    }

    #[test]
    fn vocab_learned_from_train_population_only() {
        let train = vec![(
            "t".to_string(),
            vec![
                pair(Some("house"), None, None),
                // Excluded from the population → must not shape the vocab.
                pair(Some("apartments"), Some(20.0), None),
            ],
        )];
        let holdout = vec![("h".to_string(), vec![pair(Some("castle"), None, None)])];

        let (ds, ho) = build_datasets(&train, &holdout, 4, 4);
        assert_eq!(ds.encoder.class_vocab, vec!["house"]);
        assert!(!ds.encoder.class_vocab.contains(&"castle".to_string()));

        // The holdout row still encodes — its unseen class fires =other.
        assert_eq!(ho.rows.len(), 1);
        let names = ds.encoder.feature_names();
        let other_idx = names.iter().position(|n| n == "class=other").unwrap();
        assert!((ho.rows[0].features[other_idx] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn target_is_clamped_log1p() {
        let mut p = pair(None, None, None);
        p.truth_height_m = 500.0; // above CLAMP_MAX_M
        let train = vec![("t".to_string(), vec![p])];
        let (ds, _) = build_datasets(&train, &[], 4, 4);
        assert!((ds.rows[0].target_log1p - CLAMP_MAX_M.ln_1p()).abs() < 1e-5);
    }
}
