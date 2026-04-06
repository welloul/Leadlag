# EAL Module — Exchange Abstraction Layer (v0.6.1)

## Objective
Provide a unified trait-based interface for exchange connectivity. Supports three live venues (Binance, Hyperliquid, OKX, MEXC) with real L2 order book subscriptions.

## Invariants

1. **Trait polymorphism**: All exchanges implement `MarketData` + `OrderExecution`
2. **Arc-wrapped data**: `Arc<Tick>` and `Arc<BookUpdate>` for zero-copy fan-out
3. **Bounded channels**: All channels use `bounded(1024)` for backpressure
4. **try_send only**: Never block the WebSocket task
5. **Real L2 books**: Binance `@depth@100ms` diff stream, HL `l2Book` snapshots, OKX `books5`, MEXC `depth`
6. **Local order book**: `LocalOrderBook` with BTreeMap state for Binance diff stream reconciliation
7. **Symbol normalization**: `ZECUSDT → ZEC`, `LINK-USDT-SWAP → LINK` for cross-venue keying
8. **TCP_NODELAY**: Enforced on all connections for minimum latency

## Key Functions

### `MarketData::subscribe_ticks(symbols) -> Receiver<Arc<Tick>>`
- Binance: separate WS per symbol, `@trade` stream; symbol auto-formatted as `{BASE}usdt`
- OKX: separate WS per symbol, `trades` channel; bare symbol → `{BASE}-USDT-SWAP` instId
- MEXC: WebSocket `sub.deal` channel; symbol formatted as `{BASE}_USDT`
- Hyperliquid: one WS, `trades` channel per symbol

### `MarketData::subscribe_book(symbol) -> Receiver<Arc<BookUpdate>>`
- Binance: `@depth@100ms` diff stream + REST snapshot reconciliation
- OKX: `books5` channel (5-level snapshot, best trade-off between update rate and OBI depth)
- MEXC: `sub.depth` channel (full L2 snapshot)
- Hyperliquid: `l2Book` channel on existing WS

### `OrderExecution::submit_order(order) -> Result<OrderAck>`
- **PaperSimulator**: per-venue L2 matching with conservative fill
- **HyperliquidLiveExecutor**: EIP-712 signing, non-blocking async dispatch, boot-time Meta State sync
- **OkxLiveExecutor**: HMAC-SHA256 signing, REST V5 `/api/v5/trade/order`; instId from `symbol.0` (must be formatted as `BASE-USDT-SWAP` by OMS)
- **MexcLiveExecutor**: HMAC-SHA256 signing, REST Futures `/api/v1/private/order/submit`

## Symbol Normalization (critical)

All exchanges emit symbols in different formats. The EAL normalizes them to bare base symbols at the point of data emission (ticks/books) so the `SignalPipeline` engine map lookup always succeeds.

| Source | Raw format | Normalized |
|--------|-----------|------------|
| Binance | `LINKUSDT` | `LINK` (via `strip_suffix("USDT")`) |
| OKX | `LINK-USDT-SWAP` | `LINK` (set in `run_connection` from caller's `symbol.normalize()`) |
| MEXC | `LINK_USDT` | `LINK` (via `strip_suffix("_USDT")` — TODO: ensure in handle_message) |
| Hyperliquid | `LINK` | `LINK` (already bare) |

> **Bug fixed in v0.6.1**: OKX previously subscribed using bare symbols (`LINK`) which OKX silently rejected. Now correctly converts to `LINK-USDT-SWAP` via `OkxExchange::to_inst_id()`.

## Binance L2 Book

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
5. Gap detection: if `prev_final_update_id != last_update_id`, mark unsynced and re-sync

**LocalOrderBook:**
```
bids: BTreeMap<u64, f64>  // price*1e8 as key, descending
asks: BTreeMap<u64, f64>  // price*1e8 as key, ascending
last_update_id: u64
synced: bool
```

## OKX L2 Book (v0.6.1)

**Stream:** `wss://ws.okx.com:8443/ws/v5/public`

**Channel:** `books5` (5-level snapshot, ~100ms update)

> **Why `books5` not `bbo-tbt`?** `bbo-tbt` sends only 1 level per side. OBI calculated from a single level is essentially tick-by-tick and noisy. `books5` gives 5 levels, enabling the weighted OBI divergence detector to work correctly.

**Subscription message:**
```json
{
  "op": "subscribe",
  "args": [{"channel": "books5", "instId": "LINK-USDT-SWAP"}]
}
```

**Response format:**
```json
{
  "data": [{
    "bids": [["price", "qty", "0", "1"], ...],
    "asks": [["price", "qty", "0", "1"], ...],
    "ts": "1234567890000"
  }]
}
```

## MEXC Futures Market Data

**Stream:** `wss://contract.mexc.com/edge`

**Channels:** `sub.deal` (trades), `sub.depth` (L2 book)

**Subscription:**
```json
{"method": "sub.deal", "param": {"symbol": "LINK_USDT"}}
```

## Exchange Implementations

| Exchange | Venue | Tick rate | Book channel | Levels |
|----------|-------|-----------|-------------|--------|
| Binance (A) | EXCHANGE_A | ~79/sec | `@depth@100ms` diff | 20 |
| OKX (B) | EXCHANGE_B | trade-by-trade | `books5` snapshot | 5 |
| MEXC (B) | EXCHANGE_B | trade-by-trade | `sub.depth` snapshot | variable |
| Hyperliquid (B) | EXCHANGE_B | ~1.3/sec | `l2Book` snapshot | ~20 |

## Hyperliquid Live Execution

**Endpoint:** `https://api.hyperliquid.xyz/exchange`

**Workflow:**
1. **Boot Sync**: Calls `load_asset_context()` to fetch `asset_index` mappings from `/info`.
2. **Signature Pipeline**: Serializes `action` JSON → MessagePack → Keccak256 → EIP-712 sign.
3. **Async Dispatch**: `submit_order` spawns a detached task; returns `OrderAck(Pending)` immediately.
4. **Fills**: Verified via authenticated WebSocket `userEvents` stream.

**Security Firewall:**
- Secondary hardware-check in `hyperliquid_exec.rs` limits max absolute notional to $50.0 regardless of `settings.toml`.
- Rejects any non-Post-Only order in entry phase.

## OKX Live Execution

**Endpoint:** `https://www.okx.com/api/v5/trade/order`

**Auth:** HMAC-SHA256, headers: `OK-ACCESS-KEY`, `OK-ACCESS-SIGN`, `OK-ACCESS-TIMESTAMP`, `OK-ACCESS-PASSPHRASE`
