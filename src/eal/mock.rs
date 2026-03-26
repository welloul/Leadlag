//! Mock exchange implementation for testing.
//!
//! Provides a deterministic exchange that can be controlled programmatically.
//! Useful for unit tests, integration tests, and paper trading simulation.

use async_trait::async_trait;
use crossbeam_channel::{self, Receiver, Sender};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use super::{
    AccountState, BookLevel, BookUpdate, ExchangeError, ExecutionError, FillEvent, MarketData,
    OrderAck, OrderExecution, OrderId, OrderRequest, OrderSide, Position, Symbol, Tick, VenueId,
};

/// Mock exchange for testing.
///
/// Simulates exchange behavior with controllable responses.
pub struct MockExchange {
    /// Venue ID for this mock exchange.
    venue_id: VenueId,
    /// Tick senders per symbol.
    tick_senders: Arc<Mutex<HashMap<Symbol, Sender<Arc<Tick>>>>>,
    /// Book senders per symbol.
    book_senders: Arc<Mutex<HashMap<Symbol, Sender<Arc<BookUpdate>>>>>,
    /// Order counter.
    order_counter: Arc<Mutex<u64>>,
    /// Submitted orders.
    orders: Arc<Mutex<Vec<OrderRequest>>>,
    /// Positions.
    positions: Arc<Mutex<Vec<Position>>>,
    /// Whether to simulate errors.
    simulate_error: Arc<Mutex<bool>>,
}

impl MockExchange {
    /// Create a new mock exchange.
    pub fn new(venue_id: VenueId) -> Self {
        Self {
            venue_id,
            tick_senders: Arc::new(Mutex::new(HashMap::new())),
            book_senders: Arc::new(Mutex::new(HashMap::new())),
            order_counter: Arc::new(Mutex::new(0)),
            orders: Arc::new(Mutex::new(Vec::new())),
            positions: Arc::new(Mutex::new(Vec::new())),
            simulate_error: Arc::new(Mutex::new(false)),
        }
    }

    /// Inject a tick into the mock exchange.
    pub fn inject_tick(&self, tick: Tick) {
        let senders = self.tick_senders.lock().unwrap();
        if let Some(sender) = senders.get(&tick.symbol) {
            let _ = sender.send(Arc::new(tick));
        }
    }

    /// Inject a book update into the mock exchange.
    pub fn inject_book_update(&self, update: BookUpdate) {
        let senders = self.book_senders.lock().unwrap();
        if let Some(sender) = senders.get(&update.symbol) {
            let _ = sender.send(Arc::new(update));
        }
    }

    /// Set whether to simulate errors.
    pub fn set_simulate_error(&self, simulate: bool) {
        *self.simulate_error.lock().unwrap() = simulate;
    }

    /// Get submitted orders (for testing).
    pub fn get_orders(&self) -> Vec<OrderRequest> {
        self.orders.lock().unwrap().clone()
    }

    /// Set positions (for testing).
    pub fn set_positions(&self, positions: Vec<Position>) {
        *self.positions.lock().unwrap() = positions;
    }
}

#[async_trait]
impl MarketData for MockExchange {
    async fn subscribe_ticks(
        &self,
        symbols: &[Symbol],
    ) -> Result<Receiver<Arc<Tick>>, ExchangeError> {
        let (tx, rx) = crossbeam_channel::bounded(1024);
        let mut senders = self.tick_senders.lock().unwrap();
        for symbol in symbols {
            senders.insert(symbol.clone(), tx.clone());
        }
        Ok(rx)
    }

    async fn subscribe_book(
        &self,
        symbol: &Symbol,
    ) -> Result<Receiver<Arc<BookUpdate>>, ExchangeError> {
        let (tx, rx) = crossbeam_channel::bounded(1024);
        let mut senders = self.book_senders.lock().unwrap();
        senders.insert(symbol.clone(), tx);
        Ok(rx)
    }

    fn venue_id(&self) -> VenueId {
        self.venue_id
    }
}

#[async_trait]
impl OrderExecution for MockExchange {
    async fn submit_order(&self, order: &OrderRequest) -> Result<OrderAck, ExecutionError> {
        if *self.simulate_error.lock().unwrap() {
            return Err(ExecutionError::ExchangeError("Simulated error".to_string()));
        }

        let mut counter = self.order_counter.lock().unwrap();
        *counter += 1;
        let order_id = OrderId(*counter);

        self.orders.lock().unwrap().push(order.clone());

        Ok(OrderAck {
            order_id,
            client_order_id: order.client_order_id.clone(),
            venue: self.venue_id,
            timestamp_ns: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos() as u64,
        })
    }

    async fn cancel_order(&self, _order_id: OrderId) -> Result<(), ExecutionError> {
        if *self.simulate_error.lock().unwrap() {
            return Err(ExecutionError::ExchangeError("Simulated error".to_string()));
        }
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
            daily_realized_pnl: 0.0,
            available_balance_usd: 100_000.0,
        })
    }

    fn venue_id(&self) -> VenueId {
        self.venue_id
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_mock_exchange_tick_subscription() {
        let mock = MockExchange::new(VenueId::EXCHANGE_A);
        let symbol = Symbol::new("BTC");

        let rx = mock.subscribe_ticks(&[symbol.clone()]).await.unwrap();

        mock.inject_tick(Tick {
            venue: VenueId::EXCHANGE_A,
            symbol: symbol.clone(),
            price: 60000.0,
            size: 1.0,
            exchange_ts_ns: 0,
            local_ts_ns: 0,
        });

        let tick = rx.recv().unwrap();
        assert_eq!(tick.price, 60000.0);
    }

    #[tokio::test]
    async fn test_mock_exchange_order_submission() {
        let mock = MockExchange::new(VenueId::EXCHANGE_A);
        let order = OrderRequest::market_buy(
            VenueId::EXCHANGE_A,
            Symbol::new("BTC"),
            0.5,
        );

        let ack = mock.submit_order(&order).await.unwrap();
        assert_eq!(ack.order_id, OrderId(1));
        assert_eq!(mock.get_orders().len(), 1);
    }

    #[tokio::test]
    async fn test_mock_exchange_error_simulation() {
        let mock = MockExchange::new(VenueId::EXCHANGE_A);
        mock.set_simulate_error(true);

        let order = OrderRequest::market_buy(
            VenueId::EXCHANGE_A,
            Symbol::new("BTC"),
            0.5,
        );

        let result = mock.submit_order(&order).await;
        assert!(result.is_err());
    }
}