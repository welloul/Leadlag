//! Combined Impulse-OBI strategy engine.
//!
//! Combines trade impulse detection with OBI divergence
//! for high-conviction signals.
//!
//! # Signal Priority
//! - Impulse + OBI confirms → HIGH conviction
//! - Impulse only → MEDIUM conviction
//! - OBI only → MEDIUM conviction

use super::impulse::{ImpulseDetector, ImpulseSignal};
use super::obi_divergence::{ObiDivergenceDetector, ObiSignal};
use crate::eal::{BookUpdate, OrderSide, Symbol, Tick, TradeSignal, VenueId};

/// Signal strength enum
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SignalStrength {
    /// Impulse + OBI confirms direction
    High,
    /// Impulse only or OBI only
    Medium,
}

/// Combined signal from impulse-obi engine
#[derive(Debug, Clone)]
pub struct CombinedSignal {
    /// Direction of the trade
    pub side: OrderSide,
    /// Target venue (the laggard)
    pub target_venue: VenueId,
    /// Symbol to trade
    pub symbol: Symbol,
    /// Signal strength
    pub strength: SignalStrength,
    /// Impulse signal (if any)
    pub impulse: Option<ImpulseSignal>,
    /// OBI signal (if any)
    pub obi: Option<ObiSignal>,
    /// Timestamp when signal was generated
    pub timestamp_ns: u64,
}

/// Impulse-OBI strategy engine
///
/// Combines trade impulse detection with OBI divergence
/// for high-conviction signals.
pub struct ImpulseObiEngine {
    /// Impulse detector
    impulse_detector: ImpulseDetector,
    /// OBI divergence detector
    obi_detector: ObiDivergenceDetector,
    /// Spread filter in bps
    max_spread_bps: f64,
    /// Pending impulse (waiting for OBI confirmation)
    pending_impulse: Option<ImpulseSignal>,
    /// Pending OBI (waiting for impulse confirmation)
    pending_obi: Option<ObiSignal>,
    /// Signal timeout in nanoseconds
    signal_timeout_ns: u64,
    /// Last signal timestamp
    last_signal_ns: u64,
}

impl ImpulseObiEngine {
    /// Create a new impulse-obi engine
    pub fn new(
        impulse_detector: ImpulseDetector,
        obi_detector: ObiDivergenceDetector,
        max_spread_bps: f64,
        signal_timeout_ns: u64,
    ) -> Self {
        Self {
            impulse_detector,
            obi_detector,
            max_spread_bps,
            pending_impulse: None,
            pending_obi: None,
            signal_timeout_ns,
            last_signal_ns: 0,
        }
    }

    /// Process tick (from hot path)
    ///
    /// Returns combined signal if impulse detected and OBI confirms
    pub fn process_tick(&mut self, tick: &Tick) -> Option<CombinedSignal> {
        let timestamp_ns = tick.exchange_ts_ns;

        // Check for timeout
        if timestamp_ns - self.last_signal_ns > self.signal_timeout_ns {
            self.pending_impulse = None;
            self.pending_obi = None;
        }

        // Process tick for impulse detection
        if let Some(impulse) = self.impulse_detector.process_tick(tick) {
            // Check if we have pending OBI that confirms direction
            if let Some(obi) = self.pending_obi.take() {
                if self.direction_matches(&impulse, &obi) {
                    // HIGH conviction: Impulse + OBI confirms
                    self.last_signal_ns = timestamp_ns;
                    return Some(CombinedSignal {
                        side: impulse.side,
                        target_venue: impulse.target_venue,
                        symbol: impulse.symbol.clone(),
                        strength: SignalStrength::High,
                        impulse: Some(impulse),
                        obi: Some(obi),
                        timestamp_ns,
                    });
                }
            }

            // Store pending impulse
            self.pending_impulse = Some(impulse.clone());

            // MEDIUM conviction: Impulse only
            self.last_signal_ns = timestamp_ns;
            return Some(CombinedSignal {
                side: impulse.side,
                target_venue: impulse.target_venue,
                symbol: impulse.symbol.clone(),
                strength: SignalStrength::Medium,
                impulse: Some(impulse),
                obi: None,
                timestamp_ns,
            });
        }

        None
    }

    /// Process book update (from hot path)
    ///
    /// Returns combined signal if OBI detected and impulse confirms
    pub fn process_book(&mut self, book: &BookUpdate) -> Option<CombinedSignal> {
        let timestamp_ns = book.exchange_ts_ns;

        // Check for timeout
        if timestamp_ns - self.last_signal_ns > self.signal_timeout_ns {
            self.pending_impulse = None;
            self.pending_obi = None;
        }

        // Process book for OBI detection
        if let Some(obi) = self.obi_detector.process_book(book) {
            // Check if we have pending impulse that confirms direction
            if let Some(impulse) = self.pending_impulse.take() {
                if self.direction_matches(&impulse, &obi) {
                    // HIGH conviction: OBI + Impulse confirms
                    self.last_signal_ns = timestamp_ns;
                    return Some(CombinedSignal {
                        side: obi.side,
                        target_venue: obi.target_venue,
                        symbol: obi.symbol.clone(),
                        strength: SignalStrength::High,
                        impulse: Some(impulse),
                        obi: Some(obi),
                        timestamp_ns,
                    });
                }
            }

            // Store pending OBI
            self.pending_obi = Some(obi.clone());

            // MEDIUM conviction: OBI only
            self.last_signal_ns = timestamp_ns;
            return Some(CombinedSignal {
                side: obi.side,
                target_venue: obi.target_venue,
                symbol: obi.symbol.clone(),
                strength: SignalStrength::Medium,
                impulse: None,
                obi: Some(obi),
                timestamp_ns,
            });
        }

        None
    }

    /// Check if impulse and OBI directions match
    fn direction_matches(&self, impulse: &ImpulseSignal, obi: &ObiSignal) -> bool {
        // Both should point to same target venue and same side
        impulse.target_venue == obi.target_venue && impulse.side == obi.side
    }

    /// Check spread filter
    pub fn is_spread_acceptable(&self, bid: f64, ask: f64) -> bool {
        if bid <= 0.0 || ask <= 0.0 {
            return false;
        }
        let spread_bps = (ask - bid) / bid * 10000.0;
        spread_bps <= self.max_spread_bps
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eal::{BookLevel, Symbol};

    fn make_tick(venue: VenueId, price: f64, size: f64, ts_ns: u64) -> Tick {
        Tick {
            venue,
            symbol: Symbol::new("BTC"),
            price,
            size,
            exchange_ts_ns: ts_ns,
            local_ts_ns: ts_ns,
        }
    }

    fn make_book(venue: VenueId, bids: Vec<(f64, f64)>, asks: Vec<(f64, f64)>) -> BookUpdate {
        BookUpdate {
            venue,
            symbol: Symbol::new("BTC"),
            bids: bids.into_iter().map(|(p, s)| BookLevel { price: p, size: s }).collect(),
            asks: asks.into_iter().map(|(p, s)| BookLevel { price: p, size: s }).collect(),
            exchange_ts_ns: 0,
            local_ts_ns: 0,
        }
    }

    #[test]
    fn test_impulse_obi_engine_basic() {
        let impulse_detector = ImpulseDetector::new(5_000_000, 5.0, 1.5, 0.001, 10_000_000);
        let obi_detector = ObiDivergenceDetector::new(0.7, 0.2, 5, 0.3);
        let mut engine = ImpulseObiEngine::new(impulse_detector, obi_detector, 10.0, 10_000_000);

        // Initialize trackers
        engine.process_tick(&make_tick(VenueId::EXCHANGE_A, 100.0, 1.0, 0));
        engine.process_tick(&make_tick(VenueId::EXCHANGE_B, 100.0, 1.0, 0));
    }

    #[test]
    fn test_spread_filter() {
        let impulse_detector = ImpulseDetector::new(5_000_000, 5.0, 1.5, 0.001, 10_000_000);
        let obi_detector = ObiDivergenceDetector::new(0.7, 0.2, 5, 0.3);
        let engine = ImpulseObiEngine::new(impulse_detector, obi_detector, 10.0, 10_000_000);

        // Good spread
        assert!(engine.is_spread_acceptable(100.0, 100.01));

        // Bad spread (too wide)
        assert!(!engine.is_spread_acceptable(100.0, 100.2));
    }
}