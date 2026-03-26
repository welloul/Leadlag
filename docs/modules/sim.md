# Simulator Module — Paper Trading

## Objective
Simulate realistic exchange behavior for paper trading. Matches orders against L2 order book depth, applies fees and slippage, and tracks alpha decay statistics.

## Latency Profile

| Operation | O(n) | Cycles | Notes |
|-----------|------|--------|-------|
| Order matching | O(d) | ~500 | d = L2 depth levels |
| Fee calculation | O(1) | ~20 | Simple multiplication |
| Position update | O(1) | ~50 | HashMap lookup |
| **Total** | **O(d)** | **~570** | **~0.9µs @ 3GHz** |

## Invariants

1. **Realistic fills**: Only fills against actual L2 liquidity
2. **VWAP calculation**: Multi-level fills use volume-weighted average price
3. **Fee accuracy**: Uses exact exchange fee tiers
4. **Latency simulation**: Configurable RTT delay before fill

## Memory Layout

```
OrderBookMatcher:
┌─────────────────────────────────────────┐
│ bids: Vec<BookLevel>  (heap)            │
│ asks: Vec<BookLevel>  (heap)            │
│ max_depth: usize                        │
└─────────────────────────────────────────┘

PaperSimulator:
┌─────────────────────────────────────────┐
│ settings: SimulationSettings            │
│ matchers: Arc<Mutex<HashMap<Symbol,...>>>│
│ order_counter: Arc<Mutex<u64>>          │
│ positions: Arc<Mutex<Vec<Position>>>    │
│ daily_pnl: Arc<Mutex<f64>>              │
│ total_fees: Arc<Mutex<f64>>             │
│ fill_history: Arc<Mutex<Vec<FillEvent>>>│
└─────────────────────────────────────────┘
```

## Key Functions

### `OrderBookMatcher::match_order(side, size, limit) -> (filled, avg_price, slippage_bps)`
- **Input**: Order side, size, optional limit price
- **Output**: (filled_size, average_price, slippage_in_bps)
- **Side effects**: None (read-only)
- **Complexity**: O(d) where d = max_depth

### `PaperSimulator::simulate_fill(order) -> Result<FillEvent>`
- **Input**: Order request
- **Output**: Fill event with fees
- **Side effects**: Updates total_fees, fill_history
- **Complexity**: O(d)

## Fill Logic

```
1. Get order book for symbol
2. Iterate levels from best to worst:
   - If limit price set, skip levels beyond limit
   - Fill min(remaining, level.size)
   - Accumulate cost = fill_size * level.price
3. Calculate VWAP = total_cost / total_filled
4. Calculate slippage = |VWAP - best_price| / best_price * 10000
5. Calculate fee = notional * fee_tier_bps / 10000
6. Return FillEvent
```

## Alpha Decay Statistics

```rust
pub struct AlphaDecayStats {
    pub total_fills: usize,
    pub avg_slippage_bps: f64,
    pub total_fees: f64,
}
```

**Purpose:** Measure how much profit is lost to execution latency and market impact.