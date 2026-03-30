//! Signal Processing Pipeline module.
//!
//! Implements the lead-lag detection engine with:
//! - Time-grid alignment (forward-fill)
//! - Incremental Pearson cross-correlation
//! - Hysteresis state machine for role-flip validation
//! - Order Book Imbalance (OBI) fusion
//! - Impulse-OBI strategy (event-driven alpha)

pub mod correlation;
pub mod hysteresis;
pub mod impulse;
pub mod impulse_obi;
pub mod obi_divergence;
pub mod ring_buffer;
pub mod timegrid;

pub use correlation::CrossCorrelator;
pub use hysteresis::{Hysteresis, LeadRole};
pub use impulse::ImpulseDetector;
pub use impulse_obi::{CombinedSignal, ImpulseObiEngine, SignalStrength};
pub use obi_divergence::ObiDivergenceDetector;
pub use ring_buffer::RingBuffer;
pub use timegrid::{AlignedPair, IngestResult, TimeGrid};

use crate::config::StrategySettings;
use crate::eal::{BookUpdate, OrderSide, Symbol, Tick, TradeSignal, VenueId};

/// Active strategy enum
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ActiveStrategy {
    /// Correlation-hysteresis (existing)
    CorrelationHysteresis,
    /// Impulse-OBI (new event-driven)
    ImpulseObi,
}

/// Complete signal processing pipeline.
///
/// Orchestrates time-grid alignment, cross-correlation, and hysteresis
/// to generate trade signals. Supports multiple strategies.
pub struct SignalPipeline<const N: usize> {
    /// Active strategy
    active_strategy: ActiveStrategy,
    /// Cross-correlator for each symbol (for correlation-hysteresis).
    correlators: std::collections::HashMap<Symbol, CrossCorrelator<N>>,
    /// Hysteresis state machine for each symbol (for correlation-hysteresis).
    hysteresis: std::collections::HashMap<Symbol, Hysteresis>,
    /// Impulse-OBI engine (for impulse-obi strategy).
    impulse_obi_engine: Option<ImpulseObiEngine>,
    /// Strategy settings.
    settings: StrategySettings,
    /// Minimum correlation to generate a signal.
    min_r: f64,
    /// Time grid precision in nanoseconds (passed from main.rs).
    timegrid_precision_ns: u64,
}

impl<const N: usize> SignalPipeline<N> {
    /// Create a new signal pipeline.
    pub fn new(settings: StrategySettings) -> Self {
        let min_r = settings.min_correlation_r;
        let hysteresis = Hysteresis::new(settings.hysteresis_buffer, 3);

        let mut correlators = std::collections::HashMap::new();
        let mut hysteresis_map = std::collections::HashMap::new();

        for symbol_str in &settings.symbols {
            let symbol = Symbol::new(symbol_str);
            correlators.insert(symbol.clone(), CrossCorrelator::new());
            hysteresis_map.insert(symbol.clone(), hysteresis.clone());
        }

        // Determine active strategy
        let active_strategy = match settings.active_strategy.as_str() {
            "impulse_obi" => ActiveStrategy::ImpulseObi,
            _ => ActiveStrategy::CorrelationHysteresis,
        };

        // Initialize impulse-obi engine if needed
        let impulse_obi_engine = if active_strategy == ActiveStrategy::ImpulseObi {
            let window_ns = settings.impulse_window_ms * 1_000_000;
            let signal_timeout_ns = settings.signal_timeout_ms * 1_000_000;
            let venue_freshness_ns = settings.venue_freshness_ms * 1_000_000;
            let impulse_detector = ImpulseDetector::new(
                window_ns,
                settings.impulse_threshold_bps as f64,
                settings.lag_threshold_bps as f64,
                settings.min_trade_size_filter,
                signal_timeout_ns,
                venue_freshness_ns,
            );
            let obi_detector = ObiDivergenceDetector::new(
                settings.obi_strong_threshold,
                settings.obi_neutral_threshold,
                settings.obi_depth,
                settings.obi_spike_threshold,
                settings.obi_persist_ms * 1_000_000,
            );
            Some(ImpulseObiEngine::new(
                impulse_detector,
                obi_detector,
                settings.entry_threshold_bps as f64,
                signal_timeout_ns,
            ))
        } else {
            None
        };

        Self {
            active_strategy,
            correlators,
            hysteresis: hysteresis_map,
            impulse_obi_engine,
            settings,
            min_r,
            timegrid_precision_ns: 5_000_000, // Default 5ms, overridden by set_precision()
        }
    }

    /// Set the time grid precision (called from main.rs with actual settings value).
    pub fn set_precision(&mut self, precision_ns: u64) {
        self.timegrid_precision_ns = precision_ns;
    }

    /// Process an aligned pair and generate a signal if conditions are met.
    pub fn process_pair(&mut self, symbol: &Symbol, pair: &AlignedPair) -> Option<TradeSignal> {
        // Route to active strategy
        match self.active_strategy {
            ActiveStrategy::CorrelationHysteresis => {
                self.process_correlation_hysteresis(symbol, pair)
            }
            ActiveStrategy::ImpulseObi => {
                // Impulse-OBI uses tick and book, not aligned pairs
                None
            }
        }
    }

    /// Process tick for impulse-obi strategy
    pub fn process_tick(&mut self, tick: &Tick) -> Option<TradeSignal> {
        if let Some(engine) = &mut self.impulse_obi_engine {
            if let Some(signal) = engine.process_tick(tick) {
                return Some(TradeSignal {
                    side: signal.side,
                    target_venue: signal.target_venue,
                    symbol: signal.symbol,
                    correlation_r: 0.0, // Not applicable for impulse-obi
                    lag_offset_ns: 0,
                    timestamp_ns: signal.timestamp_ns,
                });
            }
        }
        None
    }

    /// Process book update for impulse-obi strategy
    pub fn process_book(&mut self, book: &BookUpdate) -> Option<TradeSignal> {
        if let Some(engine) = &mut self.impulse_obi_engine {
            if let Some(signal) = engine.process_book(book) {
                return Some(TradeSignal {
                    side: signal.side,
                    target_venue: signal.target_venue,
                    symbol: signal.symbol,
                    correlation_r: 0.0, // Not applicable for impulse-obi
                    lag_offset_ns: 0,
                    timestamp_ns: signal.timestamp_ns,
                });
            }
        }
        None
    }

    /// Process using correlation-hysteresis strategy
    fn process_correlation_hysteresis(
        &mut self,
        symbol: &Symbol,
        pair: &AlignedPair,
    ) -> Option<TradeSignal> {
        let correlator = self.correlators.get_mut(symbol)?;
        let hyst = self.hysteresis.get_mut(symbol)?;

        // Push price pair into correlator
        correlator.push(pair.price_a, pair.price_b);

        // Need at least window_size samples for reliable correlation
        // Lag detection needs FULL window, not half
        if correlator.len() < self.settings.window_size_ticks {
            return None;
        }

        // Calculate correlations for both exchanges
        // We calculate correlation at lag 0 (synchronized)
        let r = correlator.correlation();

        // For lead-lag detection, we need to check which exchange leads
        // by finding the lag that maximizes correlation
        let (best_lag, best_r) = correlator.find_best_lag(-10, 10);

        // Determine leader from actual best lag (not hardcoded ±10)
        let (r_a, r_b) = if best_lag < 0 {
            // A leads B (negative lag has higher correlation)
            (best_r, r)
        } else if best_lag > 0 {
            // B leads A (positive lag has higher correlation)
            (r, best_r)
        } else {
            // No clear leader, both equal
            (r, r)
        };

        // Update hysteresis (always update to track current lead)
        if let Some(new_lead) = hyst.update(r_a, r_b) {
            // Role flip detected!
            // Only generate signal if correlation is above threshold
            if best_r >= self.min_r {
                let laggard = new_lead.laggard();
                let laggard_venue = match laggard {
                    LeadRole::ExchangeA => VenueId::EXCHANGE_A,
                    LeadRole::ExchangeB => VenueId::EXCHANGE_B,
                    LeadRole::Undetermined => return None,
                };

                // Determine trade direction based on price movement
                let side = if pair.price_a > pair.price_b {
                    // A is leading up, buy on laggard
                    OrderSide::Buy
                } else {
                    // A is leading down, sell on laggard
                    OrderSide::Sell
                };

                return Some(TradeSignal {
                    side,
                    target_venue: laggard_venue,
                    symbol: symbol.clone(),
                    correlation_r: best_r,
                    lag_offset_ns: best_lag as i64 * self.timegrid_precision_ns as i64,
                    timestamp_ns: pair.timestamp_ns,
                });
            }
        }

        None
    }

    /// Get the current lead role for a symbol.
    pub fn current_lead(&self, symbol: &Symbol) -> Option<LeadRole> {
        self.hysteresis
            .get(symbol)
            .map(|h: &Hysteresis| h.current_lead())
    }

    /// Get the current correlation for a symbol.
    pub fn current_correlation(&self, symbol: &Symbol) -> Option<f64> {
        self.correlators
            .get(symbol)
            .map(|c: &CrossCorrelator<N>| c.correlation())
    }

    /// Reset the pipeline for a symbol.
    pub fn clear_symbol(&mut self, symbol: &Symbol) {
        if let Some(c) = self.correlators.get_mut(symbol) {
            c.clear();
        }
        if let Some(h) = self.hysteresis.get_mut(symbol) {
            h.clear();
        }
    }

    /// Reset the entire pipeline.
    pub fn clear(&mut self) {
        for c in self.correlators.values_mut() {
            c.clear();
        }
        for h in self.hysteresis.values_mut() {
            h.clear();
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_signal_pipeline_creation() {
        let settings = StrategySettings {
            active_strategy: "correlation_hysteresis".to_string(),
            symbols: vec!["BTC".to_string(), "ETH".to_string()],
            window_size_ticks: 256,
            min_correlation_r: 0.85,
            hysteresis_buffer: 0.10,
            enable_obi: false,
            obi_weight: 0.0,
            impulse_threshold_bps: 5,
            lag_threshold_bps: 1,
            impulse_window_ms: 5,
            signal_timeout_ms: 10,
            min_trade_size_filter: 0.001,
            spread_filter_bps: 10,
            obi_strong_threshold: 0.7,
            obi_neutral_threshold: 0.2,
            obi_depth: 5,
            obi_spike_threshold: 0.3,
            venue_freshness_ms: 400,
            entry_threshold_bps: 8,
            cooldown_ms: 200,
            max_levels_consumed: 3,
            obi_persist_ms: 200,
            fill_conservatism: 0.5,
        };

        let pipeline = SignalPipeline::<256>::new(settings);
        assert_eq!(pipeline.correlators.len(), 2);
        assert_eq!(pipeline.hysteresis.len(), 2);
    }
}
