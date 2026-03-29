//! Hyperliquid exchange implementation for market data.
//!
//! Connects to Hyperliquid WebSocket for real-time trade data.
//! No API keys required for market data.

use async_trait::async_trait;
use crossbeam_channel::Sender;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio_tungstenite::{connect_async, tungstenite::Message};

use super::{ExchangeError, MarketData, Symbol, Tick, VenueId};

/// Hyperliquid WebSocket subscription message
#[derive(Serialize)]
struct SubscribeMessage {
    method: String,
    subscription: Subscription,
}

#[derive(Serialize)]
struct Subscription {
    #[serde(rename = "type")]
    sub_type: String,
    coin: String,
}

/// Hyperliquid trade message
#[derive(Debug, Deserialize)]
struct HyperliquidTrade {
    coin: String,
    px: String, // price
    sz: String, // size
    time: u64,  // timestamp in ms
}

/// Hyperliquid WebSocket message wrapper.
/// Responses come as {"channel": "trades", "data": [...]} not raw arrays.
#[derive(Debug, Deserialize)]
struct HyperliquidWsMessage {
    channel: Option<String>,
    data: Option<serde_json::Value>,
}

/// Hyperliquid exchange for market data
pub struct HyperliquidExchange {
    venue_id: VenueId,
    tick_senders: Arc<Mutex<HashMap<Symbol, Sender<Arc<Tick>>>>>,
}

impl HyperliquidExchange {
    pub fn new() -> Self {
        Self {
            venue_id: VenueId::EXCHANGE_B,
            tick_senders: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    async fn connect_websocket(
        &self,
        symbol: &Symbol,
        sender: Sender<Arc<Tick>>,
    ) -> Result<(), ExchangeError> {
        let url = "wss://api.hyperliquid.xyz/ws";

        let (ws_stream, _) = connect_async(url)
            .await
            .map_err(|e| ExchangeError::ConnectionFailed(e.to_string()))?;

        let (mut write, mut read) = ws_stream.split();

        // Subscribe to trades
        let subscribe = SubscribeMessage {
            method: "subscribe".to_string(),
            subscription: Subscription {
                sub_type: "trades".to_string(),
                coin: symbol.0.clone(),
            },
        };

        let msg = serde_json::to_string(&subscribe)
            .map_err(|e| ExchangeError::ParseError(e.to_string()))?;

        write
            .send(Message::Text(msg))
            .await
            .map_err(|e| ExchangeError::WebSocketError(e.to_string()))?;

        let symbol_name = symbol.0.clone();

        tokio::spawn(async move {
            tracing::info!("Hyperliquid WS task started for {}", symbol_name);

            while let Some(msg) = read.next().await {
                match msg {
                    Ok(Message::Text(text)) => {
                        // Try parsing as channel-wrapped message first
                        if let Ok(ws_msg) = serde_json::from_str::<HyperliquidWsMessage>(&text) {
                            if let (Some(channel), Some(data)) = (ws_msg.channel, ws_msg.data) {
                                if channel == "trades" {
                                    if let Ok(trades) = serde_json::from_value::<Vec<HyperliquidTrade>>(data) {
                                        for trade in trades {
                                            let price: f64 = trade.px.parse().unwrap_or(0.0);
                                            let size: f64 = trade.sz.parse().unwrap_or(0.0);

                                            if price > 0.0 && size > 0.0 {
                                                let tick = Tick {
                                                    venue: VenueId::EXCHANGE_B,
                                                    symbol: Symbol::new(&trade.coin),
                                                    price,
                                                    size,
                                                    exchange_ts_ns: trade.time * 1_000_000,
                                                    local_ts_ns: std::time::SystemTime::now()
                                                        .duration_since(std::time::UNIX_EPOCH)
                                                        .unwrap()
                                                        .as_nanos() as u64,
                                                };

                                                let _ = sender.send(Arc::new(tick));
                                            }
                                        }
                                    }
                                }
                                // Skip non-trades channels silently
                                continue;
                            }
                        }
                    }
                    Ok(Message::Close(_)) => {
                        tracing::warn!("Hyperliquid WS closed for {}", symbol_name);
                        break;
                    }
                    Err(e) => {
                        tracing::error!("Hyperliquid WS error for {}: {}", symbol_name, e);
                        break;
                    }
                    _ => {}
                }
            }

            tracing::warn!("Hyperliquid WS task ended for {}", symbol_name);
        });

        Ok(())
    }
}

impl Default for HyperliquidExchange {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl MarketData for HyperliquidExchange {
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
    fn test_hyperliquid_exchange_creation() {
        let exchange = HyperliquidExchange::new();
        assert_eq!(exchange.venue_id(), VenueId::EXCHANGE_B);
    }
}