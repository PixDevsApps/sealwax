//! Nearest-rank percentiles over durations, in milliseconds.

use std::time::Duration;

pub fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

#[derive(Clone, Copy, Debug)]
pub struct Summary {
    pub runs: usize,
    pub p50: f64,
    pub p95: f64,
    pub p99: f64,
    pub max: f64,
}

pub fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    let rank = ((p / 100.0) * sorted.len() as f64).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
}

pub fn summary(samples: &[Duration]) -> Summary {
    let mut values: Vec<f64> = samples.iter().copied().map(ms).collect();
    values.sort_by(f64::total_cmp);
    Summary {
        runs: values.len(),
        p50: percentile(&values, 50.0),
        p95: percentile(&values, 95.0),
        p99: percentile(&values, 99.0),
        max: values.last().copied().unwrap_or(f64::NAN),
    }
}

impl std::fmt::Display for Summary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "p50 {:.2} / p95 {:.2} / p99 {:.2} / max {:.2} ms (n={})",
            self.p50, self.p95, self.p99, self.max, self.runs
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_rank() {
        let values: Vec<f64> = (1..=100).map(f64::from).collect();
        assert_eq!(percentile(&values, 50.0), 50.0);
        assert_eq!(percentile(&values, 95.0), 95.0);
        assert_eq!(percentile(&values, 100.0), 100.0);
        assert_eq!(percentile(&[7.0], 99.0), 7.0);
        let summary = summary(&[Duration::from_millis(3), Duration::from_millis(1)]);
        assert_eq!((summary.p50, summary.max), (1.0, 3.0));
    }
}
