//! Exchange Abstraction Layer (EAL) module.
//!
//! Provides trait-based abstractions for exchange connectivity.
//! Both live exchanges and mock implementations satisfy these traits.

mod binance;
mod hyperliquid;
mod mock;
mod types;

pub use binance::BinanceExchange;
pub use hyperliquid::HyperliquidExchange;
pub use mock::MockExchange;
pub use types::*;

use async_trait::async_trait;
use crossbeam_channel::Sender;
use std::sync::Arc;

// ============================================================================
// Core Traits
// ============================================================================

/// Trait for subscribing to market data from an exchange.
///
/// Implementations must push ticks and book updates into the provided channels.
/// The hot path consumes from these channels via the sync-async bridge.
#[async_trait]
pub trait MarketData: Send + Sync {
    /// Subscribe to trade ticks for the given symbols.
    ///
    /// Returns a receiver that will get tick updates.
    async fn subscribe_ticks(
        &self,
        symbols: &[Symbol],
    ) -> Result<crossbeam_channel::Receiver<Arc<Tick>>, ExchangeError>;

    /// Subscribe to L2 order book updates for the given symbol.
    ///
    /// Returns a receiver that will get book updates.
    async fn subscribe_book(
        &self,
        symbol: &Symbol,
    ) -> Result<crossbeam_channel::Receiver<Arc<BookUpdate>>, ExchangeError>;

    /// Get the venue ID for this exchange.
    fn venue_id(&self) -> VenueId;
}

/// Trait for executing orders on an exchange.
///
/// Implementations handle the actual order submission, cancellation, and position queries.
#[async_trait]
pub trait OrderExecution: Send + Sync {
    /// Submit an order to the exchange.
    async fn submit_order(&self, order: &OrderRequest) -> Result<OrderAck, ExecutionError>;

    /// Cancel an existing order.
    async fn cancel_order(&self, order_id: OrderId) -> Result<(), ExecutionError>;

    /// Get current positions.
    async fn get_positions(&self) -> Result<Vec<Position>, ExecutionError>;

    /// Get account state.
    async fn get_account_state(&self) -> Result<AccountState, ExecutionError>;

    /// Get the venue ID for this exchange.
    fn venue_id(&self) -> VenueId;
}

/// Combined trait for full exchange functionality.
///
/// Most exchange implementations will implement both MarketData and OrderExecution.
pub trait Exchange: MarketData + OrderExecution {}

// ============================================================================
// Exchange Error
// ============================================================================

/// Errors that can occur during exchange operations.
#[derive(Debug, Clone, thiserror::Error)]
pub enum ExchangeError {
    #[error("Connection failed: {0}")]
    ConnectionFailed(String),

    #[error("Authentication failed: {0}")]
    AuthFailed(String),

    #[error("WebSocket error: {0}")]
    WebSocketError(String),

    #[error("Parse error: {0}")]
    ParseError(String),

    #[error("Rate limited")]
    RateLimited,

    #[error("Timeout")]
    Timeout,

    #[error("Internal error: {0}")]
    Internal(String),
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_exchange_error_display() {
        let err = ExchangeError::ConnectionFailed("timeout".to_string());
        assert_eq!(err.to_string(), "Connection failed: timeout");
    }
}