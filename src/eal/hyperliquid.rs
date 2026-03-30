//! Hyperliquid exchange implementation for market data.
//!
//! Connects to Hyperliquid WebSocket for real-time trade and L2 book data.
//! No API keys required for market data.

use async_trait::async_trait;
use crossbeam_channel::Sender;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio_tungstenite::{connect_async, tungstenite::Message};

use super::{BookLevel, BookUpdate, ExchangeError, MarketData, Symbol, Tick, VenueId};

/// Hyperliquid WebSocket subscription message
#[derive(Serialize)]
struct SubscribeMessage {
    method: String,
    subscription: SubscriptionType,
}

#[derive(Serialize)]
#[serde(tag = "type")]
enum SubscriptionType {
    #[serde(rename = "trades")]
    Trades { coin: String },
    #[serde(rename = "l2Book")]
    L2Book { coin: String },
}

/// Hyperliquid trade message
#[derive(Debug, Deserialize)]
struct HyperliquidTrade {
    coin: String,
    px: String,
    sz: String,
    time: u64,
}

/// Hyperliquid L2 book level
#[derive(Debug, Deserialize)]
struct HyperliquidLevel {
    px: String,
    sz: String,
    #[allow(dead_code)]
    n: u32,
}

/// Hyperliquid L2 book data
#[derive(Debug, Deserialize)]
struct HyperliquidBook {
    coin: String,
    levels: Vec<Vec<HyperliquidLevel>>,
    time: u64,
}

/// Hyperliquid WebSocket message wrapper
#[derive(Debug, Deserialize)]
struct HyperliquidWsMessage {
    channel: Option<String>,
    data: Option<serde_json::Value>,
}

/// Hyperliquid exchange for market data
pub struct HyperliquidExchange {
    venue_id: VenueId,
}

impl HyperliquidExchange {
    pub fn new() -> Self {
        Self {
            venue_id: VenueId::EXCHANGE_B,
        }
    }

    /// Connect a single WS that subscribes to both trades and l2Book for a symbol.
    /// Routes trades to tick_sender, book snapshots to book_sender.
    async fn connect_websocket(
        symbol: &Symbol,
        tick_sender: Sender<Arc<Tick>>,
        book_sender: Option<Sender<Arc<BookUpdate>>>,
    ) -> Result<(), ExchangeError> {
        let url = "wss://api.hyperliquid.xyz/ws";

        let (ws_stream, _) = connect_async(url)
            .await
            .map_err(|e| ExchangeError::ConnectionFailed(e.to_string()))?;

        let (mut write, mut read) = ws_stream.split();

        // Subscribe to trades
        let sub_trades = SubscribeMessage {
            method: "subscribe".to_string(),
            subscription: SubscriptionType::Trades {
                coin: symbol.0.clone(),
            },
        };
        let msg = serde_json::to_string(&sub_trades)
            .map_err(|e| ExchangeError::ParseError(e.to_string()))?;
        write
            .send(Message::Text(msg))
            .await
            .map_err(|e| ExchangeError::WebSocketError(e.to_string()))?;

        // Subscribe to l2Book
        let sub_book = SubscribeMessage {
            method: "subscribe".to_string(),
            subscription: SubscriptionType::L2Book {
                coin: symbol.0.clone(),
            },
        };
        let msg = serde_json::to_string(&sub_book)
            .map_err(|e| ExchangeError::ParseError(e.to_string()))?;
        write
            .send(Message::Text(msg))
            .await
            .map_err(|e| ExchangeError::WebSocketError(e.to_string()))?;

        let symbol_name = symbol.0.clone();

        tokio::spawn(async move {
            tracing::info!(
                "Hyperliquid WS task started for {} (trades + l2Book)",
                symbol_name
            );

            while let Some(msg) = read.next().await {
                match msg {
                    Ok(Message::Text(text)) => {
                        if let Ok(ws_msg) =
                            serde_json::from_str::<HyperliquidWsMessage>(&text)
                        {
                            if let (Some(channel), Some(data)) =
                                (ws_msg.channel, ws_msg.data)
                            {
                                match channel.as_str() {
                                    "trades" => {
                                        if let Ok(trades) = serde_json::from_value::<
                                            Vec<HyperliquidTrade>,
                                        >(data)
                                        {
                                            for trade in trades {
                                                let price: f64 =
                                                    trade.px.parse().unwrap_or(0.0);
                                                let size: f64 =
                                                    trade.sz.parse().unwrap_or(0.0);

                                                if price > 0.0 && size > 0.0 {
                                                    let tick = Tick {
                                                        venue: VenueId::EXCHANGE_B,
                                                        symbol: Symbol::new(
                                                            &trade.coin,
                                                        ),
                                                        price,
                                                        size,
                                                        exchange_ts_ns: trade.time
                                                            * 1_000_000,
                                                        local_ts_ns: std::time::SystemTime::now()
                                                            .duration_since(std::time::UNIX_EPOCH)
                                                            .unwrap()
                                                            .as_nanos() as u64,
                                                    };
                                                    let _ =
                                                        tick_sender.send(Arc::new(tick));
                                                }
                                            }
                                        }
                                    }
                                    "l2Book" => {
                                        if let Some(ref book_tx) = book_sender {
                                            if let Ok(book_data) =
                                                serde_json::from_value::<
                                                    HyperliquidBook,
                                                >(data)
                                            {
                                                // levels[0] = bids, levels[1] = asks
                                                let mut bids = Vec::new();
                                                let mut asks = Vec::new();

                                                if book_data.levels.len() >= 2 {
                                                    for level in &book_data.levels[0] {
                                                        let px: f64 = level
                                                            .px
                                                            .parse()
                                                            .unwrap_or(0.0);
                                                        let sz: f64 = level
                                                            .sz
                                                            .parse()
                                                            .unwrap_or(0.0);
                                                        if px > 0.0 && sz > 0.0 {
                                                            bids.push(BookLevel {
                                                                price: px,
                                                                size: sz,
                                                            });
                                                        }
                                                    }
                                                    for level in &book_data.levels[1] {
                                                        let px: f64 = level
                                                            .px
                                                            .parse()
                                                            .unwrap_or(0.0);
                                                        let sz: f64 = level
                                                            .sz
                                                            .parse()
                                                            .unwrap_or(0.0);
                                                        if px > 0.0 && sz > 0.0 {
                                                            asks.push(BookLevel {
                                                                price: px,
                                                                size: sz,
                                                            });
                                                        }
                                                    }
                                                }

                                                if !bids.is_empty() || !asks.is_empty() {
                                                    let update = BookUpdate {
                                                        venue: VenueId::EXCHANGE_B,
                                                        symbol: Symbol::new(
                                                            &book_data.coin,
                                                        ),
                                                        bids,
                                                        asks,
                                                        exchange_ts_ns: book_data.time
                                                            * 1_000_000,
                                                        local_ts_ns: std::time::SystemTime::now()
                                                            .duration_since(std::time::UNIX_EPOCH)
                                                            .unwrap()
                                                            .as_nanos() as u64,
                                                    };
                                                    let _ = book_tx
                                                        .send(Arc::new(update));
                                                }
                                            }
                                        }
                                    }
                                    "subscriptionResponse" => {
                                        tracing::debug!(
                                            "Hyperliquid subscription confirmed for {}",
                                            symbol_name
                                        );
                                    }
                                    _ => {}
                                }
                                continue;
                            }
                        }
                    }
                    Ok(Message::Close(_)) => {
                        tracing::warn!("Hyperliquid WS closed for {}", symbol_name);
                        break;
                    }
                    Err(e) => {
                        tracing::error!(
                            "Hyperliquid WS error for {}: {}",
                            symbol_name,
                            e
                        );
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
            // Tick-only connection (no book sender)
            Self::connect_websocket(symbol, sender, None).await?;
        }

        Ok(rx)
    }

    async fn subscribe_book(
        &self,
        symbol: &Symbol,
    ) -> Result<crossbeam_channel::Receiver<Arc<BookUpdate>>, ExchangeError> {
        let (tx, rx) = crossbeam_channel::bounded(1024);

        // Book-only connection (no tick sender — trades go through subscribe_ticks)
        let (tick_tx, _) = crossbeam_channel::bounded::<Arc<Tick>>(1);
        Self::connect_websocket(symbol, tick_tx, Some(tx)).await?;

        Ok(rx)
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

    #[test]
    fn test_l2book_parsing() {
        let json = r#"{
            "channel": "l2Book",
            "data": {
                "coin": "BTC",
                "levels": [
                    [{"px": "213.45", "sz": "100.0", "n": 5}, {"px": "213.40", "sz": "50.0", "n": 3}],
                    [{"px": "213.50", "sz": "80.0", "n": 4}, {"px": "213.55", "sz": "30.0", "n": 2}]
                ],
                "time": 1774740841877
            }
        }"#;

        let ws_msg: HyperliquidWsMessage = serde_json::from_str(json).unwrap();
        assert_eq!(ws_msg.channel.as_deref(), Some("l2Book"));

        let book: HyperliquidBook =
            serde_json::from_value(ws_msg.data.unwrap()).unwrap();
        assert_eq!(book.coin, "BTC");
        assert_eq!(book.levels.len(), 2);
        assert_eq!(book.levels[0].len(), 2); // 2 bids
        assert_eq!(book.levels[1].len(), 2); // 2 asks

        let bid_px: f64 = book.levels[0][0].px.parse().unwrap();
        assert_eq!(bid_px, 213.45);
    }
}
