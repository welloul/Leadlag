# Simulator Module — Paper Trading (v0.1.3)

## Objective
Simulate realistic exchange behavior for paper trading. Maintains separate order books per `(Symbol, VenueId)` pair with per-venue spread models, conservative fill model, and staleness tracking.

## Invariants

1. **Per-venue books**: Matchers keyed by `(Symbol, VenueId)`. Each venue has independent L2 books.
2. **No silent matcher creation**: `simulate_fill` uses `get_mut()`.
3. **Conservative fill**: Only fills 50% of best level size. Real books shift during latency.
4. **VWAP calculation**: Multi-level fills use volume-weighted average price
5. **Fee accuracy**: Uses exact exchange fee tiers
6. **Latency simulation**: Configurable RTT delay before fill
7. **Staleness tracking**: Per-venue `last_update_ns` and `has_real_data` flags
8. **Fill provenance**: Each fill tagged as `Fresh` or `Stale` for metrics
9. **No cross-venue fallback**: `get_mid_price()` returns `None` if target venue has no book

## Conservative Fill (v0.1.3)

```rust
let best_size = match side {
    Buy  => matcher.best_ask_size(),
    Sell => matcher.best_bid_size(),
};
let allowed_size = order.size.min(best_size * 0.5);
// Only fill 50% of what's visible at best level
```

## Per-Venue Spread Model

| Venue | Base spread | Size impact | Rationale |
|-------|-------------|-------------|-----------|
| Binance (A) | 1.0 bps | 0.0005 bps/$1000 | Deep liquidity |
| Hyperliquid (B) | 5.0 bps | 0.002 bps/$1000 | Thinner book |

```
half_spread = price * (base_spread_bps / 10000.0)
            + price * (size_impact_bps * (notional / 1000.0) / 10000.0)
```

## Key Functions

### `PaperSimulator::update_book(book_update)`
- Updates real L2 book data from Binance diff stream or Hyperliquid l2Book
- Updates `last_update_ns` and `has_real_data` for staleness tracking

### `PaperSimulator::update_book_from_tick(symbol, price, venue)`
- Synthesizes L2 book from tick price using per-venue spread model
- Only updates the venue that sent the tick (no cross-venue seeding)

### `PaperSimulator::get_mid_price(symbol, venue) -> Option<f64>`
- Returns midpoint of best bid/ask for target venue
- No cross-venue fallback

### `PaperSimulator::is_book_stale(symbol, venue, max_age_ns) -> bool`
- Checks if book for (symbol, venue) is older than max_age_ns

### `PaperSimulator::simulate_fill(order, provenance) -> Result<FillEvent>`
- Conservative fill: 50% of best level size
- Tags fill as `Fresh` or `Stale`

## Memory Layout

```
PaperSimulator:
┌──────────────────────────────────────────────────────┐
│ settings: SimulationSettings                         │
│ matchers: Arc<Mutex<HashMap<(Symbol,VenueId),...>>>  │
│ book_states: Arc<Mutex<HashMap<(Symbol,VenueId),...>>>│
│ order_counter: Arc<Mutex<u64>>                       │
│ positions: Arc<Mutex<Vec<Position>>>                 │
│ total_fees: Arc<Mutex<f64>>                          │
│ fill_history: Arc<Mutex<Vec<FillEvent>>>             │
│ metrics: Arc<Mutex<SimMetrics>>                      │
└──────────────────────────────────────────────────────┘

SimMetrics:
┌─────────────────────────────────────────┐
│ total_signals: u64                      │
│ fills_with_fresh_book: u64              │
│ fills_with_stale_book: u64              │
│ skipped_no_book: u64                    │
│ skipped_stale_over_2s: u64              │
└─────────────────────────────────────────┘

OrderBookMatcher (pub(crate)):
┌─────────────────────────────────────────┐
│ bids: Vec<BookLevel>                    │
│ asks: Vec<BookLevel>                    │
│ max_depth: usize                        │
└─────────────────────────────────────────┘
```

## Matcher Functions

| Function | Description |
|----------|-------------|
| `match_order(side, size, limit)` | VWAP fill across levels |
| `best_bid()` / `best_ask()` | Best price |
| `best_bid_size()` / `best_ask_size()` | Best level size (v0.1.3) |
| `mid_price()` | Midpoint |
| `update_book(bids, asks)` | Replace book levels |
