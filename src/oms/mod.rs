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
const MAX_SYMBOLS: usize = 64;

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
    pending_orders: HashMap<String, (OrderRequest, u64)>,
    /// Side-aware cooldown: last trade timestamp per (symbol, side)
    last_trade_per_symbol: HashMap<(String, OrderSide), u64>,
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
        }
    }

    /// Register a kill switch for a venue.
    pub fn register_kill_switch(&mut self, venue: VenueId, kill_switch: Arc<AtomicBool>) {
        self.net_delta.register_kill_switch(venue, kill_switch);
    }

    /// Seed initial positions from the exchange (Startup Sync)
    pub fn seed_positions(&mut self, positions: Vec<Position>) {
        for pos in positions {
            tracing::info!("SYNC POSITION: {} on {:?} size={:.4} price={:.4}", 
                pos.symbol.0, pos.venue, pos.size, pos.entry_price);
            
            // Create a pseudo-fill to update net delta
            let side = if pos.size > 0.0 { OrderSide::Buy } else { OrderSide::Sell };
            let fill = FillEvent {
                order_id: crate::eal::OrderId(0),
                client_order_id: "BOOTSTRAP".to_string(),
                venue: pos.venue,
                symbol: pos.symbol.clone(),
                side,
                filled_size: pos.size.abs(),
                avg_price: pos.entry_price,
                fee: 0.0,
                fee_currency: "USDC".to_string(),
                timestamp_ns: pos.timestamp_ns,
            };
            self.net_delta.update_position(&fill);
        }
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

        // Exposure and Pending Check: don't double-dip on the same symbol before fill
        let sig_sym = signal.symbol.normalize();
        let pending_count = self.pending_orders.values()
            .filter(|(o, _)| o.symbol.normalize() == sig_sym)
            .count();
            
        if pending_count > 0 {
            return Err(RiskError::ExecutionFailed(format!(
                "Already have {} pending orders for {}. Skipping signal.",
                pending_count, signal.symbol
            )));
        }

        // Position cap: $100 max notional per (venue, symbol), direction-aware.
        // Check per-symbol exposure limits
        let max_position_notional = self.risk_settings.max_position_usd;
        
        let mut current_size = self.net_delta.position_size(signal.target_venue, &signal.symbol);
        let mut current_notional = self.net_delta.position_notional(signal.target_venue, &signal.symbol);
        
        let sig_sym_norm = signal.symbol.normalize();
        for (pending, _) in self.pending_orders.values() {
            if pending.symbol.normalize() == sig_sym_norm && pending.venue == signal.target_venue {
                let pending_signed = match pending.side {
                    OrderSide::Buy => pending.size,
                    OrderSide::Sell => -pending.size,
                };
                current_size += pending_signed;
                current_notional += pending.size * current_price;
            }
        }
        
        let _trade_notional = self.risk_settings.max_notional_usd;

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

        // Calculate order size based on max notional.
        // For impulse-obi strategy, correlation_r carries the signal strength factor
        // (set by the signal pipeline as impulse_bps / impulse_threshold_bps, clamped 0.5-2.0).
        // Larger impulse magnitude → proportionally larger position, up to 2x base.
        let base_size = self.risk_settings.max_notional_usd / current_price;
        let strength_factor = if self.strategy_settings.active_strategy == "impulse_obi"
            && signal.correlation_r > 0.0
        {
            signal.correlation_r.clamp(0.5, 2.0)
        } else {
            1.0
        };
        let size = base_size * strength_factor;
        tracing::debug!(
            "Order size: base={:.6} × strength={:.2} = {:.6} (price={:.4})",
            base_size, strength_factor, size, current_price
        );

        // Create order request: Passive Limit Entry
        let order = if self.strategy_settings.active_strategy == "impulse_obi" {
             // For impulse lead-lag, use Post-Only Limit at mid-price to save 5bps taker fee.
             OrderRequest::limit(
                signal.target_venue,
                signal.symbol.clone(),
                signal.side,
                size,
                current_price,
                true, // post_only
                false // reduce_only
             )
        } else {
            OrderRequest {
                venue: signal.target_venue,
                symbol: signal.symbol.clone(),
                side: signal.side,
                order_type: crate::eal::OrderType::IOC,
                size,
                price: None,
                post_only: false,
                reduce_only: false,
                client_order_id: format!("0x{:032x}", uuid::Uuid::new_v4().as_u128()),
            }
        };

        // Record last trade time for cooldown (intent)
        self.last_trade_per_symbol.insert(cooldown_key.clone(), now);

        // Store pending order
        self.pending_orders
            .insert(order.client_order_id.clone(), (order.clone(), now));

        // Submit order
        match executor.submit_order(&order).await {
            Ok(ack) => {
                // Update side-aware cooldown
                self.last_trade_per_symbol.insert(cooldown_key, now);

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
    /// Returns an optional Take Profit order if an entry was filled.
    pub fn process_fill(&mut self, fill: &FillEvent) -> Option<OrderRequest> {
        let venue_key = format!("{:?}", fill.venue);
        let sym_key = fill.symbol.0.clone();
        let _cap_key = (venue_key.clone(), sym_key.clone());

        // Handle pending order decay for partial fills (Internal Ripple Effect B)
        if let Some((pending, _)) = self.pending_orders.get_mut(&fill.client_order_id) {
            pending.size -= fill.filled_size;
            if pending.size <= 0.000001 {
                self.pending_orders.remove(&fill.client_order_id);
            }
        }

        // Update net delta
        self.net_delta.update_position(fill);

        // Store open timestamp for time-based exit (start the clock on FILL, not signal)
        // Handled directly by net_delta stamping `timestamp_ns`
        
        let current_size = self.net_delta.position_size(fill.venue, &fill.symbol);
        
        let is_entry = (current_size > 0.0 && fill.side == OrderSide::Buy)
            || (current_size < 0.0 && fill.side == OrderSide::Sell);
        
        // If we filled a BUY, we want to SELL at entry + profit bps
        // If we filled a SELL, we want to BUY at entry - profit bps
        if is_entry && self.strategy_settings.active_strategy == "impulse_obi" {
            let tp_bps = self.strategy_settings.take_profit_bps; 
            let tp_price = if current_size > 0.0 {
                fill.avg_price * (1.0 + (tp_bps / 10000.0))
            } else {
                fill.avg_price * (1.0 - (tp_bps / 10000.0))
            };
            
            let exit_side = if current_size > 0.0 { OrderSide::Sell } else { OrderSide::Buy };
            
            let tp_order = OrderRequest::limit(
                fill.venue,
                fill.symbol.clone(),
                exit_side,
                fill.filled_size,
                tp_price,
                false, // Closing order shouldn't be post-only to ensure we exit at the profit target
                true   // reduce_only
            );
            
            return Some(tp_order);
        }
        
        None
    }

    /// Check for self-trade.
    fn check_self_trade(&self, signal: &TradeSignal) -> Result<(), RiskError> {
        for (pending, _) in self.pending_orders.values() {
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

    /// Check all positions for time-based exits.
    /// Returns exit TradeSignals for positions older than exit_timeout_ms.
    /// Update strategy settings for hot-reload.
    pub fn update_strategy_settings(&mut self, settings: crate::config::StrategySettings) {
        self.strategy_settings = settings;
    }

    pub fn check_time_exits(&self) -> Vec<TradeSignal> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64;
        let _exit_timeout_ns = self.strategy_settings.exit_timeout_ms * 1_000_000;

        let mut exits = Vec::new();

        for pos in self.net_delta.positions() {
            let current_size = pos.size;
            if current_size.abs() < 1e-6 { continue; }
            
            // Per-Symbol Timeout Lookup
            let timeout_ms = self.strategy_settings.symbol_timeouts
                .get(&pos.symbol.0) // Symbol part of (Venue, Symbol) key
                .copied()
                .unwrap_or(self.strategy_settings.exit_timeout_ms);
                
            let open_ts = pos.timestamp_ns;
            if now > open_ts && (now - open_ts) > (timeout_ms * 1_000_000) {
                let exit_side = if current_size > 0.0 { OrderSide::Sell } else { OrderSide::Buy };
                let venue = pos.venue;

                let age_ms = (now.saturating_sub(open_ts)) as f64 / 1_000_000.0;
                tracing::info!(
                    "TIME EXIT: {} on {:?} held for {:.0}ms (timeout={}ms)",
                    pos.symbol, venue, age_ms, timeout_ms
                );

                exits.push(TradeSignal {
                    side: exit_side,
                    target_venue: venue,
                    symbol: pos.symbol.clone(),
                    correlation_r: 0.0,
                    lag_offset_ns: 0,
                    timestamp_ns: now,
                });
            }
        }
        exits
    }

    /// Process an exit signal — bypasses cooldown, self-trade, and position cap.
    pub async fn process_exit_signal(
        &mut self,
        signal: &TradeSignal,
        _current_price: f64,
        executor: &dyn OrderExecution,
    ) -> Result<OrderAck, RiskError> {
        let current_size = self.net_delta.position_size(signal.target_venue, &signal.symbol);

        if current_size == 0.0 {
            return Err(RiskError::ExecutionFailed("No position to exit".to_string()));
        }

        let exit_size = current_size.abs();

        let order = OrderRequest {
            venue: signal.target_venue,
            symbol: signal.symbol.clone(),
            side: signal.side,
            order_type: crate::eal::OrderType::Market,
            size: exit_size,
            price: None,
            post_only: false,
            reduce_only: true,
            client_order_id: format!("0x{:032x}", uuid::Uuid::new_v4().as_u128()),
        };

        match executor.submit_order(&order).await {
            Ok(ack) => {
                // Exposure recalculates natively when the fill arrives.
                Ok(ack)
            }
            Err(e) => Err(RiskError::ExecutionFailed(e.to_string())),
        }
    }

    /// Check for stale pending orders and trigger cancellation.
    pub async fn check_pending_ttl(&mut self, executor: &dyn OrderExecution) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64;
        // TTL from strategy settings (signal_timeout_ms)
        let ttl_ns = self.strategy_settings.signal_timeout_ms * 1_000_000;

        let mut to_cancel = Vec::new();

        for (id, (order, sent_at)) in &self.pending_orders {
            if now > *sent_at && (now - *sent_at) > ttl_ns {
                to_cancel.push((id.clone(), order.symbol.clone()));
            }
        }

        for (cloid, symbol) in to_cancel {
            tracing::info!("TTL EXPIRED: Canceling stale pending order {} for {}", cloid, symbol.0);
            let _ = executor.cancel_order_by_cloid(&symbol, &cloid).await;
            self.pending_orders.remove(&cloid);
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
        let mut delta = NetDelta::new(500.0);

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