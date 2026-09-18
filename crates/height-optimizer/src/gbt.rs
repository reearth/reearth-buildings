//! Deterministic, dependency-free gradient-boosted-tree regressor.
//!
//! Fits squared-loss regression trees to the `log1p(height_m)` target,
//! exact-greedy (no histograms — the datasets are only tens of thousands of
//! rows). Every design choice serves one goal: **byte-identical retrains**.
//! There is no floating-point-order ambiguity introduced by hash-map
//! iteration, `total_cmp` orders the presorts, and split ties resolve by
//! (lowest feature index, lowest threshold). The only randomness is an
//! optional seeded LCG used for sub-sampling, which is skipped entirely when
//! the sub-sample fractions are `1.0` (the default).
//!
//! Missing values are LightGBM-style: NaN feature rows are bucketed
//! separately per feature, and each split tries assigning them left vs right,
//! keeping the better direction as the node's `default_left`.
//!
//! The trainer is deliberately index-driven (column-major arrays walked by
//! row/feature index), so `needless_range_loop` is allowed module-wide — the
//! index *is* the algorithm here, and rewriting to iterators would obscure it.
#![allow(clippy::needless_range_loop)]

use crate::dataset::Dataset;
use buildings_core::features::FeatureEncoder;
use buildings_core::height_model::{GbtModel, Tree};

/// Hyperparameters. Defaults match the plan's v1 model.
#[derive(Debug, Clone)]
pub struct TrainParams {
    pub n_trees: usize,
    pub max_depth: usize,
    pub learning_rate: f32,
    pub min_samples_leaf: usize,
    pub l2_lambda: f32,
    pub min_gain: f32,
    pub feature_subsample: f32,
    pub row_subsample: f32,
    pub seed: u64,
}

impl Default for TrainParams {
    fn default() -> Self {
        Self {
            n_trees: 48,
            max_depth: 4,
            learning_rate: 0.1,
            min_samples_leaf: 20,
            l2_lambda: 1.0,
            min_gain: 1e-6,
            feature_subsample: 1.0,
            row_subsample: 1.0,
            seed: 42,
        }
    }
}

/// Per-iteration diagnostics captured after each tree is added.
#[derive(Debug, Clone)]
pub struct IterStat {
    pub tree_idx: usize,
    pub train_rmse_log: f32,
    pub holdout_rmse_log: f32,
    pub holdout_mae_m: f32,
}

/// The trained model plus its learning curve.
pub struct TrainReport {
    pub iters: Vec<IterStat>,
    pub model: GbtModel,
}

/// Small linear-congruential generator (glibc constants). Only used when a
/// sub-sample fraction is `< 1.0`; deterministic from `seed`.
struct Lcg(u64);

impl Lcg {
    fn next_u64(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0
    }
    /// Uniform in `[0, 1)`.
    fn next_f32(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32
    }
}

/// A node in the tree under construction. Children indices point forward in
/// the BFS-ordered `nodes` vec (child index > parent index always), which is
/// exactly the invariant [`GbtModel::validate`] enforces.
#[derive(Clone)]
struct Node {
    /// `None` while a leaf; `Some` once split.
    split: Option<Split>,
    /// Leaf output (learning-rate shrinkage already baked in). Only
    /// meaningful for leaves.
    value: f32,
    depth: usize,
}

#[derive(Clone)]
struct Split {
    feature: usize,
    threshold: f32,
    default_left: bool,
    left: usize,
    right: usize,
}

/// A candidate split evaluated during level growth.
struct BestSplit {
    gain: f32,
    feature: usize,
    threshold: f32,
    default_left: bool,
}

pub fn train(
    train: &Dataset,
    holdout: &Dataset,
    params: &TrainParams,
    clamp: (f32, f32),
    name: &str,
) -> TrainReport {
    let n_feats = train.encoder.len();
    let n_rows = train.rows.len();

    // Column-major features + presorted (non-NaN) row indices per feature,
    // with NaN rows bucketed separately.
    let mut cols: Vec<Vec<f32>> = vec![vec![0.0; n_rows]; n_feats];
    for (i, row) in train.rows.iter().enumerate() {
        for f in 0..n_feats {
            cols[f][i] = row.features[f];
        }
    }
    let y: Vec<f32> = train.rows.iter().map(|r| r.target_log1p).collect();

    let mut sorted_idx: Vec<Vec<u32>> = Vec::with_capacity(n_feats);
    let mut nan_idx: Vec<Vec<u32>> = Vec::with_capacity(n_feats);
    for f in 0..n_feats {
        let mut non_nan: Vec<u32> = Vec::new();
        let mut nans: Vec<u32> = Vec::new();
        for i in 0..n_rows {
            if cols[f][i].is_nan() {
                nans.push(i as u32);
            } else {
                non_nan.push(i as u32);
            }
        }
        non_nan.sort_by(|&a, &b| cols[f][a as usize].total_cmp(&cols[f][b as usize]));
        sorted_idx.push(non_nan);
        nan_idx.push(nans);
    }

    let base_score = if n_rows == 0 {
        0.0
    } else {
        (y.iter().map(|&v| v as f64).sum::<f64>() / n_rows as f64) as f32
    };

    // Running predictions in log space, seeded with base_score.
    let mut train_pred = vec![base_score; n_rows];

    // Encode holdout once for the learning curve.
    let holdout_x: Vec<&[f32]> = holdout.rows.iter().map(|r| r.features.as_slice()).collect();
    let holdout_y: Vec<f32> = holdout.rows.iter().map(|r| r.target_log1p).collect();
    let mut holdout_pred = vec![base_score; holdout.rows.len()];

    let mut rng = Lcg(params.seed);
    let mut trees: Vec<Vec<Node>> = Vec::with_capacity(params.n_trees);
    let mut iters: Vec<IterStat> = Vec::with_capacity(params.n_trees);

    for t in 0..params.n_trees {
        // Residuals for squared loss: r = y - pred.
        let residuals: Vec<f32> = (0..n_rows).map(|i| y[i] - train_pred[i]).collect();

        // Optional per-tree sub-sampling (skipped at 1.0).
        let row_active: Vec<bool> = if params.row_subsample < 1.0 {
            (0..n_rows)
                .map(|_| rng.next_f32() < params.row_subsample)
                .collect()
        } else {
            vec![true; n_rows]
        };
        let feat_active: Vec<bool> = if params.feature_subsample < 1.0 {
            (0..n_feats)
                .map(|_| rng.next_f32() < params.feature_subsample)
                .collect()
        } else {
            vec![true; n_feats]
        };

        let nodes = grow_tree(
            &cols,
            &sorted_idx,
            &nan_idx,
            &residuals,
            &row_active,
            &feat_active,
            n_rows,
            n_feats,
            params,
        );

        // Update running predictions by routing every row through the tree.
        for i in 0..n_rows {
            train_pred[i] += walk(&nodes, |f| cols[f][i]);
        }
        for (i, x) in holdout_x.iter().enumerate() {
            holdout_pred[i] += walk(&nodes, |f| x[f]);
        }
        trees.push(nodes);

        iters.push(IterStat {
            tree_idx: t,
            train_rmse_log: rmse(&train_pred, &y),
            holdout_rmse_log: rmse(&holdout_pred, &holdout_y),
            holdout_mae_m: mae_metres(&holdout_pred, &holdout_y, clamp),
        });
    }

    let model = export_model(&trees, base_score, clamp, &train.encoder, name);
    // The artifact must round-trip through the strict wasm-side validator.
    let json = serde_json::to_string(&model).expect("serialize gbt model");
    GbtModel::from_json(&json).expect("trained model must pass GbtModel::from_json");

    TrainReport { iters, model }
}

/// Grow one squared-loss tree level-by-level with a per-row node-membership
/// array. Returns the nodes in BFS order.
#[allow(clippy::too_many_arguments)]
fn grow_tree(
    cols: &[Vec<f32>],
    sorted_idx: &[Vec<u32>],
    nan_idx: &[Vec<u32>],
    residuals: &[f32],
    row_active: &[bool],
    feat_active: &[bool],
    n_rows: usize,
    n_feats: usize,
    params: &TrainParams,
) -> Vec<Node> {
    let mut nodes: Vec<Node> = vec![Node {
        split: None,
        value: 0.0,
        depth: 0,
    }];
    // Which node each row currently belongs to (0 = root).
    let mut node_of_row: Vec<u32> = vec![0; n_rows];

    // Frontier = node ids eligible to split at the current depth.
    let mut frontier: Vec<usize> = vec![0];

    for _depth in 0..params.max_depth {
        if frontier.is_empty() {
            break;
        }
        // Map node id -> slot in the per-level accumulator arrays.
        let mut node_slot: Vec<i32> = vec![-1; nodes.len()];
        for (slot, &nid) in frontier.iter().enumerate() {
            node_slot[nid] = slot as i32;
        }
        let n_open = frontier.len();

        // Per-open-node totals over active rows (Σr, count), all features.
        let mut total_sum = vec![0.0f64; n_open];
        let mut total_cnt = vec![0usize; n_open];
        for i in 0..n_rows {
            if !row_active[i] {
                continue;
            }
            let slot = node_slot[node_of_row[i] as usize];
            if slot >= 0 {
                total_sum[slot as usize] += residuals[i] as f64;
                total_cnt[slot as usize] += 1;
            }
        }

        let mut best: Vec<Option<BestSplit>> = (0..n_open).map(|_| None).collect();

        for f in 0..n_feats {
            if !feat_active[f] {
                continue;
            }
            // NaN totals per open node for this feature.
            let mut nan_sum = vec![0.0f64; n_open];
            let mut nan_cnt = vec![0usize; n_open];
            for &row in &nan_idx[f] {
                if !row_active[row as usize] {
                    continue;
                }
                let slot = node_slot[node_of_row[row as usize] as usize];
                if slot >= 0 {
                    nan_sum[slot as usize] += residuals[row as usize] as f64;
                    nan_cnt[slot as usize] += 1;
                }
            }

            // Running prefix (rows with value < current threshold) per node.
            let mut pre_sum = vec![0.0f64; n_open];
            let mut pre_cnt = vec![0usize; n_open];

            let sorted = &sorted_idx[f];
            let mut k = 0usize;
            let mut prev_v: Option<f32> = None;
            while k < sorted.len() {
                let v = cols[f][sorted[k] as usize];
                if let Some(pv) = prev_v {
                    // Boundary between distinct values pv < v. prefix holds
                    // every row with value <= pv.
                    let thr = 0.5 * (pv + v);
                    for slot in 0..n_open {
                        consider_split(
                            slot,
                            f,
                            thr,
                            pre_sum[slot],
                            pre_cnt[slot],
                            nan_sum[slot],
                            nan_cnt[slot],
                            total_sum[slot],
                            total_cnt[slot],
                            params,
                            &mut best[slot],
                        );
                    }
                }
                // Accumulate the whole value == v group into the prefix.
                while k < sorted.len() && cols[f][sorted[k] as usize] == v {
                    let row = sorted[k] as usize;
                    if row_active[row] {
                        let slot = node_slot[node_of_row[row] as usize];
                        if slot >= 0 {
                            pre_sum[slot as usize] += residuals[row] as f64;
                            pre_cnt[slot as usize] += 1;
                        }
                    }
                    k += 1;
                }
                prev_v = Some(v);
            }
        }

        // Apply the winning splits, creating children in BFS order.
        let mut next_frontier: Vec<usize> = Vec::new();
        let depth_of = nodes[frontier[0]].depth;
        // Record which node ids split, and their split params, before we
        // mutate node_of_row.
        let mut split_of_node: Vec<Option<Split>> = vec![None; nodes.len()];
        for (slot, &nid) in frontier.iter().enumerate() {
            if let Some(bs) = best[slot].take() {
                let left = nodes.len();
                let right = nodes.len() + 1;
                nodes.push(Node {
                    split: None,
                    value: 0.0,
                    depth: depth_of + 1,
                });
                nodes.push(Node {
                    split: None,
                    value: 0.0,
                    depth: depth_of + 1,
                });
                let split = Split {
                    feature: bs.feature,
                    threshold: bs.threshold,
                    default_left: bs.default_left,
                    left,
                    right,
                };
                split_of_node[nid] = Some(split.clone());
                nodes[nid].split = Some(split);
                next_frontier.push(left);
                next_frontier.push(right);
            }
        }

        // Re-route every row that belonged to a node that split.
        for i in 0..n_rows {
            let nid = node_of_row[i] as usize;
            if let Some(split) = &split_of_node[nid] {
                let x = cols[split.feature][i];
                let go_left = if x.is_nan() {
                    split.default_left
                } else {
                    x < split.threshold
                };
                node_of_row[i] = if go_left {
                    split.left as u32
                } else {
                    split.right as u32
                };
            }
        }

        frontier = next_frontier;
    }

    // Compute leaf values from the residuals of the rows that landed there.
    let mut leaf_sum = vec![0.0f64; nodes.len()];
    let mut leaf_cnt = vec![0usize; nodes.len()];
    for i in 0..n_rows {
        if !row_active[i] {
            continue;
        }
        let nid = node_of_row[i] as usize;
        leaf_sum[nid] += residuals[i] as f64;
        leaf_cnt[nid] += 1;
    }
    for (nid, node) in nodes.iter_mut().enumerate() {
        if node.split.is_none() {
            let denom = leaf_cnt[nid] as f64 + params.l2_lambda as f64;
            node.value = if denom > 0.0 {
                (params.learning_rate as f64 * leaf_sum[nid] / denom) as f32
            } else {
                0.0
            };
        }
    }

    nodes
}

/// Evaluate a candidate split for one node at `threshold`, trying both NaN
/// directions, and update `best` if it strictly improves. `pre_*` are the
/// non-NaN rows with value `< threshold`; `total_*` includes NaN rows.
#[allow(clippy::too_many_arguments)]
fn consider_split(
    _slot: usize,
    feature: usize,
    threshold: f32,
    pre_sum: f64,
    pre_cnt: usize,
    nan_sum: f64,
    nan_cnt: usize,
    total_sum: f64,
    total_cnt: usize,
    params: &TrainParams,
    best: &mut Option<BestSplit>,
) {
    // Non-NaN split halves.
    let non_nan_sum = total_sum - nan_sum;
    let non_nan_cnt = total_cnt - nan_cnt;
    let left_nn_sum = pre_sum;
    let left_nn_cnt = pre_cnt;
    let right_nn_sum = non_nan_sum - pre_sum;
    let right_nn_cnt = non_nan_cnt - pre_cnt;
    // The non-NaN rows must be genuinely split.
    if left_nn_cnt == 0 || right_nn_cnt == 0 {
        return;
    }

    let parent_term = total_sum * total_sum / total_cnt as f64;
    let min_leaf = params.min_samples_leaf;

    let eval = |nan_left: bool| -> Option<f64> {
        let (l_sum, l_cnt, r_sum, r_cnt) = if nan_left {
            (
                left_nn_sum + nan_sum,
                left_nn_cnt + nan_cnt,
                right_nn_sum,
                right_nn_cnt,
            )
        } else {
            (
                left_nn_sum,
                left_nn_cnt,
                right_nn_sum + nan_sum,
                right_nn_cnt + nan_cnt,
            )
        };
        if l_cnt < min_leaf || r_cnt < min_leaf {
            return None;
        }
        Some(l_sum * l_sum / l_cnt as f64 + r_sum * r_sum / r_cnt as f64 - parent_term)
    };

    // Prefer routing NaN left on an exact tie (deterministic).
    let g_left = eval(true);
    let g_right = eval(false);
    let (gain, default_left) = match (g_left, g_right) {
        (Some(gl), Some(gr)) => {
            if gr > gl {
                (gr, false)
            } else {
                (gl, true)
            }
        }
        (Some(gl), None) => (gl, true),
        (None, Some(gr)) => (gr, false),
        (None, None) => return,
    };

    let gain = gain as f32;
    if gain < params.min_gain {
        return;
    }
    // Strictly-better wins → first-found (lower feature idx, lower
    // threshold) survives ties.
    let improves = match best {
        Some(b) => gain > b.gain,
        None => true,
    };
    if improves {
        *best = Some(BestSplit {
            gain,
            feature,
            threshold,
            default_left,
        });
    }
}

/// Walk a grown tree, returning the leaf value. `feat(f)` yields feature `f`.
fn walk(nodes: &[Node], feat: impl Fn(usize) -> f32) -> f32 {
    let mut i = 0usize;
    loop {
        match &nodes[i].split {
            None => return nodes[i].value,
            Some(s) => {
                let x = feat(s.feature);
                let go_left = if x.is_nan() {
                    s.default_left
                } else {
                    x < s.threshold
                };
                i = if go_left { s.left } else { s.right };
            }
        }
    }
}

fn rmse(pred: &[f32], truth: &[f32]) -> f32 {
    if pred.is_empty() {
        return 0.0;
    }
    let mut s = 0.0f64;
    for i in 0..pred.len() {
        let d = (pred[i] - truth[i]) as f64;
        s += d * d;
    }
    (s / pred.len() as f64).sqrt() as f32
}

/// MAE in metres: `expm1`+clamp both prediction and (already-clamped) target.
fn mae_metres(pred_log: &[f32], truth_log: &[f32], clamp: (f32, f32)) -> f32 {
    if pred_log.is_empty() {
        return 0.0;
    }
    let mut s = 0.0f64;
    for i in 0..pred_log.len() {
        let ph = pred_log[i].exp_m1();
        let ph = if ph.is_finite() {
            ph.clamp(clamp.0, clamp.1)
        } else {
            clamp.0
        };
        let th = truth_log[i].exp_m1();
        s += (ph - th).abs() as f64;
    }
    (s / pred_log.len() as f64) as f32
}

/// Convert grown trees into a validated [`GbtModel`]. Drops trees whose root
/// never split *and* carries a negligible leaf value (pure no-ops).
fn export_model(
    trees: &[Vec<Node>],
    base_score: f32,
    clamp: (f32, f32),
    encoder: &FeatureEncoder,
    name: &str,
) -> GbtModel {
    let mut out_trees: Vec<Tree> = Vec::new();
    for nodes in trees {
        if nodes.len() == 1 && nodes[0].split.is_none() && nodes[0].value.abs() < 1e-9 {
            continue; // no-op tree
        }
        let n = nodes.len();
        let mut feature = vec![0i16; n];
        let mut threshold = vec![0.0f32; n];
        let mut left = vec![0u16; n];
        let mut right = vec![0u16; n];
        let mut default_left = vec![false; n];
        for (i, node) in nodes.iter().enumerate() {
            match &node.split {
                None => {
                    feature[i] = -1;
                    threshold[i] = node.value;
                }
                Some(s) => {
                    feature[i] = s.feature as i16;
                    threshold[i] = s.threshold;
                    left[i] = s.left as u16;
                    right[i] = s.right as u16;
                    default_left[i] = s.default_left;
                }
            }
        }
        out_trees.push(Tree {
            feature,
            threshold,
            left,
            right,
            default_left,
        });
    }

    GbtModel {
        version: 1,
        name: name.to_string(),
        target: "log1p_height_m".to_string(),
        base_score,
        clamp_min_m: clamp.0,
        clamp_max_m: clamp.1,
        encoder: encoder.clone(),
        feature_names: encoder.feature_names(),
        trees: out_trees,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dataset::{Dataset, Row};

    const N_FEATS: usize = 12; // empty-vocab encoder width

    /// Deterministic synthetic regression set: y is a step function of
    /// features 0 and 1 plus small LCG noise. Feature 5 is NaN for every
    /// third row (those rows skew high, so its default direction is
    /// learnable); the rest are uninformative constants.
    fn synthetic(n: usize) -> Dataset {
        let mut rng = Lcg(7);
        let rows = (0..n)
            .map(|_| {
                let x0 = rng.next_f32() * 10.0;
                let x1 = rng.next_f32() * 4.0;
                let nan5 = rng.next_u64() % 3 == 0;
                let noise = (rng.next_f32() - 0.5) * 0.1;
                let mut y = if x0 > 5.0 { 3.0 } else { 1.5 };
                y += if x1 > 2.0 { 0.6 } else { 0.0 };
                if nan5 {
                    y += 0.8;
                }
                let mut features = vec![0.25f32; N_FEATS];
                features[0] = x0;
                features[1] = x1;
                features[5] = if nan5 { f32::NAN } else { rng.next_f32() };
                Row {
                    preset: "synthetic".to_string(),
                    features,
                    target_log1p: y + noise,
                }
            })
            .collect();
        Dataset {
            encoder: FeatureEncoder {
                class_vocab: vec![],
                subtype_vocab: vec![],
            },
            rows,
        }
    }

    fn params() -> TrainParams {
        TrainParams {
            n_trees: 12,
            max_depth: 3,
            min_samples_leaf: 10,
            ..Default::default()
        }
    }

    #[test]
    fn training_is_byte_deterministic() {
        let ds = synthetic(600);
        let ho = synthetic(100);
        let a = train(&ds, &ho, &params(), (2.5, 300.0), "det-test");
        let b = train(&ds, &ho, &params(), (2.5, 300.0), "det-test");
        let ja = serde_json::to_string(&a.model).unwrap();
        let jb = serde_json::to_string(&b.model).unwrap();
        assert_eq!(
            ja, jb,
            "two identical train() runs must serialize identically"
        );
    }

    #[test]
    fn training_beats_the_constant_baseline() {
        let ds = synthetic(600);
        let ho = synthetic(100);
        let report = train(&ds, &ho, &params(), (2.5, 300.0), "learn-test");

        // RMSE of predicting the mean (what base_score alone gives).
        let y: Vec<f32> = ds.rows.iter().map(|r| r.target_log1p).collect();
        let mean = y.iter().sum::<f32>() / y.len() as f32;
        let base_rmse = rmse(&vec![mean; y.len()], &y);

        let final_rmse = report.iters.last().unwrap().train_rmse_log;
        assert!(
            final_rmse < base_rmse * 0.5,
            "boosting must clearly beat the mean predictor \
             ({final_rmse} vs base {base_rmse})"
        );
        assert!(!report.model.trees.is_empty());
        // The NaN-informative feature must actually get used, with its
        // default direction steering the NaN (high-y) rows.
        let uses_f5 = report.model.trees.iter().any(|t| t.feature.contains(&5));
        assert!(uses_f5, "feature 5 (NaN-informative) should be split on");
    }

    #[test]
    fn every_leaf_respects_min_samples_leaf() {
        let ds = synthetic(600);
        let ho = synthetic(50);
        let p = params();
        let report = train(&ds, &ho, &p, (2.5, 300.0), "leaf-test");

        for tree in &report.model.trees {
            let mut leaf_counts = vec![0usize; tree.feature.len()];
            for row in &ds.rows {
                let mut i = 0usize;
                while tree.feature[i] >= 0 {
                    let x = row.features[tree.feature[i] as usize];
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
                leaf_counts[i] += 1;
            }
            for (i, &cnt) in leaf_counts.iter().enumerate() {
                if tree.feature[i] < 0 && cnt > 0 {
                    assert!(
                        cnt >= p.min_samples_leaf,
                        "leaf {i} holds {cnt} rows < min_samples_leaf {}",
                        p.min_samples_leaf
                    );
                }
            }
        }
    }

    #[test]
    fn all_nan_feature_trains_without_panic() {
        let mut ds = synthetic(300);
        for row in &mut ds.rows {
            row.features[7] = f32::NAN; // e.g. min_height never present
        }
        let ho = synthetic(50);
        let report = train(&ds, &ho, &params(), (2.5, 300.0), "nan-test");
        // A fully-NaN column can never split (no non-NaN halves), so it
        // must simply be ignored, and predictions stay finite.
        for t in &report.model.trees {
            assert!(t.feature.iter().all(|&f| f != 7));
        }
        let json = serde_json::to_string(&report.model).unwrap();
        assert!(GbtModel::from_json(&json).is_ok());
    }
}
