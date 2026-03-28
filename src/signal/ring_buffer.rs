//! Zero-allocation circular buffer for the hot path.
//!
//! Uses power-of-two sizing for bitwise mask optimization.
//! All operations are O(1) with no heap allocation after initialization.

/// A fixed-size circular buffer optimized for the hot path.
///
/// # Invariants
/// - Size must be a power of 2 (enforced at compile time via const generics)
/// - No heap allocation after `new()`
/// - All operations are O(1)
/// - Uses bitwise AND instead of modulo for index wrapping
///
/// # Example
/// ```
/// use tokioparasite::signal::ring_buffer::RingBuffer;
///
/// let mut buf = RingBuffer::<256>::new();
/// buf.push(1.0);
/// buf.push(2.0);
/// assert_eq!(buf.len(), 2);
/// ```
pub struct RingBuffer<const N: usize> {
    /// Pre-allocated data array.
    data: [f64; N],
    /// Current head position (next write position).
    head: usize,
    /// Number of valid elements (up to N).
    len: usize,
    /// Bitwise mask for index wrapping (N - 1).
    mask: usize,
    /// Running sum for O(1) mean calculation.
    sum: f64,
    /// Running sum of squares for O(1) variance calculation.
    sum_sq: f64,
}

impl<const N: usize> RingBuffer<N> {
    /// Create a new ring buffer.
    ///
    /// # Panics
    /// Panics if N is not a power of 2.
    #[inline(always)]
    pub fn new() -> Self {
        assert!(
            N.is_power_of_two(),
            "RingBuffer size must be a power of 2, got {N}"
        );
        assert!(N > 0, "RingBuffer size must be > 0");

        Self {
            data: [0.0; N],
            head: 0,
            len: 0,
            mask: N - 1,
            sum: 0.0,
            sum_sq: 0.0,
        }
    }

    /// Push a value into the buffer.
    ///
    /// If the buffer is full, the oldest value is overwritten.
    /// Returns the value that was dropped (if any).
    #[inline(always)]
    pub fn push(&mut self, val: f64) -> Option<f64> {
        let old_val = self.data[self.head];

        if self.len < N {
            // Buffer not yet full, just add
            self.sum += val;
            self.sum_sq += val * val;
            self.len += 1;
        } else {
            // Buffer full, replace oldest
            self.sum += val - old_val;
            self.sum_sq += (val * val) - (old_val * old_val);
        }

        self.data[self.head] = val;
        self.head = (self.head + 1) & self.mask;

        if self.len == N {
            Some(old_val)
        } else {
            None
        }
    }

    /// Get the value at the given logical index (0 = oldest).
    #[inline(always)]
    pub fn get(&self, index: usize) -> Option<f64> {
        if index >= self.len {
            return None;
        }
        let actual_index = if self.len < N {
            index
        } else {
            (self.head + index) & self.mask
        };
        Some(self.data[actual_index])
    }

    /// Get the most recently pushed value.
    #[inline(always)]
    pub fn latest(&self) -> Option<f64> {
        if self.len == 0 {
            return None;
        }
        let prev = (self.head + N - 1) & self.mask;
        Some(self.data[prev])
    }

    /// Get the number of valid elements in the buffer.
    #[inline(always)]
    pub fn len(&self) -> usize {
        self.len
    }

    /// Check if the buffer is empty.
    #[inline(always)]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Check if the buffer is full.
    #[inline(always)]
    pub fn is_full(&self) -> bool {
        self.len == N
    }

    /// Get the capacity of the buffer.
    #[inline(always)]
    pub fn capacity(&self) -> usize {
        N
    }

    /// Calculate the mean of all values in the buffer.
    ///
    /// Returns 0.0 if the buffer is empty.
    #[inline(always)]
    pub fn mean(&self) -> f64 {
        if self.len == 0 {
            0.0
        } else {
            self.sum / (self.len as f64)
        }
    }

    /// Calculate the variance of all values in the buffer.
    ///
    /// Returns 0.0 if the buffer has fewer than 2 elements.
    #[inline(always)]
    pub fn variance(&self) -> f64 {
        if self.len < 2 {
            return 0.0;
        }
        let n = self.len as f64;
        let mean = self.sum / n;
        (self.sum_sq / n) - (mean * mean)
    }

    /// Get the running sum (for incremental correlation calculation).
    #[inline(always)]
    pub fn sum(&self) -> f64 {
        self.sum
    }

    /// Get the running sum of squares (for incremental correlation calculation).
    #[inline(always)]
    pub fn sum_sq(&self) -> f64 {
        self.sum_sq
    }

    /// Reset the buffer to empty state.
    pub fn clear(&mut self) {
        self.head = 0;
        self.len = 0;
        self.sum = 0.0;
        self.sum_sq = 0.0;
        // Don't zero the array, it's not necessary
    }

    /// Recalculate sums from scratch (for recovery from floating-point drift).
    pub fn recalculate(&mut self) {
        self.sum = 0.0;
        self.sum_sq = 0.0;
        for i in 0..self.len {
            let idx = if self.len < N {
                i
            } else {
                (self.head + i) & self.mask
            };
            let val = self.data[idx];
            self.sum += val;
            self.sum_sq += val * val;
        }
    }
}

impl<const N: usize> Default for RingBuffer<N> {
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
    fn test_basic_push_and_get() {
        let mut buf = RingBuffer::<4>::new();
        assert!(buf.is_empty());

        buf.push(1.0);
        buf.push(2.0);
        buf.push(3.0);

        assert_eq!(buf.len(), 3);
        assert_eq!(buf.get(0), Some(1.0));
        assert_eq!(buf.get(1), Some(2.0));
        assert_eq!(buf.get(2), Some(3.0));
        assert_eq!(buf.get(3), None);
    }

    #[test]
    fn test_wraparound() {
        let mut buf = RingBuffer::<4>::new();

        buf.push(1.0);
        buf.push(2.0);
        buf.push(3.0);
        buf.push(4.0);
        assert!(buf.is_full());

        // Push 5.0, should overwrite 1.0
        let dropped = buf.push(5.0);
        assert_eq!(dropped, Some(1.0));

        // Buffer should now contain [2.0, 3.0, 4.0, 5.0]
        assert_eq!(buf.get(0), Some(2.0));
        assert_eq!(buf.get(1), Some(3.0));
        assert_eq!(buf.get(2), Some(4.0));
        assert_eq!(buf.get(3), Some(5.0));
    }

    #[test]
    fn test_mean_and_variance() {
        let mut buf = RingBuffer::<4>::new();

        buf.push(1.0);
        buf.push(2.0);
        buf.push(3.0);
        buf.push(4.0);

        assert_eq!(buf.mean(), 2.5);
        // Variance of [1,2,3,4] = 1.25
        assert!((buf.variance() - 1.25).abs() < 1e-9);
    }

    #[test]
    fn test_sums_after_wraparound() {
        let mut buf = RingBuffer::<4>::new();

        buf.push(1.0);
        buf.push(2.0);
        buf.push(3.0);
        buf.push(4.0);
        buf.push(5.0); // Overwrites 1.0

        // Buffer is [2, 3, 4, 5]
        // sum = 14, sum_sq = 4+9+16+25 = 54
        assert!((buf.sum() - 14.0).abs() < 1e-9);
        assert!((buf.sum_sq() - 54.0).abs() < 1e-9);
    }

    #[test]
    fn test_recalculate() {
        let mut buf = RingBuffer::<4>::new();

        buf.push(1.0);
        buf.push(2.0);
        buf.push(3.0);
        buf.push(4.0);
        buf.push(5.0);

        // Corrupt sums intentionally
        buf.sum = 999.0;
        buf.sum_sq = 999.0;

        // Recalculate should fix them
        buf.recalculate();
        assert!((buf.sum() - 14.0).abs() < 1e-9);
        assert!((buf.sum_sq() - 54.0).abs() < 1e-9);
    }

    #[test]
    fn test_latest() {
        let mut buf = RingBuffer::<4>::new();
        assert_eq!(buf.latest(), None);

        buf.push(1.0);
        assert_eq!(buf.latest(), Some(1.0));

        buf.push(2.0);
        assert_eq!(buf.latest(), Some(2.0));
    }

    #[test]
    fn test_clear() {
        let mut buf = RingBuffer::<4>::new();
        buf.push(1.0);
        buf.push(2.0);

        buf.clear();
        assert!(buf.is_empty());
        assert_eq!(buf.sum(), 0.0);
        assert_eq!(buf.sum_sq(), 0.0);
    }

    #[test]
    #[should_panic(expected = "power of 2")]
    fn test_non_power_of_two_panics() {
        let _ = RingBuffer::<3>::new();
    }
}
