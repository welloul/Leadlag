# Impulse-OBI Strategy (v0.2.0)

## Overview
Passive market-making strategy that captures Tokyo-based lead-lag inefficiencies. Transitions from liquidity-taking to **Post-Only Limit** entries to eliminate fee drag and exploit the ~500ms alpha window.

## Entry Logic Gate (v0.2.0 — Maker Mode)

Every signal must pass ALL gates before execution:

```
Tick/Book ──▶ Signal Detected
                │
                ├─ (1) Warmup: both trackers init + warmed_up?
                ├─ (2) Freshness: both venues ticked within 400ms (local)?
                ├─ (3) Lag gate: other venue |delta| < 1.0 bps?
                ├─ (4) Edge gate: cross-venue spread ≥ 4.5 bps?
                ├─ (5) Post-Only: Order must sit at mid-price (Maker)
                └─ (6) Capture Alpha Window (~500ms Tokyo)
                │
                ▼
            POST-ONLY LIMIT ORDER PLACED
```

## Alpha Decay Telemetry (NEW v0.2.0)
High-resolution measurement of the predictive window. Uses local wall-clock timers to measure the gap between a lead breakout and laggard convergence.
*   **Avg Window:** 500ms.
*   **High-Stability Assets:** TON, ARB (1.5s - 2.5s).
*   **High-Competition Assets:** BCH, ADA (<200ms).

## Datapoint 1: Trade Impulse Detection

### Algorithm

1. **Track midprice** for both exchanges over 5ms window (exchange timestamps)
2. **Local freshness**: `last_local_update_ns` uses `SystemTime::now()` — exchange clocks unreliable across venues
3. **Warmup**: First delta after init is skipped (prevents 382k bps spike artifacts)
4. **Detect impulse**: `|delta_A| > 5 bps AND |delta_B| < 1.5 bps`
5. **Direction**: `delta > 0` → BUY laggard, `delta < 0` → SELL laggard

### Parameters (v0.1.3)

| Parameter | Default | Description |
|-----------|---------|-------------|
| `impulse_threshold_bps` | 5 | Price move threshold (3-10 bps) |
| `lag_threshold_bps` | 1.5 | Max move on other exchange (1-2 bps) |
| `impulse_window_ms` | 5 | Lookback window |
| `venue_freshness_ms` | 400 | Both venues must have ticked within this window |
| `entry_threshold_bps` | 8 | Minimum cross-venue edge (covers fees + slippage) |
| `cooldown_ms` | 200 | Side-aware cooldown between trades |

## Datapoint 2: OBI Divergence (v0.1.3)

### Depth-Weighted OBI

```rust
OBI = Σ(size_i * weight_i) on bid side / total_weighted_volume
weight_i = 1.0 / (i + 1.0)  // Level 0: 1.0, Level 1: 0.5, Level 2: 0.33...
```

Top levels dominate — spoofing on deep levels has less impact.

### Time-Based Persistence

OBI must stay strong for `obi_persist_ms` (200ms default) before generating a signal. Not count-based (which varies by venue: Binance 80/sec vs HL 4/sec).

### Parameters

| Parameter | Default | Description |
|-----------|---------|-------------|
| `obi_strong_threshold` | 0.7 | Strong imbalance |
| `obi_neutral_threshold` | 0.2 | Neutral zone |
| `obi_depth` | 5 | Book levels for OBI calc |
| `obi_spike_threshold` | 0.3 | Liquidity shift detection |
| `obi_persist_ms` | 200 | Time-based persistence duration |

## Combined Signal Priority

| Combination | Priority | Action |
|-------------|----------|--------|
| Impulse + OBI confirms | HIGH | Execute immediately |
| Impulse only | MEDIUM | Execute with caution |
| OBI only | MEDIUM | Maker-only |
| Neither | NONE | No signal |

## Cross-Venue Edge Check (v0.1.3)

Direction-normalized edge calculation:

```rust
edge_bps = match side {
    Buy  => (target_mid - source_mid) / source_mid * 10_000.0,
    Sell => (source_mid - target_mid) / source_mid * 10_000.0,
};
// Positive = good trade, negative = bad trade
if edge_bps < entry_threshold_bps (8 bps) { reject }
```

## Position Cap (v0.1.3)

- $100 max cumulative notional per `(venue, symbol)`
- Direction-aware: if LONG, can SHORT to reduce but can't add more LONG
- `max_notional_usd = $10` per individual trade
- Prevents the $1.6M PUMP incident (323 accumulated trades with no cap)

## Configuration (v0.1.3)

```toml
[strategy]
active_strategy = "impulse_obi"
symbols = ["ZEC", "WLD", "FARTCOIN", "DOGE", "SUI", "BCH", "PUMP", "ADA"]
impulse_threshold_bps = 5
lag_threshold_bps = 1.5
impulse_window_ms = 5
signal_timeout_ms = 150         # Combo window
min_trade_size_filter = 0.001
spread_filter_bps = 10
obi_strong_threshold = 0.7
obi_neutral_threshold = 0.2
obi_depth = 5
obi_spike_threshold = 0.3
venue_freshness_ms = 400
entry_threshold_bps = 8
cooldown_ms = 200
max_levels_consumed = 3
obi_persist_ms = 200
fill_conservatism = 0.5

[risk]
max_notional_usd = 10.0
signal_ttl_ms = 500
```
