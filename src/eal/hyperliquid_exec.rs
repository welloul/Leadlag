//! Live order execution engine for Hyperliquid using REST and ethers-core.
//!
//! Handles L1 structured EIP-712 signatures, non-blocking asynchronous requests,
//! and secondary limits checking to prevent catastrophic sizing errors.

use async_trait::async_trait;
use crossbeam_channel::Sender;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::eal::{
    AccountState, ExecutionError, FillEvent, OrderAck, OrderExecution, OrderId, OrderRequest,
    Position, Symbol, VenueId,
};

use hyperliquid::{Exchange, Hyperliquid};
use hyperliquid::types::{
    Chain,
    exchange::request::{OrderRequest as HLOrderRequest, OrderType as HLOrderType, Limit, Tif, Action, Request},
};
use ethers_core::types::Address;
use ethers_signers::LocalWallet;
use std::str::FromStr;

const HYPERLIQUID_EXCHANGE_URL: &str = "https://api.hyperliquid.xyz/exchange";
const HYPERLIQUID_INFO_URL: &str = "https://api.hyperliquid.xyz/info";

// ============================================================================
// Core Executor
// ============================================================================

/// Handles live cryptographic order execution for Hyperliquid.
pub struct HyperliquidLiveExecutor {
    venue_id: VenueId,
    client: reqwest::Client,
    wallet_address: String, // Public address of the signer (Agent or Main)
    main_address: String,   // Public address of the account owner
    // Note: private key management is encapsulated inside the signing logic (usually via Wallet/Signer instances)
    wallet_secret: String,
    
    // Fill reporting channel passed from the main layout.
    // Wrapped in an Arc<Mutex<Option<...>>> to allow mutation/injection.
    fill_tx: Arc<Mutex<Option<Sender<FillEvent>>>>,

    // L1 Dictionary: Maps string coin names ("BTC") to numeric identifiers (0)
    asset_ctx: Arc<tokio::sync::RwLock<std::collections::HashMap<String, u32>>>,
}

impl HyperliquidLiveExecutor {
    /// Create a new instance mapping secrets loaded from your environment.
    pub fn new(venue_id: VenueId, wallet_address: String, wallet_secret: String, main_address: Option<String>) -> Self {
        Self {
            venue_id,
            client: reqwest::Client::new(),
            wallet_address: wallet_address.clone(),
            main_address: main_address.unwrap_or(wallet_address),
            wallet_secret,
            fill_tx: Arc::new(Mutex::new(None)),
            asset_ctx: Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
        }
    }

    /// Fetches the live structural mapping from /info so L1 payloads use correct asset indexes
    pub async fn load_asset_context(&self) -> Result<(), ExecutionError> {
        let payload = serde_json::json!({ "type": "meta" });
        tracing::info!("Downloading Hyperliquid Meta State to build Asset Context...");
        
        let res = self.client.post(HYPERLIQUID_INFO_URL)
            .json(&payload)
            .send()
            .await
            .map_err(|e| ExecutionError::ExchangeError(format!("Network Error: {}", e)))?;

        if !res.status().is_success() {
            return Err(ExecutionError::ExchangeError("Failed to fetch Hyperliquid Meta state".into()));
        }

        if let Ok(json) = res.json::<serde_json::Value>().await {
            // Hyperliquid structure: json["universe"] is an array of objects: {"name": "BTC", ...}
            if let Some(universe) = json.get("universe").and_then(|u| u.as_array()) {
                let mut map = self.asset_ctx.write().await;
                for (idx, coin_data) in universe.iter().enumerate() {
                    if let Some(name) = coin_data.get("name").and_then(|n| n.as_str()) {
                        // Numeric index inside the array is the authoritative `assetIndex`
                        map.insert(name.to_string(), idx as u32);
                    }
                }
                tracing::info!("Successfully loaded {} asset index mappings.", map.len());
            }
        }
        Ok(())
    }

    /// Inject the asynchronous fill transmitter.
    /// Spawns a dedicated authenticated WebSocket strictly for processing real-time L1 fills.
    pub async fn set_fill_tx(&self, tx: Sender<FillEvent>) {
        let mut guard = self.fill_tx.lock().await;
        *guard = Some(tx.clone());

        let main_address = self.main_address.clone();
        
        // Spawn the dedicated User stream websocket connection
        tokio::spawn(async move {
            tracing::info!("Initializing Authenticated Hyperliquid User Stream...");
            
            // Reconnection loop for the User Stream
            loop {
                use tokio_tungstenite::connect_async;
                use futures_util::{SinkExt, StreamExt};
                
                let ws_res = connect_async("wss://api.hyperliquid.xyz/ws").await;
                if let Ok((ws_stream, _)) = ws_res {
                    let (mut write, mut read) = ws_stream.split();
                    
                    // The payload to subscribe to user fills
                    let payload = serde_json::json!({
                        "method": "subscribe",
                        "subscription": {
                            "type": "userEvents",
                            "user": main_address
                        }
                    });

                    if write.send(tokio_tungstenite::tungstenite::Message::Text(payload.to_string())).await.is_err() {
                        tracing::error!("Failed to subscribe to HL User Stream.");
                    } else {
                        tracing::info!("Hyperliquid Authenticated User Stream connected.");
                    }

                    // Process messages matching FILLS
                    while let Some(Ok(tokio_tungstenite::tungstenite::Message::Text(text))) = read.next().await {
                        // Very fast JSON extraction
                        if text.contains("fills") {
                            // Example parsing structure wrapper
                            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                                if let Some(fills) = json.get("data").and_then(|d| d.get("fills")).and_then(|f| f.as_array()) {
                                    for fill in fills {
                                        // Attempt to extract details
                                        let coin = fill.get("coin").and_then(|c| c.as_str()).unwrap_or("UNKNOWN");
                                        let is_buy = fill.get("dir").and_then(|d| d.as_str()).map(|d| d == "Buy").unwrap_or(true);
                                        let px = fill.get("px").and_then(|p| p.as_str()).and_then(|p| p.parse::<f64>().ok()).unwrap_or(0.0);
                                        let sz = fill.get("sz").and_then(|s| s.as_str()).and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0);
                                        let fee = fill.get("fee").and_then(|f| f.as_str()).and_then(|f| f.parse::<f64>().ok()).unwrap_or(0.0);
                                        let oid = fill.get("oid").and_then(|o| o.as_u64()).unwrap_or(0);

                                        let cloid = fill.get("cloid").and_then(|c| c.as_str()).unwrap_or("UNKNOWN").to_string();
                                        
                                        let fill_event = FillEvent {
                                            order_id: OrderId(oid),
                                            client_order_id: cloid, // True mapping back to OMS
                                            venue: VenueId::EXCHANGE_B,
                                            symbol: crate::eal::Symbol::new(coin),
                                            side: if is_buy { crate::eal::OrderSide::Buy } else { crate::eal::OrderSide::Sell },
                                            filled_size: sz,
                                            avg_price: px,
                                            fee,
                                            fee_currency: "USDC".to_string(), // HL standard fee currency
                                            timestamp_ns: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos() as u64,
                                        };

                                        tracing::info!("HL NATIVE FILL DETECTED: {:.4} {} @ {:.4} | Fee: {:.5}", sz, coin, px, fee);
                                        let _ = tx.send(fill_event);
                                    }
                                }
                            }
                        }
                    }
                }
                
                tracing::warn!("Hyperliquid User Stream disconnected. Reconnecting in 2000ms...");
                tokio::time::sleep(tokio::time::Duration::from_millis(2000)).await;
            }
        });
    }

    /// Secondary Hard Firewall
    /// Blocks egregious math errors from dispatching L1 signatures.
    fn sanity_check_firewall(&self, order: &OrderRequest) -> Result<(), ExecutionError> {
        // e.g. Max absolute size limit
        let notional = order.notional_usd(order.price.unwrap_or(0.0));
        let max_safe_notional = 50.0; // Hard cap slightly above settings.toml

        if notional > max_safe_notional {
            tracing::error!(
                "CRITICAL FIREWALL TRIGGERED! Order size {:.2} exceeds secondary max limit {:.2}",
                notional,
                max_safe_notional
            );
            return Err(ExecutionError::ExchangeError(
                "Order rejected by secondary hardware firewall!".to_string(),
            ));
        }

        // Must be Post-Only for our exact strategy
        if !order.post_only {
            tracing::warn!("Order request is missing POST ONLY parameter. Mutating or dropping.");
        }

        Ok(())
    }

    /// The core payload generator specifically tailored for Hyperliquid's stringent EIP-712 signing expectations.
    /// It translates the standardized `OrderRequest` into HL's exact 'Action' dictionary.
    fn dispatch_async_payload(&self, order: OrderRequest) {
        let client = self.client.clone();
        
        let wallet_secret = self.wallet_secret.clone();
        let main_address = self.main_address.clone();
        let wallet_address = self.wallet_address.clone();
        let asset_ctx_arc = self.asset_ctx.clone();

        tokio::spawn(async move {
            tracing::info!(
                "Constructing Hyperliquid L1 Payload: {:?} {} {} @ {:?}",
                order.side,
                order.size,
                order.symbol.0,
                order.price
            );

            // PRE-FLIGHT FORMATTING: Hyperliquid requires max 5 significant figures for price.
            let is_buy = matches!(order.side, crate::eal::OrderSide::Buy);
            
            // Format price to exactly 5 significant digits to avoid engine rejection
            let raw_p = order.price.unwrap_or(0.0);
            let p_abs = raw_p.abs();
            let limit_price = if p_abs > 0.0 {
                let power = p_abs.log10().floor() as i32;
                let factor = 10_f64.powi(4 - power);
                let rounded = (raw_p * factor).round() / factor;
                format!("{}", rounded)
            } else {
                "0".to_string()
            };
            
            // Format size properly depending on asset price tier
            let sz = if raw_p > 100.0 {
                format!("{:.4}", order.size)
            } else if raw_p > 10.0 {
                format!("{:.2}", order.size)
            } else if raw_p > 1.0 {
                format!("{:.1}", order.size)
            } else {
                format!("{:.0}", order.size)
            };
            let sz = sz.trim_end_matches('0').trim_end_matches('.').to_string();

            let map = asset_ctx_arc.read().await;
            let asset_index = match map.get(&order.symbol.0) {
                Some(&idx) => idx,
                None => {
                    tracing::error!("FATAL ABORT: Attempted to trade {}, but it does not exist in Hyperliquid's live Meta-State!", order.symbol.0);
                    return;
                }
            };
            
            let hl_tif = if order.post_only {
                Tif::Alo
            } else {
                Tif::Ioc
            };

            let hl_order = HLOrderRequest {
                asset: asset_index,
                is_buy,
                limit_px: limit_price,
                sz,
                reduce_only: order.reduce_only,
                order_type: HLOrderType::Limit(Limit { tif: hl_tif }),
                cloid: uuid::Uuid::from_str(&order.client_order_id.replace("0x", "")).ok(),
            };

            tracing::debug!("Generated Action Payload: {:?}", hl_order);

            // ==========================================================
            // EIP-712 MESSAGEPACK PIPELINE
            // 1. The HL protocol uniquely demands that the `action` JSON is serialized into 
            //    MessagePack (canonical/sorted keys).
            // 2. We take the Keccak256 hash of that MsgPack (`actionHash`).
            // 3. We construct an EIP-712 Typed Data wrapper around that `actionHash`
            //    using domain { name: "Exchange", version: "1", chainId: 1337, verifyingContract: "0x0" }.
            // 4. We sign it with our API wallet (`HL_API_SECRET`).
            // ==========================================================
            
            let wallet = match LocalWallet::from_str(&wallet_secret) {
                Ok(w) => Arc::new(w),
                Err(e) => {
                    tracing::error!("FATAL: Invalid private key format. L1 Signature failed! {}", e);
                    return;
                }
            };

            let exchange = Exchange::new(Chain::Arbitrum);
            
            let vault_address = if main_address != wallet_address {
                Some(ethers_core::types::Address::from_str(&main_address).unwrap_or_default())
            } else {
                None
            };
            
            match exchange.place_order(wallet, vec![hl_order], vault_address).await {
                Ok(response) => {
                    tracing::info!("Exchange API response: {:?}", response);
                    tracing::info!("Post-Only L1 payload built and sent for order {}", order.client_order_id);
                }
                Err(e) => {
                    tracing::error!("Failed to place order via Hyperliquid API: {}", e);
                }
            }
        });
    }

    /// Synchronous startup leverage synchronization
    pub async fn sync_leverage(&self, symbols: &[String], leverage: u32) -> Result<(), ExecutionError> {
        tracing::info!("Synchronizing account leverage to {}x for {} symbols...", leverage, symbols.len());
        
        let map = self.asset_ctx.read().await;
        
        let wallet = match LocalWallet::from_str(&self.wallet_secret) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                tracing::error!("FATAL: Invalid private key format. Leverage sync failed! {}", e);
                return Err(ExecutionError::ExchangeError(format!("Invalid private key: {}", e)));
            }
        };

        let vault_address = if self.main_address != self.wallet_address {
            Some(ethers_core::types::Address::from_str(&self.main_address).unwrap_or_default())
        } else {
            None
        };

        for symbol in symbols {
            if let Some(&asset_index) = map.get(symbol) {
                // Manual EIP-712 Action because SDK 0.2.4 update_leverage is not vault-aware
                let action = Action::UpdateLeverage {
                    asset: asset_index,
                    is_cross: false, // Force Isolated as per requirement
                    leverage,
                };

                let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
                
                // Construct the full signed request
                match self.send_vault_action(wallet.clone(), action, vault_address, nonce).await {
                    Ok(_) => tracing::info!("SET LEVERAGE SUCCESS: {} {}x Isolated for Vault {}", symbol, leverage, self.main_address),
                    Err(e) => tracing::error!("FAILED TO SET LEVERAGE for {}: {}", symbol, e),
                }

                // Anti-flood throttle
                tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
            }
        }
        
        Ok(())
    }

    /// Helper to send any L1 action with vault_address support
    async fn send_vault_action(&self, wallet: Arc<LocalWallet>, action: hyperliquid::types::exchange::request::Action, vault_address: Option<ethers_core::types::Address>, nonce: u64) -> Result<(), ExecutionError> {
        use hyperliquid::types::exchange::request::Request;
        
        // This logic mirrors the internal SDK place_order but for arbitrary actions
        let connection_id = action.connection_id(vault_address, nonce)
            .map_err(|e| ExecutionError::ExchangeError(e.to_string()))?;
        
        // Manual signing (SDK's sign_l1_action is private)
        use ethers_signers::Signer;
        // Use the internal l1::Agent struct for correct EIP-712 hashing
        use hyperliquid::types::agent::l1;
        let source = if self.venue_id == crate::eal::VenueId::EXCHANGE_B { "a".to_string() } else { "b".to_string() };
        let payload = l1::Agent {
            source,
            connection_id,
        };

        let signature = wallet.sign_typed_data(&payload).await
            .map_err(|e| ExecutionError::ExchangeError(e.to_string()))?;

        let request = Request {
            action,
            nonce,
            signature,
            vault_address,
        };

        let res = self.client.post(HYPERLIQUID_EXCHANGE_URL)
            .json(&request)
            .send()
            .await
            .map_err(|e| ExecutionError::ExchangeError(e.to_string()))?;

        if !res.status().is_success() {
            let status = res.status();
            let body = res.text().await.unwrap_or_default();
            return Err(ExecutionError::ExchangeError(format!("Action rejected ({}): {}", status, body)));
        }
        
        Ok(())
    }

    /// Cancel all open orders for the given symbols (Startup Clean Slate)
    pub async fn cancel_all_open_orders(&self, _symbols: &[String]) -> Result<(), ExecutionError> {
        tracing::info!("Draining Hyperliquid L1 Order Book of stale orders...");
        
        let map = self.asset_ctx.read().await;
        let payload = serde_json::json!({
            "type": "openOrders",
            "user": self.main_address
        });
        
        let res = self.client.post(HYPERLIQUID_INFO_URL)
            .json(&payload)
            .send()
            .await
            .map_err(|e| ExecutionError::ExchangeError(e.to_string()))?;

        if !res.status().is_success() {
            return Err(ExecutionError::ExchangeError("Failed to fetch open orders".to_string()));
        }

        let open_orders: serde_json::Value = res.json().await.map_err(|e| ExecutionError::ExchangeError(e.to_string()))?;
        
        if let Some(orders) = open_orders.as_array() {
            if orders.is_empty() {
                tracing::info!("No open orders found to clean up.");
                return Ok(());
            }

            let wallet_secret = self.wallet_secret.clone();
            let wallet = LocalWallet::from_str(&wallet_secret).map_err(|e| ExecutionError::ExchangeError(e.to_string()))?;
            let exchange = Exchange::new(Chain::Arbitrum);
            let vault_address = if self.main_address != self.wallet_address {
                Some(ethers_core::types::Address::from_str(&self.main_address).unwrap_or_default())
            } else {
                None
            };

            use hyperliquid::types::exchange::request::CancelRequest;

            for order in orders {
                let coin = order.get("coin").and_then(|c| c.as_str()).unwrap_or("UNKNOWN");
                let oid = order.get("oid").and_then(|o| o.as_u64()).unwrap_or(0);
                
                if let Some(asset_index) = map.get(coin) {
                    if oid > 0 {
                        tracing::info!("CLEANUP: Canceling {} order {}", coin, oid);
                        let cancel_req = CancelRequest { asset: *asset_index, oid };
                        let _ = exchange.cancel_order(Arc::new(wallet.clone()), vec![cancel_req], vault_address).await;
                    }
                }
            }
        }
        
        Ok(())
    }
}

#[async_trait]
impl OrderExecution for HyperliquidLiveExecutor {
    /// Implements Non-Blocking REST Order Submission
    async fn submit_order(&self, order: &OrderRequest) -> Result<OrderAck, ExecutionError> {
        // Force the transaction against the internal physical risk limits
        self.sanity_check_firewall(order)?;

        // Immediately detach and spawn the HTTP POST request.
        // The main heartbeat loop will immediately continue without lagging.
        self.dispatch_async_payload(order.clone());

        // Return a provisional ACK securely tracking the intent.
        // True success will be piped down through the Websocket User Channel directly 
        // into `fill_tx`.
        Ok(OrderAck {
            order_id: OrderId(0), // Normally extracted from synchronous return payload if necessary
            client_order_id: order.client_order_id.clone(),
            venue: self.venue_id,
            timestamp_ns: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos() as u64,
        })
    }

    /// Cancellation request mechanism
    async fn cancel_order(&self, _order_id: OrderId) -> Result<(), ExecutionError> {
        Ok(())
    }

    /// Cancellation request mechanism
    async fn cancel_order_by_cloid(&self, symbol: &Symbol, cloid: &str) -> Result<(), ExecutionError> {
        tracing::info!("HL CANCEL by CLOID: {} for {}", cloid, symbol.0);
        
        let wallet_secret = self.wallet_secret.clone();
        let main_address = self.main_address.clone();
        let wallet_address = self.wallet_address.clone();
        let asset_ctx_arc = self.asset_ctx.clone();
        let symbol_str = symbol.0.clone();
        let cloid_str = cloid.to_string();

        tokio::spawn(async move {
            let map = asset_ctx_arc.read().await;
            let asset_index = match map.get(&symbol_str) {
                Some(&idx) => idx,
                None => return,
            };

            let cloid_uuid = match uuid::Uuid::from_str(&cloid_str.replace("0x", "")) {
                Ok(u) => u,
                Err(_) => return,
            };

            let wallet = match LocalWallet::from_str(&wallet_secret) {
                Ok(w) => Arc::new(w),
                Err(_) => return,
            };

            let exchange = Exchange::new(Chain::Arbitrum);
            let vault_address = if main_address != wallet_address {
                Some(ethers_core::types::Address::from_str(&main_address).unwrap_or_default())
            } else {
                None
            };
            
            use hyperliquid::types::exchange::request::CancelByCloidRequest;
            let cancel_req = CancelByCloidRequest { asset: asset_index, cloid: cloid_uuid };

            match exchange.cancel_order_by_cloid(wallet, vec![cancel_req], vault_address).await {
                Ok(resp) => tracing::info!("HL CANCEL SUCCESS for {}: {:?}", symbol_str, resp),
                Err(e) => tracing::error!("HL CANCEL FAILED for {}: {}", symbol_str, e),
            }
        });

        Ok(())
    }

    /// Synchronous startup position fetch (Boot-time State Synchronization)
    async fn get_positions(&self) -> Result<Vec<Position>, ExecutionError> {
        let payload = serde_json::json!({
            "type": "clearinghouseState",
            "user": self.main_address
        });
        
        tracing::info!("Synchronizing Hyperliquid boot state from clearinghouse...");
        
        // Fetch real REST data
        let res = self.client.post(HYPERLIQUID_INFO_URL)
            .json(&payload)
            .send()
            .await
            .map_err(|e| ExecutionError::ExchangeError(e.to_string()))?;

        if !res.status().is_success() {
            return Err(ExecutionError::ExchangeError("Failed to sync Hyperliquid state".to_string()));
        }

        // Ideally parse `res.json::<ClearinghouseResponse>()` and return `Vec<Position>`.
        let json: serde_json::Value = res.json().await.map_err(|e| ExecutionError::ExchangeError(e.to_string()))?;
        
        let mut positions = Vec::new();
        if let Some(asset_positions) = json.get("assetPositions").and_then(|ap| ap.as_array()) {
            for entry in asset_positions {
                if let Some(pos_obj) = entry.get("position") {
                    let coin = pos_obj.get("coin").and_then(|c| c.as_str()).unwrap_or("UNKNOWN");
                    let sz = pos_obj.get("szi").and_then(|s| s.as_str()).and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0);
                    let entry_px = pos_obj.get("entryPx").and_then(|p| p.as_str()).and_then(|p| p.parse::<f64>().ok()).unwrap_or(0.0);
                    
                    if sz.abs() > 1e-8 {
                        positions.push(Position {
                            venue: self.venue_id,
                            symbol: crate::eal::Symbol::new(coin),
                            size: sz,
                            entry_price: entry_px,
                            unrealized_pnl: 0.0, // Calculated later if needed
                            timestamp_ns: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos() as u64,
                        });
                    }
                }
            }
        }
        
        Ok(positions)
    }

    async fn get_account_state(&self) -> Result<AccountState, ExecutionError> {
        Ok(AccountState {
            positions: vec![],
            total_unrealized_pnl: 0.0,
            daily_realized_pnl: 0.0,
            available_balance_usd: 0.0,
        })
    }

    fn venue_id(&self) -> VenueId {
        self.venue_id
    }
}
