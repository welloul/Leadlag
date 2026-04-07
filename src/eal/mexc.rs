//! MEXC Exchange Abstraction Layer (EAL).
//!
//! Provides the `MexcExchange` for market data (WebSocket) and 
//! `MexcLiveExecutor` for order execution (REST).
//!
//! Market Data: `wss://contract.mexc.com/edge`
//! Execution: `https://contract.mexc.com` (Futures REST API)

use crate::eal::{MarketData, OrderExecution, Symbol, Tick, BookUpdate, BookLevel, ExchangeError, OrderRequest, OrderAck, ExecutionError, OrderId, Position, AccountState, VenueId};
use async_trait::async_trait;
use crossbeam_channel::{Receiver, Sender};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio_tungstenite::tungstenite::Message;
use hmac::{Hmac, Mac, KeyInit};
use sha2::Sha256;
use chrono::Utc;

const WS_URL: &str = "wss://contract.mexc.com/edge";
const RECONNECT_BASE_MS: u64 = 1000;
const RECONNECT_MAX_MS: u64 = 30000;

#[derive(Debug, Serialize)]
struct MexcSubscribeParam {
    symbol: String,
}

#[derive(Debug, Serialize)]
struct MexcSubscribe {
    method: String,
    param: MexcSubscribeParam,
}

#[derive(Debug, Deserialize)]
struct MexcTick {
    p: f64,
    v: f64,
    #[serde(rename = "T")]
    side: i32, // 1 for buy, 2 for sell
    t: u64,
}

#[derive(Debug, Deserialize)]
struct MexcDepthData {
    asks: Vec<Vec<f64>>,
    bids: Vec<Vec<f64>>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "channel")]
enum MexcMessage {
    #[serde(rename = "push.deal")]
    Deal {
        symbol: String,
        data: Vec<MexcTick>,
        ts: u64,
    },
    #[serde(rename = "push.depth")]
    Depth {
        symbol: String,
        data: MexcDepthData,
        ts: u64,
    },
    #[serde(rename = "rs.sub.deal")]
    SubDealResp { data: String, ts: u64 },
    #[serde(rename = "rs.sub.depth")]
    SubDepthResp { data: String, ts: u64 },
    #[serde(other)]
    Other,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct MexcOrderReq {
    symbol: String,
    price: f64,
    vol: f64,
    side: i32,      // 1:Open Long, 2:Close Short, 3:Open Short, 4:Close Long
    r#type: i32,    // 1:Limit, 2:Market
    open_type: i32, // 1:Isolated, 2:Cross
    external_id: String,
}

#[derive(Debug, Deserialize)]
struct MexcOrderRespData {
    #[serde(rename = "orderId")]
    order_id: String,
}

#[derive(Debug, Deserialize)]
struct MexcResp<T> {
    success: bool,
    code: i32,
    data: Option<T>,
}

#[derive(Debug, Deserialize)]
struct MexcPosition {
    symbol: String,
    hold_vol: f64,
    hold_avg_price: f64,
    realised: f64,
    unrealised: f64,
    position_type: i32, // 1:Long, 2:Short
}

#[derive(Debug, Deserialize)]
struct MexcAsset {
    currency: String,
    available_balance: f64,
    cash_balance: f64,
}

pub struct MexcExchange;

impl MexcExchange {
    pub fn new() -> Self {
        Self
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

        let mexc_symbol = if symbol.0.contains('_') {
            symbol.0.clone()
        } else {
            format!("{}_USDT", symbol.0.to_uppercase())
        };

        // Sub trades
        let sub_trades = serde_json::to_string(&MexcSubscribe {
            method: "sub.deal".to_string(),
            param: MexcSubscribeParam { symbol: mexc_symbol.clone() },
        }).map_err(|e| ExchangeError::ParseError(e.to_string()))?;
        write.send(Message::Text(sub_trades)).await.map_err(|e| ExchangeError::WebSocketError(e.to_string()))?;

        // Sub books
        if book_sender.is_some() {
            let sub_depth = serde_json::to_string(&MexcSubscribe {
                method: "sub.depth".to_string(),
                param: MexcSubscribeParam { symbol: mexc_symbol },
            }).map_err(|e| ExchangeError::ParseError(e.to_string()))?;
            write.send(Message::Text(sub_depth)).await.map_err(|e| ExchangeError::WebSocketError(e.to_string()))?;
        }

        tracing::info!("MEXC WS connected for {}", symbol.0);
        let symbol_name = symbol.0.clone();

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
        symbol_name: &str,
        tick_sender: &Sender<Arc<Tick>>,
        book_sender: &Option<Sender<Arc<BookUpdate>>>,
    ) {
        if let Ok(msg) = serde_json::from_str::<MexcMessage>(text) {
            match msg {
                MexcMessage::Deal { data, .. } => {
                    for trade in data {
                        // T=1 is buy, T=2 is sell (typically on MEXC)
                        let tick = Arc::new(Tick {
                            venue: VenueId::EXCHANGE_B, // Assumes MEXC is Lag
                            symbol: Symbol(symbol_name.to_string()),
                            price: trade.p,
                            size: trade.v,
                            exchange_ts_ns: trade.t * 1_000_000,
                            local_ts_ns: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos() as u64,
                        });
                        let _ = tick_sender.try_send(tick);
                    }
                }
                MexcMessage::Depth { data, ts, .. } => {
                    if let Some(bs) = book_sender {
                        let mut bids = Vec::new();
                        let mut asks = Vec::new();

                        for b in data.bids {
                            if b.len() >= 2 {
                                bids.push(BookLevel { price: b[0], size: b[1] });
                            }
                        }
                        for a in data.asks {
                            if a.len() >= 2 {
                                asks.push(BookLevel { price: a[0], size: a[1] });
                            }
                        }

                        let update = Arc::new(BookUpdate {
                            venue: VenueId::EXCHANGE_B,
                            symbol: Symbol(symbol_name.to_string()),
                            bids,
                            asks,
                            exchange_ts_ns: ts * 1_000_000,
                            local_ts_ns: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos() as u64,
                        });
                        let _ = bs.try_send(update);
                    }
                }
                _ => {}
            }
        }
    }
}

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
impl MarketData for MexcExchange {
    async fn subscribe_ticks(&self, symbols: &[Symbol]) -> Result<Receiver<Arc<Tick>>, ExchangeError> {
        let (tx, rx) = crossbeam_channel::unbounded();
        for symbol in symbols {
            let sym = symbol.clone();
            let tx_clone = tx.clone();
            tokio::spawn(async move {
                let mut backoff = Backoff::new(RECONNECT_BASE_MS, RECONNECT_MAX_MS);
                loop {
                    if let Err(e) = Self::run_connection(&sym, &tx_clone, &None).await {
                        tracing::warn!("MEXC WS dropped for {}: {:?}", sym.0, e);
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
                    tracing::warn!("MEXC WS Book dropped for {}: {:?}", sym.0, e);
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

pub struct MexcLiveExecutor {
    api_key: String,
    api_secret: String,
    base_url: String,
    client: reqwest::Client,
}

impl MexcLiveExecutor {
    pub fn new(api_key: String, api_secret: String) -> Self {
        Self {
            api_key,
            api_secret,
            base_url: "https://contract.mexc.com".to_string(),
            client: reqwest::Client::new(),
        }
    }

    fn sign_headers(&self, method: &str, path: &str, query: &str, body: &str) -> reqwest::header::HeaderMap {
        let timestamp = Utc::now().timestamp_millis().to_string();
        let payload = format!("{}{}{}{}{}", method, path, query, body, timestamp);
        
        type HmacSha256 = Hmac<Sha256>;
        let mut mac = HmacSha256::new_from_slice(self.api_secret.as_bytes())
            .expect("HMAC can take key of any size");
        mac.update(payload.as_bytes());
        let signature = hex::encode(mac.finalize().into_bytes());

        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("ApiKey", self.api_key.parse().unwrap());
        headers.insert("Request-Time", timestamp.parse().unwrap());
        headers.insert("Signature", signature.parse().unwrap());
        headers.insert("Content-Type", "application/json".parse().unwrap());
        headers
    }
}

#[async_trait]
impl OrderExecution for MexcLiveExecutor {
    async fn submit_order(&self, req: &OrderRequest) -> Result<OrderAck, ExecutionError> {
        let path = "/api/v1/private/order/submit";
        
        // Map canonical side to MEXC side
        // MEXC Side: 1:Open Long, 2:Close Short, 3:Open Short, 4:Close Long
        let mexc_side = match (req.side, req.purpose) {
            (crate::eal::types::OrderSide::Buy, crate::eal::types::OrderPurpose::Entry) => 1,
            (crate::eal::types::OrderSide::Sell, crate::eal::types::OrderPurpose::Entry) => 3,
            (crate::eal::types::OrderSide::Buy, _) => 2, // Closing Short
            (crate::eal::types::OrderSide::Sell, _) => 4, // Closing Long
        };

        let is_limit = matches!(req.order_type, crate::eal::types::OrderType::Limit);
        
        let mexc_req = MexcOrderReq {
            symbol: req.symbol.0.clone(),
            price: req.price.unwrap_or(0.0),
            vol: req.size,
            side: mexc_side,
            r#type: if is_limit { 1 } else { 2 },
            open_type: 2, // Default to Cross
            external_id: req.client_order_id.clone(),
        };

        let body = serde_json::to_string(&mexc_req)
            .map_err(|e| ExecutionError::ExchangeError(e.to_string()))?;
        let headers = self.sign_headers("POST", path, "", &body);

        let url = format!("{}{}", self.base_url, path);
        let resp = self.client.post(&url)
            .headers(headers)
            .body(body)
            .send()
            .await
            .map_err(|e| ExecutionError::ExchangeError(e.to_string()))?;

        let text = resp.text().await.unwrap_or_default();
        let parsed: MexcResp<MexcOrderRespData> = serde_json::from_str(&text)
            .map_err(|e| ExecutionError::ExchangeError(format!("Parse error: {}, text: {}", e, text)))?;

        if !parsed.success {
            return Err(ExecutionError::ExchangeError(format!("MEXC error {}: {:?}", parsed.code, parsed.data)));
        }

        let order_id_str = parsed.data.map(|d| d.order_id).unwrap_or_default();
        let order_id = order_id_str.parse::<u64>().unwrap_or(0);

        Ok(OrderAck {
            order_id: OrderId(order_id),
            client_order_id: req.client_order_id.clone(),
            venue: VenueId::EXCHANGE_B,
            timestamp_ns: Utc::now().timestamp_nanos_opt().unwrap_or(0) as u64,
        })
    }

    async fn cancel_order(&self, order_id: OrderId) -> Result<(), ExecutionError> {
        let path = "/api/v1/private/order/cancel";
        let body = format!("[\"{}\"]", order_id.0);
        let headers = self.sign_headers("POST", path, "", &body);

        let url = format!("{}{}", self.base_url, path);
        let resp = self.client.post(&url)
            .headers(headers)
            .body(body)
            .send()
            .await
            .map_err(|e| ExecutionError::ExchangeError(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(ExecutionError::ExchangeError(format!("MEXC HTTP error: {}", resp.status())));
        }

        Ok(())
    }

    async fn get_positions(&self) -> Result<Vec<Position>, ExecutionError> {
        let path = "/api/v1/private/position/open_details";
        let headers = self.sign_headers("GET", path, "", "");

        let url = format!("{}{}", self.base_url, path);
        let resp = self.client.get(&url)
            .headers(headers)
            .send()
            .await
            .map_err(|e| ExecutionError::ExchangeError(e.to_string()))?;

        let text = resp.text().await.unwrap_or_default();
        let parsed: MexcResp<Vec<MexcPosition>> = serde_json::from_str(&text)
            .map_err(|e| ExecutionError::ExchangeError(format!("Parse error: {}, text: {}", e, text)))?;

        if !parsed.success {
            return Err(ExecutionError::ExchangeError(format!("MEXC error {}: {:?}", parsed.code, parsed.data)));
        }

        let mut positions = Vec::new();
        if let Some(data) = parsed.data {
            for p in data {
                let size = if p.position_type == 1 { p.hold_vol } else { -p.hold_vol };
                positions.push(Position {
                    venue: VenueId::EXCHANGE_B,
                    symbol: Symbol(p.symbol),
                    size,
                    entry_price: p.hold_avg_price,
                    unrealized_pnl: p.unrealised,
                    timestamp_ns: Utc::now().timestamp_nanos_opt().unwrap_or(0) as u64,
                });
            }
        }

        Ok(positions)
    }

    async fn get_account_state(&self) -> Result<AccountState, ExecutionError> {
        let path = "/api/v1/private/account/assets";
        let headers = self.sign_headers("GET", path, "", "");

        let url = format!("{}{}", self.base_url, path);
        let resp = self.client.get(&url)
            .headers(headers)
            .send()
            .await
            .map_err(|e| ExecutionError::ExchangeError(e.to_string()))?;

        let text = resp.text().await.unwrap_or_default();
        let parsed: MexcResp<Vec<MexcAsset>> = serde_json::from_str(&text)
            .map_err(|e| ExecutionError::ExchangeError(format!("Parse error: {}, text: {}", e, text)))?;

        if !parsed.success {
            return Err(ExecutionError::ExchangeError(format!("MEXC error {}: {:?}", parsed.code, parsed.data)));
        }

        let mut available_balance_usd = 0.0;
        if let Some(data) = parsed.data {
            for a in data {
                if a.currency == "USDT" {
                    available_balance_usd = a.available_balance;
                    break;
                }
            }
        }

        Ok(AccountState {
            positions: vec![], // get_positions handles this separately in our runner logic usually
            total_unrealized_pnl: 0.0,
            daily_realized_pnl: 0.0,
            available_balance_usd,
        })
    }

    fn venue_id(&self) -> VenueId {
        VenueId::EXCHANGE_B
    }
}
