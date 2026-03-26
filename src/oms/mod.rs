//! Order Management System (OMS) module.
//!
//! Implements risk pre-flight checks, cross-venue position tracking,
//! and order execution logic.

pub mod preflight;

pub use preflight::PreflightChecker;

use crate::config::{RiskSettings, StrategySettings};
use crate::eal::{
    ExecutionError, FillEvent, OrderAck, OrderExecution, OrderRequest, OrderSide, Position,
    RiskError, Symbol, TradeSignal, VenueId,
};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

/// Cross-venue net delta tracker.
///
/// Tracks positions across both venues and calculates the global net delta.
pub struct NetDelta {
    /// Positions per venue per symbol.
    positions: HashMap<(VenueId, Symbol), Position>,
    /// Daily realized PnL.
    daily_realized_pnl: f64,
    /// Daily loss limit.
    daily_loss_limit: f64,
    /// Kill switch per venue.
    kill_switches: HashMap<VenueId, Arc<AtomicBool>>,
}

impl NetDelta {
    /// Create a new net delta tracker.
    pub fn new(daily_loss_limit: f64) -> Self {
        Self {
            positions: HashMap::new(),
            daily_realized_pnl: 0.0,
            daily_loss_limit,
            kill_switches: HashMap::new(),
        }
    }

    /// Register a kill switch for a venue.
    pub fn register_kill_switch(&mut self, venue: VenueId, kill_switch: Arc<AtomicBool>) {
        self.kill_switches.insert(venue, kill_switch);
    }

    /// Update position after a fill.
    pub fn update_position(&mut self, fill: &FillEvent) {
        let key = (fill.venue, fill.symbol.clone());
        let position = self.positions.entry(key).or_insert(Position {
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
        self.positions
            .iter()
            .filter(|((_, s), _)| s == symbol)
            .map(|(_, p)| p.size)
            .sum()
    }

    /// Get total net delta across all symbols.
    pub fn total_net_delta(&self) -> f64 {
        self.positions.values().map(|p| p.size).sum()
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
        self.kill_switches
            .get(venue)
            .map(|ks| ks.load(Ordering::SeqCst))
            .unwrap_or(false)
    }

    /// Get all positions.
    pub fn positions(&self) -> &HashMap<(VenueId, Symbol), Position> {
        &self.positions
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
    /// Risk settings.
    risk_settings: RiskSettings,
    /// Strategy settings.
    strategy_settings: StrategySettings,
    /// Net delta tracker.
    net_delta: NetDelta,
    /// Preflight checker.
    preflight: PreflightChecker,
    /// Pending orders (for self-trade prevention).
    pending_orders: HashMap<String, OrderRequest>,
}

impl OrderManagementSystem {
    /// Create a new OMS.
    pub fn new(risk_settings: RiskSettings, strategy_settings: StrategySettings) -> Self {
        let net_delta = NetDelta::new(risk_settings.max_drawdown_daily);
        let preflight = PreflightChecker::new(risk_settings.clone(), strategy_settings.clone());

        Self {
            risk_settings,
            strategy_settings,
            net_delta,
            preflight,
            pending_orders: HashMap::new(),
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
        // Run pre-flight checks
        self.preflight.check_signal(signal, current_price, &self.net_delta)?;

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
            Ok(ack) => Ok(ack),
            Err(e) => {
                // Remove pending order on failure
                self.pending_orders.remove(&order.client_order_id);
                Err(RiskError::ExceedsMaxNotional {
                    notional: 0.0,
                    max: 0.0,
                })
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