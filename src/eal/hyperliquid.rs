//! Hyperliquid exchange implementation for market data.
//!
//! Connects to Hyperliquid WebSocket for real-time trade and L2 book data.
//! Includes reconnection with exponential backoff, staggered startup (100ms
//! between symbols), and 30-second keepalive pings.

use async_trait::async_trait;
use crossbeam_channel::Sender;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio_tungstenite::{connect_async, tungstenite::Message};

use super::{BookLevel, BookUpdate, ExchangeError, MarketData, Symbol, Tick, VenueId};

const WS_URL: &str = "wss://api.hyperliquid.xyz/ws";
const KEEPALIVE_SECS: u64 = 30;
const RECONNECT_BASE_MS: u64 = 1000;
const RECONNECT_MAX_MS: u64 = 30000;
const STAGGER_MS: u64 = 100;

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

#[derive(Debug, Deserialize)]
struct HyperliquidTrade {
    coin: String,
    px: String,
    sz: String,
    time: u64,
}

#[derive(Debug, Deserialize)]
struct HyperliquidLevel {
    px: String,
    sz: String,
    #[allow(dead_code)]
    n: u32,
}

#[derive(Debug, Deserialize)]
struct HyperliquidBook {
    coin: String,
    levels: Vec<Vec<HyperliquidLevel>>,
    time: u64,
}

#[derive(Debug, Deserialize)]
struct HyperliquidWsMessage {
    channel: Option<String>,
    data: Option<serde_json::Value>,
}

pub struct HyperliquidExchange {
    venue_id: VenueId,
}

impl HyperliquidExchange {
    pub fn new() -> Self {
        Self {
            venue_id: VenueId::EXCHANGE_B,
        }
    }

    /// Single WS connection lifecycle: connect, subscribe, process messages.
    /// Returns when connection drops (triggers reconnect from caller).
    async fn run_connection(
        symbol: &Symbol,
        tick_sender: &Sender<Arc<Tick>>,
        book_sender: &Option<Sender<Arc<BookUpdate>>>,
    ) -> Result<(), ExchangeError> {
        let (ws_stream, _) = connect_async(WS_URL)
            .await
            .map_err(|e| ExchangeError::ConnectionFailed(e.to_string()))?;

        let (mut write, mut read) = ws_stream.split();

        // Subscribe to trades
        let sub_trades = serde_json::to_string(&SubscribeMessage {
            method: "subscribe".to_string(),
            subscription: SubscriptionType::Trades {
                coin: symbol.0.clone(),
            },
        })
        .map_err(|e| ExchangeError::ParseError(e.to_string()))?;
        write
            .send(Message::Text(sub_trades))
            .await
            .map_err(|e| ExchangeError::WebSocketError(e.to_string()))?;

        // Subscribe to l2Book
        let sub_book = serde_json::to_string(&SubscribeMessage {
            method: "subscribe".to_string(),
            subscription: SubscriptionType::L2Book {
                coin: symbol.0.clone(),
            },
        })
        .map_err(|e| ExchangeError::ParseError(e.to_string()))?;
        write
            .send(Message::Text(sub_book))
            .await
            .map_err(|e| ExchangeError::WebSocketError(e.to_string()))?;

        tracing::info!("Hyperliquid WS connected for {}", symbol.0);

        let mut keepalive_interval =
            tokio::time::interval(std::time::Duration::from_secs(KEEPALIVE_SECS));
        keepalive_interval.tick().await; // skip first immediate tick

        let symbol_name = symbol.0.clone();

        loop {
            tokio::select! {
                msg = read.next() => {
                    match msg {
                        Some(Ok(Message::Text(text))) => {
                            Self::handle_message(&text, &symbol_name, tick_sender, book_sender);
                        }
                        Some(Ok(Message::Close(_))) => {
                            tracing::warn!("Hyperliquid WS closed for {}", symbol_name);
                            return Ok(());
                        }
                        Some(Ok(Message::Pong(_))) => {
                            tracing::debug!("Hyperliquid pong for {}", symbol_name);
                        }
                        Some(Err(e)) => {
                            tracing::error!("Hyperliquid WS error for {}: {}", symbol_name, e);
                            return Err(ExchangeError::WebSocketError(e.to_string()));
                        }
                        None => {
                            tracing::warn!("Hyperliquid WS stream ended for {}", symbol_name);
                            return Ok(());
                        }
                        _ => {}
                    }
                }
                _ = keepalive_interval.tick() => {
                    if write.send(Message::Ping(vec![])).await.is_err() {
                        tracing::warn!("Hyperliquid keepalive failed for {}", symbol_name);
                        return Ok(());
                    }
                    tracing::debug!("Hyperliquid keepalive ping for {}", symbol_name);
                }
            }
        }
    }

    fn handle_message(
        text: &str,
        symbol_name: &str,
        tick_sender: &Sender<Arc<Tick>>,
        book_sender: &Option<Sender<Arc<BookUpdate>>>,
    ) {
        if let Ok(ws_msg) = serde_json::from_str::<HyperliquidWsMessage>(text) {
            if let (Some(channel), Some(data)) = (ws_msg.channel, ws_msg.data) {
                match channel.as_str() {
                    "trades" => {
                        if let Ok(trades) =
                            serde_json::from_value::<Vec<HyperliquidTrade>>(data)
                        {
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
                                    let _ = tick_sender.send(Arc::new(tick));
                                }
                            }
                        }
                    }
                    "l2Book" => {
                        if let Some(ref book_tx) = book_sender {
                            if let Ok(book_data) =
                                serde_json::from_value::<HyperliquidBook>(data)
                            {
                                let mut bids = Vec::new();
                                let mut asks = Vec::new();
                                if book_data.levels.len() >= 2 {
                                    for level in &book_data.levels[0] {
                                        let px: f64 = level.px.parse().unwrap_or(0.0);
                                        let sz: f64 = level.sz.parse().unwrap_or(0.0);
                                        if px > 0.0 && sz > 0.0 {
                                            bids.push(BookLevel { price: px, size: sz });
                                        }
                                    }
                                    for level in &book_data.levels[1] {
                                        let px: f64 = level.px.parse().unwrap_or(0.0);
                                        let sz: f64 = level.sz.parse().unwrap_or(0.0);
                                        if px > 0.0 && sz > 0.0 {
                                            asks.push(BookLevel { price: px, size: sz });
                                        }
                                    }
                                }
                                if !bids.is_empty() || !asks.is_empty() {
                                    let update = BookUpdate {
                                        venue: VenueId::EXCHANGE_B,
                                        symbol: Symbol::new(&book_data.coin),
                                        bids,
                                        asks,
                                        exchange_ts_ns: book_data.time * 1_000_000,
                                        local_ts_ns: std::time::SystemTime::now()
                                            .duration_since(std::time::UNIX_EPOCH)
                                            .unwrap()
                                            .as_nanos() as u64,
                                    };
                                    let _ = book_tx.send(Arc::new(update));
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
            }
        }
    }

    /// Spawn a reconnection loop with exponential backoff.
    fn spawn_reconnect_loop(
        symbol: Symbol,
        tick_sender: Sender<Arc<Tick>>,
        book_sender: Option<Sender<Arc<BookUpdate>>>,
        stagger_ms: u64,
    ) {
        tokio::spawn(async move {
            if stagger_ms > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(stagger_ms)).await;
            }

            let mut attempt: u32 = 0;

            loop {
                let result =
                    Self::run_connection(&symbol, &tick_sender, &book_sender).await;

                match &result {
                    Ok(()) => {
                        tracing::warn!(
                            "Hyperliquid WS disconnected for {} (attempt {})",
                            symbol.0,
                            attempt + 1
                        );
                    }
                    Err(e) => {
                        tracing::error!(
                            "Hyperliquid WS failed for {}: {} (attempt {})",
                            symbol.0,
                            e,
                            attempt + 1
                        );
                    }
                }

                let delay_ms =
                    (RECONNECT_BASE_MS * 2u64.pow(attempt)).min(RECONNECT_MAX_MS);
                tracing::info!(
                    "Hyperliquid reconnecting {} in {}ms",
                    symbol.0,
                    delay_ms
                );
                tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;

                attempt = (attempt + 1).min(5);
            }
        });
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

        for (i, symbol) in symbols.iter().enumerate() {
            let sender = tx.clone();
            let stagger = (i as u64) * STAGGER_MS;
            Self::spawn_reconnect_loop(symbol.clone(), sender, None, stagger);
        }

        Ok(rx)
    }

    async fn subscribe_book(
        &self,
        symbol: &Symbol,
    ) -> Result<crossbeam_channel::Receiver<Arc<BookUpdate>>, ExchangeError> {
        let (tx, rx) = crossbeam_channel::bounded(1024);
        let (tick_tx, _) = crossbeam_channel::bounded::<Arc<Tick>>(1);
        Self::spawn_reconnect_loop(symbol.clone(), tick_tx, Some(tx), 0);
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
        assert_eq!(book.levels[0].len(), 2);
        assert_eq!(book.levels[1].len(), 2);

        let bid_px: f64 = book.levels[0][0].px.parse().unwrap();
        assert_eq!(bid_px, 213.45);
    }

    #[test]
    fn test_trades_parsing() {
        let json = r#"{
            "channel": "trades",
            "data": [
                {"coin": "BTC", "px": "60000.5", "sz": "0.01", "time": 1774740841877},
                {"coin": "BTC", "px": "60001.0", "sz": "0.02", "time": 1774740841878}
            ]
        }"#;

        let ws_msg: HyperliquidWsMessage = serde_json::from_str(json).unwrap();
        assert_eq!(ws_msg.channel.as_deref(), Some("trades"));

        let trades: Vec<HyperliquidTrade> =
            serde_json::from_value(ws_msg.data.unwrap()).unwrap();
        assert_eq!(trades.len(), 2);
        assert_eq!(trades[0].coin, "BTC");
        assert_eq!(trades[0].px, "60000.5");
    }
}
