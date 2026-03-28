# Impulse-OBI Strategy

## Overview

The Impulse-OBI strategy is an **event-driven microstructure alpha** strategy that exploits real-time market microstructure inefficiencies between cryptocurrency exchanges. Unlike the correlation-hysteresis strategy which relies on statistical lead-lag relationships, Impulse-OBI reacts to immediate market events.

## Core Philosophy

> "Be EARLY, not fastest"

The strategy detects:
1. **Trade Impulse** — One exchange moves, the other hasn't reacted yet
2. **OBI Divergence** — One exchange shows strong order book pressure, the other is neutral

By combining these two signals, the strategy generates high-conviction trades with minimal latency.

---

## Datapoint 1: Trade Impulse Detection

### What It Detects

Fast price moves on one exchange while the other remains flat. This is **pure alpha** — no correlation needed.

```
┌─────────────────────────────────────────────────────────────┐
│  IMPULSE DETECTION (5ms window, ~50ns)                      │
├─────────────────────────────────────────────────────────────┤
│                                                             │
│  Exchange A:  100.00 → 100.05 (+50 bps)  ← IMPULSE        │
│  Exchange B:  100.00 → 100.00 (flat)     ← NOT REACTED    │
│                                                             │
│  Action: BUY Exchange B (expect catch-up)                   │
│                                                             │
└─────────────────────────────────────────────────────────────┘
```

### Algorithm

1. **Track midprice** for both exchanges over 5ms window
2. **Calculate delta**: `delta = mid_now - mid_prev_5ms`
3. **Detect impulse**: `|delta_A| > 5 bps AND |delta_B| < 1.5 bps`
4. **Direction**: `delta > 0` → bullish, `delta < 0` → bearish

### Parameters

| Parameter | Default | Description |
|-----------|---------|-------------|
| `impulse_threshold_bps` | 5 | Price move threshold to detect impulse (3-10 bps) |
| `lag_threshold_bps` | 1.5 | Max move on other exchange to consider it "lagging" (1-2 bps) |
| `impulse_window_ms` | 5 | Lookback window for price change detection |
| `min_trade_size_filter` | 0.001 | Minimum trade size to filter fake impulses |

### Execution

**Bullish impulse (A moves up, B flat):**
- Place BUY limit at B's best bid
- Expect B to catch up to A

**Bearish impulse (A moves down, B flat):**
- Place SELL limit at B's best ask
- Expect B to catch down to A

### Timing

- Signal valid for: **1-20ms window**
- After that: alpha gone
- Cancel if: B starts moving, spread widens, OBI turns against you

---

## Datapoint 2: OBI Divergence

### What It Detects

Order book imbalance divergence between exchanges. When one exchange shows strong bid/ask pressure while the other is neutral, expect price movement.

```
┌─────────────────────────────────────────────────────────────┐
│  OBI DIVERGENCE (order book update, ~100ns)                 │
├─────────────────────────────────────────────────────────────┤
│                                                             │
│  Exchange A:  OBI = +0.6 (bid-heavy, bullish)              │
│  Exchange B:  OBI = +0.1 (neutral)                         │
│                                                             │
│  Divergence: 0.5 > threshold (0.3)                         │
│  Action: BUY Exchange B (expect propagation)                │
│                                                             │
└─────────────────────────────────────────────────────────────┘
```

### Algorithm

1. **Calculate OBI** for both exchanges:
   ```
   OBI = (bid_volume - ask_volume) / (bid_volume + ask_volume)
   ```
   Range: [-1.0, 1.0] where positive = bid-heavy (bullish)

2. **Detect divergence**:
   - `OBI_A > 0.7` AND `|OBI_B| < 0.2` → Bullish divergence
   - `OBI_A < -0.7` AND `|OBI_B| < 0.2` → Bearish divergence

3. **Liquidity shift detection**:
   - `delta_OBI = OBI_now - OBI_prev`
   - Trigger when `delta_OBI > spike_threshold`

### Parameters

| Parameter | Default | Description |
|-----------|---------|-------------|
| `obi_strong_threshold` | 0.7 | OBI value considered strong (0.6-0.8) |
| `obi_neutral_threshold` | 0.2 | OBI value considered neutral |
| `obi_depth` | 5 | Order book depth for OBI calculation (5-10 levels) |
| `obi_spike_threshold` | 0.3 | OBI delta for liquidity shift detection |

### Execution

**Bullish imbalance on A:**
- Place BUY limit at B's bid
- Expect B to follow A's bullish pressure

**Bearish imbalance on A:**
- Place SELL limit at B's ask
- Expect B to follow A's bearish pressure

### Timing

- OBI signals last longer than impulse: **10ms → 100ms**
- Better for maker fills and patience

---

## Combined Signal Priority

```
┌─────────────────────────────────────────────────────────────┐
│  SIGNAL PRIORITY                                            │
├─────────────────────────────────────────────────────────────┤
│                                                             │
│  Impulse + OBI confirms  →  🔥 HIGH (execute immediately)  │
│  Impulse only            →  ⚡ MEDIUM (execute with caution)│
│  OBI only                →  ⚡ MEDIUM (maker-only)          │
│  Neither                 →  ❌ NO SIGNAL                    │
│                                                             │
└─────────────────────────────────────────────────────────────┘
```

### Enhancement: Liquidity Shift Detection

Instead of just OBI level, detect **delta_obi**:

```
delta_obi = obi_a_now - obi_a_prev
```

Trigger when:
```
delta_obi > spike_threshold
```

This catches **sudden liquidity pulls** (VERY predictive).

---

## Cancel Conditions

Cancel order if:
1. **Lag closed** — B midprice starts moving
2. **Spread widened** — Spread exceeds `spread_filter_bps`
3. **OBI against** — OBI turns against your direction
4. **Timeout** — Signal age exceeds `signal_timeout_ms`

---

## Critical Pitfalls

### ❌ Fake Impulses

**Cause:** Single small trade, thin book
**Fix:** `min_trade_size_filter` — ignore trades below threshold

### ❌ Spoofing (OBI Trap)

**Cause:** Fake large orders that disappear
**Fix:** Require persistence (e.g., 2-3 updates)

### ❌ Self-Impact

**Cause:** Your own order moves price
**Fix:** Stay small (you already do)

### ❌ Spread Traps

**Cause:** Wide spread = fake opportunity
**Fix:** Strict `spread_filter_bps` filter

---

## Integration with Correlation-Hysteresis

The Impulse-OBI strategy can run alongside correlation-hysteresis:

```
┌─────────────────────────────────────────────────────────────┐
│                 HYBRID SIGNAL ENGINE                         │
├─────────────────────────────────────────────────────────────┤
│                                                             │
│  ┌─────────────┐    ┌─────────────┐                        │
│  │  IMPULSE    │    │    OBI      │                        │
│  │  DETECTOR   │    │ DIVERGENCE  │                        │
│  └──────┬──────┘    └──────┬──────┘                        │
│         └──────────────────┼────────────────┐               │
│                            │                │               │
│                    ┌───────▼───────┐        │               │
│                    │   EVENT       │        │               │
│                    │   AGGREGATOR  │        │               │
│                    └───────┬───────┘        │               │
│                            │                │               │
│                    ┌───────▼───────┐        │               │
│                    │ CORRELATION   │ ← Confirmation layer  │
│                    │   ENGINE      │        │               │
│                    └───────┬───────┘        │               │
│                            │                │               │
│                    ┌───────▼───────┐        │               │
│                    │  HYSTERESIS   │        │               │
│                    │   + OMS       │        │               │
│                    └───────────────┘        │               │
│                                                             │
└─────────────────────────────────────────────────────────────┘
```

**Event detectors are PRIMARY triggers** (not correlation)
**Correlation is CONFIRMATION** (not primary signal)
**Hysteresis validates** event-driven signals

---

## Configuration Example

```toml
[strategy]
active_strategy = "impulse_obi"
symbols = ["ZEC", "XMR", "LINK"]

# Impulse settings
impulse_threshold_bps = 5
lag_threshold_bps = 1.5
impulse_window_ms = 5
signal_timeout_ms = 10
min_trade_size_filter = 0.001
spread_filter_bps = 10

# OBI settings
obi_strong_threshold = 0.7
obi_neutral_threshold = 0.2
obi_depth = 5
obi_spike_threshold = 0.3
```

---

## Performance Characteristics

| Metric | Value |
|--------|-------|
| Latency | ~100ns per tick |
| Memory | Stack-allocated, zero heap |
| Signal frequency | 1-10 per minute (market dependent) |
| Win rate | 55-65% (backtested) |
| Avg hold time | 5-50ms |

---

## When to Use

**Use Impulse-OBI when:**
- Market is volatile (frequent price moves)
- You want fast, event-driven signals
- You're comfortable with maker-only execution
- You have low-latency infrastructure

**Use Correlation-Hysteresis when:**
- Market is ranging (low volatility)
- You want confirmed, statistical signals
- You're comfortable with slower execution
- You want higher win rate

---

## Future Enhancements

1. **Multi-venue OBI** — Aggregate OBI across 3+ exchanges
2. **Trade flow toxicity** — Detect informed flow
3. **Inventory pressure** — Market maker position signals
4. **Cross-asset correlation** — BTC impulse → ALT reaction