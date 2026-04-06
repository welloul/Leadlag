//! OKX Exchange Abstraction Layer (EAL).
//!
//! Provides the `OkxExchange` for market data (WebSocket) and 
//! `OkxLiveExecutor` for order execution (REST).
//!
//! Market Data: `wss://ws.okx.com:8443/ws/v5/public`
//! Execution: `https://www.okx.com` (V5 REST API)

use crate::eal::{MarketData, OrderExecution, Symbol, Tick, BookUpdate, BookLevel, ExchangeError, OrderRequest, OrderAck, ExecutionError, VenueId, OrderId, Position, AccountState};
use async_trait::async_trait;
use crossbeam_channel::{Receiver, Sender};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use hmac::{Hmac, Mac, KeyInit};
use sha2::Sha256;
use base64::{Engine as _, engine::general_purpose};
use chrono::Utc;

const WS_URL: &str = "wss://ws.okx.com:8443/ws/v5/public";
const RECONNECT_BASE_MS: u64 = 1000;
const RECONNECT_MAX_MS: u64 = 30000;

#[derive(Debug, Serialize)]
struct OkxSubscribeArgs {
    channel: String,
    #[serde(rename = "instId")]
    inst_id: String,
}

#[derive(Debug, Serialize)]
struct OkxSubscribe {
    op: String,
    args: Vec<OkxSubscribeArgs>,
}

#[derive(Debug, Deserialize)]
struct OkxTick {
    #[serde(rename = "instId")]
    inst_id: String,
    px: String,
    sz: String,
    side: String,
    ts: String,
}

#[derive(Debug, Deserialize)]
struct OkxBbo {
    #[serde(rename = "instId")]
    inst_id: String,
    bids: Vec<[String; 4]>,
    asks: Vec<[String; 4]>,
    ts: String,
    #[serde(rename = "seqId")]
    seq_id: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum OkxMessage {
    Trades {
        arg: serde_json::Value,
        data: Vec<OkxTick>,
    },
    Bbo {
        arg: serde_json::Value,
        data: Vec<OkxBbo>,
    },
    Event {
        event: String,
        arg: Option<serde_json::Value>,
        #[serde(rename = "connId")]
        conn_id: Option<String>,
    },
    Other(serde_json::Value),
}

pub struct OkxExchange;

impl OkxExchange {
    pub fn new() -> Self {
        Self
    }

    /// Convert a bare settings symbol to an OKX perpetual swap instId.
    /// e.g. "LINK" -> "LINK-USDT-SWAP"
    fn to_inst_id(symbol: &Symbol) -> String {
        // Already in OKX format
        if symbol.0.contains('-') {
            return symbol.0.clone();
        }
        format!("{}-USDT-SWAP", symbol.0.to_uppercase())
    }

    async fn run_connection(
        symbol: &Symbol,
        tick_sender: &Sender<Arc<Tick>>,
        book_sender: &Option<Sender<Arc<BookUpdate>>>,
    ) -> Result<(), ExchangeError> {
        let url = url::Url::parse(WS_URL)
            .map_err(|e| ExchangeError::ConnectionFailed(format!("URL Parse: {}", e)))?;
        let host = url.host_str().unwrap_or("");
        let port = url.port_or_known_default().unwrap_or(443);
        
        let tcp_stream = tokio::net::TcpStream::connect((host, port))
            .await
            .map_err(|e| ExchangeError::ConnectionFailed(format!("TCP: {}", e)))?;
        tcp_stream.set_nodelay(true)
            .map_err(|e| ExchangeError::ConnectionFailed(format!("TCP NoDelay: {}", e)))?;

        let (ws_stream, _) = tokio_tungstenite::client_async_tls(WS_URL, tcp_stream)
            .await
            .map_err(|e| ExchangeError::ConnectionFailed(format!("WS: {}", e)))?;

        let (mut write, mut read) = ws_stream.split();

        // Subscribe trades
        let inst_id = Self::to_inst_id(symbol);
        let sub_trades = serde_json::to_string(&OkxSubscribe {
            op: "subscribe".to_string(),
            args: vec![OkxSubscribeArgs {
                channel: "trades".to_string(),
                inst_id: inst_id.clone(),
            }],
        }).map_err(|e| ExchangeError::ParseError(e.to_string()))?;
        write.send(Message::Text(sub_trades)).await.map_err(|e| ExchangeError::WebSocketError(e.to_string()))?;

        // Subscribe books
        if book_sender.is_some() {
            let sub_bbo = serde_json::to_string(&OkxSubscribe {
                op: "subscribe".to_string(),
                args: vec![OkxSubscribeArgs {
                    channel: "books5".to_string(), // 5-level book for better OBI quality
                    inst_id: inst_id.clone(),
                }],
            }).map_err(|e| ExchangeError::ParseError(e.to_string()))?;
            write.send(Message::Text(sub_bbo)).await.map_err(|e| ExchangeError::WebSocketError(e.to_string()))?;
        }

        tracing::info!("OKX WS connected for {} (instId={})", symbol.0, inst_id);
        // Use BARE symbol name for all downstream routing (matches pipeline engine keys)
        let symbol_name = symbol.normalize().0.clone();

        loop {
            tokio::select! {
                msg = read.next() => {
                    match msg {
                        Some(Ok(Message::Text(text))) => {
                            Self::handle_message(&text, &symbol_name, tick_sender, book_sender);
                        }
                        Some(Ok(Message::Ping(ping))) => {
                            let _ = write.send(Message::Pong(ping)).await;
                        }
                        Some(Err(e)) => return Err(ExchangeError::WebSocketError(e.to_string())),
                        None => return Err(ExchangeError::WebSocketError("Stream closed".into())),
                        _ => {}
                    }
                }
            }
        }
    }

    fn handle_message(
        text: &str,
        symbol_name: &str, // already normalized to bare symbol (e.g. "LINK")
        tick_sender: &Sender<Arc<Tick>>,
        book_sender: &Option<Sender<Arc<BookUpdate>>>,
    ) {
        if let Ok(msg) = serde_json::from_str::<OkxMessage>(text) {
            match msg {
                OkxMessage::Trades { data, .. } => {
                    for trade in data {
                        let timestamp_ms: u64 = trade.ts.parse().unwrap_or(0);
                        let price: f64 = trade.px.parse().unwrap_or(0.0);
                        let size: f64 = trade.sz.parse().unwrap_or(0.0);

                        let tick = Arc::new(Tick {
                            venue: VenueId::EXCHANGE_B,
                            // Use the normalized bare symbol so pipeline engine lookup succeeds
                            symbol: Symbol(symbol_name.to_string()),
                            price,
                            size,
                            exchange_ts_ns: timestamp_ms * 1_000_000,
                            local_ts_ns: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos() as u64,
                        });
                        let _ = tick_sender.try_send(tick);
                    }
                }
                OkxMessage::Bbo { data, .. } => {
                    if let Some(bs) = book_sender {
                        for bbo in data {
                            let timestamp_ms: u64 = bbo.ts.parse().unwrap_or(0);
                            let mut bids = Vec::new();
                            let mut asks = Vec::new();

                            for b in bbo.bids {
                                let price: f64 = b[0].parse().unwrap_or(0.0);
                                let size: f64 = b[1].parse().unwrap_or(0.0);
                                bids.push(BookLevel { price, size });
                            }
                            for a in bbo.asks {
                                let price: f64 = a[0].parse().unwrap_or(0.0);
                                let size: f64 = a[1].parse().unwrap_or(0.0);
                                asks.push(BookLevel { price, size });
                            }

                            let update = Arc::new(BookUpdate {
                                venue: VenueId::EXCHANGE_B,
                                // Use the normalized bare symbol
                                symbol: Symbol(symbol_name.to_string()),
                                bids,
                                asks,
                                exchange_ts_ns: timestamp_ms * 1_000_000,
                                local_ts_ns: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos() as u64,
                            });
                            let _ = bs.try_send(update);
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

// Basic exponential backoff local implementation
struct Backoff {
    base: u64,
    max: u64,
    attempts: u32,
}
impl Backoff {
    fn new(base: u64, max: u64) -> Self { Self { base, max, attempts: 0 } }
    fn next_delay(&mut self) -> std::time::Duration {
        let delay = std::cmp::min(self.max, self.base * (2_u64.pow(self.attempts)));
        if self.attempts < 10 { self.attempts += 1; }
        std::time::Duration::from_millis(delay)
    }
}

#[async_trait]
impl MarketData for OkxExchange {
    async fn subscribe_ticks(&self, symbols: &[Symbol]) -> Result<Receiver<Arc<Tick>>, ExchangeError> {
        let (tx, rx) = crossbeam_channel::unbounded();
        for symbol in symbols {
            let sym = symbol.clone();
            let tx_clone = tx.clone();
            tokio::spawn(async move {
                let mut backoff = Backoff::new(RECONNECT_BASE_MS, RECONNECT_MAX_MS);
                loop {
                    if let Err(e) = Self::run_connection(&sym, &tx_clone, &None).await {
                        tracing::warn!("OKX WS dropped for {}: {:?}", sym.0, e);
                    }
                    tokio::time::sleep(backoff.next_delay()).await;
                }
            });
        }
        Ok(rx)
    }

    async fn subscribe_book(&self, symbol: &Symbol) -> Result<Receiver<Arc<BookUpdate>>, ExchangeError> {
        let (tx, rx) = crossbeam_channel::unbounded();
        let sym = symbol.clone();
        let (tick_tx, _) = crossbeam_channel::unbounded(); // dummy
        
        tokio::spawn(async move {
            let mut backoff = Backoff::new(RECONNECT_BASE_MS, RECONNECT_MAX_MS);
            loop {
                if let Err(e) = Self::run_connection(&sym, &tick_tx, &Some(tx.clone())).await {
                    tracing::warn!("OKX WS Book dropped for {}: {:?}", sym.0, e);
                }
                tokio::time::sleep(backoff.next_delay()).await;
            }
        });
        Ok(rx)
    }

    fn venue_id(&self) -> VenueId {
        VenueId::EXCHANGE_B
    }
}

pub struct OkxLiveExecutor {
    api_key: String,
    api_secret: String,
    passphrase: String,
    client: reqwest::Client,
    base_url: String,
}

impl OkxLiveExecutor {
    pub fn new(api_key: String, api_secret: String, passphrase: Option<String>) -> Self {
        Self {
            api_key,
            api_secret,
            passphrase: passphrase.unwrap_or_default(),
            client: reqwest::Client::new(),
            base_url: "https://www.okx.com".to_string(),
        }
    }

    fn sign_headers(&self, method: &str, path: &str, body: &str) -> reqwest::header::HeaderMap {
        let timestamp = Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
        let message = format!("{}{}{}{}", timestamp, method, path, body);
        
        let mut mac = Hmac::<Sha256>::new_from_slice(self.api_secret.as_bytes())
            .expect("HMAC can take key of any size");
        mac.update(message.as_bytes());
        let signature = general_purpose::STANDARD.encode(mac.finalize().into_bytes());

        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("OK-ACCESS-KEY", self.api_key.parse().unwrap());
        headers.insert("OK-ACCESS-SIGN", signature.parse().unwrap());
        headers.insert("OK-ACCESS-TIMESTAMP", timestamp.parse().unwrap());
        headers.insert("OK-ACCESS-PASSPHRASE", self.passphrase.parse().unwrap());
        headers.insert("Content-Type", "application/json".parse().unwrap());
        headers
    }
}

#[derive(Serialize)]
struct OkxOrderReq {
    #[serde(rename = "instId")]
    inst_id: String,
    #[serde(rename = "tdMode")]
    td_mode: String,
    side: String,
    #[serde(rename = "ordType")]
    ord_type: String,
    sz: String,
    #[serde(rename = "clOrdId")]
    cl_ord_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    px: Option<String>,
}

#[derive(Deserialize)]
struct OkxResp {
    code: String,
    msg: String,
    data: Option<Vec<serde_json::Value>>,
}

#[async_trait]
impl OrderExecution for OkxLiveExecutor {
    async fn submit_order(&self, req: &OrderRequest) -> Result<OrderAck, ExecutionError> {
        let path = "/api/v5/trade/order";
        let is_buy = matches!(req.side, crate::eal::types::OrderSide::Buy);
        let is_limit = matches!(req.order_type, crate::eal::types::OrderType::Limit);
        let okx_req = OkxOrderReq {
            inst_id: req.symbol.0.clone(),
            td_mode: "cross".to_string(), // assuming cross margin
            side: if is_buy { "buy".to_string() } else { "sell".to_string() },
            ord_type: if is_limit { "limit".to_string() } else { "market".to_string() },
            sz: req.size.to_string(),
            cl_ord_id: req.client_order_id.clone(),
            px: req.price.map(|p| p.to_string()),
        };

        let body = serde_json::to_string(&okx_req)
            .map_err(|e| ExecutionError::ExchangeError(e.to_string()))?;
        let headers = self.sign_headers("POST", path, &body);
        
        let url = format!("{}{}", self.base_url, path);
        let resp = self.client.post(&url)
            .headers(headers)
            .body(body)
            .send()
            .await
            .map_err(|e| ExecutionError::ExchangeError(e.to_string()))?;

        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(ExecutionError::ExchangeError(format!("OKX HTTP {}: {}", status, text)));
        }

        let parsed: OkxResp = serde_json::from_str(&text)
            .map_err(|e| ExecutionError::ExchangeError(e.to_string()))?;
        if parsed.code != "0" {
            return Err(ExecutionError::ExchangeError(format!("OKX Error {}: {}", parsed.code, parsed.msg)));
        }

        let order_id_str = parsed.data
            .and_then(|mut d: Vec<serde_json::Value>| d.pop())
            .and_then(|item: serde_json::Value| item.get("ordId").cloned())
            .and_then(|v: serde_json::Value| v.as_str().map(|s: &str| s.to_string()))
            .unwrap_or_default();

        let order_id = order_id_str.parse::<u64>().unwrap_or(0);

        Ok(OrderAck {
            order_id: OrderId(order_id),
            client_order_id: req.client_order_id.clone(),
            venue: VenueId::EXCHANGE_B,
            timestamp_ns: Utc::now().timestamp_nanos_opt().unwrap_or(0) as u64,
        })
    }

    async fn cancel_order(&self, _order_id: OrderId) -> Result<(), ExecutionError> {
        unimplemented!("OkxLiveExecutor::cancel_order not fully implemented")
    }

    async fn get_positions(&self) -> Result<Vec<Position>, ExecutionError> {
        let path = "/api/v5/account/positions";
        let headers = self.sign_headers("GET", path, "");
        let url = format!("{}{}", self.base_url, path);
        let resp = self.client.get(&url)
            .headers(headers)
            .send()
            .await
            .map_err(|e| ExecutionError::ExchangeError(e.to_string()))?;
        
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(ExecutionError::ExchangeError(format!("OKX HTTP {}: {}", status, text)));
        }

        Ok(vec![])
    }

    async fn get_account_state(&self) -> Result<AccountState, ExecutionError> {
        Ok(AccountState {
            positions: vec![],
            daily_realized_pnl: 0.0,
            total_unrealized_pnl: 0.0,
            available_balance_usd: 0.0,
        })
    }

    fn venue_id(&self) -> VenueId {
        VenueId::EXCHANGE_B
    }
}
