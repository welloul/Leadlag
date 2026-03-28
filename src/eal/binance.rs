//! Binance Futures exchange implementation for market data.
//!
//! Connects to Binance WebSocket for real-time trade data.
//! No API keys required for market data.

use async_trait::async_trait;
use crossbeam_channel::Sender;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio_tungstenite::{connect_async, tungstenite::Message};

use super::{ExchangeError, MarketData, Symbol, Tick, VenueId};

/// Binance trade message from WebSocket
#[derive(Debug, Deserialize)]
struct BinanceTrade {
    #[serde(rename = "s")]
    symbol: String,
    #[serde(rename = "p")]
    price: String,
    #[serde(rename = "q")]
    quantity: String,
    #[serde(rename = "T")]
    timestamp: u64,
}

/// Binance exchange for market data
pub struct BinanceExchange {
    venue_id: VenueId,
    tick_senders: Arc<Mutex<HashMap<Symbol, Sender<Arc<Tick>>>>>,
}

impl BinanceExchange {
    pub fn new() -> Self {
        Self {
            venue_id: VenueId::EXCHANGE_A,
            tick_senders: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    async fn connect_websocket(
        &self,
        symbol: &Symbol,
        sender: Sender<Arc<Tick>>,
    ) -> Result<(), ExchangeError> {
        // Binance Futures uses {symbol}usdt@trade format (e.g., zecusdt@trade)
        let ws_symbol = format!("{}usdt", symbol.0.to_lowercase());
        let url = format!("wss://fstream.binance.com/ws/{ws_symbol}@trade");

        let (ws_stream, _) = connect_async(&url)
            .await
            .map_err(|e| ExchangeError::ConnectionFailed(e.to_string()))?;

        let (_, mut read) = ws_stream.split();

        tokio::spawn(async move {
            while let Some(msg) = read.next().await {
                if let Ok(Message::Text(text)) = msg {
                    if let Ok(trade) = serde_json::from_str::<BinanceTrade>(&text) {
                        let price: f64 = trade.price.parse().unwrap_or(0.0);
                        let size: f64 = trade.quantity.parse().unwrap_or(0.0);

                        let tick = Tick {
                            venue: VenueId::EXCHANGE_A,
                            symbol: Symbol::new(&trade.symbol),
                            price,
                            size,
                            exchange_ts_ns: trade.timestamp * 1_000_000,
                            local_ts_ns: std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap()
                                .as_nanos() as u64,
                        };

                        let _ = sender.send(Arc::new(tick));
                    }
                }
            }
        });

        Ok(())
    }
}

impl Default for BinanceExchange {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl MarketData for BinanceExchange {
    async fn subscribe_ticks(
        &self,
        symbols: &[Symbol],
    ) -> Result<crossbeam_channel::Receiver<Arc<Tick>>, ExchangeError> {
        let (tx, rx) = crossbeam_channel::bounded(1024);

        for symbol in symbols {
            let sender = tx.clone();
            self.connect_websocket(symbol, sender).await?;
        }

        Ok(rx)
    }

    async fn subscribe_book(
        &self,
        _symbol: &Symbol,
    ) -> Result<crossbeam_channel::Receiver<Arc<super::BookUpdate>>, ExchangeError> {
        // TODO: Implement order book subscription
        Err(ExchangeError::Internal("Not implemented".to_string()))
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

    #[test]
    fn test_binance_exchange_creation() {
        let exchange = BinanceExchange::new();
        assert_eq!(exchange.venue_id(), VenueId::EXCHANGE_A);
    }
}