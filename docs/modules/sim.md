# Simulator Module — Paper Trading

## Objective
Simulate realistic exchange behavior for paper trading. Maintains separate order books per `(Symbol, VenueId)` pair with per-venue spread models. Matches orders against L2 order book depth, applies fees and slippage, and tracks alpha decay statistics. **Used as the primary order execution engine in main.rs** (replaced `MockExchange` in v0.1.1).

## Latency Profile

| Operation | O(n) | Cycles | Notes |
|-----------|------|--------|-------|
| Order matching | O(d) | ~500 | d = L2 depth levels |
| Fee calculation | O(1) | ~20 | Simple multiplication |
| Position update | O(1) | ~50 | HashMap lookup |
| **Total** | **O(d)** | **~570** | **~0.9µs @ 3GHz** |

## Invariants

1. **Per-venue books**: Matchers keyed by `(Symbol, VenueId)`. Each venue has independent L2 books. Reverting to `Symbol`-only keying destroys cross-venue execution correctness.
2. **No silent matcher creation**: `simulate_fill` uses `get_mut()` — never `entry().or_insert_with()`. If a venue has no book, return an explicit error.
3. **Realistic fills**: Only fills against actual L2 liquidity
4. **VWAP calculation**: Multi-level fills use volume-weighted average price
5. **Fee accuracy**: Uses exact exchange fee tiers
6. **Latency simulation**: Configurable RTT delay before fill

## Per-Venue Spread Model

```
VenueSpreadModel:
┌─────────────────────────────────────────┐
│ Binance (Exchange A):                   │
│   base_spread_bps: 1.0 (0.01%)         │
│   size_impact_bps: 0.0005              │
│                                         │
│ Hyperliquid (Exchange B):              │
│   base_spread_bps: 5.0 (0.05%)         │
│   size_impact_bps: 0.002               │
│                                         │
│ half_spread = price * (base / 10000)   │
│             + price * (impact * notional / 1000 / 10000) │
└─────────────────────────────────────────┘
```

## Memory Layout

```
OrderBookMatcher (pub(crate) fields):
┌─────────────────────────────────────────┐
│ bids: Vec<BookLevel>  (heap)            │
│ asks: Vec<BookLevel>  (heap)            │
│ max_depth: usize                        │
└─────────────────────────────────────────┘

PaperSimulator:
┌──────────────────────────────────────────────────────┐
│ settings: SimulationSettings                         │
│ matchers: Arc<Mutex<HashMap<(Symbol,VenueId),...>>>  │ ← Per-venue key
│ order_counter: Arc<Mutex<u64>>                       │
│ positions: Arc<Mutex<Vec<Position>>>                 │
│ daily_pnl: Arc<Mutex<f64>>                           │
│ total_fees: Arc<Mutex<f64>>                          │
│ fill_history: Arc<Mutex<Vec<FillEvent>>>             │
└──────────────────────────────────────────────────────┘
```

## Key Functions

### `OrderBookMatcher::match_order(side, size, limit) -> (filled, avg_price, slippage_bps)`
- **Input**: Order side, size, optional limit price
- **Output**: (filled_size, average_price, slippage_in_bps)
- **Side effects**: None (read-only)
- **Complexity**: O(d) where d = max_depth

### `PaperSimulator::simulate_fill(order) -> Result<FillEvent>`
- **Input**: Order request (uses `order.venue` for book lookup)
- **Output**: Fill event with fees
- **Side effects**: Updates total_fees, fill_history
- **Complexity**: O(d)
- **Key**: Uses `get_mut()` — returns error if venue has no book

### `PaperSimulator::update_book_from_tick(symbol, price, venue)`
- **Synthesizes** L2 book from tick price using per-venue spread model
- Creates `match_l2_depth` levels of bids/asks
- Each level: ±(half_spread + depth_bps) from mid price

### `PaperSimulator::get_mid_price(symbol, venue) -> Option<f64>`
- Returns midpoint of best bid/ask for target venue
- Falls back to other venue if target has no book yet

### `PaperSimulator::is_venue_liquid(symbol, venue) -> bool`
- Returns true if book has at least one bid and one ask

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