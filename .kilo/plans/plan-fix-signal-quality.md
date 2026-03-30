# Plan: Fix Signal Quality & Execution Logic (Critical Bugs)

## Context
The bot is losing money on Binance because of 6 critical bugs in the signal and execution layer. The strategy logic is sound but the implementation has timestamp mismatches, inverted edge calculations, and architectural assumptions that break on Binance (deep, efficient market) while accidentally working on Hyperliquid (thin, momentum-driven).

## Step 1: Fix Timestamp Mismatch (CRITICAL)

**Problem:** Three different clocks mixed:
- `impulse_obi.rs:117` — `tick.exchange_ts_ns` (exchange time)
- `obi_divergence.rs:96` — `now_ns()` (local time)
- `impulse.rs:174` — `now_ns()` (local time)

**Fix:** Unify all signal timestamps to `exchange_ts_ns`. Keep `now_ns()` only for freshness gating.

**Files:**
- `src/signal/impulse.rs:174` — change `let timestamp_ns = now_ns()` to `let timestamp_ns = tick.exchange_ts_ns`
- `src/signal/obi_divergence.rs:96` — change `let timestamp_ns = now_ns()` to `let timestamp_ns = book.exchange_ts_ns`
- `src/signal/impulse_obi.rs:117` — already uses `tick.exchange_ts_ns` ✅
- `src/signal/impulse_obi.rs:183` — already uses `book.exchange_ts_ns` ✅

## Step 2: Fix Edge Calculation (Inverted)

**Problem:** `edge_bps` Buy = `(target - source) / source`. If target is CHEAPER than source (good buy), this is NEGATIVE. The check `edge >= 8 bps` rejects good trades.

**Fix:**
```rust
OrderSide::Buy => (source_mid - target_mid) / target_mid * 10_000.0,
OrderSide::Sell => (target_mid - source_mid) / target_mid * 10_000.0,
```

**File:** `src/signal/impulse_obi.rs:93-96`

## Step 3: Use Book Mid Instead of Trade Price

**Problem:** `tick.price` is last trade, not midprice. In thin markets, single trades create false impulses.

**Fix:** Feed book midprice `(best_bid + best_ask) / 2.0` into MidpriceTracker when available. Fall back to trade price.

**Files:**
- `src/signal/impulse.rs` — add `MidpriceTracker::update_from_book(mid, ts_ns)`, add `ImpulseDetector::process_book()`
- `src/signal/mod.rs` — add `process_book_for_impulse()` to pipeline
- `src/main.rs` — call it in book processing loop

## Step 4: Disable MEDIUM Signals

**Problem:** 90%+ of trades are MEDIUM (unconfirmed). Noise.

**Fix:** Add `high_conviction_only: bool` config flag (default true). Gate MEDIUM returns.

**Files:**
- `src/config/schema.rs` — add field
- `src/signal/impulse_obi.rs` — gate MEDIUM returns
- `settings.toml` — `high_conviction_only = true`

## Step 5: Fix Cumulative Notional (Always Increases)

**Problem:** `cumulative_notional += trade_notional` even when reducing. Grows monotonically.

**Fix:** Subtract notional on reduce, clear when position fully closed.

**File:** `src/oms/mod.rs:334-341`

## Step 6: Fix Lag Detection (Delta Drift)

**Problem:** `current_delta()` returns stale delta (prev_mid only updates every window).

**Fix:** Replace with direct midprice comparison:
```rust
let spread_bps = (mid_a - mid_b) / mid_b * 10_000.0;
let other_is_lagging = spread_bps > lag_threshold; // A ahead of B
```

**File:** `src/signal/impulse.rs:210-220`

## Step 7: Reduce Cooldown

**Problem:** 200ms blocks follow-up trades.

**Fix:** `cooldown_ms = 20`

**File:** `settings.toml`

## Step 8: Log Signal Distribution

**Problem:** No HIGH vs MEDIUM visibility.

**Fix:** Add counters, log in heartbeat.

**File:** `src/main.rs`

## Expected Impact
- Trade count: ↓↓↓ (only HIGH signals)
- PnL quality: ↑↑↑ (confirmed signals only)
- Binance losses: stop (no noise trades)
- Edge: correct (positive = good trade)
- Lag: robust (midprice comparison, not stale delta)
- Positions: correct (notional decreases on reduce)
