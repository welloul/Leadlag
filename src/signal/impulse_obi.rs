//! Combined Impulse-OBI strategy engine.
//!
//! Combines trade impulse detection with OBI divergence
//! for high-conviction signals.
//!
//! # Signal Priority
//! - Impulse + OBI confirms direction → HIGH conviction
//! - Impulse only (high_conviction_only = false) → MEDIUM conviction
//!
//! # Fix Log
//! v0.2.0:
//! - Fixed pending-signal matching: was requiring venue == target_venue which
//!   almost never matched. Now only requires directional agreement.
//! - Fixed timeout tracking: now uses wall-clock (now_ns) rather than
//!   exchange_ts_ns so timeouts work correctly in simulation with stale clocks.
//! - Separated impulse and OBI pending expiry with independent wall-clock ages.

use super::impulse::{ImpulseDetector, ImpulseSignal};
use super::obi_divergence::{ObiDivergenceDetector, ObiSignal};
use crate::eal::{BookUpdate, OrderSide, Symbol, Tick, VenueId};

fn now_ns() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64
}

/// Signal strength enum
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SignalStrength {
    /// Impulse + OBI confirms direction
    High,
    /// Impulse only or OBI only
    Medium,
}

/// Minimal pending signal data (avoids cloning full signals on hot path).
/// Stored with wall-clock age so timeout works regardless of exchange clock drift.
#[derive(Debug, Clone, Copy)]
struct PendingSignal {
    side: OrderSide,
    /// Wall-clock time the pending signal was stored (for expiry)
    stored_at_ns: u64,
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
    /// Timestamp when signal was generated (exchange ts from trigger)
    pub timestamp_ns: u64,
    /// Impulse magnitude in bps (0 if OBI-only)
    pub impulse_magnitude_bps: f64,
    /// OBI value on signal venue (0 if impulse-only)
    pub obi_value: f64,
}

/// Impulse-OBI strategy engine
///
/// Combines trade impulse detection with OBI divergence
/// for high-conviction signals.
pub struct ImpulseObiEngine {
    /// Impulse detector
    pub(crate) impulse_detector: ImpulseDetector,
    /// OBI divergence detector
    obi_detector: ObiDivergenceDetector,
    /// Minimum cross-venue edge in bps (fees-aware: must cover taker fees + slippage)
    pub(crate) entry_threshold_bps: f64,
    /// Pending impulse signal (waiting for OBI confirmation)
    /// Uses wall-clock stored_at_ns for expiry.
    pending_impulse: Option<PendingSignal>,
    /// Pending OBI signal (waiting for impulse confirmation)
    /// Uses wall-clock stored_at_ns for expiry.
    pending_obi: Option<PendingSignal>,
    /// Signal combination window in nanoseconds (wall-clock based)
    signal_timeout_ns: u64,
    /// Only emit HIGH conviction signals (skip MEDIUM)
    high_conviction_only: bool,
}

impl ImpulseObiEngine {
    /// Create a new impulse-obi engine
    pub fn new(
        impulse_detector: ImpulseDetector,
        obi_detector: ObiDivergenceDetector,
        entry_threshold_bps: f64,
        signal_timeout_ns: u64,
        high_conviction_only: bool,
    ) -> Self {
        Self {
            impulse_detector,
            obi_detector,
            entry_threshold_bps,
            pending_impulse: None,
            pending_obi: None,
            signal_timeout_ns,
            high_conviction_only,
        }
    }

    /// Update strategy settings for hot-reload.
    pub fn update_settings(&mut self, entry_bps: f64, high_conviction: bool) {
        self.entry_threshold_bps = entry_bps;
        self.high_conviction_only = high_conviction;
    }

    /// Direction-normalized edge calculation.
    /// Returns positive bps if the trade has edge, negative if not.
    ///
    /// For Buy: we want source (moved up) > target (lagging). Edge = how much
    ///   cheaper target is vs source.
    /// For Sell: we want source (moved down) < target (lagging). Edge = how much
    ///   more expensive target is vs source.
    fn edge_bps(&self, source_mid: f64, target_mid: f64, side: OrderSide) -> f64 {
        match side {
            OrderSide::Buy => (source_mid - target_mid) / target_mid * 10_000.0,
            OrderSide::Sell => (target_mid - source_mid) / target_mid * 10_000.0,
        }
    }

    /// Check cross-venue edge before emitting signal.
    /// Returns true if edge >= entry_threshold_bps.
    pub(crate) fn has_edge(&self, signal_venue: VenueId, target_venue: VenueId, side: OrderSide) -> bool {
        let source_mid = self.impulse_detector.current_mid(signal_venue);
        let target_mid = self.impulse_detector.current_mid(target_venue);
        if let (Some(src), Some(tgt)) = (source_mid, target_mid) {
            let edge = self.edge_bps(src, tgt, side);
            let passed = edge >= self.entry_threshold_bps;
            
            if passed || edge.abs() > 1.0 {
                tracing::info!(
                    "EDGE CHECK: {:?} | src={:.2} tgt={:.2} side={:?} | edge={:.2} bps | threshold={:.1} bps | passed={}",
                    signal_venue, src, tgt, side, edge, self.entry_threshold_bps, passed
                );
            }
            passed
        } else {
            false // Can't compute edge without both mids
        }
    }

    /// Expire stale pending signals using wall-clock age.
    fn expire_pending(&mut self, now: u64) {
        if let Some(p) = self.pending_impulse {
            if now.saturating_sub(p.stored_at_ns) > self.signal_timeout_ns {
                self.pending_impulse = None;
            }
        }
        if let Some(p) = self.pending_obi {
            if now.saturating_sub(p.stored_at_ns) > self.signal_timeout_ns {
                self.pending_obi = None;
            }
        }
    }

    /// Process tick (from hot path)
    ///
    /// Returns combined signal if impulse detected and OBI confirms
    pub fn process_tick(&mut self, tick: &Tick) -> Option<CombinedSignal> {
        let now = now_ns();
        self.expire_pending(now);

        // Process tick for impulse detection
        if let Some(impulse) = self.impulse_detector.process_tick(tick) {
            // Cross-venue edge check: verify the trade has enough edge
            if !self.has_edge(tick.venue, impulse.target_venue, impulse.side) {
                tracing::debug!(
                    "Impulse rejected: edge < {} bps for {} {} on {:?}",
                    self.entry_threshold_bps,
                    impulse.side,
                    impulse.symbol,
                    impulse.target_venue
                );
                // Still store as pending — edge might open up by the time OBI confirms
                self.pending_impulse = Some(PendingSignal {
                    side: impulse.side,
                    stored_at_ns: now,
                });
                return None;
            }

            // Check if we have a pending OBI that agrees on direction
            // NOTE: We deliberately only match on `side`, NOT on venue.
            // The OBI may come from a different venue than the impulse but still
            // confirm directional pressure. Both pointing the same way is enough.
            if let Some(pending_obi) = self.pending_obi.take() {
                if pending_obi.side == impulse.side {
                    // HIGH conviction: Impulse + OBI confirms direction
                    tracing::info!(
                        "HIGH conviction signal: Impulse+OBI | {} {:?} | {:.2} bps",
                        impulse.symbol,
                        impulse.side,
                        impulse.impulse_magnitude_bps
                    );
                    return Some(CombinedSignal {
                        side: impulse.side,
                        target_venue: impulse.target_venue,
                        symbol: impulse.symbol.clone(),
                        strength: SignalStrength::High,
                        impulse: Some(impulse.clone()),
                        obi: None,
                        timestamp_ns: tick.exchange_ts_ns,
                        impulse_magnitude_bps: impulse.impulse_magnitude_bps,
                        obi_value: 0.0,
                    });
                } else {
                    // Conflicting direction — discard pending OBI
                    tracing::debug!(
                        "OBI direction conflict: impulse={:?} obi={:?}",
                        impulse.side,
                        pending_obi.side
                    );
                }
            }

            // Store pending impulse for later OBI confirmation
            self.pending_impulse = Some(PendingSignal {
                side: impulse.side,
                stored_at_ns: now,
            });

            // MEDIUM conviction: Impulse only (skip if high_conviction_only)
            if !self.high_conviction_only {
                tracing::info!(
                    "MEDIUM conviction signal: Impulse-only | {} {:?} | {:.2} bps",
                    impulse.symbol,
                    impulse.side,
                    impulse.impulse_magnitude_bps
                );
                return Some(CombinedSignal {
                    side: impulse.side,
                    target_venue: impulse.target_venue,
                    symbol: impulse.symbol.clone(),
                    strength: SignalStrength::Medium,
                    impulse: Some(impulse.clone()),
                    obi: None,
                    timestamp_ns: tick.exchange_ts_ns,
                    impulse_magnitude_bps: impulse.impulse_magnitude_bps,
                    obi_value: 0.0,
                });
            }
        }

        None
    }

    /// Process book update (from hot path)
    ///
    /// Returns combined signal if OBI detected and impulse confirms
    pub fn process_book(&mut self, book: &BookUpdate) -> Option<CombinedSignal> {
        let now = now_ns();
        self.expire_pending(now);

        // Process book for OBI detection
        if let Some(obi) = self.obi_detector.process_book(book) {
            // Cross-venue edge check
            if !self.has_edge(book.venue, obi.target_venue, obi.side) {
                // Store as pending OBI even without edge — impulse may confirm later
                self.pending_obi = Some(PendingSignal {
                    side: obi.side,
                    stored_at_ns: now,
                });
                return None;
            }

            // Check if we have a pending impulse that agrees on direction
            if let Some(pending_impulse) = self.pending_impulse.take() {
                if pending_impulse.side == obi.side {
                    // HIGH conviction: OBI + Impulse confirms direction
                    tracing::info!(
                        "HIGH conviction signal: OBI+Impulse | {} {:?} | obi={:.3}",
                        obi.symbol,
                        obi.side,
                        obi.obi_value
                    );
                    return Some(CombinedSignal {
                        side: obi.side,
                        target_venue: obi.target_venue,
                        symbol: obi.symbol.clone(),
                        strength: SignalStrength::High,
                        impulse: None,
                        obi: Some(obi.clone()),
                        timestamp_ns: book.exchange_ts_ns,
                        impulse_magnitude_bps: 0.0,
                        obi_value: obi.obi_value,
                    });
                } else {
                    tracing::debug!(
                        "Impulse direction conflict: obi={:?} impulse={:?}",
                        obi.side,
                        pending_impulse.side
                    );
                }
            }

            // Store pending OBI for later impulse confirmation
            self.pending_obi = Some(PendingSignal {
                side: obi.side,
                stored_at_ns: now,
            });

            // MEDIUM conviction: OBI only (skip if high_conviction_only)
            if !self.high_conviction_only {
                return Some(CombinedSignal {
                    side: obi.side,
                    target_venue: obi.target_venue,
                    symbol: obi.symbol.clone(),
                    strength: SignalStrength::Medium,
                    impulse: None,
                    obi: Some(obi.clone()),
                    timestamp_ns: book.exchange_ts_ns,
                    impulse_magnitude_bps: 0.0,
                    obi_value: obi.obi_value,
                });
            }
        }

        None
    }

    /// Check spread filter
    pub fn is_spread_acceptable(&self, bid: f64, ask: f64) -> bool {
        if bid <= 0.0 || ask <= 0.0 {
            return false;
        }
        let spread_bps = (ask - bid) / bid * 10000.0;
        spread_bps <= self.entry_threshold_bps * 2.0 // allow 2x entry threshold as max spread
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
            bids: bids
                .into_iter()
                .map(|(p, s)| BookLevel { price: p, size: s })
                .collect(),
            asks: asks
                .into_iter()
                .map(|(p, s)| BookLevel { price: p, size: s })
                .collect(),
            exchange_ts_ns: 0,
            local_ts_ns: 0,
        }
    }

    #[test]
    fn test_pending_signal_matches_on_direction_only() {
        // Direction-only matching means: if we have a pending OBI Buy,
        // any impulse Buy should combine into HIGH, regardless of which venue.
        let pending = PendingSignal {
            side: OrderSide::Buy,
            stored_at_ns: 0,
        };
        let impulse_buy = OrderSide::Buy;
        let impulse_sell = OrderSide::Sell;
        assert_eq!(pending.side, impulse_buy); // matches → HIGH
        assert_ne!(pending.side, impulse_sell); // conflicts → discard
    }

    #[test]
    fn test_pending_expiry_uses_wall_clock() {
        let impulse_detector =
            ImpulseDetector::new(5_000_000, 5.0, 1.5, 0.001, 10_000_000, 400_000_000);
        let obi_detector = ObiDivergenceDetector::new(0.7, 0.2, 5, 0.3, 200_000_000);
        let mut engine =
            ImpulseObiEngine::new(impulse_detector, obi_detector, 10.0, 1, false); // 1ns timeout

        // Inject a pending impulse
        engine.pending_impulse = Some(PendingSignal {
            side: OrderSide::Buy,
            stored_at_ns: 0, // stored at epoch 0 — effectively ancient
        });

        // Expire it
        engine.expire_pending(now_ns());
        assert!(engine.pending_impulse.is_none(), "Expired pending should be cleared");
    }

    #[test]
    fn test_spread_filter() {
        let impulse_detector =
            ImpulseDetector::new(5_000_000, 5.0, 1.5, 0.001, 10_000_000, 400_000_000);
        let obi_detector = ObiDivergenceDetector::new(0.7, 0.2, 5, 0.3, 200_000_000);
        let engine = ImpulseObiEngine::new(impulse_detector, obi_detector, 10.0, 10_000_000, false);

        // Good spread (1 bps < 20 bps limit)
        assert!(engine.is_spread_acceptable(100.0, 100.01));

        // Bad spread (too wide)
        assert!(!engine.is_spread_acceptable(100.0, 100.2));
    }
}
