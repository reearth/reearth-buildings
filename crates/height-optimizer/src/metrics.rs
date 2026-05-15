//! Residual statistics, sliced by `height_method` / `class` / `subtype`.

use crate::matcher::Pair;
use std::collections::BTreeMap;

#[derive(Debug, Default, Clone)]
pub struct Stats {
    pub n: usize,
    pub mae: f32,
    pub rmse: f32,
    pub bias: f32,
    pub p50_abs: f32,
    pub p90_abs: f32,
    pub within_20pct: f32,
    pub within_50pct: f32,
}

impl Stats {
    pub fn compute(pairs: &[&Pair<'_>]) -> Self {
        let n = pairs.len();
        if n == 0 {
            return Self::default();
        }
        let mut sum_abs = 0.0f64;
        let mut sum_sq = 0.0f64;
        let mut sum_signed = 0.0f64;
        let mut abs_residuals: Vec<f32> = Vec::with_capacity(n);
        let mut hits20 = 0usize;
        let mut hits50 = 0usize;
        for p in pairs {
            let r = p.residual_m as f64;
            sum_abs += r.abs();
            sum_sq += r * r;
            sum_signed += r;
            abs_residuals.push(r.abs() as f32);
            let truth = p.truth.measured_height_m as f64;
            let pct = if truth > 0.0 {
                r.abs() / truth
            } else {
                f64::INFINITY
            };
            if pct < 0.20 {
                hits20 += 1;
            }
            if pct < 0.50 {
                hits50 += 1;
            }
        }
        abs_residuals.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let p50 = abs_residuals[n * 50 / 100];
        let p90 = abs_residuals[(n * 90 / 100).min(n - 1)];
        Self {
            n,
            mae: (sum_abs / n as f64) as f32,
            rmse: (sum_sq / n as f64).sqrt() as f32,
            bias: (sum_signed / n as f64) as f32,
            p50_abs: p50,
            p90_abs: p90,
            within_20pct: hits20 as f32 / n as f32,
            within_50pct: hits50 as f32 / n as f32,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Report {
    pub overall: Stats,
    pub by_method: BTreeMap<String, Stats>,
    pub by_class: BTreeMap<String, Stats>,
    pub by_subtype: BTreeMap<String, Stats>,
    pub unmatched_truth: usize,
    pub n_truth: usize,
    pub n_estimate: usize,
}

pub fn build_report(
    pairs: &[Pair<'_>],
    unmatched_truth: usize,
    n_truth: usize,
    n_estimate: usize,
) -> Report {
    let all: Vec<&Pair<'_>> = pairs.iter().collect();
    let mut by_method: BTreeMap<String, Vec<&Pair<'_>>> = BTreeMap::new();
    let mut by_class: BTreeMap<String, Vec<&Pair<'_>>> = BTreeMap::new();
    let mut by_subtype: BTreeMap<String, Vec<&Pair<'_>>> = BTreeMap::new();
    for p in pairs {
        by_method
            .entry(p.estimate.height_method.to_string())
            .or_default()
            .push(p);
        if let Some(c) = &p.estimate.class {
            by_class.entry(c.clone()).or_default().push(p);
        }
        if let Some(s) = &p.estimate.subtype {
            by_subtype.entry(s.clone()).or_default().push(p);
        }
    }
    Report {
        overall: Stats::compute(&all),
        by_method: by_method
            .into_iter()
            .map(|(k, v)| (k, Stats::compute(&v)))
            .collect(),
        by_class: by_class
            .into_iter()
            .map(|(k, v)| (k, Stats::compute(&v)))
            .collect(),
        by_subtype: by_subtype
            .into_iter()
            .map(|(k, v)| (k, Stats::compute(&v)))
            .collect(),
        unmatched_truth,
        n_truth,
        n_estimate,
    }
}

pub fn print_report(name: &str, r: &Report) {
    println!("\n===== {name} =====");
    println!(
        "truth={} estimate={} matched={} unmatched_truth={}",
        r.n_truth, r.n_estimate, r.overall.n, r.unmatched_truth
    );
    println!();
    print_row_header();
    print_row("OVERALL", &r.overall);
    println!("\nby height_method:");
    print_row_header();
    for (k, s) in &r.by_method {
        print_row(k, s);
    }
    println!("\nby class (top 10 by n):");
    print_row_header();
    let mut by_class: Vec<_> = r.by_class.iter().collect();
    by_class.sort_by(|a, b| b.1.n.cmp(&a.1.n));
    for (k, s) in by_class.iter().take(10) {
        print_row(k, s);
    }
    println!("\nby subtype:");
    print_row_header();
    let mut by_st: Vec<_> = r.by_subtype.iter().collect();
    by_st.sort_by(|a, b| b.1.n.cmp(&a.1.n));
    for (k, s) in by_st.iter().take(10) {
        print_row(k, s);
    }
}

fn print_row_header() {
    println!(
        "{:<22} {:>6} {:>7} {:>7} {:>7} {:>7} {:>7} {:>7} {:>7}",
        "slice", "n", "MAE", "RMSE", "bias", "P50", "P90", "<20%", "<50%"
    );
}

fn print_row(name: &str, s: &Stats) {
    if s.n == 0 {
        return;
    }
    println!(
        "{:<22} {:>6} {:>7.2} {:>7.2} {:>+7.2} {:>7.2} {:>7.2} {:>6.0}% {:>6.0}%",
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
