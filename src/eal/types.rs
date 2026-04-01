//! Core types for the Exchange Abstraction Layer (EAL).
//!
//! These types are used throughout the bot for market data, orders, and positions.
//! Designed for zero-copy where possible and stack allocation on the hot path.

use serde::{Deserialize, Serialize};
use std::fmt;

// ============================================================================
// Identifiers
// ============================================================================

/// Unique identifier for an exchange venue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct VenueId(pub u8);

impl VenueId {
    pub const EXCHANGE_A: Self = Self(0);
    pub const EXCHANGE_B: Self = Self(1);
}

impl fmt::Display for VenueId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            0 => write!(f, "ExchangeA"),
            1 => write!(f, "ExchangeB"),
            n => write!(f, "Venue({n})"),
        }
    }
}

/// Unique identifier for a trading symbol.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Symbol(pub String);

impl Symbol {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    /// Normalize venue symbol to a canonical form for cross-venue keying.
    /// Strips common suffixes like "USDT", "USDC" so Binance's "ZECUSDT"
    /// matches Hyperliquid's "ZEC".
    pub fn normalize(&self) -> Symbol {
        let s = &self.0;
        if let Some(stripped) = s.strip_suffix("USDT") {
            return Symbol::new(stripped);
        }
        if let Some(stripped) = s.strip_suffix("USDC") {
            return Symbol::new(stripped);
        }
        self.clone()
    }
}

impl fmt::Display for Symbol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Unique identifier for an order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct OrderId(pub u64);

impl fmt::Display for OrderId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ORD-{}", self.0)
    }
}

// ============================================================================
// Market Data Types
// ============================================================================

/// A single price tick from an exchange.
///
/// Designed for zero-copy on the hot path. All fields are stack-allocated.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tick {
    /// Exchange venue this tick came from.
    pub venue: VenueId,
    /// Trading symbol.
    pub symbol: Symbol,
    /// Last traded price.
    pub price: f64,
    /// Trade size.
    pub size: f64,
    /// Exchange timestamp in nanoseconds since epoch.
    pub exchange_ts_ns: u64,
    /// Local arrival timestamp in nanoseconds since epoch.
    pub local_ts_ns: u64,
}

/// Side of the order book or trade.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Side {
    Bid,
    Ask,
}

impl fmt::Display for Side {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Side::Bid => write!(f, "BID"),
            Side::Ask => write!(f, "ASK"),
        }
    }
}

/// A single level in the L2 order book.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct BookLevel {
    pub price: f64,
    pub size: f64,
}

/// L2 order book update from an exchange.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BookUpdate {
    /// Exchange venue.
    pub venue: VenueId,
    /// Trading symbol.
    pub symbol: Symbol,
    /// Bid levels (sorted descending by price).
    pub bids: Vec<BookLevel>,
    /// Ask levels (sorted ascending by price).
    pub asks: Vec<BookLevel>,
    /// Exchange timestamp in nanoseconds since epoch.
    pub exchange_ts_ns: u64,
    /// Local arrival timestamp in nanoseconds since epoch.
    pub local_ts_ns: u64,
}

impl BookUpdate {
    /// Get the best bid price.
    #[inline(always)]
    pub fn best_bid(&self) -> Option<f64> {
        self.bids.first().map(|l| l.price)
    }

    /// Get the best ask price.
    #[inline(always)]
    pub fn best_ask(&self) -> Option<f64> {
        self.asks.first().map(|l| l.price)
    }

    /// Get the mid price.
    #[inline(always)]
    pub fn mid_price(&self) -> Option<f64> {
        match (self.best_bid(), self.best_ask()) {
            (Some(bid), Some(ask)) => Some((bid + ask) / 2.0),
            _ => None,
        }
    }

    /// Calculate Order Book Imbalance (OBI).
    ///
    /// OBI = (bid_volume - ask_volume) / (bid_volume + ask_volume)
    /// Range: [-1.0, 1.0] where positive means bid-heavy (bullish).
    #[inline(always)]
    pub fn obi(&self, depth: usize) -> f64 {
        let bid_vol: f64 = self.bids.iter().take(depth).map(|l| l.size).sum();
        let ask_vol: f64 = self.asks.iter().take(depth).map(|l| l.size).sum();
        let total = bid_vol + ask_vol;
        if total > 0.0 {
            (bid_vol - ask_vol) / total
        } else {
            0.0
        }
    }
}

// ============================================================================
// Order Types
// ============================================================================

/// Order side (buy or sell).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OrderSide {
    Buy,
    Sell,
}

impl fmt::Display for OrderSide {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OrderSide::Buy => write!(f, "BUY"),
            OrderSide::Sell => write!(f, "SELL"),
        }
    }
}

/// Order type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OrderType {
    /// Market order - execute immediately at best available price.
    Market,
    /// Limit order - execute only at specified price or better.
    Limit,
    /// Immediate-or-Cancel - fill what's available, cancel the rest.
    IOC,
}

impl fmt::Display for OrderType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OrderType::Market => write!(f, "MARKET"),
            OrderType::Limit => write!(f, "LIMIT"),
            OrderType::IOC => write!(f, "IOC"),
        }
    }
}

/// Order request to be sent to an exchange.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderRequest {
    /// Target venue.
    pub venue: VenueId,
    /// Trading symbol.
    pub symbol: Symbol,
    /// Buy or sell.
    pub side: OrderSide,
    /// Order type.
    pub order_type: OrderType,
    /// Order size in base currency.
    pub size: f64,
    /// Limit price (ignored for market orders).
    pub price: Option<f64>,
    /// Post-Only flag (ensure we are a maker).
    pub post_only: bool,
    /// Reduce-only flag (ensure we only close positions).
    pub reduce_only: bool,
    /// Client-generated order ID for idempotency.
    pub client_order_id: String,
}

impl OrderRequest {
    /// Create a market buy order.
    pub fn market_buy(venue: VenueId, symbol: Symbol, size: f64) -> Self {
        Self {
            venue,
            symbol,
            side: OrderSide::Buy,
            order_type: OrderType::Market,
            size,
            price: None,
            post_only: false,
            reduce_only: false,
            client_order_id: uuid::Uuid::new_v4().to_string(),
        }
    }

    /// Create a market sell order.
    pub fn market_sell(venue: VenueId, symbol: Symbol, size: f64) -> Self {
        Self {
            venue,
            symbol,
            side: OrderSide::Sell,
            order_type: OrderType::Market,
            size,
            price: None,
            post_only: false,
            reduce_only: false,
            client_order_id: uuid::Uuid::new_v4().to_string(),
        }
    }

    /// Create a limit order.
    pub fn limit(venue: VenueId, symbol: Symbol, side: OrderSide, size: f64, price: f64, post_only: bool, reduce_only: bool) -> Self {
        Self {
            venue,
            symbol,
            side,
            order_type: OrderType::Limit,
            size,
            price: Some(price),
            post_only,
            reduce_only,
            client_order_id: uuid::Uuid::new_v4().to_string(),
        }
    }

    /// Calculate notional value in USD.
    pub fn notional_usd(&self, price: f64) -> f64 {
        self.size * price
    }
}

/// Order acknowledgment from an exchange.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderAck {
    /// Exchange-assigned order ID.
    pub order_id: OrderId,
    /// Client-generated order ID.
    pub client_order_id: String,
    /// Venue that acknowledged the order.
    pub venue: VenueId,
    /// Timestamp of acknowledgment.
    pub timestamp_ns: u64,
}

/// Fill event from an exchange.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FillEvent {
    /// Order that was filled.
    pub order_id: OrderId,
    /// Client-generated order ID.
    pub client_order_id: String,
    /// Venue where the fill occurred.
    pub venue: VenueId,
    /// Trading symbol.
    pub symbol: Symbol,
    /// Side of the fill.
    pub side: OrderSide,
    /// Filled size.
    pub filled_size: f64,
    /// Average fill price.
    pub avg_price: f64,
    /// Fee paid.
    pub fee: f64,
    /// Fee currency.
    pub fee_currency: String,
    /// Timestamp of fill.
    pub timestamp_ns: u64,
}

// ============================================================================
// Position Types
// ============================================================================

/// Position on a single venue.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Position {
    /// Venue.
    pub venue: VenueId,
    /// Trading symbol.
    pub symbol: Symbol,
    /// Position size (positive = long, negative = short).
    pub size: f64,
    /// Average entry price.
    pub entry_price: f64,
    /// Unrealized PnL.
    pub unrealized_pnl: f64,
    /// Timestamp of last update.
    pub timestamp_ns: u64,
}

/// Account state across all venues.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountState {
    /// Positions per venue.
    pub positions: Vec<Position>,
    /// Total unrealized PnL.
    pub total_unrealized_pnl: f64,
    /// Daily realized PnL.
    pub daily_realized_pnl: f64,
    /// Available balance in USD.
    pub available_balance_usd: f64,
}

// ============================================================================
// Signal Types
// ============================================================================

/// Trade signal generated by the lead-lag detector.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeSignal {
    /// Direction of the trade.
    pub side: OrderSide,
    /// Target venue (the laggard).
    pub target_venue: VenueId,
    /// Symbol to trade.
    pub symbol: Symbol,
    /// Correlation R value at signal generation.
    pub correlation_r: f64,
    /// Lead-lag offset in nanoseconds.
    pub lag_offset_ns: i64,
    /// Timestamp when signal was generated.
    pub timestamp_ns: u64,
    /// Optional explicit price (for exits)
    pub price: Option<f64>,
}

// ============================================================================
// Error Types
// ============================================================================

/// Errors that can occur on the hot path.
///
/// These are stack-allocated enums with no heap allocation.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotPathError {
    InvalidTickData,
    MathOverflow,
    BufferDesync,
    QueueFull,
}

impl fmt::Display for HotPathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HotPathError::InvalidTickData => write!(f, "Invalid tick data"),
            HotPathError::MathOverflow => write!(f, "Math overflow"),
            HotPathError::BufferDesync => write!(f, "Buffer desync"),
            HotPathError::QueueFull => write!(f, "Queue full"),
        }
    }
}

/// Risk management errors.
#[derive(Debug, Clone, thiserror::Error)]
pub enum RiskError {
    #[error("Order exceeds max notional: {notional:.2} > {max:.2}")]
    ExceedsMaxNotional { notional: f64, max: f64 },

    #[error("Daily drawdown limit reached: {drawdown:.2} > {max:.2}")]
    DailyDrawdownLimit { drawdown: f64, max: f64 },

    #[error("Slippage too high: {slippage_bps:.1} bps > {max_bps:.1} bps")]
    ExcessiveSlippage { slippage_bps: f64, max_bps: f64 },

    #[error("Signal expired: age {age_ms}ms > TTL {ttl_ms}ms")]
    SignalExpired { age_ms: u64, ttl_ms: u64 },

    #[error("Self-trade detected")]
    SelfTrade,

    #[error("Kill switch active for venue {venue}")]
    KillSwitchActive { venue: VenueId },

    #[error("Correlation too low: {r:.3} < {min:.3}")]
    CorrelationTooLow { r: f64, min: f64 },

    #[error("Order execution failed: {0}")]
    ExecutionFailed(String),
}

/// Execution errors.
#[derive(Debug, Clone, thiserror::Error)]
pub enum ExecutionError {
    #[error("Exchange error: {0}")]
    ExchangeError(String),

    #[error("Rate limited by {venue}")]
    RateLimited { venue: VenueId },

    #[error("Insufficient balance")]
    InsufficientBalance,

    #[error("Order not found: {0}")]
    OrderNotFound(OrderId),

    #[error("Network timeout")]
    Timeout,

    #[error("Connection lost to {venue}")]
    ConnectionLost { venue: VenueId },
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_book_update_mid_price() {
        let book = BookUpdate {
            venue: VenueId::EXCHANGE_A,
            symbol: Symbol::new("BTC"),
            bids: vec![BookLevel {
                price: 60000.0,
                size: 1.0,
            }],
            asks: vec![BookLevel {
                price: 60001.0,
                size: 1.0,
            }],
            exchange_ts_ns: 0,
            local_ts_ns: 0,
        };
        assert_eq!(book.mid_price(), Some(60000.5));
    }

    #[test]
    fn test_book_update_obi() {
        let book = BookUpdate {
            venue: VenueId::EXCHANGE_A,
            symbol: Symbol::new("BTC"),
            bids: vec![
                BookLevel {
                    price: 60000.0,
                    size: 10.0,
                },
                BookLevel {
                    price: 59999.0,
                    size: 5.0,
                },
            ],
            asks: vec![
                BookLevel {
                    price: 60001.0,
                    size: 2.0,
                },
                BookLevel {
                    price: 60002.0,
                    size: 3.0,
                },
            ],
            exchange_ts_ns: 0,
            local_ts_ns: 0,
        };
        // OBI = (15 - 5) / (15 + 5) = 0.5
        let obi = book.obi(2);
        assert!((obi - 0.5).abs() < 1e-9);
    }

    #[test]
    fn test_order_request_notional() {
        let order = OrderRequest::market_buy(VenueId::EXCHANGE_A, Symbol::new("BTC"), 0.5);
        assert_eq!(order.notional_usd(60000.0), 30000.0);
    }

    #[test]
    fn test_venue_id_display() {
        assert_eq!(VenueId::EXCHANGE_A.to_string(), "ExchangeA");
        assert_eq!(VenueId::EXCHANGE_B.to_string(), "ExchangeB");
    }
}
