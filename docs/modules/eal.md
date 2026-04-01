# EAL Module — Exchange Abstraction Layer (v0.1.3)

## Objective
Provide a unified trait-based interface for exchange connectivity. Implements real L2 order book subscriptions via Binance diff stream and Hyperliquid l2Book snapshots.

## Invariants

1. **Trait polymorphism**: All exchanges implement `MarketData` + `OrderExecution`
2. **Arc-wrapped data**: `Arc<Tick>` and `Arc<BookUpdate>` for zero-copy fan-out
3. **Bounded channels**: All channels use `bounded(1024)` for backpressure
4. **try_send only**: Never block the WebSocket task
5. **Real L2 books**: Binance `@depth@100ms` diff stream, Hyperliquid `l2Book` snapshots
6. **Local order book**: `LocalOrderBook` with BTreeMap state for Binance diff stream reconciliation
7. **Symbol normalization**: `ZECUSDT` → `ZEC` for cross-venue keying

## Key Functions

### `MarketData::subscribe_ticks(symbols) -> Receiver<Arc<Tick>>`
- Binance: separate WS per symbol, `@trade` stream
- Hyperliquid: one WS, subscribes `trades` channel per symbol

### `MarketData::subscribe_book(symbol) -> Receiver<Arc<BookUpdate>>`
- Binance: `@depth@100ms` diff stream + REST snapshot reconciliation
- Hyperliquid: `l2Book` channel on existing WS (alongside `trades`)

### `OrderExecution::submit_order(order) -> Result<OrderAck>`
- **PaperSimulator**: per-venue L2 matching with conservative fill
- **HyperliquidLiveExecutor**: 
    - EIP-712 cryptographic signing (`ethers-core`)
    - Non-blocking async dispatch (`tokio::spawn`)
    - Post-Only (`tif: Alo`) and Reduce-Only support
    - Boot-time Meta State synchronization (`/info` meta)

## Binance L2 Book (v0.1.2)

**Stream:** `wss://fstream.binance.com/ws/{symbol}usdt@depth@100ms`

**Diff stream format:**
```json
{
  "e": "depthUpdate",
  "s": "BTCUSDT",
  "U": 157, "u": 160, "pu": 156,
  "b": [["60000.00", "2.5"]],
  "a": [["60001.00", "1.5"]]
}
```

**Reconciliation strategy (per Binance docs):**
1. Buffer diffs from WS
2. Fetch REST snapshot: `GET /fapi/v1/depth?symbol=BTCUSDT&limit=20`
3. Drop diffs with `u <= lastUpdateId`
4. Apply remaining diffs to `LocalOrderBook`
5. Gap detection: if `prev_final_update_id != last_update_id`, mark unsynced

**LocalOrderBook:**
```
bids: BTreeMap<u64, f64>  // price*1e8 as key, descending
asks: BTreeMap<u64, f64>  // price*1e8 as key, ascending
last_update_id: u64
synced: bool
```

## Hyperliquid L2 Book (v0.1.2)

**Subscription:** `{"method":"subscribe","subscription":{"type":"l2Book","coin":"BTC"}}`

**Response:**
```json
{
  "channel": "l2Book",
  "data": {
    "coin": "BTC",
    "levels": [
      [{"px":"213.45","sz":"100.0","n":5}],
      [{"px":"213.50","sz":"50.0","n":3}]
    ],
    "time": 1774740841877
  }
}
```

**No local state needed** — each message is a full snapshot. levels[0] = bids, levels[1] = asks.

## Exchange Implementations

| Exchange | Tick rate | Book rate | Book stream |
|----------|-----------|-----------|-------------|
| Binance (A) | ~79/sec | ~80/sec | `@depth@100ms` diff |
| Hyperliquid (B) | ~1.3/sec | ~4/sec | `l2Book` snapshot per block |

## Hyperliquid Live Execution (v0.2.0)

**Endpoint:** `https://api.hyperliquid.xyz/exchange` (REST for submission)

**Workflow:**
1. **Boot Sync**: Calls `load_asset_context()` to fetch `asset_index` mappings from `/info`.
2. **Signature Pipeline**:
    - Serializes `action` JSON to canonical MessagePack.
    - Hashes with Keccak256.
    - Signs EIP-712 Typed Data with API Private Key.
3. **Async Dispatch**: `submit_order` spawns a detached task; returns `OrderAck(Pending)` immediately.
4. **Fills**: Verified via authenticated WebSocket `userEvents` stream.

**Security Firewall:**
- Secondary hardware-check in `hyperliquid_exec.rs` limits max absolute notional to $50.0 regardless of `settings.toml`.
- Rejects any non-Post-Only order in entry phase.
