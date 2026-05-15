//! Markdown rendering for the `compare` subcommand.

use crate::metrics::{Report, Stats};
use std::path::Path;

pub struct ComparisonSection {
    pub city_name: String,
    pub city_note: String,
    pub baseline: Report,
    pub candidate: Report,
}

pub fn render_markdown(
    sections: &[ComparisonSection],
    baseline_path: Option<&Path>,
    candidate_path: &Path,
    release: &str,
) -> String {
    let mut out = String::new();
    out.push_str("# Height-cascade calibration report\n\n");
    out.push_str(&format!(
        "- **baseline**: `{}`\n",
        baseline_path
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "<production defaults>".to_string())
    ));
    out.push_str(&format!(
        "- **candidate**: `{}`\n",
        candidate_path.display()
    ));
    out.push_str(&format!("- **Overture release**: `{release}`\n\n"));

    // Top-line summary table.
    out.push_str("## Summary (overall residuals)\n\n");
    out.push_str(
        "| city | n | MAE base→cand | RMSE base→cand | bias base→cand | <20% base→cand |\n",
    );
    out.push_str("|---|---:|---|---|---|---|\n");
    for s in sections {
        let b = &s.baseline.overall;
        let c = &s.candidate.overall;
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} |\n",
            s.city_name,
            b.n,
            fmt_pair(b.mae, c.mae, false),
            fmt_pair(b.rmse, c.rmse, false),
            fmt_pair_signed(b.bias, c.bias),
            fmt_pct_pair(b.within_20pct, c.within_20pct),
        ));
    }
    out.push('\n');

    // Per-city detail.
    for s in sections {
        out.push_str(&format!("## {} — {}\n\n", s.city_name, s.city_note));
        out.push_str(&format!(
            "PLATEAU truth: **{}**, Overture in-bbox: **{}**, matched: **{}**, unmatched truth: **{}**\n\n",
            s.baseline.n_truth,
            s.baseline.n_estimate,
            s.baseline.overall.n,
            s.baseline.unmatched_truth,
        ));

        out.push_str("### Overall\n\n");
        push_compare_table(
            &mut out,
            &[("OVERALL", &s.baseline.overall, &s.candidate.overall)],
        );

        out.push_str("\n### By height_method\n\n");
        let rows = pair_rows(&s.baseline.by_method, &s.candidate.by_method);
        push_compare_table(&mut out, &rows);

        out.push_str("\n### By class (top 10 by baseline n)\n\n");
        let rows = pair_rows_topn(&s.baseline.by_class, &s.candidate.by_class, 10);
        push_compare_table(&mut out, &rows);

        out.push_str("\n### By subtype\n\n");
        let rows = pair_rows_topn(&s.baseline.by_subtype, &s.candidate.by_subtype, 10);
        push_compare_table(&mut out, &rows);

        out.push('\n');
    }
    out
}

fn pair_rows<'a>(
    base: &'a std::collections::BTreeMap<String, Stats>,
    cand: &'a std::collections::BTreeMap<String, Stats>,
) -> Vec<(&'a str, &'a Stats, &'a Stats)> {
    let mut keys: std::collections::BTreeSet<&str> = base.keys().map(|s| s.as_str()).collect();
    for k in cand.keys() {
        keys.insert(k.as_str());
    }
    keys.into_iter()
        .map(|k| {
            let b = base.get(k).unwrap_or(EMPTY);
            let c = cand.get(k).unwrap_or(EMPTY);
            (k, b, c)
        })
        .collect()
}

fn pair_rows_topn<'a>(
    base: &'a std::collections::BTreeMap<String, Stats>,
    cand: &'a std::collections::BTreeMap<String, Stats>,
    n: usize,
) -> Vec<(&'a str, &'a Stats, &'a Stats)> {
    let mut all = pair_rows(base, cand);
    all.sort_by(|a, b| b.1.n.cmp(&a.1.n));
    all.into_iter().take(n).collect()
}

const EMPTY: &Stats = &Stats {
    n: 0,
    mae: 0.0,
    rmse: 0.0,
    bias: 0.0,
    p50_abs: 0.0,
    p90_abs: 0.0,
    within_20pct: 0.0,
    within_50pct: 0.0,
};

fn push_compare_table(out: &mut String, rows: &[(&str, &Stats, &Stats)]) {
    out.push_str("| slice | n | MAE base→cand | RMSE base→cand | bias base→cand | P50 base→cand | P90 base→cand | <20% base→cand |\n");
    out.push_str("|---|---:|---|---|---|---|---|---|\n");
    for (k, b, c) in rows {
        if b.n == 0 && c.n == 0 {
            continue;
        }
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} | {} |\n",
            k,
            b.n.max(c.n),
            fmt_pair(b.mae, c.mae, false),
            fmt_pair(b.rmse, c.rmse, false),
            fmt_pair_signed(b.bias, c.bias),
            fmt_pair(b.p50_abs, c.p50_abs, false),
            fmt_pair(b.p90_abs, c.p90_abs, false),
            fmt_pct_pair(b.within_20pct, c.within_20pct),
        ));
    }
}

fn fmt_pair(b: f32, c: f32, _signed: bool) -> String {
    let arrow = arrow_for(b, c, /* lower_is_better = */ true);
    format!("{:.2} → {:.2} {arrow}", b, c)
}

fn fmt_pair_signed(b: f32, c: f32) -> String {
    let arrow = arrow_for(b.abs(), c.abs(), true);
    format!("{:+.2} → {:+.2} {arrow}", b, c)
}

fn fmt_pct_pair(b: f32, c: f32) -> String {
    let arrow = arrow_for(b, c, /* lower_is_better = */ false);
    format!("{:.0}% → {:.0}% {arrow}", b * 100.0, c * 100.0)
}

fn arrow_for(b: f32, c: f32, lower_is_better: bool) -> &'static str {
    let delta = c - b;
    if delta.abs() < 1e-3 {
        ""
    } else if (delta < 0.0) == lower_is_better {
        "✅"
    } else {
        "⚠️"
    }
}
