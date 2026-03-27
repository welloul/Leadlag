//! Incremental Pearson cross-correlation for lead-lag detection.
//!
//! Uses running sums for O(1) updates. All calculations are defensive
//! against NaN, Inf, and division-by-zero.

use super::ring_buffer::RingBuffer;

/// Incremental Pearson cross-correlation calculator.
///
/// Maintains running sums for two synchronized price streams and calculates
/// the Pearson correlation coefficient R at multiple time offsets.
///
/// # Algorithm
/// Uses the computational form of Pearson's R:
/// ```text
/// R = (N * Σxy - Σx * Σy) / sqrt([(N * Σx² - (Σx)²] * [N * Σy² - (Σy)²])
/// ```
///
/// # Defensive Math
/// - Adds epsilon to denominator to prevent division by zero
/// - Clamps R to [-1.0, 1.0] range
/// - Returns 0.0 for flat-line inputs (zero variance)
pub struct CrossCorrelator<const N: usize> {
    /// Ring buffer for exchange A prices.
    buf_a: RingBuffer<N>,
    /// Ring buffer for exchange B prices.
    buf_b: RingBuffer<N>,
    /// Running cross-sum: Σ(a_i * b_i)
    sum_ab: f64,
    /// Epsilon for defensive division.
    epsilon: f64,
}

impl<const N: usize> CrossCorrelator<N> {
    /// Create a new cross-correlator.
    pub fn new() -> Self {
        Self {
            buf_a: RingBuffer::new(),
            buf_b: RingBuffer::new(),
            sum_ab: 0.0,
            epsilon: 1e-12,
        }
    }

    /// Push a new price pair (exchange A, exchange B).
    ///
    /// Returns the dropped pair if the buffer was full.
    #[inline(always)]
    pub fn push(&mut self, price_a: f64, price_b: f64) -> Option<(f64, f64)> {
        let dropped_a = self.buf_a.push(price_a);
        let dropped_b = self.buf_b.push(price_b);

        // Update cross-sum
        self.sum_ab += price_a * price_b;
        if let (Some(old_a), Some(old_b)) = (dropped_a, dropped_b) {
            self.sum_ab -= old_a * old_b;
        }

        match (dropped_a, dropped_b) {
            (Some(a), Some(b)) => Some((a, b)),
            _ => None,
        }
    }

    /// Calculate the Pearson correlation coefficient R.
    ///
    /// Returns a value in [-1.0, 1.0].
    /// Returns 0.0 if:
    /// - Buffer has fewer than 2 elements
    /// - Either stream has zero variance (flat line)
    /// - Calculation produces NaN or Inf
    #[inline(always)]
    pub fn correlation(&self) -> f64 {
        let n = self.buf_a.len() as f64;
        if n < 2.0 {
            return 0.0;
        }

        let sum_x = self.buf_a.sum();
        let sum_y = self.buf_b.sum();
        let sum_x2 = self.buf_a.sum_sq();
        let sum_y2 = self.buf_b.sum_sq();

        // Numerator: N * Σxy - Σx * Σy
        let numerator = (n * self.sum_ab) - (sum_x * sum_y);

        // Denominator: sqrt([N * Σx² - (Σx)²] * [N * Σy² - (Σy)²])
        let var_x = (n * sum_x2) - (sum_x * sum_x);
        let var_y = (n * sum_y2) - (sum_y * sum_y);

        // Check for zero variance in either stream
        if var_x < self.epsilon || var_y < self.epsilon {
            return 0.0;
        }

        let denominator = (var_x * var_y).sqrt();
        let r = numerator / denominator;

        // Clamp to valid range and guard against NaN/Inf
        if r.is_finite() {
            r.clamp(-1.0, 1.0)
        } else {
            0.0
        }
    }

    /// Calculate correlation at a specific lag offset.
    ///
    /// Positive lag means exchange B is lagging behind A.
    /// Negative lag means exchange A is lagging behind B.
    ///
    /// This is done by shifting the B buffer index by `lag` positions.
    #[inline(always)]
    pub fn correlation_at_lag(&self, lag: i32) -> f64 {
        if self.buf_a.len() < 2 || self.buf_b.len() < 2 {
            return 0.0;
        }

        let n = self.buf_a.len().min(self.buf_b.len());
        if n < 2 {
            return 0.0;
        }

        // Calculate cross-sum at the given lag
        let mut sum_xy = 0.0;
        let mut count = 0;

        for i in 0..n {
            let idx_a = i;
            let idx_b = (i as i32 - lag) as usize;

            if idx_b < n {
                if let (Some(a), Some(b)) = (self.buf_a.get(idx_a), self.buf_b.get(idx_b)) {
                    sum_xy += a * b;
                    count += 1;
                }
            }
        }

        if count < 2 {
            return 0.0;
        }

        let n = count as f64;
        let sum_x = self.buf_a.sum();
        let sum_y = self.buf_b.sum();
        let sum_x2 = self.buf_a.sum_sq();
        let sum_y2 = self.buf_b.sum_sq();

        let numerator = (n * sum_xy) - (sum_x * sum_y);
        let var_x = (n * sum_x2) - (sum_x * sum_x);
        let var_y = (n * sum_y2) - (sum_y * sum_y);
        let denominator = (var_x * var_y + self.epsilon).sqrt();

        if denominator < self.epsilon {
            return 0.0;
        }

        let r = numerator / denominator;
        if r.is_finite() {
            r.clamp(-1.0, 1.0)
        } else {
            0.0
        }
    }

    /// Find the lag offset that maximizes correlation.
    ///
    /// Searches from `min_lag` to `max_lag` (inclusive).
    /// Returns (best_lag, best_correlation).
    pub fn find_best_lag(&self, min_lag: i32, max_lag: i32) -> (i32, f64) {
        let mut best_lag = 0;
        let mut best_r = 0.0;

        for lag in min_lag..=max_lag {
            let r = self.correlation_at_lag(lag).abs();
            if r > best_r {
                best_r = r;
                best_lag = lag;
            }
        }

        (best_lag, best_r)
    }

    /// Get the number of samples in the buffer.
    #[inline(always)]
    pub fn len(&self) -> usize {
        self.buf_a.len().min(self.buf_b.len())
    }

    /// Check if the buffer has enough samples for correlation.
    #[inline(always)]
    pub fn is_ready(&self) -> bool {
        self.len() >= 2
    }

    /// Reset the correlator.
    pub fn clear(&mut self) {
        self.buf_a.clear();
        self.buf_b.clear();
        self.sum_ab = 0.0;
    }
}

impl<const N: usize> Default for CrossCorrelator<N> {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_perfect_positive_correlation() {
        let mut corr = CrossCorrelator::<16>::new();

        // Identical streams should have R = 1.0
        for i in 0..10 {
            let val = i as f64;
            corr.push(val, val);
        }

        let r = corr.correlation();
        assert!((r - 1.0).abs() < 1e-6, "Expected R ≈ 1.0, got {r}");
    }

    #[test]
    fn test_perfect_negative_correlation() {
        let mut corr = CrossCorrelator::<16>::new();

        // Opposite streams should have R = -1.0
        for i in 0..10 {
            let val = i as f64;
            corr.push(val, -val);
        }

        let r = corr.correlation();
        assert!((r - (-1.0)).abs() < 1e-6, "Expected R ≈ -1.0, got {r}");
    }

    #[test]
    fn test_no_correlation() {
        let mut corr = CrossCorrelator::<16>::new();

        // Constant vs varying should have R ≈ 0
        for i in 0..10 {
            corr.push(1.0, i as f64);
        }

        let r = corr.correlation();
        assert!(r.abs() < 0.1, "Expected R ≈ 0, got {r}");
    }

    #[test]
    fn test_flat_line_returns_zero() {
        let mut corr = CrossCorrelator::<16>::new();

        // Both streams flat should return 0.0 (no variance)
        for _ in 0..10 {
            corr.push(5.0, 5.0);
        }

        let r = corr.correlation();
        assert_eq!(r, 0.0, "Flat line should return 0.0");
    }

    #[test]
    fn test_insufficient_data_returns_zero() {
        let corr = CrossCorrelator::<16>::new();
        assert_eq!(corr.correlation(), 0.0);

        let mut corr = CrossCorrelator::<16>::new();
        corr.push(1.0, 2.0);
        assert_eq!(corr.correlation(), 0.0); // Only 1 sample
    }

    #[test]
    fn test_correlation_bounds() {
        let mut corr = CrossCorrelator::<16>::new();

        // Random-ish values should still be in [-1, 1]
        let values_a = [1.0, 5.0, 3.0, 8.0, 2.0, 7.0, 4.0, 6.0];
        let values_b = [2.0, 4.0, 3.0, 7.0, 1.0, 6.0, 5.0, 8.0];

        for (a, b) in values_a.iter().zip(values_b.iter()) {
            corr.push(*a, *b);
        }

        let r = corr.correlation();
        assert!(r >= -1.0 && r <= 1.0, "R out of bounds: {r}");
    }

    #[test]
    fn test_find_best_lag() {
        let mut corr = CrossCorrelator::<32>::new();

        // Create a lagged relationship: B follows A with lag of 2
        for i in 0..20 {
            let a = (i as f64).sin();
            let b = ((i as f64) - 2.0).sin(); // B lags A by 2
            corr.push(a, b);
        }

        let (best_lag, best_r) = corr.find_best_lag(-5, 5);
        assert!(best_r > 0.8, "Expected high correlation, got {best_r}");
        // The best lag should be close to 2 (B lagging A)
        assert!(best_lag >= 1 && best_lag <= 3, "Expected lag around 2, got {best_lag}");
    }
}