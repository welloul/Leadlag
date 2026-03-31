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
use tracing::{info, warn};

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
    has_real_data: bool,
}

/// Fill provenance — whether the fill used fresh or stale book data.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FillProvenance {
    Fresh,
    Stale,
}

/// Paper trading simulator.
pub struct PaperSimulator {
    settings: SimulationSettings,
    matchers: Arc<Mutex<HashMap<(Symbol, VenueId), OrderBookMatcher>>>,
    book_states: Arc<Mutex<HashMap<(Symbol, VenueId), VenueBookState>>>,
    order_counter: Arc<Mutex<u64>>,
    positions: Arc<Mutex<Vec<Position>>>,
    daily_pnl: Arc<Mutex<f64>>,
    total_fees: Arc<Mutex<f64>>,
    fill_history: Arc<Mutex<Vec<FillEvent>>>,
    fill_tx: Arc<Mutex<Option<crossbeam_channel::Sender<FillEvent>>>>,
    pending_limits: Arc<Mutex<Vec<OrderRequest>>>,
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
        format!(
            "signals={} fills={} ({:.0}% fresh) skipped={}",
            self.total_signals, total, fresh_pct, self.skipped_no_book + self.skipped_stale_over_2s
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
            fill_tx: Arc::new(Mutex::new(None)),
            pending_limits: Arc::new(Mutex::new(Vec::new())),
            metrics: Arc::new(Mutex::new(SimMetrics::default())),
        }
    }

    pub fn set_fill_tx(&self, tx: crossbeam_channel::Sender<FillEvent>) {
        *self.fill_tx.lock().unwrap() = Some(tx);
    }

    /// Update the order book from a real L2 BookUpdate.
    pub fn update_book(&self, update: BookUpdate) {
        let key = (update.symbol.clone(), update.venue);
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos() as u64;

        {
            let mut matchers = self.matchers.lock().unwrap();
            let matcher = matchers
                .entry(key.clone())
                .or_insert_with(|| OrderBookMatcher::new(self.settings.match_l2_depth));
            matcher.update_book(update.bids, update.asks);
            
            // Check for limit fills
            self.check_limit_fills(matcher, &update.symbol, update.venue);
        }

        {
            let mut states = self.book_states.lock().unwrap();
            let state = states.entry(key).or_default();
            state.last_update_ns = now;
            state.has_real_data = true;
        }
    }

    pub fn update_book_from_tick(&self, symbol: &Symbol, price: f64, venue: VenueId) {
        if price <= 0.0 || !price.is_finite() {
            return;
        }

        let spread_model = VenueSpreadModel::for_venue(venue);
        let half_spread = spread_model.half_spread(price, 5000.0);

        let mut bids = Vec::new();
        let mut asks = Vec::new();

        for i in 0..self.settings.match_l2_depth {
            let offset = price * (i as f64 * 0.5) / 10000.0;
            bids.push(BookLevel { price: price - half_spread - offset, size: 10000.0 });
            asks.push(BookLevel { price: price + half_spread + offset, size: 10000.0 });
        }

        let key = (symbol.clone(), venue);
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos() as u64;

        {
            let mut matchers = self.matchers.lock().unwrap();
            let matcher = matchers
                .entry(key.clone())
                .or_insert_with(|| OrderBookMatcher::new(self.settings.match_l2_depth));
            matcher.update_book(bids, asks);
            self.check_limit_fills(matcher, symbol, venue);
        }

        {
            let mut states = self.book_states.lock().unwrap();
            let state = states.entry(key).or_default();
            state.last_update_ns = now;
            state.has_real_data = true;
        }
    }

    fn check_limit_fills(&self, matcher: &OrderBookMatcher, symbol: &Symbol, venue: VenueId) {
        let mut pending = self.pending_limits.lock().unwrap();
        if pending.is_empty() { return; }

        let mut remaining = Vec::new();
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos() as u64;
        let mut counter = self.order_counter.lock().unwrap();

        for order in pending.drain(..) {
            if &order.symbol != symbol || order.venue != venue {
                remaining.push(order);
                continue;
            }

            let limit_price = order.price.unwrap_or(0.0);
            let filled = match order.side {
                OrderSide::Buy => matcher.best_ask().map_or(false, |p| p <= limit_price),
                OrderSide::Sell => matcher.best_bid().map_or(false, |p| p >= limit_price),
            };

            if filled {
                *counter += 1;
                let fill = FillEvent {
                    order_id: OrderId(*counter),
                    client_order_id: order.client_order_id.clone(),
                    venue: order.venue,
                    symbol: order.symbol.clone(),
                    side: order.side,
                    filled_size: order.size,
                    avg_price: limit_price,
                    fee: 0.0,
                    fee_currency: "USD [MAKER]".to_string(),
                    timestamp_ns: now,
                };
                
                info!("LIMIT FILL: {} {} @ {}", fill.side, fill.symbol, fill.avg_price);
                self.fill_history.lock().unwrap().push(fill.clone());
                if let Some(ref tx) = *self.fill_tx.lock().unwrap() {
                    let _ = tx.send(fill);
                }
            } else {
                remaining.push(order);
            }
        }
        *pending = remaining;
    }

    pub fn is_venue_liquid(&self, symbol: &Symbol, venue: VenueId) -> bool {
        let matchers = self.matchers.lock().unwrap();
        matchers.get(&(symbol.clone(), venue)).map_or(false, |m| !m.bids.is_empty())
    }

    pub fn book_staleness_ns(&self, symbol: &Symbol, venue: VenueId) -> Option<u64> {
        let states = self.book_states.lock().unwrap();
        states.get(&(symbol.clone(), venue)).and_then(|s| {
            if s.has_real_data {
                let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos() as u64;
                Some(now.saturating_sub(s.last_update_ns))
            } else { None }
        })
    }

    pub fn get_mid_price(&self, symbol: &Symbol, venue: VenueId) -> Option<f64> {
        let matchers = self.matchers.lock().unwrap();
        matchers.get(&(symbol.clone(), venue)).and_then(|m| m.mid_price())
    }

    async fn simulate_fill(&self, order: &OrderRequest, _prov: FillProvenance) -> Result<FillEvent, ExecutionError> {
        if self.settings.latency_simulation_ms > 0 {
            tokio::time::sleep(Duration::from_millis(self.settings.latency_simulation_ms)).await;
        }

        let mut matchers = self.matchers.lock().unwrap();
        let matcher = matchers.get_mut(&(order.symbol.clone(), order.venue))
            .ok_or_else(|| ExecutionError::ExchangeError("No book".to_string()))?;

        let (filled_size, avg_price, _) = matcher.match_order(order.side, order.size, order.price)?;

        let fee = filled_size * avg_price * (self.settings.fee_tier_bps / 10000.0);
        *self.total_fees.lock().unwrap() += fee;

        let mut counter = self.order_counter.lock().unwrap();
        *counter += 1;
        
        let fill = FillEvent {
            order_id: OrderId(*counter),
            client_order_id: order.client_order_id.clone(),
            venue: order.venue,
            symbol: order.symbol.clone(),
            side: order.side,
            filled_size,
            avg_price,
            fee,
            fee_currency: "USD [TAKER]".to_string(),
            timestamp_ns: SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos() as u64,
        };

        self.fill_history.lock().unwrap().push(fill.clone());
        Ok(fill)
    }

    pub fn total_fees(&self) -> f64 {
        *self.total_fees.lock().unwrap()
    }

    pub fn fill_history(&self) -> Vec<FillEvent> {
        self.fill_history.lock().unwrap().clone()
    }

    pub fn metrics(&self) -> SimMetrics {
        self.metrics.lock().unwrap().clone()
    }
}

#[async_trait]
impl OrderExecution for PaperSimulator {
    async fn submit_order(&self, order: &OrderRequest) -> Result<OrderAck, ExecutionError> {
        let staleness = self.book_staleness_ns(&order.symbol, order.venue);
        let has_book = self.is_venue_liquid(&order.symbol, order.venue);

        self.metrics.lock().unwrap().total_signals += 1;

        let provenance = match (has_book, staleness) {
            (false, _) => return Err(ExecutionError::ExchangeError("No book".to_string())),
            (true, None) => FillProvenance::Stale,
            (true, Some(age)) => {
                if age <= MAX_BOOK_AGE_NS { FillProvenance::Fresh }
                else { return Err(ExecutionError::ExchangeError("Stale book".to_string())); }
            }
        };

        if order.order_type == crate::eal::OrderType::Limit {
            if order.post_only {
                let matchers = self.matchers.lock().unwrap();
                if let Some(matcher) = matchers.get(&(order.symbol.clone(), order.venue)) {
                    let p = order.price.unwrap_or(0.0);
                    let cross = match order.side {
                        OrderSide::Buy => matcher.best_ask().map_or(false, |ask| ask <= p),
                        OrderSide::Sell => matcher.best_bid().map_or(false, |bid| bid >= p),
                    };
                    if cross { return Err(ExecutionError::ExchangeError("Post-Only would cross".to_string())); }
                }
            }
            self.pending_limits.lock().unwrap().push(order.clone());
            return Ok(OrderAck {
                order_id: OrderId(0),
                client_order_id: order.client_order_id.clone(),
                venue: order.venue,
                timestamp_ns: SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos() as u64,
            });
        }

        let fill = self.simulate_fill(order, provenance).await?;
        
        let mut positions = self.positions.lock().unwrap();
        let pos = positions.iter_mut().find(|p| p.venue == order.venue && p.symbol == order.symbol);
        let signed = match fill.side { OrderSide::Buy => fill.filled_size, OrderSide::Sell => -fill.filled_size };

        if let Some(p) = pos {
            if p.size == 0.0 { p.entry_price = fill.avg_price; }
            p.size += signed;
            p.timestamp_ns = fill.timestamp_ns;
        } else {
            positions.push(Position {
                venue: order.venue, symbol: order.symbol.clone(), size: signed,
                entry_price: fill.avg_price, unrealized_pnl: 0.0, timestamp_ns: fill.timestamp_ns,
            });
        }

        Ok(OrderAck {
            order_id: fill.order_id, client_order_id: order.client_order_id.clone(),
            venue: order.venue, timestamp_ns: fill.timestamp_ns,
        })
    }

    async fn cancel_order(&self, _id: OrderId) -> Result<(), ExecutionError> { Ok(()) }
    async fn get_positions(&self) -> Result<Vec<Position>, ExecutionError> { Ok(self.positions.lock().unwrap().clone()) }
    async fn get_account_state(&self) -> Result<AccountState, ExecutionError> {
        let positions = self.positions.lock().unwrap().clone();
        Ok(AccountState {
            positions, total_unrealized_pnl: 0.0,
            daily_realized_pnl: *self.daily_pnl.lock().unwrap(), available_balance_usd: 100000.0,
        })
    }
    fn venue_id(&self) -> VenueId { VenueId::EXCHANGE_A }
}
