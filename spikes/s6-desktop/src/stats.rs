//! Nearest-rank percentiles (the convention of the other spikes).

use std::fmt;

#[derive(Clone, Copy, Debug, Default)]
pub struct Summary {
    pub n: usize,
    pub min: f64,
    pub p50: f64,
    pub p95: f64,
    pub max: f64,
    pub mean: f64,
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
        min: sorted[0],
        p50: rank(50.0),
        p95: rank(95.0),
        max: sorted[sorted.len() - 1],
        mean: sorted.iter().sum::<f64>() / sorted.len() as f64,
    }
}

impl fmt::Display for Summary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "n={} min={:.1} p50={:.1} p95={:.1} max={:.1}",
            self.n, self.min, self.p50, self.p95, self.max
        )
    }
}

#[cfg(test)]
mod tests {
    use super::summarize;

    #[test]
    fn nearest_rank() {
        let samples: Vec<f64> = (1..=20).map(f64::from).collect();
        let summary = summarize(&samples);
        assert_eq!(summary.p50, 10.0);
        assert_eq!(summary.p95, 19.0);
        assert_eq!(summary.min, 1.0);
        assert_eq!(summary.max, 20.0);
        assert_eq!(summarize(&[]).n, 0);
    }
}
