//! Nearest-rank percentiles for the measurement reports.

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Summary {
    pub count: usize,
    pub min: f64,
    pub p50: f64,
    pub p95: f64,
    pub p99: f64,
    pub max: f64,
    pub mean: f64,
}

/// The nearest-rank percentile of an ascending slice; `p` is in 0..=100.
pub fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    let rank = ((p / 100.0) * sorted.len() as f64).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
}

pub fn summarize(values: &[f64]) -> Option<Summary> {
    if values.is_empty() {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    Some(Summary {
        count: sorted.len(),
        min: sorted[0],
        p50: percentile(&sorted, 50.0),
        p95: percentile(&sorted, 95.0),
        p99: percentile(&sorted, 99.0),
        max: sorted[sorted.len() - 1],
        mean: sorted.iter().sum::<f64>() / sorted.len() as f64,
    })
}

impl std::fmt::Display for Summary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "n={} min={:.2} p50={:.2} p95={:.2} p99={:.2} max={:.2} mean={:.2}",
            self.count, self.min, self.p50, self.p95, self.p99, self.max, self.mean
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_rank() {
        let values: Vec<f64> = (1..=20).map(f64::from).collect();
        assert_eq!(percentile(&values, 50.0), 10.0);
        assert_eq!(percentile(&values, 95.0), 19.0);
        assert_eq!(percentile(&values, 100.0), 20.0);
        assert_eq!(percentile(&values, 0.0), 1.0);
    }

    #[test]
    fn summary_of_unsorted_values() {
        let summary = summarize(&[3.0, 1.0, 2.0]).unwrap();
        assert_eq!(summary.min, 1.0);
        assert_eq!(summary.max, 3.0);
        assert_eq!(summary.p50, 2.0);
        assert!(summarize(&[]).is_none());
    }
}
