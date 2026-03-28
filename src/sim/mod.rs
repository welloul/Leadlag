//! Paper Trading Simulator module.
//!
//! Implements high-fidelity paper trading with:
//! - L2 order book matching
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

/// Paper trading simulator.
///
/// Simulates exchange behavior with realistic fills, latency, and fees.
pub struct PaperSimulator {
    /// Simulation settings.
    settings: SimulationSettings,
    /// Order book matcher per symbol.
    matchers: Arc<Mutex<HashMap<Symbol, OrderBookMatcher>>>,
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

    /// Update the order book for a symbol.
    pub fn update_book(&self, update: BookUpdate) {
        let mut matchers = self.matchers.lock().unwrap();
        let matcher = matchers
            .entry(update.symbol.clone())
            .or_insert_with(|| OrderBookMatcher::new(self.settings.match_l2_depth));

        matcher.update_book(update.bids, update.asks);
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

        let mut matchers = self.matchers.lock().unwrap();
        let matcher = matchers
            .entry(order.symbol.clone())
            .or_insert_with(|| OrderBookMatcher::new(self.settings.match_l2_depth));

        // Match the order
        let (filled_size, avg_price, slippage_bps) = matcher.match_order(
            order.side,
            order.size,
            order.price,
        )?;

        if filled_size == 0.0 {
            return Err(ExecutionError::ExchangeError("No liquidity".to_string()));
        }

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

        // Set up order book
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
}