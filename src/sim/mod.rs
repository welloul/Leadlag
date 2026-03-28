//! Paper Trading Simulator module.
//!
//! Implements high-fidelity paper trading with:
//! - Per-venue L2 order book matching
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

/// Spread model per venue type.
///
/// Binance is the most liquid venue (tight spread).
/// Hyperliquid has wider spreads due to lower liquidity.
#[derive(Debug, Clone, Copy)]
struct VenueSpreadModel {
    /// Base spread in bps
    base_spread_bps: f64,
    /// Size impact factor (bps per $1000 notional)
    size_impact_bps: f64,
}

impl VenueSpreadModel {
    /// Spread model for Binance (tight, deep book).
    fn binance() -> Self {
        Self {
            base_spread_bps: 1.0, // 0.01%
            size_impact_bps: 0.0005,
        }
    }

    /// Spread model for Hyperliquid (wider, thinner book).
    fn hyperliquid() -> Self {
        Self {
            base_spread_bps: 5.0, // 0.05%
            size_impact_bps: 0.002,
        }
    }

    /// Default spread model for unknown venues.
    fn default_venue() -> Self {
        Self {
            base_spread_bps: 2.5,
            size_impact_bps: 0.001,
        }
    }

    /// Get spread model for a venue.
    fn for_venue(venue: VenueId) -> Self {
        match venue {
            VenueId::EXCHANGE_A => Self::binance(),   // Binance
            VenueId::EXCHANGE_B => Self::hyperliquid(), // Hyperliquid
            _ => Self::default_venue(),
        }
    }

    /// Calculate half-spread for a given price.
    fn half_spread(&self, price: f64, order_notional: f64) -> f64 {
        let base = price * (self.base_spread_bps / 10000.0);
        let impact = price * (self.size_impact_bps * (order_notional / 1000.0) / 10000.0);
        base + impact
    }
}

/// Paper trading simulator.
///
/// Maintains separate order books per (Symbol, VenueId) pair.
/// Each venue has its own price, spread, and liquidity depth.
pub struct PaperSimulator {
    /// Simulation settings.
    settings: SimulationSettings,
    /// Order book matcher per (symbol, venue) pair.
    matchers: Arc<Mutex<HashMap<(Symbol, VenueId), OrderBookMatcher>>>,
    /// Order counter.
    order_counter: Arc<Mutex<u64>>,
    /// Positions.
    positions: Arc<Mutex<Vec<Position>>>,
    /// Daily realized PnL.
    daily_pnl: Arc<Mutex<f64>>,
    /// Total fees paid.
    total_fees: Arc<Mutex<f64>>,
    /// Fill events for alpha decay analysis.
    fill_history: Arc<Mutex<Vec<FillEvent>>>,
}

impl PaperSimulator {
    /// Create a new paper simulator.
    pub fn new(settings: SimulationSettings) -> Self {
        Self {
            settings,
            matchers: Arc::new(Mutex::new(HashMap::new())),
            order_counter: Arc::new(Mutex::new(0)),
            positions: Arc::new(Mutex::new(Vec::new())),
            daily_pnl: Arc::new(Mutex::new(0.0)),
            total_fees: Arc::new(Mutex::new(0.0)),
            fill_history: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Update the order book from a real L2 BookUpdate.
    pub fn update_book(&self, update: BookUpdate) {
        let key = (update.symbol.clone(), update.venue);
        let mut matchers = self.matchers.lock().unwrap();
        let matcher = matchers
            .entry(key)
            .or_insert_with(|| OrderBookMatcher::new(self.settings.match_l2_depth));

        matcher.update_book(update.bids, update.asks);
    }

    /// Synthesize an order book from a tick price.
    ///
    /// Creates a synthetic L2 book with per-venue spread and depth.
    /// Each venue gets its own independent book keyed by (Symbol, VenueId).
    pub fn update_book_from_tick(&self, symbol: &Symbol, price: f64, venue: VenueId) {
        if price <= 0.0 || !price.is_finite() {
            return;
        }

        let spread_model = VenueSpreadModel::for_venue(venue);

        // Use max_notional as estimate for size impact
        let estimated_notional = 5000.0;
        let half_spread = spread_model.half_spread(price, estimated_notional);

        let mut bids = Vec::with_capacity(self.settings.match_l2_depth);
        let mut asks = Vec::with_capacity(self.settings.match_l2_depth);

        for i in 0..self.settings.match_l2_depth {
            let depth_bps = (i as f64) * 0.5; // 0.5 bps per level
            let bid_price = price - half_spread - (price * depth_bps / 10000.0);
            let ask_price = price + half_spread + (price * depth_bps / 10000.0);
            // Large synthetic size — will fill any realistic order
            let size = 10_000.0;
            bids.push(BookLevel { price: bid_price, size });
            asks.push(BookLevel { price: ask_price, size });
        }

        let key = (symbol.clone(), venue);
        let mut matchers = self.matchers.lock().unwrap();
        let matcher = matchers
            .entry(key)
            .or_insert_with(|| OrderBookMatcher::new(self.settings.match_l2_depth));

        matcher.update_book(bids, asks);
    }

    /// Check if a venue has enough liquidity to trade.
    ///
    /// Returns true if the book for (symbol, venue) has at least one bid and one ask.
    pub fn is_venue_liquid(&self, symbol: &Symbol, venue: VenueId) -> bool {
        let matchers = self.matchers.lock().unwrap();
        if let Some(matcher) = matchers.get(&(symbol.clone(), venue)) {
            !matcher.bids.is_empty() && !matcher.asks.is_empty()
        } else {
            false
        }
    }

    /// Get the mid price for a (symbol, venue) pair.
    ///
    /// Returns the midpoint of the best bid and ask for the target venue.
    /// Falls back to the other venue if the target venue has no book yet.
    pub fn get_mid_price(&self, symbol: &Symbol, venue: VenueId) -> Option<f64> {
        let matchers = self.matchers.lock().unwrap();

        // Try target venue first
        if let Some(matcher) = matchers.get(&(symbol.clone(), venue)) {
            if let Some(mid) = matcher.mid_price() {
                if mid > 0.0 && mid.is_finite() {
                    return Some(mid);
                }
            }
        }

        // Fall back to the other venue
        let other_venue = match venue {
            VenueId::EXCHANGE_A => VenueId::EXCHANGE_B,
            _ => VenueId::EXCHANGE_A,
        };
        if let Some(matcher) = matchers.get(&(symbol.clone(), other_venue)) {
            if let Some(mid) = matcher.mid_price() {
                if mid > 0.0 && mid.is_finite() {
                    return Some(mid);
                }
            }
        }

        None
    }

    /// Simulate order matching with latency.
    async fn simulate_fill(
        &self,
        order: &OrderRequest,
    ) -> Result<FillEvent, ExecutionError> {
        // Simulate latency
        if self.settings.latency_simulation_ms > 0 {
            tokio::time::sleep(Duration::from_millis(self.settings.latency_simulation_ms)).await;
        }

        // Key by (symbol, target_venue) — NOT just symbol.
        // Each venue has its own independent order book.
        let key = (order.symbol.clone(), order.venue);
        let mut matchers = self.matchers.lock().unwrap();

        // Use get_mut — do NOT silently create an empty matcher.
        // If the venue has no book, return a clear error.
        let matcher = matchers.get_mut(&key)
            .ok_or_else(|| ExecutionError::ExchangeError(
                format!("No book for {} on {:?}", order.symbol, order.venue)
            ))?;

        // Match the order against this venue's book
        let (filled_size, avg_price, slippage_bps) = matcher.match_order(
            order.side,
            order.size,
            order.price,
        )?;

        // Calculate fee
        let notional = filled_size * avg_price;
        let fee = notional * (self.settings.fee_tier_bps / 10000.0);

        // Update total fees
        *self.total_fees.lock().unwrap() += fee;

        // Generate order ID
        let mut counter = self.order_counter.lock().unwrap();
        *counter += 1;
        let order_id = OrderId(*counter);

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64;

        let fill = FillEvent {
            order_id,
            client_order_id: order.client_order_id.clone(),
            venue: order.venue,
            symbol: order.symbol.clone(),
            side: order.side,
            filled_size,
            avg_price,
            fee,
            fee_currency: "USD".to_string(),
            timestamp_ns: now,
        };

        // Store fill for alpha decay analysis
        self.fill_history.lock().unwrap().push(fill.clone());

        Ok(fill)
    }

    /// Get total fees paid.
    pub fn total_fees(&self) -> f64 {
        *self.total_fees.lock().unwrap()
    }

    /// Get fill history for alpha decay analysis.
    pub fn fill_history(&self) -> Vec<FillEvent> {
        self.fill_history.lock().unwrap().clone()
    }

    /// Calculate alpha decay statistics.
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

        let avg_slippage_bps = total_slippage / total_fills as f64;

        AlphaDecayStats {
            total_fills,
            avg_slippage_bps,
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
        let fill = self.simulate_fill(order).await?;

        // Update positions
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
        // Paper simulator doesn't support cancellation
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
        VenueId::EXCHANGE_A // Paper simulator uses Exchange A as default
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

        // Set up order book for Exchange A
        sim.update_book(BookUpdate {
            venue: VenueId::EXCHANGE_A,
            symbol: Symbol::new("BTC"),
            bids: vec![BookLevel { price: 60000.0, size: 1.0 }],
            asks: vec![BookLevel { price: 60001.0, size: 1.0 }],
            exchange_ts_ns: 0,
            local_ts_ns: 0,
        });

        // Order targets Exchange A — should fill
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

        // Set up book for Exchange A only
        sim.update_book_from_tick(&Symbol::new("BTC"), 60000.0, VenueId::EXCHANGE_A);

        // Order targeting Exchange B — should fail (no book for B)
        let order_b = OrderRequest::market_buy(
            VenueId::EXCHANGE_B,
            Symbol::new("BTC"),
            0.5,
        );
        let result = sim.submit_order(&order_b).await;
        assert!(result.is_err(), "Exchange B has no book — should fail");

        // Now populate Exchange B's book
        sim.update_book_from_tick(&Symbol::new("BTC"), 59995.0, VenueId::EXCHANGE_B);

        // Order targeting Exchange B — should now fill
        let result = sim.submit_order(&order_b).await;
        assert!(result.is_ok(), "Exchange B now has book — should fill");
    }

    #[tokio::test]
    async fn test_per_venue_different_prices() {
        let settings = SimulationSettings {
            enabled: true,
            use_real_data: false,
            latency_simulation_ms: 0,
            fee_tier_bps: 2.5,
            match_l2_depth: 10,
        };

        let sim = PaperSimulator::new(settings);

        // Different prices per venue (the core alpha scenario)
        sim.update_book_from_tick(&Symbol::new("ZEC"), 37.00, VenueId::EXCHANGE_A);
        sim.update_book_from_tick(&Symbol::new("ZEC"), 36.95, VenueId::EXCHANGE_B);

        // Buy on Exchange B (cheaper)
        let order_b = OrderRequest::market_buy(
            VenueId::EXCHANGE_B,
            Symbol::new("ZEC"),
            1.0,
        );
        let fill_b = sim.submit_order(&order_b).await.unwrap();

        // Buy on Exchange A (more expensive)
        let order_a = OrderRequest::market_buy(
            VenueId::EXCHANGE_A,
            Symbol::new("ZEC"),
            1.0,
        );
        let fill_a = sim.submit_order(&order_a).await.unwrap();

        // B should be cheaper than A (Hyperliquid spread model is wider, but base price is lower)
        // The fills should reflect each venue's independent pricing
        let pos_b = sim.get_positions().await.unwrap().iter()
            .find(|p| p.venue == VenueId::EXCHANGE_B).unwrap().entry_price;
        let pos_a = sim.get_positions().await.unwrap().iter()
            .find(|p| p.venue == VenueId::EXCHANGE_A).unwrap().entry_price;

        // B's entry should be based on B's book (36.95), A's on A's book (37.00)
        assert!(pos_b < pos_a, "B should be cheaper: B={} vs A={}", pos_b, pos_a);
    }

    #[test]
    fn test_is_venue_liquid() {
        let settings = SimulationSettings {
            enabled: true,
            use_real_data: false,
            latency_simulation_ms: 0,
            fee_tier_bps: 2.5,
            match_l2_depth: 10,
        };

        let sim = PaperSimulator::new(settings);

        // Exchange A not liquid yet
        assert!(!sim.is_venue_liquid(&Symbol::new("BTC"), VenueId::EXCHANGE_A));

        // Populate A
        sim.update_book_from_tick(&Symbol::new("BTC"), 60000.0, VenueId::EXCHANGE_A);
        assert!(sim.is_venue_liquid(&Symbol::new("BTC"), VenueId::EXCHANGE_A));

        // Exchange B still not liquid
        assert!(!sim.is_venue_liquid(&Symbol::new("BTC"), VenueId::EXCHANGE_B));
    }
}
