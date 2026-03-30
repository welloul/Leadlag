# EAL Module — Exchange Abstraction Layer

## Objective
Provide a unified trait-based interface for exchange connectivity. Both live exchanges and mock implementations satisfy the same traits, enabling seamless switching between paper and live trading.

## Latency Profile

| Operation | O(n) | Cycles | Notes |
|-----------|------|--------|-------|
| Tick parsing | O(1) | ~200 | simd-json or serde |
| Channel send | O(1) | ~50 | crossbeam try_send |
| Order submit | O(1) | ~1000 | HTTP/TLS overhead |
| **Total (hot path)** | **O(1)** | **~250** | **Parsing + channel only** |

## Invariants

1. **Trait polymorphism**: All exchanges implement `MarketData` + `OrderExecution`
2. **Arc-wrapped data**: `Arc<Tick>` and `Arc<BookUpdate>` for zero-copy fan-out
3. **Bounded channels**: All channels use `bounded(1024)` for backpressure
4. **try_send only**: Never block the WebSocket task
5. **Real L2 books**: Binance `@depth@100ms` diff stream, Hyperliquid `l2Book` snapshots
6. **Local order book**: `LocalOrderBook` with BTreeMap state for Binance diff stream reconciliation
7. **Symbol normalization**: `ZECUSDT` → `ZEC` for cross-venue keying
8. **Subscriptions per symbol**: Binance creates separate WS per symbol. Hyperliquid subscribes multiple symbols on one WS.

## Memory Layout

```
Tick:
┌─────────────────────────────────────────┐
│ venue: VenueId        (1 byte)          │
│ symbol: Symbol        (24 bytes, String)│ ← Heap allocated
│ price: f64            (8 bytes)         │
│ size: f64             (8 bytes)         │
│ exchange_ts_ns: u64   (8 bytes)         │
│ local_ts_ns: u64      (8 bytes)         │
└─────────────────────────────────────────┘
Total: 57 bytes + String heap

MockExchange:
┌─────────────────────────────────────────┐
│ venue_id: VenueId                       │
│ tick_senders: Arc<Mutex<HashMap<...>>>  │
│ book_senders: Arc<Mutex<HashMap<...>>>  │
│ order_counter: Arc<Mutex<u64>>          │
│ orders: Arc<Mutex<Vec<OrderRequest>>>   │
│ positions: Arc<Mutex<Vec<Position>>>    │
│ simulate_error: Arc<Mutex<bool>>        │
└─────────────────────────────────────────┘
```

## Key Traits

### `MarketData`
```rust
#[async_trait]
pub trait MarketData: Send + Sync {
    async fn subscribe_ticks(&self, symbols: &[Symbol]) 
        -> Result<Receiver<Arc<Tick>>, ExchangeError>;
    async fn subscribe_book(&self, symbol: &Symbol) 
        -> Result<Receiver<Arc<BookUpdate>>, ExchangeError>;
    fn venue_id(&self) -> VenueId;
}
```

### `OrderExecution`
```rust
#[async_trait]
pub trait OrderExecution: Send + Sync {
    async fn submit_order(&self, order: &OrderRequest) 
        -> Result<OrderAck, ExecutionError>;
    async fn cancel_order(&self, id: OrderId) 
        -> Result<(), ExecutionError>;
    async fn get_positions(&self) 
        -> Result<Vec<Position>, ExecutionError>;
    fn venue_id(&self) -> VenueId;
}
```

## Key Functions

### `MockExchange::inject_tick(tick)`
- **Input**: Tick to inject
- **Output**: None
- **Side effects**: Sends tick to all subscribers
- **Complexity**: O(s) where s = subscribers

### `MockExchange::submit_order(order) -> Result<OrderAck>`
- **Input**: Order request
- **Output**: Order acknowledgment
- **Side effects**: Increments order counter, stores order
- **Complexity**: O(1)

## Exchange Implementations

### BinanceExchange (Exchange A)
- **WebSocket**: `wss://fstream.binance.com/ws`
- **Tick format**: `{"e":"trade","s":"BTCUSDT","p":"66439.00","q":"0.001","T":1774740841877}`
- **Book subscription**: `Not implemented` — returns `Err(ExchangeError::Internal)`
- **Tick rate**: ~18/sec per symbol (most liquid venue)

### HyperliquidExchange (Exchange B)
- **WebSocket**: `wss://api.hyperliquid.xyz/ws`
- **Tick format**: `{"channel":"trades","data":[{"coin":"BTC","px":"66439.0","sz":"0.00017","time":1774740841877}]}`
- **Important**: Responses are channel-wrapped (`{"channel":"trades","data":[...]}`), NOT raw arrays. Parser handles both formats.
- **Book subscription**: `Not implemented`
- **Tick rate**: ~1/sec per symbol (significantly slower than Binance)
- **Connection handling**: Detects `Close` frames and `WebSocketError` — logs and exits task (no reconnection yet)

### WebSocket Message Flow
```
1. connect_async(url) → (write, read)
2. send subscribe message: {"method":"subscribe","subscription":{"type":"trades","coin":"BTC"}}
3. receive subscription confirmation: {"channel":"subscriptionResponse",...}
4. receive trades: {"channel":"trades","data":[{"coin":"BTC","px":"...",...}]}
5. parse data as Vec<HyperliquidTrade>, convert to Tick, send to channel
```