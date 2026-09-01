//! Feature-vector contract shared by the offline trainer
//! (`height-optimizer`) and the wasm inference path (`height_model`).
//!
//! This module is the **single source of truth** for how a building is
//! encoded into the numeric vector a [`crate::height_model::GbtModel`]
//! consumes. The height-optimizer must never re-implement the encoding:
//! it builds a [`FeatureInput`] from an `ExtractedBuilding` and calls
//! [`FeatureEncoder::encode`], exactly as inference does. Keeping the
//! encoder here — and shipping its vocabularies inside the model
//! artifact — is the "feature parity firewall" that guarantees the
//! trained model sees the same features at train and predict time.

use mvt_decoder::DecodedTile;

/// Number of fixed numeric features that always precede the one-hot
/// categorical groups. Kept as a named constant so the offsets in
/// [`FeatureEncoder::encode`] and [`FeatureEncoder::feature_names`]
/// cannot drift apart.
const NUM_NUMERIC_FEATURES: usize = 10;

/// Names of the fixed numeric features, in encoding order (indices 0-9).
const NUMERIC_FEATURE_NAMES: [&str; NUM_NUMERIC_FEATURES] = [
    "log1p_area_m2",
    "log1p_perimeter_m",
    "compactness",
    "log1p_buildings_in_tile",
    "log1p_anchor_count",
    "log1p_anchor_median_m",
    "log1p_anchor_p90_m",
    "min_height_m",
    "has_name",
    "has_parts",
];

/// Upper bound on the encoded feature-vector length. Inference encodes
/// into a `[f32; MAX_FEATURES]` stack buffer to avoid a heap allocation
/// per building on the wasm hot path, so any encoder longer than this
/// is rejected by [`crate::height_model::GbtModel::validate`].
pub const MAX_FEATURES: usize = 64;

/// Tile-level "surroundings" signal computed once per source MVT tile in
/// a first pass, then attached to every [`FeatureInput`] built from that
/// tile. The anchor stats summarise the heights we *trust* nearby
/// (buildings resolved by cascade steps 1-2), which is the strongest
/// predictor of a metadata-less building's height.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TileContext {
    /// Non-underground building count in this source tile.
    pub buildings_in_tile: u32,
    /// Buildings in this tile resolved by cascade steps 1-2 (explicit
    /// height or `num_floors`).
    pub anchor_count: u32,
    /// Median trusted-anchor height. `f32::NAN` when `anchor_count == 0`.
    pub anchor_median_height_m: f32,
    /// 90th-percentile trusted-anchor height. `f32::NAN` when
    /// `anchor_count == 0`.
    pub anchor_p90_height_m: f32,
}

/// Pass-A scan of one decoded tile.
///
/// An "anchor" is a building resolvable by cascade steps 1-2: it has an
/// explicit `height > 0`, or failing that a `num_floors > 0` (in which
/// case its anchor height is `floors * cfg.meters_per_floor`, matching
/// step 2 of the production cascade). Buildings flagged
/// [`is_underground_structure`](mvt_decoder::BuildingFeature::is_underground_structure)
/// are skipped entirely — they are dropped before extrusion, so they
/// neither count toward `buildings_in_tile` nor contribute anchors.
///
/// Percentile convention: anchor heights are sorted with
/// [`f32::total_cmp`]. The median is the middle element (the average of
/// the two middle elements for even `n`). The p90 is the nearest-rank
/// value at sorted index `((n-1) * 0.9).round()` — chosen over
/// interpolation because it needs no special-casing for small `n` and
/// stays a value actually observed in the tile.
pub fn tile_context(tile: &DecodedTile, cfg: &crate::HeightConfig) -> TileContext {
    let mut buildings_in_tile = 0u32;
    let mut anchor_heights: Vec<f32> = Vec::new();

    for feat in &tile.buildings {
        if feat.is_underground_structure() {
            continue;
        }
        buildings_in_tile += 1;

        // Step 1: explicit height wins. Step 2: num_floors otherwise.
        let anchor_h = match feat.height {
            Some(h) if h > 0.0 => Some(h as f32),
            _ => feat
                .num_floors
                .filter(|&l| l > 0)
                .map(|l| l as f32 * cfg.meters_per_floor as f32),
        };
        if let Some(h) = anchor_h {
            anchor_heights.push(h);
        }
    }

    let anchor_count = anchor_heights.len() as u32;
    let (anchor_median_height_m, anchor_p90_height_m) = if anchor_heights.is_empty() {
        (f32::NAN, f32::NAN)
    } else {
        anchor_heights.sort_by(f32::total_cmp);
        (median_sorted(&anchor_heights), p90_sorted(&anchor_heights))
    };

    TileContext {
        buildings_in_tile,
        anchor_count,
        anchor_median_height_m,
        anchor_p90_height_m,
    }
}

/// Median of a slice already sorted ascending; averages the two middle
/// elements for even length. Caller guarantees non-empty.
fn median_sorted(sorted: &[f32]) -> f32 {
    let n = sorted.len();
    if n % 2 == 1 {
        sorted[n / 2]
    } else {
        0.5 * (sorted[n / 2 - 1] + sorted[n / 2])
    }
}

/// Nearest-rank p90 of a slice already sorted ascending. Caller
/// guarantees non-empty.
fn p90_sorted(sorted: &[f32]) -> f32 {
    let n = sorted.len();
    let idx = ((n - 1) as f32 * 0.9).round() as usize;
    sorted[idx]
}

/// Everything the encoder needs to turn one building into a feature
/// vector. Borrows string fields (`class`, `subtype`, `roof_shape`) so
/// the trainer can encode from borrowed `ExtractedBuilding` data without
/// cloning.
#[derive(Debug, Clone, Copy)]
pub struct FeatureInput<'a> {
    pub footprint_m2: f32,
    pub perimeter_m: f32,
    pub class: Option<&'a str>,
    pub subtype: Option<&'a str>,
    pub has_name: bool,
    pub has_parts: bool,
    /// Reserved for future features; not encoded in v1.
    pub roof_shape: Option<&'a str>,
    pub min_height_m: Option<f32>,
    pub tile: TileContext,
}

/// Encoder mapping a [`FeatureInput`] to a fixed-order `f32` vector. The
/// two vocabularies are learned by the trainer and shipped inside the
/// model artifact, so an isolate deserialises the encoder alongside the
/// trees and every prediction re-uses the exact train-time layout.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FeatureEncoder {
    pub class_vocab: Vec<String>,
    pub subtype_vocab: Vec<String>,
}

impl FeatureEncoder {
    /// Total encoded length: the 10 numeric features, then one one-hot
    /// slot per class-vocab entry plus a `class=other` catch-all, then
    /// the same for the subtype vocab.
    pub fn len(&self) -> usize {
        NUM_NUMERIC_FEATURES + self.class_vocab.len() + 1 + self.subtype_vocab.len() + 1
    }

    /// Always `false` — there are always the 10 numeric features. Present
    /// only so clippy does not demand it alongside [`len`](Self::len).
    pub fn is_empty(&self) -> bool {
        false
    }

    /// The feature names in encoding order, matching [`encode`](Self::encode)
    /// index-for-index. Used by the trainer's report and validated
    /// against the artifact's `feature_names` in
    /// [`crate::height_model::GbtModel::validate`].
    pub fn feature_names(&self) -> Vec<String> {
        let mut names = Vec::with_capacity(self.len());
        for n in NUMERIC_FEATURE_NAMES {
            names.push(n.to_string());
        }
        for v in &self.class_vocab {
            names.push(format!("class={v}"));
        }
        names.push("class=other".to_string());
        for v in &self.subtype_vocab {
            names.push(format!("subtype={v}"));
        }
        names.push("subtype=other".to_string());
        names
    }

    /// Write exactly [`len`](Self::len) values into `out`.
    ///
    /// Panics if `out` is shorter than `self.len()`. Inference passes a
    /// `[f32; MAX_FEATURES]` buffer and `validate` guarantees
    /// `len() <= MAX_FEATURES`, so the panic is unreachable in production.
    pub fn encode(&self, input: &FeatureInput<'_>, out: &mut [f32]) {
        assert!(
            out.len() >= self.len(),
            "encode: output buffer too short ({} < {})",
            out.len(),
            self.len()
        );

        let t = &input.tile;
        out[0] = input.footprint_m2.max(0.0).ln_1p();
        out[1] = input.perimeter_m.max(0.0).ln_1p();
        out[2] = compactness(input.footprint_m2, input.perimeter_m);
        out[3] = (t.buildings_in_tile as f32).ln_1p();
        out[4] = (t.anchor_count as f32).ln_1p();
        out[5] = log1p_nonneg(t.anchor_median_height_m);
        out[6] = log1p_nonneg(t.anchor_p90_height_m);
        out[7] = input.min_height_m.unwrap_or(f32::NAN);
        out[8] = if input.has_name { 1.0 } else { 0.0 };
        out[9] = if input.has_parts { 1.0 } else { 0.0 };

        encode_one_hot(
            &self.class_vocab,
            input.class,
            &mut out[NUM_NUMERIC_FEATURES..],
        );
        let subtype_off = NUM_NUMERIC_FEATURES + self.class_vocab.len() + 1;
        encode_one_hot(&self.subtype_vocab, input.subtype, &mut out[subtype_off..]);
    }
}

/// 4πA/P² isoperimetric compactness. `f32::NAN` for degenerate geometry
/// (non-positive area or perimeter) so the tree walker routes it via the
/// learned default direction rather than treating "0" as a real value.
fn compactness(area_m2: f32, perimeter_m: f32) -> f32 {
    if area_m2 <= 0.0 || perimeter_m <= 0.0 {
        return f32::NAN;
    }
    4.0 * std::f32::consts::PI * area_m2 / (perimeter_m * perimeter_m)
}

/// `ln_1p` with a NaN pass-through and a non-negative guard. A NaN input
/// (missing anchor stat) must stay NaN so the walker uses `default_left`;
/// note `f32::max` would silently turn NaN into 0, so we branch first.
fn log1p_nonneg(x: f32) -> f32 {
    if x.is_nan() {
        x
    } else {
        x.max(0.0).ln_1p()
    }
}

/// Fill a `vocab.len() + 1` one-hot group at the start of `out`. The
/// trailing slot is `<field>=other`. A `None` value leaves the whole
/// group zero — deliberately distinct from `=other`, which fires only
/// when the value is present but unseen at train time.
fn encode_one_hot(vocab: &[String], value: Option<&str>, out: &mut [f32]) {
    for slot in out.iter_mut().take(vocab.len() + 1) {
        *slot = 0.0;
    }
    if let Some(v) = value {
        match vocab.iter().position(|k| k == v) {
            Some(i) => out[i] = 1.0,
            None => out[vocab.len()] = 1.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::HeightConfig;
    use mvt_decoder::BuildingFeature;

    fn tile(buildings: Vec<BuildingFeature>) -> DecodedTile {
        DecodedTile {
            extent: 4096,
            buildings,
        }
    }

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-5
    }

    #[test]
    fn tile_context_empty_tile_has_nan_anchor_stats() {
        let ctx = tile_context(&tile(vec![]), &HeightConfig::default());
        assert_eq!(ctx.buildings_in_tile, 0);
        assert_eq!(ctx.anchor_count, 0);
        assert!(ctx.anchor_median_height_m.is_nan());
        assert!(ctx.anchor_p90_height_m.is_nan());
    }

    #[test]
    fn tile_context_median_and_p90_odd_count() {
        let cfg = HeightConfig::default();
        let feats = vec![
            BuildingFeature {
                height: Some(10.0),
                ..Default::default()
            },
            BuildingFeature {
                height: Some(30.0),
                ..Default::default()
            },
            BuildingFeature {
                height: Some(20.0),
                ..Default::default()
            },
            // No height/floors: counted in the tile but not an anchor.
            BuildingFeature::default(),
        ];
        let ctx = tile_context(&tile(feats), &cfg);
        assert_eq!(ctx.buildings_in_tile, 4);
        assert_eq!(ctx.anchor_count, 3);
        assert!(approx(ctx.anchor_median_height_m, 20.0));
        // nearest-rank p90 of [10, 20, 30]: index round(2 * 0.9) = 2.
        assert!(approx(ctx.anchor_p90_height_m, 30.0));
    }

    #[test]
    fn tile_context_even_count_averages_median() {
        let cfg = HeightConfig::default();
        let feats = vec![
            BuildingFeature {
                height: Some(10.0),
                ..Default::default()
            },
            BuildingFeature {
                height: Some(20.0),
                ..Default::default()
            },
        ];
        let ctx = tile_context(&tile(feats), &cfg);
        assert!(approx(ctx.anchor_median_height_m, 15.0));
        assert!(approx(ctx.anchor_p90_height_m, 20.0));
    }

    #[test]
    fn tile_context_num_floors_anchor_uses_meters_per_floor() {
        let cfg = HeightConfig::default();
        let feats = vec![
            // Non-positive explicit height falls through to floors.
            BuildingFeature {
                height: Some(0.0),
                num_floors: Some(3),
                ..Default::default()
            },
        ];
        let ctx = tile_context(&tile(feats), &cfg);
        assert_eq!(ctx.anchor_count, 1);
        let expected = 3.0 * cfg.meters_per_floor as f32;
        assert!(approx(ctx.anchor_median_height_m, expected));
    }

    #[test]
    fn tile_context_skips_underground_structures() {
        let cfg = HeightConfig::default();
        let feats = vec![
            BuildingFeature {
                height: Some(50.0),
                is_underground: Some(true),
                ..Default::default()
            },
            BuildingFeature {
                height: Some(8.0),
                ..Default::default()
            },
        ];
        let ctx = tile_context(&tile(feats), &cfg);
        assert_eq!(ctx.buildings_in_tile, 1);
        assert_eq!(ctx.anchor_count, 1);
        assert!(approx(ctx.anchor_median_height_m, 8.0));
    }

    fn encoder() -> FeatureEncoder {
        FeatureEncoder {
            class_vocab: vec!["house".to_string(), "office".to_string()],
            subtype_vocab: vec!["residential".to_string()],
        }
    }

    fn input_with(
        class: Option<&'static str>,
        subtype: Option<&'static str>,
    ) -> FeatureInput<'static> {
        FeatureInput {
            footprint_m2: 100.0,
            perimeter_m: 40.0,
            class,
            subtype,
            has_name: true,
            has_parts: false,
            roof_shape: None,
            min_height_m: None,
            tile: TileContext {
                buildings_in_tile: 500,
                anchor_count: 0,
                anchor_median_height_m: f32::NAN,
                anchor_p90_height_m: f32::NAN,
            },
        }
    }

    #[test]
    fn encoder_len_and_names_agree() {
        let enc = encoder();
        // 10 numeric + (2 class + other) + (1 subtype + other).
        assert_eq!(enc.len(), 15);
        assert!(!enc.is_empty());
        let names = enc.feature_names();
        assert_eq!(names.len(), enc.len());
        assert_eq!(names[0], "log1p_area_m2");
        assert_eq!(names[10], "class=house");
        assert_eq!(names[12], "class=other");
        assert_eq!(names[13], "subtype=residential");
        assert_eq!(names[14], "subtype=other");
    }

    #[test]
    fn encode_numeric_features() {
        let enc = encoder();
        let mut out = vec![7.0f32; enc.len()];
        enc.encode(&input_with(None, None), &mut out);
        assert!(approx(out[0], 100.0f32.ln_1p()));
        assert!(approx(out[1], 40.0f32.ln_1p()));
        // 4π·100/1600
        assert!(approx(out[2], 4.0 * std::f32::consts::PI * 100.0 / 1600.0));
        assert!(approx(out[3], 500.0f32.ln_1p()));
        assert!(approx(out[4], 0.0));
        assert!(out[5].is_nan(), "missing anchor median must stay NaN");
        assert!(out[6].is_nan());
        assert!(out[7].is_nan(), "missing min_height must encode as NaN");
        assert!(approx(out[8], 1.0));
        assert!(approx(out[9], 0.0));
    }

    #[test]
    fn encode_known_class_sets_its_slot() {
        let enc = encoder();
        let mut out = vec![0.0f32; enc.len()];
        enc.encode(&input_with(Some("office"), Some("residential")), &mut out);
        assert!(approx(out[10], 0.0)); // class=house
        assert!(approx(out[11], 1.0)); // class=office
        assert!(approx(out[12], 0.0)); // class=other
        assert!(approx(out[13], 1.0)); // subtype=residential
        assert!(approx(out[14], 0.0)); // subtype=other
    }

    #[test]
    fn encode_unknown_class_fires_other() {
        let enc = encoder();
        let mut out = vec![0.0f32; enc.len()];
        enc.encode(&input_with(Some("castle"), None), &mut out);
        assert!(approx(out[10], 0.0));
        assert!(approx(out[11], 0.0));
        assert!(approx(out[12], 1.0)); // class=other
    }

    #[test]
    fn encode_missing_class_leaves_group_zero() {
        let enc = encoder();
        // Pre-fill with garbage to prove encode overwrites the group.
        let mut out = vec![9.0f32; enc.len()];
        enc.encode(&input_with(None, None), &mut out);
        assert!(approx(out[10], 0.0));
        assert!(approx(out[11], 0.0));
        assert!(approx(out[12], 0.0), "None class must not fire =other");
        assert!(approx(out[13], 0.0));
        assert!(approx(out[14], 0.0));
    }

    #[test]
    fn encode_degenerate_geometry_does_not_panic() {
        let enc = encoder();
        let mut input = input_with(None, None);
        input.footprint_m2 = 0.0;
        input.perimeter_m = 0.0;
        let mut out = vec![0.0f32; enc.len()];
        enc.encode(&input, &mut out);
        assert!(approx(out[0], 0.0));
        assert!(approx(out[1], 0.0));
        assert!(out[2].is_nan(), "degenerate compactness must be NaN");
    }
}
