# Plan: Real L2 Order Book Subscriptions

## Problem
Both `BinanceExchange::subscribe_book()` and `HyperliquidExchange::subscribe_book()` return `Err("Not implemented")`. The PaperSimulator synthesizes books from trade ticks, causing:
- Stale books (avg 475ms for Binance, 715ms for Hyperliquid)
- No real depth data — synthetic books have uniform 10,000 size per level
- OBI strategy signals limited to tick-based detection only

## Changes

### 1. `src/eal/binance.rs` — Implement `subscribe_book()` with diff stream

**Stream URL:** `wss://fstream.binance.com/ws/{symbol}usdt@depth@100ms`

**Diff stream sends incremental updates, not snapshots.** Response format:
```json
{
  "e": "depthUpdate",
  "E": 123456789,
  "T": 123456788,
  "s": "BTCUSDT",
  "U": 157,
  "u": 160,
  "pu": 149,
  "b": [["0.0024", "10"]],   // bids to update (price, size)
  "a": [["0.0026", "100"]]   // asks to update (price, size)
}
```

**Requires local book state management.** Strategy from Binance docs:
1. Buffer diff events from WS
2. Get REST snapshot: `GET /fapi/v1/depth?symbol=BTCUSDT&limit=20`
3. Drop any event with `u` <= snapshot's `lastUpdateId`
4. First event must have `U <= lastUpdateId+1` and `u >= lastUpdateId+1`
5. Apply each diff: if size=0, remove level; otherwise upsert

**Implementation:**
```rust
struct LocalOrderBook {
    bids: BTreeMap<ReversePrice, f64>,  // descending by price
    asks: BTreeMap<Price, f64>,         // ascending by price
    last_update_id: u64,
}
```

**WS task flow:**
1. Connect to `@depth@100ms` stream
2. Buffer incoming diffs in a `Vec<BinanceDepthUpdate>`
3. Fetch REST snapshot once
4. Drop buffered events with `u <= lastUpdateId`
5. Apply remaining events to local book
6. Continue applying live diffs
7. On each update, convert to `BookUpdate` and send to channel

**New structs:**
```rust
struct BinanceDepthUpdate {
    #[serde(rename = "s")] symbol: String,
    #[serde(rename = "U")] first_update_id: u64,
    #[serde(rename = "u")] final_update_id: u64,
    #[serde(rename = "pu")] prev_final_update_id: u64,
    #[serde(rename = "b")] bids: Vec<[String; 2]>,
    #[serde(rename = "a")] asks: Vec<[String; 2]>,
    #[serde(rename = "E")] event_time: u64,
}

struct BinanceDepthSnapshot {
    last_update_id: u64,
    bids: Vec<[String; 2]>,
    asks: Vec<[String; 2]>,
}
```

### 2. `src/eal/hyperliquid.rs` — Implement `subscribe_book()` with l2Book snapshot

**Subscription:** `{"method":"subscribe","subscription":{"type":"l2Book","coin":"BTC"}}`

**Response (snapshot every ~500ms per block):**
```json
{
  "channel": "l2Book",
  "data": {
    "coin": "BTC",
    "levels": [
      [{"px":"213.45","sz":"100.0","n":5}, ...],  // bids
      [{"px":"213.50","sz":"50.0","n":3}, ...]    // asks
    ],
    "time": 1774740841877
  }
}
```

**No local book state needed** — each message is a full snapshot.

**Logic:**
- Add `l2Book` channel handling alongside existing `trades` in WS task
- Route messages: `trades` → tick sender, `l2Book` → book sender
- Parse `levels[0]` as bids, `levels[1]` as asks
- Convert to `BookUpdate` and send

### 3. `src/main.rs` — Fix book subscription loop + feed real books

**Fix receiver loop** (currently overwrites `book_a = Some(rx)` per symbol):
```rust
let mut book_receivers_a: Vec<(Symbol, Receiver<Arc<BookUpdate>>)> = Vec::new();
let mut book_receivers_b: Vec<(Symbol, Receiver<Arc<BookUpdate>>)> = Vec::new();
for sym in &symbols {
    match exchange_a.subscribe_book(sym).await {
        Ok(rx) => book_receivers_a.push((sym.clone(), rx)),
        Err(e) => warn!("Book sub failed for A {}: {}", sym, e),
    }
    match exchange_b.subscribe_book(sym).await {
        Ok(rx) => book_receivers_b.push((sym.clone(), rx)),
        Err(e) => warn!("Book sub failed for B {}: {}", sym, e),
    }
}
```

**Feed books into simulator + pipeline** in main loop:
```rust
for (sym, ref book_rx) in &book_receivers_a {
    if let Ok(book) = book_rx.try_recv() {
        simulator.update_book(&book);
        if let Some(signal) = pipeline.process_book(&book) {
            // ... handle signal
        }
    }
}
```

## File Impact

| File | Change | Description |
|------|--------|-------------|
| `src/eal/binance.rs` | Refactor + Feature | Diff stream + local book state + REST snapshot |
| `src/eal/hyperliquid.rs` | Feature | Add `l2Book` channel parsing alongside trades |
| `src/main.rs` | Refactor | Fix book receiver loop, feed real books to simulator + pipeline |

## Expected Result
- Binance book freshness: 475ms → ~100ms (diff stream update rate)
- Hyperliquid book freshness: 715ms → ~500ms (block-based snapshots)
- Real L2 depth with actual size-at-level data
- OBI strategy can detect real order book imbalance
- Stale book errors drop significantly
