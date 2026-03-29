//! Paper Trading Simulator module.
//!
//! Implements high-fidelity paper trading with:
//! - Per-venue L2 order book matching
//! - Per-venue staleness tracking (allow stale books <2s)
//! - Latency simulation
//! - Fee/slippage calculation
//! - Alpha decay statistics

pub mod matcher;

pub use matcher::OrderBookMatcher;

use crate::config::SimulationSettings;
use crate::eal::{
    AccountState, BookLevel, BookUpdate, ExecutionError, FillEvent, OrderAck, OrderExecution,
    OrderId, OrderRequest, OrderSide, Position, Symbol, VenueId,
};
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Maximum age for a stale book before it's considered unusable.
const MAX_BOOK_AGE_NS: u64 = 2_000_000_000; // 2 seconds

/// Spread model per venue type.
#[derive(Debug, Clone, Copy)]
struct VenueSpreadModel {
    base_spread_bps: f64,
    size_impact_bps: f64,
}

impl VenueSpreadModel {
    fn binance() -> Self {
        Self {
            base_spread_bps: 1.0,
            size_impact_bps: 0.0005,
        }
    }

    fn hyperliquid() -> Self {
        Self {
            base_spread_bps: 5.0,
            size_impact_bps: 0.002,
        }
    }

    fn default_venue() -> Self {
        Self {
            base_spread_bps: 2.5,
            size_impact_bps: 0.001,
        }
    }

    fn for_venue(venue: VenueId) -> Self {
        match venue {
            VenueId::EXCHANGE_A => Self::binance(),
            VenueId::EXCHANGE_B => Self::hyperliquid(),
            _ => Self::default_venue(),
        }
    }

    fn half_spread(&self, price: f64, order_notional: f64) -> f64 {
        let base = price * (self.base_spread_bps / 10000.0);
        let impact = price * (self.size_impact_bps * (order_notional / 1000.0) / 10000.0);
        base + impact
    }
}

/// Book staleness info per venue.
#[derive(Debug, Clone, Copy, Default)]
struct VenueBookState {
    /// Timestamp (ns) when this venue's book was last updated from a real tick.
    last_update_ns: u64,
    /// Whether the current book data came from this venue's own ticks.
    /// true = real HL tick populated this book.
    /// false = no HL data yet, book is empty or was never populated.
    has_real_data: bool,
}

/// Fill provenance — whether the fill used fresh or stale book data.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FillProvenance {
    Fresh,
    Stale,
}

/// Paper trading simulator.
///
/// Maintains separate order books per (Symbol, VenueId) pair.
/// Each venue has its own price, spread, and liquidity depth.
/// Tracks staleness per venue and allows fills with stale books <2s.
pub struct PaperSimulator {
    settings: SimulationSettings,
    matchers: Arc<Mutex<HashMap<(Symbol, VenueId), OrderBookMatcher>>>,
    /// Per-venue book staleness tracking.
    book_states: Arc<Mutex<HashMap<(Symbol, VenueId), VenueBookState>>>,
    order_counter: Arc<Mutex<u64>>,
    positions: Arc<Mutex<Vec<Position>>>,
    daily_pnl: Arc<Mutex<f64>>,
    total_fees: Arc<Mutex<f64>>,
    fill_history: Arc<Mutex<Vec<FillEvent>>>,
    /// Staleness metrics.
    metrics: Arc<Mutex<SimMetrics>>,
}

/// Simulator metrics for monitoring.
#[derive(Debug, Clone, Default)]
pub struct SimMetrics {
    pub total_signals: u64,
    pub fills_with_fresh_book: u64,
    pub fills_with_stale_book: u64,
    pub skipped_no_book: u64,
    pub skipped_stale_over_2s: u64,
}

impl SimMetrics {
    pub fn summary(&self) -> String {
        let total = self.fills_with_fresh_book + self.fills_with_stale_book;
        let fresh_pct = if total > 0 {
            self.fills_with_fresh_book as f64 / total as f64 * 100.0
        } else { 0.0 };
        let stale_pct = if total > 0 {
            self.fills_with_stale_book as f64 / total as f64 * 100.0
        } else { 0.0 };
        format!(
            "signals={} fresh={} ({:.0}%) stale={} ({:.0}%) no_book={} stale_over_2s={}",
            self.total_signals,
            self.fills_with_fresh_book, fresh_pct,
            self.fills_with_stale_book, stale_pct,
            self.skipped_no_book, self.skipped_stale_over_2s
        )
    }
}

impl PaperSimulator {
    pub fn new(settings: SimulationSettings) -> Self {
        Self {
            settings,
            matchers: Arc::new(Mutex::new(HashMap::new())),
            book_states: Arc::new(Mutex::new(HashMap::new())),
            order_counter: Arc::new(Mutex::new(0)),
            positions: Arc::new(Mutex::new(Vec::new())),
            daily_pnl: Arc::new(Mutex::new(0.0)),
            total_fees: Arc::new(Mutex::new(0.0)),
            fill_history: Arc::new(Mutex::new(Vec::new())),
            metrics: Arc::new(Mutex::new(SimMetrics::default())),
        }
    }

    /// Update the order book from a real L2 BookUpdate.
    pub fn update_book(&self, update: BookUpdate) {
        let key = (update.symbol.clone(), update.venue);
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64;

        {
            let mut matchers = self.matchers.lock().unwrap();
            let matcher = matchers
                .entry(key.clone())
                .or_insert_with(|| OrderBookMatcher::new(self.settings.match_l2_depth));
            matcher.update_book(update.bids, update.asks);
        }

        {
            let mut states = self.book_states.lock().unwrap();
            let state = states.entry(key).or_default();
            state.last_update_ns = now;
            state.has_real_data = true;
        }
    }

    /// Update book from a real tick from THIS venue.
    /// Only updates the book for the venue that sent the tick.
    /// Does NOT seed other venues with fake data.
    pub fn update_book_from_tick(&self, symbol: &Symbol, price: f64, venue: VenueId) {
        if price <= 0.0 || !price.is_finite() {
            return;
        }

        let spread_model = VenueSpreadModel::for_venue(venue);
        let estimated_notional = 5000.0;
        let half_spread = spread_model.half_spread(price, estimated_notional);

        let mut bids = Vec::with_capacity(self.settings.match_l2_depth);
        let mut asks = Vec::with_capacity(self.settings.match_l2_depth);

        for i in 0..self.settings.match_l2_depth {
            let depth_bps = (i as f64) * 0.5;
            let bid_price = price - half_spread - (price * depth_bps / 10000.0);
            let ask_price = price + half_spread + (price * depth_bps / 10000.0);
            let size = 10_000.0;
            bids.push(BookLevel { price: bid_price, size });
            asks.push(BookLevel { price: ask_price, size });
        }

        let key = (symbol.clone(), venue);
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64;

        {
            let mut matchers = self.matchers.lock().unwrap();
            let matcher = matchers
                .entry(key.clone())
                .or_insert_with(|| OrderBookMatcher::new(self.settings.match_l2_depth));
            matcher.update_book(bids, asks);
        }

        {
            let mut states = self.book_states.lock().unwrap();
            let state = states.entry(key).or_default();
            state.last_update_ns = now;
            state.has_real_data = true;
        }
    }

    /// Check if a venue has a book (fresh or stale).
    pub fn is_venue_liquid(&self, symbol: &Symbol, venue: VenueId) -> bool {
        let matchers = self.matchers.lock().unwrap();
        if let Some(matcher) = matchers.get(&(symbol.clone(), venue)) {
            !matcher.bids.is_empty() && !matcher.asks.is_empty()
        } else {
            false
        }
    }

    /// Check if a venue's book is stale (>max_age_ns old).
    pub fn is_book_stale(&self, symbol: &Symbol, venue: VenueId, max_age_ns: u64) -> bool {
        let states = self.book_states.lock().unwrap();
        if let Some(state) = states.get(&(symbol.clone(), venue)) {
            if !state.has_real_data {
                return true; // No real data = stale
            }
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos() as u64;
            now.saturating_sub(state.last_update_ns) > max_age_ns
        } else {
            true // No state = stale
        }
    }

    /// Get staleness in nanoseconds for a venue's book.
    pub fn book_staleness_ns(&self, symbol: &Symbol, venue: VenueId) -> Option<u64> {
        let states = self.book_states.lock().unwrap();
        if let Some(state) = states.get(&(symbol.clone(), venue)) {
            if !state.has_real_data {
                return None; // No real data ever
            }
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos() as u64;
            Some(now.saturating_sub(state.last_update_ns))
        } else {
            None
        }
    }

    /// Get the mid price for a (symbol, venue) pair.
    /// NO cross-venue fallback — each venue must have its own real data.
    pub fn get_mid_price(&self, symbol: &Symbol, venue: VenueId) -> Option<f64> {
        let matchers = self.matchers.lock().unwrap();
        if let Some(matcher) = matchers.get(&(symbol.clone(), venue)) {
            if let Some(mid) = matcher.mid_price() {
                if mid > 0.0 && mid.is_finite() {
                    return Some(mid);
                }
            }
        }
        None
    }

    /// Get fill metrics.
    pub fn metrics(&self) -> SimMetrics {
        self.metrics.lock().unwrap().clone()
    }

    /// Simulate order matching with latency.
    async fn simulate_fill(
        &self,
        order: &OrderRequest,
        provenance: FillProvenance,
    ) -> Result<FillEvent, ExecutionError> {
        if self.settings.latency_simulation_ms > 0 {
            tokio::time::sleep(Duration::from_millis(self.settings.latency_simulation_ms)).await;
        }

        let key = (order.symbol.clone(), order.venue);
        let mut matchers = self.matchers.lock().unwrap();

        let matcher = matchers.get_mut(&key)
            .ok_or_else(|| ExecutionError::ExchangeError(
                format!("No book for {} on {:?}", order.symbol, order.venue)
            ))?;

        let (filled_size, avg_price, slippage_bps) = matcher.match_order(
            order.side,
            order.size,
            order.price,
        )?;

        let notional = filled_size * avg_price;
        let fee = notional * (self.settings.fee_tier_bps / 10000.0);
        *self.total_fees.lock().unwrap() += fee;

        let mut counter = self.order_counter.lock().unwrap();
        *counter += 1;
        let order_id = OrderId(*counter);

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64;

        // Track staleness in fill
        let stale_tag = match provenance {
            FillProvenance::Stale => " [STALE_BOOK]",
            FillProvenance::Fresh => "",
        };

        let fill = FillEvent {
            order_id,
            client_order_id: order.client_order_id.clone(),
            venue: order.venue,
            symbol: order.symbol.clone(),
            side: order.side,
            filled_size,
            avg_price,
            fee,
            fee_currency: format!("USD{}", stale_tag),
            timestamp_ns: now,
        };

        self.fill_history.lock().unwrap().push(fill.clone());

        // Update metrics
        {
            let mut metrics = self.metrics.lock().unwrap();
            match provenance {
                FillProvenance::Fresh => metrics.fills_with_fresh_book += 1,
                FillProvenance::Stale => metrics.fills_with_stale_book += 1,
            }
        }

        Ok(fill)
    }

    pub fn total_fees(&self) -> f64 {
        *self.total_fees.lock().unwrap()
    }

    pub fn fill_history(&self) -> Vec<FillEvent> {
        self.fill_history.lock().unwrap().clone()
    }

    pub fn alpha_decay_stats(&self) -> AlphaDecayStats {
        let fills = self.fill_history.lock().unwrap();
        let total_fills = fills.len();

        if total_fills == 0 {
            return AlphaDecayStats::default();
        }

        let total_slippage: f64 = fills
            .iter()
            .map(|f| f.fee / (f.filled_size * f.avg_price) * 10000.0)
            .sum();

        AlphaDecayStats {
            total_fills,
            avg_slippage_bps: total_slippage / total_fills as f64,
            total_fees: *self.total_fees.lock().unwrap(),
        }
    }
}

/// Alpha decay statistics.
#[derive(Debug, Clone, Default)]
pub struct AlphaDecayStats {
    pub total_fills: usize,
    pub avg_slippage_bps: f64,
    pub total_fees: f64,
}

#[async_trait]
impl OrderExecution for PaperSimulator {
    async fn submit_order(&self, order: &OrderRequest) -> Result<OrderAck, ExecutionError> {
        // Determine provenance: is the target venue's book fresh or stale?
        let staleness = self.book_staleness_ns(&order.symbol, order.venue);
        let has_book = self.is_venue_liquid(&order.symbol, order.venue);

        // Increment signal counter
        {
            self.metrics.lock().unwrap().total_signals += 1;
        }

        let provenance = match (has_book, staleness) {
            // No book at all — skip
            (false, _) => {
                self.metrics.lock().unwrap().skipped_no_book += 1;
                return Err(ExecutionError::ExchangeError(
                    format!("No book for {} on {:?}", order.symbol, order.venue)
                ));
            }
            // Has book, no staleness data — treat as stale
            (true, None) => FillProvenance::Stale,
            // Has book, freshness check
            (true, Some(age_ns)) => {
                if age_ns <= MAX_BOOK_AGE_NS {
                    FillProvenance::Fresh
                } else {
                    self.metrics.lock().unwrap().skipped_stale_over_2s += 1;
                    return Err(ExecutionError::ExchangeError(
                        format!("Book stale: {:.1}s old for {} on {:?}",
                            age_ns as f64 / 1e9, order.symbol, order.venue)
                    ));
                }
            }
        };

        let fill = self.simulate_fill(order, provenance).await?;

        let mut positions = self.positions.lock().unwrap();
        let position = positions
            .iter_mut()
            .find(|p| p.venue == order.venue && p.symbol == order.symbol);

        if let Some(pos) = position {
            let signed_size = match order.side {
                OrderSide::Buy => fill.filled_size,
                OrderSide::Sell => -fill.filled_size,
            };

            if pos.size == 0.0 {
                pos.entry_price = fill.avg_price;
            }
            pos.size += signed_size;
            pos.timestamp_ns = fill.timestamp_ns;
        } else {
            positions.push(Position {
                venue: order.venue,
                symbol: order.symbol.clone(),
                size: match order.side {
                    OrderSide::Buy => fill.filled_size,
                    OrderSide::Sell => -fill.filled_size,
                },
                entry_price: fill.avg_price,
                unrealized_pnl: 0.0,
                timestamp_ns: fill.timestamp_ns,
            });
        }

        Ok(OrderAck {
            order_id: fill.order_id,
            client_order_id: order.client_order_id.clone(),
            venue: order.venue,
            timestamp_ns: fill.timestamp_ns,
        })
    }

    async fn cancel_order(&self, _order_id: OrderId) -> Result<(), ExecutionError> {
        Ok(())
    }

    async fn get_positions(&self) -> Result<Vec<Position>, ExecutionError> {
        Ok(self.positions.lock().unwrap().clone())
    }

    async fn get_account_state(&self) -> Result<AccountState, ExecutionError> {
        let positions = self.positions.lock().unwrap().clone();
        let total_pnl: f64 = positions.iter().map(|p| p.unrealized_pnl).sum();

        Ok(AccountState {
            positions,
            total_unrealized_pnl: total_pnl,
            daily_realized_pnl: *self.daily_pnl.lock().unwrap(),
            available_balance_usd: 100_000.0,
        })
    }

    fn venue_id(&self) -> VenueId {
        VenueId::EXCHANGE_A
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_paper_simulator_basic_fill() {
        let settings = SimulationSettings {
            enabled: true,
            use_real_data: false,
            latency_simulation_ms: 0,
            fee_tier_bps: 2.5,
            match_l2_depth: 10,
        };

        let sim = PaperSimulator::new(settings);

        sim.update_book(BookUpdate {
            venue: VenueId::EXCHANGE_A,
            symbol: Symbol::new("BTC"),
            bids: vec![BookLevel { price: 60000.0, size: 1.0 }],
            asks: vec![BookLevel { price: 60001.0, size: 1.0 }],
            exchange_ts_ns: 0,
            local_ts_ns: 0,
        });

        let order = OrderRequest::market_buy(
            VenueId::EXCHANGE_A,
            Symbol::new("BTC"),
            0.5,
        );

        let ack = sim.submit_order(&order).await.unwrap();
        assert_eq!(ack.order_id, OrderId(1));
    }

    #[tokio::test]
    async fn test_per_venue_isolation() {
        let settings = SimulationSettings {
            enabled: true,
            use_real_data: false,
            latency_simulation_ms: 0,
            fee_tier_bps: 2.5,
            match_l2_depth: 10,
        };

        let sim = PaperSimulator::new(settings);

        sim.update_book_from_tick(&Symbol::new("BTC"), 60000.0, VenueId::EXCHANGE_A);

        let order_b = OrderRequest::market_buy(
            VenueId::EXCHANGE_B,
            Symbol::new("BTC"),
            0.5,
        );
        let result = sim.submit_order(&order_b).await;
        assert!(result.is_err(), "Exchange B has no book — should fail");

        sim.update_book_from_tick(&Symbol::new("BTC"), 59995.0, VenueId::EXCHANGE_B);

        let result = sim.submit_order(&order_b).await;
        assert!(result.is_ok(), "Exchange B now has book — should fill");
    }

    #[test]
    fn test_staleness_tracking() {
        let settings = SimulationSettings {
            enabled: true,
            use_real_data: false,
            latency_simulation_ms: 0,
            fee_tier_bps: 2.5,
            match_l2_depth: 10,
        };

        let sim = PaperSimulator::new(settings);

        // No data = stale
        assert!(sim.is_book_stale(&Symbol::new("BTC"), VenueId::EXCHANGE_A, MAX_BOOK_AGE_NS));

        // After tick = fresh
        sim.update_book_from_tick(&Symbol::new("BTC"), 60000.0, VenueId::EXCHANGE_A);
        assert!(!sim.is_book_stale(&Symbol::new("BTC"), VenueId::EXCHANGE_A, MAX_BOOK_AGE_NS));

        // Other venue still stale
        assert!(sim.is_book_stale(&Symbol::new("BTC"), VenueId::EXCHANGE_B, MAX_BOOK_AGE_NS));
    }

    #[test]
    fn test_staleness_ns() {
        let settings = SimulationSettings {
            enabled: true,
            use_real_data: false,
            latency_simulation_ms: 0,
            fee_tier_bps: 2.5,
            match_l2_depth: 10,
        };

        let sim = PaperSimulator::new(settings);

        // No data = None
        assert!(sim.book_staleness_ns(&Symbol::new("BTC"), VenueId::EXCHANGE_A).is_none());

        // After tick = small number
        sim.update_book_from_tick(&Symbol::new("BTC"), 60000.0, VenueId::EXCHANGE_A);
        let staleness = sim.book_staleness_ns(&Symbol::new("BTC"), VenueId::EXCHANGE_A).unwrap();
        assert!(staleness < 1_000_000_000, "Should be fresh (<1s)");
    }
}
