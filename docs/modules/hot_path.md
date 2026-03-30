# Hot Path Module (v0.1.3)

## Objective
Process incoming ticks from both exchanges, detect lead-lag relationships, and generate trade signals. Supports two strategies: correlation-hysteresis (statistical) and impulse-obi (event-driven). Must execute in <10µs with zero heap allocations.

## Invariants

1. **Power-of-2 size**: `RingBuffer<N>` where N must be `2^k`
2. **No allocation**: No `Vec::push`, `Box::new`, or `String::from` in hot path
3. **No locks**: No `Mutex`, `RwLock`, or `await` in hot path
4. **Defensive math**: All divisions guarded by epsilon, results clamped
5. **Single consumer**: One thread reads from crossbeam channel
6. **Venue-routed tracking**: `ImpulseDetector` routes ticks to `tracker_a` (Exchange A) or `tracker_b` (Exchange B) only.
7. **NaN/Inf guards**: `MidpriceTracker::update()` rejects invalid prices.
8. **Warmup gate**: Both trackers must be `initialized` AND `warmed_up` before generating impulses.
9. **Sanity check**: Deltas > 500 bps are silently rejected.
10. **Lag check**: `other_delta() < lag_threshold_bps` (1.5 bps).
11. **Local timestamps for freshness**: `last_local_update_ns` uses `SystemTime::now()`. Exchange timestamps only for delta calculation.
12. **Freshness gate**: Both venues must have ticked within 400ms (local time).

## Key Functions

### `MidpriceTracker::update(tick) -> Option<f64>`
- **Input**: Tick from one exchange only
- **Output**: Delta in bps if window elapsed, None otherwise
- **Side effects**: Updates `current_mid`, `prev_mid`, `prev_timestamp_ns`, `last_local_update_ns`, sets `initialized`/`warmed_up`
- **Complexity**: O(1)
- **Guards**: Rejects prices <= 0.0 or non-finite. First delta after init skipped (warmup).

### `ImpulseDetector::process_tick(tick) -> Option<ImpulseSignal>`
- **Input**: Tick from a specific venue
- **Output**: Impulse signal if ALL gates pass:
  1. Trade size >= min_trade_size
  2. Both trackers `initialized` AND `warmed_up`
  3. Both venues fresh (local time within 400ms)
  4. Delta > threshold (5 bps) AND < 500 bps (sanity)
  5. Other tracker's delta < lag_threshold (1.5 bps)
- **Side effects**: Routes tick to correct tracker (venue-based)
- **Complexity**: O(1)

### `ObiDivergenceDetector::weighted_obi(book) -> f64`
- **Depth-weighted OBI**: `weight = 1/(i+1)` — top levels dominate
- **Time-based persistence**: OBI must stay strong for `obi_persist_ms` (200ms)

### `ImpulseObiEngine::edge_bps(source, target, side) -> f64`
- **Direction-normalized**: Buy = `(target - source) / source * 10000`, Sell = inverse
- **Fees-aware**: `entry_threshold_bps = 8` (covers taker fees + slippage)

## Memory Layout

```
MidpriceTracker:
┌─────────────────────────────────────────┐
│ current_mid: Option<f64>                │
│ prev_mid: Option<f64>                   │
│ prev_timestamp_ns: u64  (exchange time) │
│ window_ns: u64                          │
│ initialized: bool                       │
│ warmed_up: bool                         │
│ last_local_update_ns: u64 (local time)  │
└─────────────────────────────────────────┘

PendingSignal (Copy-friendly, no heap):
┌─────────────────────────────────────────┐
│ venue: VenueId        (1 byte)          │
│ side: OrderSide       (1 byte)          │
│ timestamp_ns: u64     (8 bytes)         │
└─────────────────────────────────────────┘
```
