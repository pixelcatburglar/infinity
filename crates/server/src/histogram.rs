//! Fixed-bucket duration histogram shared by server-side diagnostics.
//! Bucket bounds are inclusive; the last bucket catches everything above.
//! Percentiles are bucket estimates; `max` keeps the true maximum.

use std::time::Duration;

const PERCENT: u64 = 100;
/// Inclusive upper bound of each wait/hold bucket; the last bucket catches
/// everything above the final bound.
const BUCKET_BOUNDS: [Duration; 25] = [
    Duration::ZERO,
    Duration::from_micros(1),
    Duration::from_micros(2),
    Duration::from_micros(5),
    Duration::from_micros(10),
    Duration::from_micros(20),
    Duration::from_micros(50),
    Duration::from_micros(100),
    Duration::from_micros(200),
    Duration::from_micros(500),
    Duration::from_millis(1),
    Duration::from_millis(2),
    Duration::from_millis(5),
    Duration::from_millis(10),
    Duration::from_millis(20),
    Duration::from_millis(50),
    Duration::from_millis(100),
    Duration::from_millis(200),
    Duration::from_millis(500),
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(5),
    Duration::from_secs(10),
    Duration::from_secs(30),
    Duration::from_secs(60),
];

pub(crate) struct Histogram {
    buckets: [u64; BUCKET_BOUNDS.len() + 1],
    pub(crate) max: Duration,
}

impl Default for Histogram {
    fn default() -> Self {
        Histogram { buckets: [0; BUCKET_BOUNDS.len() + 1], max: Duration::ZERO }
    }
}

impl Histogram {
    pub(crate) fn record(&mut self, value: Duration) {
        let index = BUCKET_BOUNDS.iter().position(|bound| value <= *bound)
            .unwrap_or(BUCKET_BOUNDS.len());
        self.buckets[index] += 1;
        self.max = self.max.max(value);
    }

    /// Bucket estimate of the `percentile` percentile over `count` samples, or
    /// zero for an empty distribution.
    pub(crate) fn percentile(&self, count: u64, percentile: u64) -> Duration {
        if count == 0 {
            return Duration::ZERO;
        }
        let rank = (u128::from(count) * u128::from(percentile))
            .div_ceil(u128::from(PERCENT)) as u64;
        let mut cumulative = 0u64;
        for (index, &bucket) in self.buckets.iter().enumerate() {
            cumulative += bucket;
            if cumulative >= rank {
                return if index < BUCKET_BOUNDS.len() {
                    BUCKET_BOUNDS[index].min(self.max)
                } else {
                    self.max
                };
            }
        }
        self.max
    }

    /// Fold another histogram's buckets and maximum into this one.
    pub(crate) fn merge(&mut self, other: &Self) {
        for (slot, count) in self.buckets.iter_mut().zip(other.buckets) {
            *slot += count;
        }
        self.max = self.max.max(other.max);
    }
}
