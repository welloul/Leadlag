//! Binance Futures exchange implementation for market data.
//!
//! Connects to Binance WebSocket for real-time trade and L2 book data.
//! No API keys required for market data.

use async_trait::async_trait;
use crossbeam_channel::Sender;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use tokio_tungstenite::{connect_async, tungstenite::Message};

use super::{BookLevel, BookUpdate, ExchangeError, MarketData, Symbol, Tick, VenueId};

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

/// Binance diff depth update from WebSocket
#[derive(Debug, Deserialize)]
struct BinanceDepthUpdate {
    #[serde(rename = "s")]
    symbol: String,
    #[serde(rename = "U")]
    first_update_id: u64,
    #[serde(rename = "u")]
    final_update_id: u64,
    #[serde(rename = "pu")]
    prev_final_update_id: u64,
    #[serde(rename = "b")]
    bids: Vec<[String; 2]>,
    #[serde(rename = "a")]
    asks: Vec<[String; 2]>,
    #[serde(rename = "E")]
    event_time: u64,
}

/// Binance REST depth snapshot
#[derive(Debug, Deserialize)]
struct BinanceDepthSnapshot {
    #[serde(rename = "lastUpdateId")]
    last_update_id: u64,
    bids: Vec<[String; 2]>,
    asks: Vec<[String; 2]>,
}

/// Local order book state for a single symbol.
///
/// Maintains the full book from diff stream + REST snapshot reconciliation.
pub struct LocalOrderBook {
    /// Bids sorted descending by price (BTreeMap with reversed f64 key)
    bids: BTreeMap<u64, f64>,  // price * 1e8 as key for ordering
    /// Asks sorted ascending by price
    asks: BTreeMap<u64, f64>,
    /// Last update ID from the exchange
    last_update_id: u64,
    /// Whether we've synced with a REST snapshot
    synced: bool,
    /// Max depth to keep
    max_depth: usize,
}

impl LocalOrderBook {
    fn new(max_depth: usize) -> Self {
        Self {
            bids: BTreeMap::new(),
            asks: BTreeMap::new(),
            last_update_id: 0,
            synced: false,
            max_depth,
        }
    }

    /// Initialize from REST snapshot
    fn apply_snapshot(&mut self, snapshot: &BinanceDepthSnapshot) {
        self.bids.clear();
        self.asks.clear();

        for [price_str, size_str] in &snapshot.bids {
            let price: f64 = price_str.parse().unwrap_or(0.0);
            let size: f64 = size_str.parse().unwrap_or(0.0);
            if price > 0.0 && size > 0.0 {
                self.bids.insert(price_to_key(price), size);
            }
        }

        for [price_str, size_str] in &snapshot.asks {
            let price: f64 = price_str.parse().unwrap_or(0.0);
            let size: f64 = size_str.parse().unwrap_or(0.0);
            if price > 0.0 && size > 0.0 {
                self.asks.insert(price_to_key(price), size);
            }
        }

        self.last_update_id = snapshot.last_update_id;
        self.synced = true;
        self.trim();
    }

    /// Apply a diff update. Returns true if the update was valid.
    fn apply_diff(&mut self, diff: &BinanceDepthUpdate) -> bool {
        if !self.synced {
            return false;
        }

        // Check continuity
        if diff.prev_final_update_id != self.last_update_id {
            // Gap detected — need to re-sync
            self.synced = false;
            return false;
        }

        // Apply bid updates
        for [price_str, size_str] in &diff.bids {
            let price: f64 = price_str.parse().unwrap_or(0.0);
            let size: f64 = size_str.parse().unwrap_or(0.0);
            let key = price_to_key(price);
            if size == 0.0 {
                self.bids.remove(&key);
            } else {
                self.bids.insert(key, size);
            }
        }

        // Apply ask updates
        for [price_str, size_str] in &diff.asks {
            let price: f64 = price_str.parse().unwrap_or(0.0);
            let size: f64 = size_str.parse().unwrap_or(0.0);
            let key = price_to_key(price);
            if size == 0.0 {
                self.asks.remove(&key);
            } else {
                self.asks.insert(key, size);
            }
        }

        self.last_update_id = diff.final_update_id;
        self.trim();
        true
    }

    /// Trim to max_depth
    fn trim(&mut self) {
        while self.bids.len() > self.max_depth {
            self.bids.pop_last();
        }
        while self.asks.len() > self.max_depth {
            self.asks.pop_last();
        }
    }

    /// Convert to BookUpdate
    fn to_book_update(&self, symbol: &Symbol, venue: VenueId) -> BookUpdate {
        let bids: Vec<BookLevel> = self
            .bids
            .iter()
            .rev() // Descending order
            .map(|(_, &size)| BookLevel {
                price: 0.0, // Will be filled below
                size,
            })
            .collect();

        // We need actual prices — rebuild from keys
        let bids: Vec<BookLevel> = self
            .bids
            .iter()
            .rev()
            .map(|(&key, &size)| BookLevel {
                price: key_to_price(key),
                size,
            })
            .collect();

        let asks: Vec<BookLevel> = self
            .asks
            .iter()
            .map(|(&key, &size)| BookLevel {
                price: key_to_price(key),
                size,
            })
            .collect();

        BookUpdate {
            venue,
            symbol: symbol.clone(),
            bids,
            asks,
            exchange_ts_ns: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos() as u64,
            local_ts_ns: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos() as u64,
        }
    }
}

/// Convert price to BTreeMap key (f64 * 1e8 as u64 for deterministic ordering)
fn price_to_key(price: f64) -> u64 {
    (price * 1e8) as u64
}

/// Convert BTreeMap key back to price
fn key_to_price(key: u64) -> f64 {
    key as f64 / 1e8
}

/// Binance exchange for market data
pub struct BinanceExchange {
    venue_id: VenueId,
}

impl BinanceExchange {
    pub fn new() -> Self {
        Self {
            venue_id: VenueId::EXCHANGE_A,
        }
    }

    /// Connect to trade stream for a symbol
    async fn connect_trade_stream(
        symbol: &Symbol,
        sender: Sender<Arc<Tick>>,
    ) -> Result<(), ExchangeError> {
        let ws_symbol = format!("{}usdt", symbol.0.to_lowercase());
        let url = format!("wss://fstream.binance.com/ws/{}@trade", ws_symbol);

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

    /// Connect to diff depth stream for a symbol.
    ///
    /// Fetches REST snapshot first, then applies live diffs.
    /// Sends BookUpdate to the channel after each successful diff application.
    async fn connect_book_stream(
        symbol: &Symbol,
        sender: Sender<Arc<BookUpdate>>,
        max_depth: usize,
    ) -> Result<(), ExchangeError> {
        let ws_symbol = format!("{}usdt", symbol.0.to_lowercase());
        let rest_symbol = format!("{}USDT", symbol.0.to_uppercase());
        let sym = symbol.clone();

        // Fetch REST snapshot
        let snapshot_url = format!(
            "https://fapi.binance.com/fapi/v1/depth?symbol={}&limit={}",
            rest_symbol, max_depth
        );

        let snapshot: BinanceDepthSnapshot = reqwest::get(&snapshot_url)
            .await
            .map_err(|e| ExchangeError::ConnectionFailed(format!("REST: {}", e)))?
            .json()
            .await
            .map_err(|e| ExchangeError::ParseError(format!("Snapshot: {}", e)))?;

        // Connect to diff stream
        let url = format!(
            "wss://fstream.binance.com/ws/{}@depth@100ms",
            ws_symbol
        );

        let (ws_stream, _) = connect_async(&url)
            .await
            .map_err(|e| ExchangeError::ConnectionFailed(e.to_string()))?;

        let (_, mut read) = ws_stream.split();

        let venue = VenueId::EXCHANGE_A;

        // Send initial snapshot as first book update
        {
            let mut initial_book = LocalOrderBook::new(max_depth);
            initial_book.apply_snapshot(&snapshot);
            let book_update = initial_book.to_book_update(symbol, venue);
            let _ = sender.send(Arc::new(book_update));
        }

        let last_snapshot_id = snapshot.last_update_id;
        let sym = symbol.clone();
        let book_sender = sender.clone();

        tokio::spawn(async move {
            let mut local_book = LocalOrderBook::new(max_depth);
            let mut synced = true;

            tracing::info!(
                "Binance book stream started for {} (snapshot id={})",
                sym, last_snapshot_id
            );

            // Snapshot already applied — set the book's last_update_id
            local_book.last_update_id = last_snapshot_id;
            local_book.synced = true;

            while let Some(msg) = read.next().await {
                if let Ok(Message::Text(text)) = msg {
                    if let Ok(diff) =
                        serde_json::from_str::<BinanceDepthUpdate>(&text)
                    {
                        // Apply diff
                        if local_book.apply_diff(&diff) {
                            let book_update = local_book.to_book_update(&sym, venue);
                            let _ = book_sender.send(Arc::new(book_update));
                        } else if !local_book.synced {
                            // Gap detected — re-sync needed
                            tracing::warn!(
                                "Binance book gap for {} — needs re-sync",
                                sym
                            );
                        }
                    }
                }
            }
        });

        // Send initial snapshot as first book update
        let mut initial_book = LocalOrderBook::new(max_depth);
            initial_book.apply_snapshot(&snapshot);
        let book_update = initial_book.to_book_update(symbol, venue);
        let _ = sender.send(Arc::new(book_update));

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
            Self::connect_trade_stream(symbol, sender).await?;
        }

        Ok(rx)
    }

    async fn subscribe_book(
        &self,
        symbol: &Symbol,
    ) -> Result<crossbeam_channel::Receiver<Arc<BookUpdate>>, ExchangeError> {
        let (tx, rx) = crossbeam_channel::bounded(1024);

        Self::connect_book_stream(symbol, tx, 20).await?;

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
    fn test_binance_exchange_creation() {
        let exchange = BinanceExchange::new();
        assert_eq!(exchange.venue_id(), VenueId::EXCHANGE_A);
    }

    #[test]
    fn test_local_order_book_snapshot() {
        let mut book = LocalOrderBook::new(20);

        let snapshot = BinanceDepthSnapshot {
            last_update_id: 100,
            bids: vec![
                ["60000.00".to_string(), "1.5".to_string()],
                ["59999.00".to_string(), "2.0".to_string()],
            ],
            asks: vec![
                ["60001.00".to_string(), "1.0".to_string()],
                ["60002.00".to_string(), "3.0".to_string()],
            ],
        };

        book.apply_snapshot(&snapshot);
        assert!(book.synced);
        assert_eq!(book.last_update_id, 100);
        assert_eq!(book.bids.len(), 2);
        assert_eq!(book.asks.len(), 2);

        // Best bid should be 60000 (highest)
        let best_bid_key = *book.bids.iter().next_back().unwrap().0;
        assert_eq!(key_to_price(best_bid_key), 60000.0);

        // Best ask should be 60001 (lowest)
        let best_ask_key = *book.asks.iter().next().unwrap().0;
        assert_eq!(key_to_price(best_ask_key), 60001.0);
    }

    #[test]
    fn test_local_order_book_diff() {
        let mut book = LocalOrderBook::new(20);

        let snapshot = BinanceDepthSnapshot {
            last_update_id: 100,
            bids: vec![["60000.00".to_string(), "1.5".to_string()]],
            asks: vec![["60001.00".to_string(), "1.0".to_string()]],
        };
        book.apply_snapshot(&snapshot);

        // Apply valid diff
        let diff = BinanceDepthUpdate {
            symbol: "BTCUSDT".to_string(),
            first_update_id: 101,
            final_update_id: 102,
            prev_final_update_id: 100,
            bids: vec![["60000.00".to_string(), "2.0".to_string()]], // Update size
            asks: vec![],
            event_time: 0,
        };
        assert!(book.apply_diff(&diff));
        assert_eq!(book.last_update_id, 102);

        // Bid size should be updated
        let bid_key = price_to_key(60000.0);
        assert_eq!(*book.bids.get(&bid_key).unwrap(), 2.0);

        // Apply diff that removes a level
        let remove_diff = BinanceDepthUpdate {
            symbol: "BTCUSDT".to_string(),
            first_update_id: 103,
            final_update_id: 104,
            prev_final_update_id: 102,
            bids: vec![["60000.00".to_string(), "0".to_string()]], // Remove
            asks: vec![],
            event_time: 0,
        };
        assert!(book.apply_diff(&remove_diff));
        assert!(!book.bids.contains_key(&bid_key));
    }

    #[test]
    fn test_local_order_book_gap_detection() {
        let mut book = LocalOrderBook::new(20);

        let snapshot = BinanceDepthSnapshot {
            last_update_id: 100,
            bids: vec![["60000.00".to_string(), "1.5".to_string()]],
            asks: vec![["60001.00".to_string(), "1.0".to_string()]],
        };
        book.apply_snapshot(&snapshot);

        // Gap: prev_final_update_id != last_update_id
        let gap_diff = BinanceDepthUpdate {
            symbol: "BTCUSDT".to_string(),
            first_update_id: 105,
            final_update_id: 110,
            prev_final_update_id: 105, // Gap! should be 100
            bids: vec![],
            asks: vec![],
            event_time: 0,
        };
        assert!(!book.apply_diff(&gap_diff));
        assert!(!book.synced);
    }

    #[test]
    fn test_to_book_update() {
        let mut book = LocalOrderBook::new(20);

        let snapshot = BinanceDepthSnapshot {
            last_update_id: 100,
            bids: vec![
                ["60000.00".to_string(), "1.5".to_string()],
                ["59999.00".to_string(), "2.0".to_string()],
            ],
            asks: vec![
                ["60001.00".to_string(), "1.0".to_string()],
                ["60002.00".to_string(), "3.0".to_string()],
            ],
        };
        book.apply_snapshot(&snapshot);

        let update = book.to_book_update(&Symbol::new("BTCUSDT"), VenueId::EXCHANGE_A);
        assert_eq!(update.bids.len(), 2);
        assert_eq!(update.asks.len(), 2);
        assert_eq!(update.bids[0].price, 60000.0); // Best bid
        assert_eq!(update.asks[0].price, 60001.0); // Best ask
    }

    #[test]
    fn test_diff_stream_parsing() {
        let json = r#"{
            "e": "depthUpdate",
            "E": 123456789,
            "T": 123456788,
            "s": "BTCUSDT",
            "U": 157,
            "u": 160,
            "pu": 156,
            "b": [["60000.00", "2.5"], ["59999.00", "0"]],
            "a": [["60001.00", "1.5"]]
        }"#;

        let diff: BinanceDepthUpdate = serde_json::from_str(json).unwrap();
        assert_eq!(diff.symbol, "BTCUSDT");
        assert_eq!(diff.first_update_id, 157);
        assert_eq!(diff.final_update_id, 160);
        assert_eq!(diff.prev_final_update_id, 156);
        assert_eq!(diff.bids.len(), 2);
        assert_eq!(diff.asks.len(), 1);
    }
}
