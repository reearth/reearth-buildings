//! Gradient-boosted-tree building-height model: the wasm inference side
//! of the offline `height-optimizer` trainer.
//!
//! The model predicts `log1p(height_m)` as `base_score + Σ tree_i`, then
//! `expm1`s and clamps the result to a sane metre range. Trees are stored
//! struct-of-arrays for compact JSON and cache-friendly walks, with
//! LightGBM-style learned missing-value directions so a NaN feature
//! (e.g. an absent tile anchor stat) takes a per-node `default_left`
//! branch instead of comparing against a threshold.
//!
//! The production model is embedded via `include_str!` and parsed once
//! per isolate through [`builtin`]; see its docs for the wasm rationale.

use crate::features::{FeatureEncoder, FeatureInput, MAX_FEATURES};
use std::sync::OnceLock;

/// A trained GBT height model plus everything needed to reproduce its
/// train-time feature encoding.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct GbtModel {
    pub version: u32,
    pub name: String,
    /// The training target, e.g. `"log1p_height_m"`. Documentation and a
    /// sanity check only — the predict path always assumes log1p.
    pub target: String,
    /// Constant added to every prediction (the boosting initial value).
    pub base_score: f32,
    pub clamp_min_m: f32,
    pub clamp_max_m: f32,
    pub encoder: FeatureEncoder,
    /// Feature names in encoding order; validated to equal
    /// `encoder.feature_names()` so a mismatched artifact is rejected at
    /// load rather than silently mispredicting.
    pub feature_names: Vec<String>,
    pub trees: Vec<Tree>,
}

/// One regression tree, struct-of-arrays over its nodes.
///
/// Node `i` is a **leaf** iff `feature[i] < 0`; its output value is then
/// `threshold[i]` (leaf values already include the learning-rate
/// shrinkage). For a leaf, `left[i]` / `right[i]` / `default_left[i]` are
/// unused (written as `0` / `false`). For a split, `feature[i]` indexes
/// the encoded feature vector and `left`/`right` are child node indices,
/// both required to be strictly greater than `i` (see
/// [`GbtModel::validate`]) so a walk always terminates.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Tree {
    pub feature: Vec<i16>,
    pub threshold: Vec<f32>,
    pub left: Vec<u16>,
    pub right: Vec<u16>,
    pub default_left: Vec<bool>,
}

/// Error from loading or validating a [`GbtModel`].
#[derive(Debug)]
pub enum ModelError {
    Json(serde_json::Error),
    Invalid(&'static str),
}

impl std::fmt::Display for ModelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ModelError::Json(e) => write!(f, "gbt model json: {e}"),
            ModelError::Invalid(m) => write!(f, "gbt model invalid: {m}"),
        }
    }
}

impl std::error::Error for ModelError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ModelError::Json(e) => Some(e),
            ModelError::Invalid(_) => None,
        }
    }
}

impl GbtModel {
    /// Parse a JSON artifact and [`validate`](Self::validate) it.
    pub fn from_json(s: &str) -> Result<Self, ModelError> {
        let model: GbtModel = serde_json::from_str(s).map_err(ModelError::Json)?;
        model.validate()?;
        Ok(model)
    }

    /// Reject structurally-unsound artifacts so the hot path can walk
    /// trees with no bounds checks and no cycle guard.
    ///
    /// Checks: `version == 1`; the encoder length is in `1..=MAX_FEATURES`;
    /// `feature_names` equals `encoder.feature_names()`; the clamp range
    /// is finite and non-empty; and for every tree all five arrays share
    /// one non-empty length, each split's feature index is within the
    /// encoder, and each child index is in bounds **and strictly greater
    /// than its parent** (which makes the node graph a DAG that always
    /// flows forward — hence acyclic and terminating).
    pub fn validate(&self) -> Result<(), ModelError> {
        if self.version != 1 {
            return Err(ModelError::Invalid("unsupported version (expected 1)"));
        }

        let n_features = self.encoder.len();
        if n_features == 0 {
            return Err(ModelError::Invalid("encoder has zero features"));
        }
        if n_features > MAX_FEATURES {
            return Err(ModelError::Invalid("encoder length exceeds MAX_FEATURES"));
        }
        if self.feature_names != self.encoder.feature_names() {
            return Err(ModelError::Invalid(
                "feature_names do not match encoder.feature_names()",
            ));
        }

        if !self.clamp_min_m.is_finite() || !self.clamp_max_m.is_finite() {
            return Err(ModelError::Invalid("clamp bounds must be finite"));
        }
        if self.clamp_min_m >= self.clamp_max_m {
            return Err(ModelError::Invalid("clamp_min_m must be < clamp_max_m"));
        }

        for tree in &self.trees {
            let n = tree.feature.len();
            if n == 0 {
                return Err(ModelError::Invalid("tree has no nodes"));
            }
            if tree.threshold.len() != n
                || tree.left.len() != n
                || tree.right.len() != n
                || tree.default_left.len() != n
            {
                return Err(ModelError::Invalid("tree arrays have mismatched lengths"));
            }
            if n > u16::MAX as usize {
                return Err(ModelError::Invalid(
                    "tree has too many nodes for u16 indices",
                ));
            }

            for i in 0..n {
                let feat = tree.feature[i];
                if feat < 0 {
                    // Leaf node: children/threshold-as-value need no bounds.
                    continue;
                }
                if feat as usize >= n_features {
                    return Err(ModelError::Invalid("split feature index out of range"));
                }
                let (l, r) = (tree.left[i] as usize, tree.right[i] as usize);
                if l >= n || r >= n {
                    return Err(ModelError::Invalid("child index out of range"));
                }
                if l <= i || r <= i {
                    return Err(ModelError::Invalid(
                        "child index must be greater than parent (acyclic)",
                    ));
                }
            }
        }

        Ok(())
    }

    /// Encoded feature-vector length this model expects.
    pub fn n_features(&self) -> usize {
        self.encoder.len()
    }

    /// Sum the tree walks onto `base_score` in log1p space. `feats` must
    /// have length `>= n_features()`; a NaN feature follows the node's
    /// `default_left`, otherwise the branch is `feats[f] < threshold`.
    pub fn predict_log1p(&self, feats: &[f32]) -> f32 {
        let mut acc = self.base_score;
        for tree in &self.trees {
            let mut i = 0usize;
            loop {
                let feat = tree.feature[i];
                if feat < 0 {
                    acc += tree.threshold[i];
                    break;
                }
                let x = feats[feat as usize];
                let go_left = if x.is_nan() {
                    tree.default_left[i]
                } else {
                    x < tree.threshold[i]
                };
                i = if go_left {
                    tree.left[i] as usize
                } else {
                    tree.right[i] as usize
                };
            }
        }
        acc
    }

    /// Encode `input`, predict, and return a clamped metre height.
    ///
    /// A non-finite prediction (NaN/±inf from `expm1` of an extreme sum)
    /// falls back to `clamp_min_m`, since `f32::clamp` would otherwise
    /// propagate NaN straight through.
    pub fn predict_height_m(&self, input: &FeatureInput<'_>) -> f32 {
        let mut buf = [0.0f32; MAX_FEATURES];
        let n = self.encoder.len();
        self.encoder.encode(input, &mut buf[..n]);
        clamp_height(
            self.predict_log1p(&buf[..n]),
            self.clamp_min_m,
            self.clamp_max_m,
        )
    }
}

/// One flattened tree node. 16 bytes, so a cache line holds four nodes
/// and a root-to-leaf walk touches at most `depth + 1` lines.
#[derive(Debug, Clone, Copy)]
struct FlatNode {
    /// Split threshold, or the leaf value when `feature < 0`.
    threshold: f32,
    /// Absolute indices into the shared `nodes` arena.
    left: u32,
    right: u32,
    feature: i16,
    default_left: bool,
}

/// A [`GbtModel`] repacked for the inference hot path.
///
/// The serde-facing `Tree` stores five `Vec`s per tree — fine as an
/// artifact schema, but a 48-tree walk pointer-chases ~240 heap
/// allocations. `PreparedModel` concatenates every tree into one
/// contiguous arena at load time (child indices rebased to absolute),
/// which measured ~2x faster per prediction with bit-identical output.
/// Built once per isolate by [`builtin`]; the original [`GbtModel`]
/// stays available for the artifact schema, validation, and tests.
#[derive(Debug, Clone)]
pub struct PreparedModel {
    pub model: GbtModel,
    nodes: Vec<FlatNode>,
    roots: Vec<u32>,
}

impl PreparedModel {
    /// Flatten a validated model. Assumes `model.validate()` passed
    /// (guaranteed by [`GbtModel::from_json`]).
    pub fn new(model: GbtModel) -> Self {
        let total: usize = model.trees.iter().map(|t| t.feature.len()).sum();
        let mut nodes = Vec::with_capacity(total);
        let mut roots = Vec::with_capacity(model.trees.len());
        for tree in &model.trees {
            let base = nodes.len() as u32;
            roots.push(base);
            for i in 0..tree.feature.len() {
                nodes.push(FlatNode {
                    threshold: tree.threshold[i],
                    left: base + u32::from(tree.left[i]),
                    right: base + u32::from(tree.right[i]),
                    feature: tree.feature[i],
                    default_left: tree.default_left[i],
                });
            }
        }
        Self {
            model,
            nodes,
            roots,
        }
    }

    /// Arena walk; same contract as [`GbtModel::predict_log1p`].
    pub fn predict_log1p(&self, feats: &[f32]) -> f32 {
        let mut acc = self.model.base_score;
        for &root in &self.roots {
            let mut i = root as usize;
            loop {
                let n = self.nodes[i];
                if n.feature < 0 {
                    acc += n.threshold;
                    break;
                }
                let x = feats[n.feature as usize];
                let go_left = if x.is_nan() {
                    n.default_left
                } else {
                    x < n.threshold
                };
                i = if go_left { n.left } else { n.right } as usize;
            }
        }
        acc
    }

    /// Same contract as [`GbtModel::predict_height_m`].
    pub fn predict_height_m(&self, input: &FeatureInput<'_>) -> f32 {
        let mut buf = [0.0f32; MAX_FEATURES];
        let n = self.model.encoder.len();
        self.model.encoder.encode(input, &mut buf[..n]);
        clamp_height(
            self.predict_log1p(&buf[..n]),
            self.model.clamp_min_m,
            self.model.clamp_max_m,
        )
    }
}

/// `expm1` + clamp with the non-finite guard shared by both walkers.
fn clamp_height(log1p: f32, min_m: f32, max_m: f32) -> f32 {
    let h = log1p.exp_m1();
    if h.is_finite() {
        h.clamp(min_m, max_m)
    } else {
        min_m
    }
}

/// The production model, parsed once per isolate from the embedded JSON
/// and repacked into the flat inference arena.
///
/// The artifact sits raw in the wasm data segment (`include_str!`), so
/// the first call parses ~30 KB of JSON exactly once; `OnceLock`
/// memoises it and works on wasm32's single-threaded isolates without
/// pulling in `once_cell`. The `expect` is safe because a unit test
/// loads the embedded artifact and asserts it validates, so a
/// malformed artifact fails CI rather than a request.
pub fn builtin() -> &'static PreparedModel {
    static BUILTIN: OnceLock<PreparedModel> = OnceLock::new();
    BUILTIN.get_or_init(|| {
        PreparedModel::new(
            GbtModel::from_json(include_str!("../models/height_gbt_v1.json"))
                .expect("embedded height_gbt_v1.json must be valid (enforced by a unit test)"),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::TileContext;

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    /// Model over an empty-vocab encoder (12 features) with two depth-1
    /// trees, exercising both NaN default directions:
    ///   tree 0 splits feature 0 (log1p_area), NaN → left,  leaves 0.5 / 1.0
    ///   tree 1 splits feature 5 (anchor med), NaN → right, leaves -0.2 / 0.3
    fn two_tree_model() -> GbtModel {
        let encoder = FeatureEncoder {
            class_vocab: vec![],
            subtype_vocab: vec![],
        };
        let feature_names = encoder.feature_names();
        GbtModel {
            version: 1,
            name: "test".to_string(),
            target: "log1p_height_m".to_string(),
            base_score: 2.0,
            clamp_min_m: 2.5,
            clamp_max_m: 300.0,
            encoder,
            feature_names,
            trees: vec![
                Tree {
                    feature: vec![0, -1, -1],
                    threshold: vec![5.0, 0.5, 1.0],
                    left: vec![1, 0, 0],
                    right: vec![2, 0, 0],
                    default_left: vec![true, false, false],
                },
                Tree {
                    feature: vec![5, -1, -1],
                    threshold: vec![2.0, -0.2, 0.3],
                    left: vec![1, 0, 0],
                    right: vec![2, 0, 0],
                    default_left: vec![false, false, false],
                },
            ],
        }
    }

    fn feats(v0: f32, v5: f32) -> Vec<f32> {
        let mut f = vec![0.0f32; 12];
        f[0] = v0;
        f[5] = v5;
        f
    }

    #[test]
    fn walker_takes_both_branches() {
        let m = two_tree_model();
        // f0=3 (<5 → left, 0.5); f5=1 (<2 → left, -0.2)
        assert!(approx(m.predict_log1p(&feats(3.0, 1.0)), 2.0 + 0.5 - 0.2));
        // f0=7 (right, 1.0); f5=9 (right, 0.3)
        assert!(approx(m.predict_log1p(&feats(7.0, 9.0)), 2.0 + 1.0 + 0.3));
    }

    #[test]
    fn walker_routes_nan_by_default_direction() {
        let m = two_tree_model();
        // NaN f0 → default_left=true → 0.5; NaN f5 → default_left=false → 0.3
        assert!(approx(
            m.predict_log1p(&feats(f32::NAN, f32::NAN)),
            2.0 + 0.5 + 0.3
        ));
    }

    #[test]
    fn leaf_only_tree_and_empty_trees() {
        let mut m = two_tree_model();
        m.trees = vec![Tree {
            feature: vec![-1],
            threshold: vec![0.25],
            left: vec![0],
            right: vec![0],
            default_left: vec![false],
        }];
        m.validate().expect("leaf-only tree is valid");
        assert!(approx(m.predict_log1p(&feats(0.0, 0.0)), 2.25));

        m.trees = vec![];
        m.validate().expect("empty forest is valid");
        assert!(approx(m.predict_log1p(&feats(0.0, 0.0)), 2.0));
    }

    fn any_input() -> crate::features::FeatureInput<'static> {
        crate::features::FeatureInput {
            footprint_m2: 120.0,
            perimeter_m: 44.0,
            class: None,
            subtype: None,
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

    #[test]
    fn predict_height_clamps_both_ends_and_guards_non_finite() {
        let mut m = two_tree_model();
        m.trees = vec![];

        m.base_score = 20.0; // expm1(20) ≈ 4.8e8 → clamp_max
        assert!(approx(m.predict_height_m(&any_input()), 300.0));

        m.base_score = -20.0; // expm1(-20) ≈ -1 → clamp_min
        assert!(approx(m.predict_height_m(&any_input()), 2.5));

        m.base_score = f32::MAX; // expm1 overflows to inf → clamp_min guard
        assert!(approx(m.predict_height_m(&any_input()), 2.5));
    }

    #[test]
    fn validate_rejects_malformed_models() {
        let mut m = two_tree_model();
        m.trees[0].feature[0] = 12; // == encoder.len(), out of range
        assert!(m.validate().is_err());

        let mut m = two_tree_model();
        m.trees[0].left[0] = 0; // child <= parent: cycle
        assert!(m.validate().is_err());

        let mut m = two_tree_model();
        m.trees[0].right[0] = 40; // out of bounds
        assert!(m.validate().is_err());

        let mut m = two_tree_model();
        m.trees[1].threshold.pop(); // mismatched array lengths
        assert!(m.validate().is_err());

        let mut m = two_tree_model();
        m.feature_names[0] = "wrong".to_string();
        assert!(m.validate().is_err());

        let mut m = two_tree_model();
        m.version = 2;
        assert!(m.validate().is_err());

        let mut m = two_tree_model();
        m.clamp_min_m = 400.0; // min >= max
        assert!(m.validate().is_err());
    }

    #[test]
    fn from_json_round_trips() {
        let m = two_tree_model();
        let json = serde_json::to_string(&m).unwrap();
        let back = GbtModel::from_json(&json).unwrap();
        assert!(approx(
            back.predict_log1p(&feats(3.0, 1.0)),
            m.predict_log1p(&feats(3.0, 1.0))
        ));
    }

    #[test]
    fn builtin_artifact_is_valid_small_and_finite() {
        // This is the test the builtin() expect() message refers to: a
        // malformed or oversized checked-in artifact must fail CI here.
        let raw = include_str!("../models/height_gbt_v1.json");
        assert!(raw.len() < 200_000, "artifact too large for wasm bundle");
        let m = builtin();
        let h = m.predict_height_m(&any_input());
        assert!(h.is_finite());
        assert!(h >= m.model.clamp_min_m && h <= m.model.clamp_max_m);
    }

    #[test]
    fn prepared_arena_matches_soa_walker() {
        // The flat arena must be a pure repacking: identical output to
        // the serde-facing walker on every branch shape, incl. NaN
        // routing in both default directions.
        let prepared = PreparedModel::new(two_tree_model());
        for (v0, v5) in [
            (3.0, 1.0),
            (7.0, 9.0),
            (3.0, 9.0),
            (7.0, 1.0),
            (f32::NAN, 1.0),
            (3.0, f32::NAN),
            (f32::NAN, f32::NAN),
        ] {
            let f = feats(v0, v5);
            assert!(
                approx(prepared.predict_log1p(&f), prepared.model.predict_log1p(&f)),
                "flat/SoA divergence at ({v0}, {v5})"
            );
        }

        // And on the real embedded artifact through the full encode path.
        let b = builtin();
        assert!(approx(
            b.predict_height_m(&any_input()),
            b.model.predict_height_m(&any_input())
        ));
    }
}
