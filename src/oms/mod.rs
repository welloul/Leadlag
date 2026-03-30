//! Order Management System (OMS) module.
//!
//! Implements risk pre-flight checks, cross-venue position tracking,
//! and order execution logic.

pub mod preflight;

pub use preflight::PreflightChecker;

use crate::config::{RiskSettings, StrategySettings};
use crate::eal::{
    FillEvent, OrderAck, OrderExecution, OrderRequest, OrderSide, Position,
    RiskError, Symbol, TradeSignal, VenueId,
};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Maximum number of symbols supported.
const MAX_SYMBOLS: usize = 16;

/// Maximum number of venues supported.
const MAX_VENUES: usize = 2;

/// Cross-venue net delta tracker.
///
/// Tracks positions across both venues and calculates the global net delta.
/// Uses fixed-size arrays for zero-cost indexing instead of HashMap.
pub struct NetDelta {
    /// Positions indexed by [venue][symbol_index].
    /// Using Option<Position> to handle uninitialized slots.
    positions: [[Option<Position>; MAX_SYMBOLS]; MAX_VENUES],
    /// Symbol to index mapping (for fast lookup).
    symbol_indices: Vec<(Symbol, usize)>,
    /// Daily realized PnL.
    daily_realized_pnl: f64,
    /// Daily loss limit.
    daily_loss_limit: f64,
    /// Kill switch per venue (indexed by venue.0).
    kill_switches: [Option<Arc<AtomicBool>>; MAX_VENUES],
}

impl NetDelta {
    /// Create a new net delta tracker.
    pub fn new(daily_loss_limit: f64) -> Self {
        Self {
            positions: std::array::from_fn(|_| std::array::from_fn(|_| None)),
            symbol_indices: Vec::new(),
            daily_realized_pnl: 0.0,
            daily_loss_limit,
            kill_switches: std::array::from_fn(|_| None),
        }
    }

    /// Get or create symbol index for fast lookup.
    fn get_symbol_index(&mut self, symbol: &Symbol) -> usize {
        // Check if symbol already exists
        for (sym, idx) in &self.symbol_indices {
            if sym == symbol {
                return *idx;
            }
        }
        
        // Add new symbol
        let idx = self.symbol_indices.len();
        if idx >= MAX_SYMBOLS {
            panic!("Exceeded maximum number of symbols ({})", MAX_SYMBOLS);
        }
        self.symbol_indices.push((symbol.clone(), idx));
        idx
    }

    /// Register a kill switch for a venue.
    pub fn register_kill_switch(&mut self, venue: VenueId, kill_switch: Arc<AtomicBool>) {
        self.kill_switches[venue.0 as usize] = Some(kill_switch);
    }

    /// Update position after a fill.
    pub fn update_position(&mut self, fill: &FillEvent) {
        let venue_idx = fill.venue.0 as usize;
        let symbol_idx = self.get_symbol_index(&fill.symbol);
        
        // Get or create position
        let position = self.positions[venue_idx][symbol_idx].get_or_insert_with(|| Position {
            venue: fill.venue,
            symbol: fill.symbol.clone(),
            size: 0.0,
            entry_price: 0.0,
            unrealized_pnl: 0.0,
            timestamp_ns: fill.timestamp_ns,
        });

        // Update position size
        let signed_size = match fill.side {
            OrderSide::Buy => fill.filled_size,
            OrderSide::Sell => -fill.filled_size,
        };

        // Calculate new average entry price
        if position.size == 0.0 {
            position.entry_price = fill.avg_price;
            position.size += signed_size;
        } else if (position.size > 0.0 && signed_size > 0.0)
            || (position.size < 0.0 && signed_size < 0.0)
        {
            // Adding to position
            let total_value = (position.size * position.entry_price) + (signed_size * fill.avg_price);
            position.size += signed_size;
            if position.size != 0.0 {
                position.entry_price = total_value / position.size;
            }
        } else {
            // Reducing position
            let pnl = if position.size > 0.0 {
                (fill.avg_price - position.entry_price) * signed_size.abs()
            } else {
                (position.entry_price - fill.avg_price) * signed_size.abs()
            };
            self.daily_realized_pnl += pnl;
            position.size += signed_size;
        }

        position.timestamp_ns = fill.timestamp_ns;
    }

    /// Get net delta for a symbol across all venues.
    pub fn net_delta(&self, symbol: &Symbol) -> f64 {
        let mut total = 0.0;
        for venue_positions in &self.positions {
            for pos in venue_positions.iter().flatten() {
                if pos.symbol == *symbol {
                    total += pos.size;
                }
            }
        }
        total
    }

    /// Get position notional for a (venue, symbol) pair.
    /// Returns 0.0 if no position exists.
    pub fn position_notional(&self, venue: VenueId, symbol: &Symbol) -> f64 {
        let venue_idx = venue.0 as usize;
        for (sym, idx) in &self.symbol_indices {
            if sym == symbol {
                if let Some(ref pos) = self.positions[venue_idx][*idx] {
                    return pos.size.abs() * pos.entry_price;
                }
            }
        }
        0.0
    }

    /// Get signed position size for a (venue, symbol) pair.
    /// Positive = LONG, Negative = SHORT, 0.0 = flat.
    pub fn position_size(&self, venue: VenueId, symbol: &Symbol) -> f64 {
        let venue_idx = venue.0 as usize;
        for (sym, idx) in &self.symbol_indices {
            if sym == symbol {
                if let Some(ref pos) = self.positions[venue_idx][*idx] {
                    return pos.size;
                }
            }
        }
        0.0
    }

    /// Get total net delta across all symbols.
    pub fn total_net_delta(&self) -> f64 {
        let mut total = 0.0;
        for venue_positions in &self.positions {
            for pos in venue_positions.iter().flatten() {
                total += pos.size;
            }
        }
        total
    }

    /// Check if daily loss limit is breached.
    pub fn is_daily_loss_limit_breached(&self) -> bool {
        self.daily_realized_pnl <= -self.daily_loss_limit
    }

    /// Get daily realized PnL.
    pub fn daily_realized_pnl(&self) -> f64 {
        self.daily_realized_pnl
    }

    /// Check if any venue kill switch is active.
    pub fn is_kill_switch_active(&self, venue: &VenueId) -> bool {
        self.kill_switches[venue.0 as usize]
            .as_ref()
            .map(|ks| ks.load(Ordering::SeqCst))
            .unwrap_or(false)
    }

    /// Get all positions as a vector.
    pub fn positions(&self) -> Vec<&Position> {
        let mut result = Vec::new();
        for venue_positions in &self.positions {
            for pos in venue_positions.iter().flatten() {
                result.push(pos);
            }
        }
        result
    }

    /// Reset daily PnL (call at start of new trading day).
    pub fn reset_daily_pnl(&mut self) {
        self.daily_realized_pnl = 0.0;
    }
}

/// Order Management System.
///
/// Orchestrates order execution with risk pre-flight checks.
pub struct OrderManagementSystem {
    risk_settings: RiskSettings,
    strategy_settings: StrategySettings,
    net_delta: NetDelta,
    preflight: PreflightChecker,
    pending_orders: HashMap<String, OrderRequest>,
    /// Side-aware cooldown: last trade timestamp per (symbol, side)
    last_trade_per_symbol: HashMap<(String, OrderSide), u64>,
    /// Cumulative position notional per (venue, symbol) — tracked at order time
    /// This is used for position cap BEFORE fills arrive
    cumulative_notional: HashMap<(String, String), f64>,  // (venue_str, symbol) → notional
    /// Signed position size per (venue, symbol) for direction-aware checks
    cumulative_size: HashMap<(String, String), f64>,       // (venue_str, symbol) → signed size
}

impl OrderManagementSystem {
    pub fn new(risk_settings: RiskSettings, strategy_settings: StrategySettings) -> Self {
        let net_delta = NetDelta::new(risk_settings.max_drawdown_daily);
        let preflight = PreflightChecker::new(risk_settings.clone(), strategy_settings.clone());

        Self {
            risk_settings,
            strategy_settings,
            net_delta,
            preflight,
            pending_orders: HashMap::new(),
            last_trade_per_symbol: HashMap::new(),
            cumulative_notional: HashMap::new(),
            cumulative_size: HashMap::new(),
        }
    }

    /// Register a kill switch for a venue.
    pub fn register_kill_switch(&mut self, venue: VenueId, kill_switch: Arc<AtomicBool>) {
        self.net_delta.register_kill_switch(venue, kill_switch);
    }

    /// Process a trade signal and generate an order if pre-flight checks pass.
    pub async fn process_signal(
        &mut self,
        signal: &TradeSignal,
        current_price: f64,
        executor: &dyn OrderExecution,
    ) -> Result<OrderAck, RiskError> {
        // Side-aware cooldown: don't re-trade same (symbol, side) within cooldown window
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64;
        let cooldown_ns = self.strategy_settings.cooldown_ms * 1_000_000;
        let cooldown_key = (signal.symbol.0.clone(), signal.side);

        if let Some(last) = self.last_trade_per_symbol.get(&cooldown_key) {
            if now - *last < cooldown_ns {
                return Err(RiskError::ExecutionFailed(format!(
                    "Cooldown: {:.0}ms since last {:?} {}",
                    (now - *last) as f64 / 1e6, signal.side, signal.symbol
                )));
            }
        }

        // Run pre-flight checks
        self.preflight.check_signal(signal, current_price, &self.net_delta)?;

        // Position cap: $100 max notional per (venue, symbol), direction-aware.
        let max_position_notional = 100.0;
        let venue_key = format!("{:?}", signal.target_venue);
        let sym_key = signal.symbol.0.clone();
        let cap_key = (venue_key, sym_key);

        let current_notional = *self.cumulative_notional.get(&cap_key).unwrap_or(&0.0);
        let current_size = *self.cumulative_size.get(&cap_key).unwrap_or(&0.0);
        let trade_notional = self.risk_settings.max_notional_usd;

        if current_notional >= max_position_notional {
            // At cap — only allow if this trade REDUCES the position
            let would_reduce = (current_size > 0.0 && signal.side == OrderSide::Sell)
                || (current_size < 0.0 && signal.side == OrderSide::Buy);
            if !would_reduce {
                return Err(RiskError::ExecutionFailed(format!(
                    "Position cap: {} on {:?} is ${:.0} notional (cap=${:.0})",
                    signal.symbol, signal.target_venue, current_notional, max_position_notional
                )));
            }
        }

        // Check for self-trade
        if self.risk_settings.self_trade_prevention {
            self.check_self_trade(signal)?;
        }

        // Calculate order size based on max notional
        let size = self.risk_settings.max_notional_usd / current_price;

        // Create order request
        let order = OrderRequest {
            venue: signal.target_venue,
            symbol: signal.symbol.clone(),
            side: signal.side,
            order_type: crate::eal::OrderType::IOC,
            size,
            price: None,
            client_order_id: uuid::Uuid::new_v4().to_string(),
        };

        // Store pending order
        self.pending_orders
            .insert(order.client_order_id.clone(), order.clone());

        // Submit order
        match executor.submit_order(&order).await {
            Ok(ack) => {
                // Update side-aware cooldown
                self.last_trade_per_symbol.insert(cooldown_key, now);

                // Update cumulative position tracking
                let signed_size = match signal.side {
                    OrderSide::Buy => size,
                    OrderSide::Sell => -size,
                };
                *self.cumulative_notional.entry(cap_key.clone()).or_insert(0.0) += trade_notional;
                *self.cumulative_size.entry(cap_key).or_insert(0.0) += signed_size;

                Ok(ack)
            }
            Err(e) => {
                // Remove pending order on failure
                self.pending_orders.remove(&order.client_order_id);
                Err(RiskError::ExecutionFailed(e.to_string()))
            }
        }
    }

    /// Process a fill event.
    pub fn process_fill(&mut self, fill: &FillEvent) {
        // Remove from pending orders
        self.pending_orders.remove(&fill.client_order_id);

        // Update net delta
        self.net_delta.update_position(fill);
    }

    /// Check for self-trade.
    fn check_self_trade(&self, signal: &TradeSignal) -> Result<(), RiskError> {
        for pending in self.pending_orders.values() {
            if pending.symbol == signal.symbol && pending.venue == signal.target_venue {
                // Check if opposite side
                if (pending.side == OrderSide::Buy && signal.side == OrderSide::Sell)
                    || (pending.side == OrderSide::Sell && signal.side == OrderSide::Buy)
                {
                    return Err(RiskError::SelfTrade);
                }
            }
        }
        Ok(())
    }

    /// Get net delta tracker.
    pub fn net_delta(&self) -> &NetDelta {
        &self.net_delta
    }

    /// Get mutable net delta tracker.
    pub fn net_delta_mut(&mut self) -> &mut NetDelta {
        &mut self.net_delta
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_net_delta_tracking() {
        let mut delta = NetDelta::new(1000.0);

        let fill = FillEvent {
            order_id: crate::eal::OrderId(1),
            client_order_id: "test".to_string(),
            venue: VenueId::EXCHANGE_A,
            symbol: Symbol::new("BTC"),
            side: OrderSide::Buy,
            filled_size: 0.5,
            avg_price: 60000.0,
            fee: 7.5,
            fee_currency: "USD".to_string(),
            timestamp_ns: 0,
        };

        delta.update_position(&fill);
        assert_eq!(delta.net_delta(&Symbol::new("BTC")), 0.5);
    }

    #[test]
    fn test_daily_loss_limit() {
        let mut delta = NetDelta::new(100.0);

        // Simulate a loss
        let fill = FillEvent {
            order_id: crate::eal::OrderId(1),
            client_order_id: "test".to_string(),
            venue: VenueId::EXCHANGE_A,
            symbol: Symbol::new("BTC"),
            side: OrderSide::Buy,
            filled_size: 1.0,
            avg_price: 60000.0,
            fee: 0.0,
            fee_currency: "USD".to_string(),
            timestamp_ns: 0,
        };

        delta.update_position(&fill);

        // Close position at a loss
        let close_fill = FillEvent {
            order_id: crate::eal::OrderId(2),
            client_order_id: "test2".to_string(),
            venue: VenueId::EXCHANGE_A,
            symbol: Symbol::new("BTC"),
            side: OrderSide::Sell,
            filled_size: 1.0,
            avg_price: 59900.0, // $100 loss
            fee: 0.0,
            fee_currency: "USD".to_string(),
            timestamp_ns: 1,
        };

        delta.update_position(&close_fill);
        assert!(delta.is_daily_loss_limit_breached());
    }
}