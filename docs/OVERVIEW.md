# Lead-Lag Strategy Overview

## Mathematical Foundation

### Incremental Pearson Cross-Correlation

The core signal detection uses a running Pearson correlation coefficient computed in O(1) per tick.

```
R = (N·Σxy - Σx·Σy) / √[(N·Σx² - (Σx)²)(N·Σy² - (Σy)²)]
```

**Running Sums (updated per tick):**
- `Σx` — sum of exchange A prices
- `Σy` — sum of exchange B prices  
- `Σx²` — sum of squared A prices
- `Σy²` — sum of squared B prices
- `Σxy` — sum of cross-products

**Update on new tick (price_a, price_b):**
```
dropped_a = ring_buffer_a.push(price_a)
dropped_b = ring_buffer_b.push(price_b)

Σx  += price_a - dropped_a
Σy  += price_b - dropped_b
Σx² += price_a² - dropped_a²
Σy² += price_b² - dropped_b²
Σxy += price_a·price_b - dropped_a·dropped_b
```

**Defensive Math:**
- Epsilon (1e-12) added to denominator to prevent division by zero
- Result clamped to [-1.0, 1.0]
- Returns 0.0 for flat-line inputs (zero variance)

### Lag Detection

To identify which exchange leads, we compute correlation at multiple time offsets:

```
R(lag) = correlation(A[t], B[t - lag])
```

- **Positive lag**: B lags behind A (A leads)
- **Negative lag**: A lags behind B (B leads)
- **Best lag**: argmax(|R(lag)|) for lag ∈ [-10, +10]

## Hysteresis State Machine

```
                    ┌─────────────────┐
                    │  UNDETERMINED   │
                    └────────┬────────┘
                             │ First update
                             ▼
                    ┌─────────────────┐
              ┌────▶│   A LEADS       │◀────┐
              │     └────────┬────────┘     │
              │              │              │
              │    B dominant │    A dominant │
              │    + margin   │    again      │
              │              │              │
              │              ▼              │
              │     ┌─────────────────┐     │
              │     │ B CANDIDATE     │     │
              │     │ (streak = 1)    │     │
              │     └────────┬────────┘     │
              │              │              │
              │    B dominant │    A breaks  │
              │    again      │    streak    │
              │              │              │
              │              ▼              │
              │     ┌─────────────────┐     │
              │     │ B CANDIDATE     │     │
              │     │ (streak = 2)    │     │
              │     └────────┬────────┘     │
              │              │              │
              │    B dominant │              │
              │    again      │              │
              │              ▼              │
              │     ┌─────────────────┐     │
              └─────│   B LEADS       │─────┘
                    │ (FLIP!)         │
                    └─────────────────┘
```

**Transition Rules:**
1. **Initial**: First update sets the lead
2. **Candidate**: New lead must exceed `current_r + threshold_margin`
3. **Streak**: Must maintain dominance for `min_consecutive` checks
4. **Flip**: Streak reaches threshold → lead role flips
5. **Reset**: Current lead reasserts dominance → candidate streak resets

**Configuration (settings.toml):**
```toml
[strategy]
min_correlation_r = 0.85    # Minimum R to generate signal
hysteresis_buffer = 0.10    # Margin to consider a flip
window_size_ticks = 256     # Ring buffer size (must be power of 2)
```

## Trend-Following Entry/Exit Logic

### Entry (Directional)
```
IF Lead_Price > Lag_Price:
    BUY on Laggard (expect catch-up)
ELSE:
    SELL on Laggard (expect catch-down)
```

### Exit (Leader Reversal)
```
IF Lead shows reversal signal:
    MARKET EXIT on Laggard immediately
```

**Reversal Detection:**
- Lead price crosses back over VWAP
- Lead-Lag correlation R flips negative
- Hysteresis detects new lead (role flip)

## Order Book Imbalance (OBI) Fusion

```
OBI = (bid_volume - ask_volume) / (bid_volume + ask_volume)
```

**Range:** [-1.0, 1.0]
- Positive: Bid-heavy (bullish pressure)
- Negative: Ask-heavy (bearish pressure)

**Fusion with Trade Delta:**
```
signal_strength = (1 - obi_weight) * trade_delta + obi_weight * OBI
```

## Time-Grid Alignment

Asynchronous exchange feeds are synchronized using Forward-Fill (LOCF):

```
Time Grid:  |----5ms----|----5ms----|----5ms----|
Exchange A: |    60000  |           |    60002  |
Exchange B: |           |    60001  |           |
Aligned:    | 60000/60001| 60000/60001| 60002/60001|
```

**Algorithm:**
1. Convert exchange timestamps to grid units: `grid = ts_ns / precision_ns`
2. Forward-fill missing prices from last known value
3. Emit aligned pairs for each grid bin

## Latency Profile

| Component | Complexity | Estimated Cycles |
|-----------|------------|------------------|
| Ring buffer push | O(1) | ~50 cycles |
| Running sum update | O(1) | ~20 cycles |
| Correlation calc | O(1) | ~100 cycles |
| Hysteresis update | O(1) | ~30 cycles |
| **Total hot path** | **O(1)** | **~200 cycles (~3µs @ 3GHz)** |