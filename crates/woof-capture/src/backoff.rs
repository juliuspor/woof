use std::time::Duration;

/// Deterministic capped exponential failure backoff.
#[derive(Clone, Debug)]
pub struct ExponentialBackoff {
    failures: u32,
    base: Duration,
    maximum: Duration,
}

impl Default for ExponentialBackoff {
    fn default() -> Self {
        Self::new(Duration::from_millis(250), Duration::from_secs(30))
    }
}

impl ExponentialBackoff {
    pub fn new(base: Duration, maximum: Duration) -> Self {
        Self {
            failures: 0,
            base,
            maximum: maximum.max(base),
        }
    }

    pub fn record_failure(&mut self) -> Duration {
        self.failures = self.failures.saturating_add(1);
        let exponent = self.failures.saturating_sub(1).min(31);
        self.base
            .saturating_mul(1_u32 << exponent)
            .min(self.maximum)
    }

    pub fn record_success(&mut self) {
        self.failures = 0;
    }

    pub fn failures(&self) -> u32 {
        self.failures
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doubles_then_caps_and_resets() {
        let mut backoff =
            ExponentialBackoff::new(Duration::from_millis(100), Duration::from_millis(450));
        assert_eq!(backoff.record_failure(), Duration::from_millis(100));
        assert_eq!(backoff.record_failure(), Duration::from_millis(200));
        assert_eq!(backoff.record_failure(), Duration::from_millis(400));
        assert_eq!(backoff.record_failure(), Duration::from_millis(450));
        backoff.record_success();
        assert_eq!(backoff.failures(), 0);
        assert_eq!(backoff.record_failure(), Duration::from_millis(100));
    }
}
