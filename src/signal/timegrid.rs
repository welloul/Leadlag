//! Time-grid alignment for synchronizing asynchronous exchange feeds.
//!
//! Uses Forward-Fill (Last Observation Carried Forward) to map incoming ticks
//! onto a synchronized, high-resolution time grid.

use crate::eal::Tick;

/// Time-grid aligner for synchronizing two exchange feeds.
///
/// Maps asynchronous ticks onto a common time grid using forward-fill.
/// The grid is defined by a fixed precision (e.g., 5ms bins).
pub struct TimeGrid {
    /// Grid precision in nanoseconds.
    pub precision_ns: u64,
    /// Last known price for exchange A.
    last_price_a: Option<f64>,
    /// Last known timestamp for exchange A (in grid units).
    last_grid_a: Option<u64>,
    /// Last known price for exchange B.
    last_price_b: Option<f64>,
    /// Last known timestamp for exchange B (in grid units).
    last_grid_b: Option<u64>,
    /// Current grid timestamp (in grid units).
    current_grid: u64,
}

/// Aligned price pair on the time grid.
#[derive(Debug, Clone, Copy)]
pub struct AlignedPair {
    /// Grid timestamp in nanoseconds.
    pub timestamp_ns: u64,
    /// Exchange A price (forward-filled if no new tick).
    pub price_a: f64,
    /// Exchange B price (forward-filled if no new tick).
    pub price_b: f64,
    /// Whether exchange A had a new tick in this bin.
    pub a_updated: bool,
    /// Whether exchange B had a new tick in this bin.
    pub b_updated: bool,
}

impl TimeGrid {
    /// Create a new time-grid aligner.
    ///
    /// # Arguments
    /// * `precision_ns` - Grid bin size in nanoseconds (e.g., 5_000_000 for 5ms)
    pub fn new(precision_ns: u64) -> Self {
        assert!(precision_ns > 0, "Precision must be > 0");

        Self {
            precision_ns,
            last_price_a: None,
            last_grid_a: None,
            last_price_b: None,
            last_grid_b: None,
            current_grid: 0,
        }
    }

    /// Convert a nanosecond timestamp to grid units.
    #[inline(always)]
    fn to_grid(&self, ts_ns: u64) -> u64 {
        ts_ns / self.precision_ns
    }

    /// Convert grid units back to nanoseconds.
    #[inline(always)]
    fn to_ns(&self, grid: u64) -> u64 {
        grid * self.precision_ns
    }

    /// Ingest a tick from an exchange.
    ///
    /// Returns aligned pairs for all grid bins between the last tick and this one.
    /// Uses forward-fill for the exchange that didn't have a new tick.
    pub fn ingest_tick(&mut self, tick: &Tick) -> Vec<AlignedPair> {
        let grid_ts = self.to_grid(tick.exchange_ts_ns);
        let mut pairs = Vec::new();

        // Determine which exchange this tick is from
        let is_exchange_a = tick.venue == crate::eal::VenueId::EXCHANGE_A;

        if is_exchange_a {
            self.last_price_a = Some(tick.price);
            self.last_grid_a = Some(grid_ts);
        } else {
            self.last_price_b = Some(tick.price);
            self.last_grid_b = Some(grid_ts);
        }

        // Generate aligned pairs for all grid bins between current and new tick
        if let (Some(price_a), Some(price_b)) = (self.last_price_a, self.last_price_b) {
            let start_grid = self.current_grid;
            let end_grid = grid_ts;

            for grid in start_grid..=end_grid {
                let a_updated = self.last_grid_a.map_or(false, |g| g == grid);
                let b_updated = self.last_grid_b.map_or(false, |g| g == grid);

                pairs.push(AlignedPair {
                    timestamp_ns: self.to_ns(grid),
                    price_a,
                    price_b,
                    a_updated,
                    b_updated,
                });
            }

            self.current_grid = end_grid + 1;
        }

        pairs
    }

    /// Get the current aligned pair (without ingesting a new tick).
    pub fn current_pair(&self) -> Option<AlignedPair> {
        match (self.last_price_a, self.last_price_b) {
            (Some(price_a), Some(price_b)) => Some(AlignedPair {
                timestamp_ns: self.to_ns(self.current_grid),
                price_a,
                price_b,
                a_updated: false,
                b_updated: false,
            }),
            _ => None,
        }
    }

    /// Reset the time grid.
    pub fn clear(&mut self) {
        self.last_price_a = None;
        self.last_grid_a = None;
        self.last_price_b = None;
        self.last_grid_b = None;
        self.current_grid = 0;
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eal::{Symbol, VenueId};

    fn make_tick(venue: VenueId, price: f64, ts_ns: u64) -> Tick {
        Tick {
            venue,
            symbol: Symbol::new("BTC"),
            price,
            size: 1.0,
            exchange_ts_ns: ts_ns,
            local_ts_ns: ts_ns,
        }
    }

    #[test]
    fn test_basic_alignment() {
        let mut grid = TimeGrid::new(5_000_000); // 5ms grid

        // First tick from exchange A
        let pairs = grid.ingest_tick(&make_tick(VenueId::EXCHANGE_A, 60000.0, 0));
        assert!(pairs.is_empty()); // No B price yet

        // First tick from exchange B
        let pairs = grid.ingest_tick(&make_tick(VenueId::EXCHANGE_B, 60001.0, 2_000_000));
        assert!(!pairs.is_empty());

        // Should have aligned pair
        let pair = &pairs[0];
        assert_eq!(pair.price_a, 60000.0);
        assert_eq!(pair.price_b, 60001.0);
    }

    #[test]
    fn test_forward_fill() {
        let mut grid = TimeGrid::new(5_000_000);

        // A ticks at 0ms
        grid.ingest_tick(&make_tick(VenueId::EXCHANGE_A, 60000.0, 0));

        // B ticks at 0ms
        let pairs = grid.ingest_tick(&make_tick(VenueId::EXCHANGE_B, 60001.0, 0));
        assert_eq!(pairs.len(), 1);

        // A ticks at 10ms (B hasn't ticked since 0ms)
        let pairs = grid.ingest_tick(&make_tick(VenueId::EXCHANGE_A, 60002.0, 10_000_000));

        // Should have pairs for grid bins 1 and 2 (5ms and 10ms)
        // Both should use B's last price (60001.0) via forward-fill
        for pair in &pairs {
            assert_eq!(pair.price_b, 60001.0);
        }
    }

    #[test]
    fn test_grid_precision() {
        let mut grid = TimeGrid::new(10_000_000); // 10ms grid

        grid.ingest_tick(&make_tick(VenueId::EXCHANGE_A, 100.0, 0));
        grid.ingest_tick(&make_tick(VenueId::EXCHANGE_B, 200.0, 0));

        // Tick at 15ms should be in grid bin 1 (10ms-20ms)
        let pairs = grid.ingest_tick(&make_tick(VenueId::EXCHANGE_A, 101.0, 15_000_000));
        assert!(!pairs.is_empty());
    }
}