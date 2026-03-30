# Plan: Fix Impulse-OBI Entry Logic (v2 — tightened)

## Context
The bot currently generates 7 orders/sec with `other_is_lagging = true` hardcoded. This overtrades without verifying cross-venue edge. Previous plan reviewed and tightened with 7 corrections.

## Changes

### 1. Freshness threshold = 400ms (unified, not asymmetric)

**Decision:** User specified 400ms as the freshness gate. Unified threshold for both venues.

**File:** `src/signal/impulse.rs` — `ImpulseDetector::process_tick()`

```rust
venue_freshness_ns: u64,  // from config venue_freshness_ms (default 400)

let a_stale = now - self.tracker_a.last_update_ns > self.venue_freshness_ns;
let b_stale = now - self.tracker_b.last_update_ns > self.venue_freshness_ns;
if a_stale || b_stale {
    return None;
}
```

**MidpriceTracker changes:**
```rust
last_update_ns: u64,  // timestamp of last tick received
```

**Config:** `venue_freshness_ms: u64` (default 400)

### 2. Use LOCAL receive timestamps only (not exchange timestamps)

**Problem:** Exchange timestamps across venues are unreliable — different clocks, drift, rounding.

**File:** `src/signal/impulse.rs`

**Remove:** All exchange timestamp alignment logic from previous plan.

**Use local timestamps for freshness:**
```rust
// In MidpriceTracker::update():
self.last_update_ns = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .unwrap()
    .as_nanos() as u64;

// Delta calculation still uses exchange_ts_ns (consistent within same venue)
// But freshness and lag detection use local receive time
```

**Lag detection via local arrival diff:**
```rust
let arrival_diff_ns = (self.tracker_a.last_update_ns as i64 
    - self.tracker_b.last_update_ns as i64).unsigned_abs() as u64;

// If one venue ticked much more recently than the other, it's leading
// If both ticked recently, can't determine lag from arrival alone
```

### 3. TTL = 500ms

**File:** `settings.toml` — `signal_ttl_ms = 500`

### 4. Spread check needs fees awareness

**Problem:** `entry_threshold_bps = 2` ignores real costs (taker fees 5-10 bps, slippage).

**File:** `src/signal/impulse_obi.rs`

**New config:** `entry_threshold_bps = 8` (realistic minimum: fees + slippage + buffer)

```rust
let edge_bps = match signal.side {
    OrderSide::Buy => (tgt - src) / src * 10_000.0,
    OrderSide::Sell => (src - tgt) / src * 10_000.0,
};
if edge_bps < entry_threshold_bps {
    return None;  // edge < fees + slippage → guaranteed loss
}
```

**Config:** `entry_threshold_bps` (default 8, range 2-50)

### 5. Cooldown should be SIDE-AWARE

**File:** `src/oms/mod.rs`

**Why:** Symbol-only cooldown blocks valid reversals (BUY then SELL on same symbol).

```rust
last_trade_per_symbol: std::collections::HashMap<(String, OrderSide), u64>,

pub async fn process_signal(&mut self, signal: &TradeSignal, ...) -> Result<OrderAck, RiskError> {
    let now = SystemTime::now()...as_nanos() as u64;
    let cooldown_ns = self.strategy_settings.cooldown_ms * 1_000_000;
    let key = (signal.symbol.0.clone(), signal.side);

    if let Some(last) = self.last_trade_per_symbol.get(&key) {
        if now - *last < cooldown_ns {
            return Err(RiskError::ExecutionFailed(
                format!("Cooldown: {:.0}ms since last {:?} {}",
                    (now - *last) as f64 / 1e6, signal.side, signal.symbol)
            ));
        }
    }
    // ... existing preflight + submission ...
    self.last_trade_per_symbol.insert(key, now);
}
```

**Config:** `cooldown_ms` (default 200)

### 6. Book consumption — conservative fill model

**Problem:** `best_size * max_levels_consumed` assumes instant sweep. Reality: latency shifts the book.

**File:** `src/sim/matcher.rs` — add `best_bid_size()` / `best_ask_size()`

**File:** `src/sim/mod.rs` — conservative check:
```rust
let best_size = match order.side {
    OrderSide::Buy => matcher.best_ask_size(),
    OrderSide::Sell => matcher.best_bid_size(),
};
// Conservative: only fill what the best level can handle
let allowed_size = best_size * 0.5;  // 50% of best level
if order.size > allowed_size {
    // Partial fill: reduce order size to allowed_size
    order.size = order.size.min(allowed_size);
}
```

**Config:** `fill_conservatism` (default 0.5 = 50% of best level)

### 7. Weighted OBI + TIME-BASED persistence

**File:** `src/signal/obi_divergence.rs`

**Depth-weighted OBI:**
```rust
fn weighted_obi(&self, book: &BookUpdate) -> f64 {
    let mut weighted_bid = 0.0;
    let mut weighted_ask = 0.0;
    for (i, level) in book.bids.iter().take(self.depth).enumerate() {
        weighted_bid += level.size / (i as f64 + 1.0);
    }
    for (i, level) in book.asks.iter().take(self.depth).enumerate() {
        weighted_ask += level.size / (i as f64 + 1.0);
    }
    let total = weighted_bid + weighted_ask;
    if total > 0.0 { (weighted_bid - weighted_ask) / total } else { 0.0 }
}
```

**Time-based persistence:**
```rust
obi_persist_start: HashMap<Symbol, Option<u64>>,
obi_persist_ns: u64,  // default 200ms

// In process_book():
let obi = self.weighted_obi(book);
let start = self.obi_persist_start.entry(book.symbol.clone()).or_insert(None);
if obi.abs() > self.strong_threshold {
    if start.is_none() {
        *start = Some(std::time::SystemTime::now()...as_nanos() as u64);
    }
    if let Some(s) = start {
        if now - *s < self.obi_persist_ns {
            return None;
        }
    }
} else {
    *start = None;
}
```

**Config:** `obi_persist_ms` (default 200)

### 8. Per-symbol performance tracking (add to heartbeat)

**File:** `src/main.rs` — add per-symbol PnL and hit rate to heartbeat

```rust
// Track per-symbol stats
symbol_fills: HashMap<String, u64>,
symbol_rejects: HashMap<String, u64>,

// In heartbeat:
for sym in &symbols {
    let fills = symbol_fills.get(&sym.0).unwrap_or(&0);
    let rejects = symbol_rejects.get(&sym.0).unwrap_or(&0);
    let rate = if fills + rejects > 0 { fills * 100 / (fills + rejects) } else { 0 };
    info!("  {}: {} fills, {} rejects, {}% hit rate", sym, fills, rejects, rate);
}
```

## Config Changes

**File:** `src/config/schema.rs` — `StrategySettings`

New fields:
```rust
#[validate(range(min = 50, max = 2000))]
pub venue_freshness_ms: u64,        // default 400

#[validate(range(min = 2, max = 50))]
pub entry_threshold_bps: u64,       // default 8 (fees-aware)

#[validate(range(min = 50, max = 5000))]
pub cooldown_ms: u64,               // default 200

#[validate(range(min = 1, max = 10))]
pub max_levels_consumed: usize,     // default 3

#[validate(range(min = 50, max = 2000))]
pub obi_persist_ms: u64,            // default 200

#[validate(range(min = 0.1, max = 1.0))]
pub fill_conservatism: f64,         // default 0.5
```

**File:** `settings.toml`
```toml
signal_timeout_ms = 150
signal_ttl_ms = 500
venue_freshness_ms = 400
entry_threshold_bps = 8
cooldown_ms = 200
max_levels_consumed = 3
obi_persist_ms = 200
fill_conservatism = 0.5
```

## File Impact

| File | Change | Description |
|------|--------|-------------|
| `src/signal/impulse.rs` | Refactor | Local timestamps, freshness gate (400ms), remove exchange TS alignment |
| `src/signal/impulse_obi.rs` | Feature | Fees-aware spread check (8 bps) |
| `src/signal/obi_divergence.rs` | Feature | Weighted OBI, time-based persistence |
| `src/sim/matcher.rs` | Feature | best_bid_size/best_ask_size |
| `src/sim/mod.rs` | Feature | Conservative fill (50% of best level) |
| `src/oms/mod.rs` | Feature | Side-aware cooldown |
| `src/main.rs` | Feature | 400ms book age gate, per-symbol performance tracking |
| `src/config/schema.rs` | Feature | 6 new config fields |
| `settings.toml` | Config | New defaults |

## Expected Impact
- Fill rate: 7/sec → ~1-2/sec (higher quality)
- Cross-venue edge verified (8 bps minimum, fees-aware)
- Spoofing filtered by time-based OBI persistence (200ms)
- Overtrading prevented by side-aware cooldown (200ms)
- Realistic fills (50% of best level)
- No "illusion lag" from exchange timestamp drift
- Per-symbol performance visible in heartbeat
