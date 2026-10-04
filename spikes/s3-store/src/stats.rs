use std::fmt;

/// Nearest-rank percentiles over a set of samples (milliseconds unless stated).
#[derive(Clone, Copy, Debug, Default)]
pub struct Summary {
    pub n: usize,
    pub mean: f64,
    pub p50: f64,
    pub p95: f64,
    pub p99: f64,
    pub max: f64,
}

pub fn summarize(samples: &[f64]) -> Summary {
    if samples.is_empty() {
        return Summary::default();
    }
    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    let rank = |p: f64| {
        let index = ((p / 100.0) * sorted.len() as f64).ceil() as usize;
        sorted[index.clamp(1, sorted.len()) - 1]
    };
    Summary {
        n: sorted.len(),
        mean: sorted.iter().sum::<f64>() / sorted.len() as f64,
        p50: rank(50.0),
        p95: rank(95.0),
        p99: rank(99.0),
        max: sorted[sorted.len() - 1],
    }
}

impl fmt::Display for Summary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "n={} mean={:.2} p50={:.2} p95={:.2} p99={:.2} max={:.2}",
            self.n, self.mean, self.p50, self.p95, self.p99, self.max
        )
    }
}

pub fn ms(duration: std::time::Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

#[cfg(test)]
mod tests {
    use super::summarize;

    #[test]
    fn nearest_rank() {
        let samples: Vec<f64> = (1..=100).map(f64::from).collect();
        let summary = summarize(&samples);
        assert_eq!(summary.p50, 50.0);
        assert_eq!(summary.p95, 95.0);
        assert_eq!(summary.p99, 99.0);
        assert_eq!(summary.max, 100.0);
        assert_eq!(summarize(&[3.0]).p99, 3.0);
    }
}
