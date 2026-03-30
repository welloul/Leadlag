# Hot Path Module

## Objective
Process incoming ticks from both exchanges, compute cross-correlation, detect lead-lag relationship, and generate trade signals. Supports two strategies: correlation-hysteresis (statistical) and impulse-obi (event-driven). Must execute in <10µs with zero heap allocations.

## Latency Profile

### Correlation-Hysteresis Path

| Operation | O(n) | Cycles | Notes |
|-----------|------|--------|-------|
| Ring buffer push | O(1) | ~50 | Bitwise mask, no modulo |
| Running sum update | O(1) | ~20 | 5 additions/subtractions |
| Correlation calc | O(1) | ~100 | 1 sqrt, 5 multiplications |
| Lag search (21 lags) | O(k) | ~2100 | k=21, could SIMD |
| Hysteresis update | O(1) | ~30 | Branch comparison |
| **Total** | **O(k)** | **~2300** | **~3µs @ 3GHz** |

### Impulse-OBI Path

| Operation | O(n) | Cycles | Notes |
|-----------|------|--------|-------|
| MidpriceTracker update | O(1) | ~30 | Venue-routed |
| Delta bps calculation | O(1) | ~20 | `(price - prev) / prev * 10000` |
| Threshold comparison | O(1) | ~10 | Branch only |
| PendingSignal store | O(1) | ~5 | Copy, no heap |
| **Total (impulse)** | **O(1)** | **~65** | **~39ns @ 3GHz** |

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
10. **Lag check restored**: `other_delta() < 1.5 bps` (was hardcoded `true` in v0.1.2).
11. **Local timestamps for freshness**: `last_local_update_ns` uses `SystemTime::now()`. Exchange timestamps only for delta calculation.
12. **Freshness gate**: Both venues must have ticked within 400ms (local time).

## Memory Layout

```
RingBuffer<256>:
┌─────────────────────────────────────────┐
│ data: [f64; 256]     (2048 bytes)       │ ← Stack allocated
│ head: usize           (8 bytes)          │
│ len: usize            (8 bytes)          │
│ mask: usize           (8 bytes) = 255    │ ← For bitwise AND
│ sum: f64              (8 bytes)          │ ← Running sum
│ sum_sq: f64           (8 bytes)          │ ← Running sum of squares
└─────────────────────────────────────────┘
Total: 2088 bytes (fits in 32 cache lines)

CrossCorrelator<256>:
┌─────────────────────────────────────────┐
│ buf_a: RingBuffer<256> (2088 bytes)     │
│ buf_b: RingBuffer<256> (2088 bytes)     │
│ sum_ab: f64           (8 bytes)         │ ← Running cross-sum
│ epsilon: f64          (8 bytes) = 1e-12 │
└─────────────────────────────────────────┘
Total: 4192 bytes (fits in 65 cache lines)

IngestResult (stack-allocated, no heap):
┌─────────────────────────────────────────┐
│ pairs: [AlignedPair; 64] (5120 bytes)   │ ← Fixed-size array
│ count: usize             (8 bytes)       │ ← Valid pair count
└─────────────────────────────────────────┘
Total: 5128 bytes

PendingSignal (Copy-friendly, no heap):
┌─────────────────────────────────────────┐
│ venue: VenueId        (1 byte)          │
│ side: OrderSide       (1 byte)          │
│ timestamp_ns: u64     (8 bytes)         │
└─────────────────────────────────────────┘
Total: 10 bytes (padded to 16)
```

## Key Functions

### `RingBuffer::push(val) -> Option<f64>`
- **Input**: New value to insert
- **Output**: Dropped value (if buffer was full)
- **Side effects**: Updates `sum`, `sum_sq`, advances `head`
- **Complexity**: O(1)

### `CrossCorrelator::push(price_a, price_b)`
- **Input**: Price pair from both exchanges
- **Output**: None (updates internal state)
- **Side effects**: Updates all 5 running sums
- **Complexity**: O(1)

### `CrossCorrelator::correlation() -> f64`
- **Input**: None (reads internal state)
- **Output**: Pearson R in [-1.0, 1.0]
- **Side effects**: None (pure function)
- **Complexity**: O(1)
- **Formula**: Numerically stable mean-subtraction: `Σxy - (n * mean_x * mean_y)` / `sqrt(var_x * var_y)`
- **Fix**: Original naïve formula `(N*Σxy - Σx*Σy) / sqrt(...)` caused catastrophic cancellation with large prices (~60,000)

### `CrossCorrelator::find_best_lag(min, max) -> (i32, f64)`
- **Input**: Lag search range
- **Output**: (best_lag, best_correlation)
- **Side effects**: None
- **Complexity**: O(k) where k = max - min + 1

### `MidpriceTracker::update(tick) -> Option<f64>`
- **Input**: Tick from one exchange only
- **Output**: Delta in bps if window elapsed, None otherwise
- **Side effects**: Updates `current_mid`, `prev_mid`, `prev_timestamp_ns`, sets `initialized`/`warmed_up`
- **Complexity**: O(1)
- **Guards**: Rejects prices <= 0.0 or non-finite. Returns None if division would produce NaN/Inf.
- **Warmup**: First delta after init is skipped (sets `warmed_up = true`). Prevents initial cross-venue price spike.

### `ImpulseDetector::process_tick(tick) -> Option<ImpulseSignal>`
- **Input**: Tick from a specific venue
- **Output**: Impulse signal if:
  1. Both trackers `initialized` AND `warmed_up`
  2. Delta > `impulse_threshold_bps` (5 bps) AND < 500 bps (sanity)
  3. Other tracker's delta < `lag_threshold_bps` (1.5 bps)
- **Side effects**: Routes tick to correct tracker (venue-based)
- **Complexity**: O(1)

## State Machine Transitions

See [OVERVIEW.md](../OVERVIEW.md#hysteresis-state-machine) for full diagram.

```
UNDETERMINED → A_LEADS (first update)
A_LEADS → A_LEADS (A still dominant)
A_LEADS → B_CANDIDATE (B exceeds threshold)
B_CANDIDATE → B_CANDIDATE (B still dominant, streak++)
B_CANDIDATE → B_LEADS (streak >= min_consecutive)
B_CANDIDATE → A_LEADS (A breaks streak)
```
RingBuffer<256>:
┌─────────────────────────────────────────┐
│ data: [f64; 256]     (2048 bytes)       │ ← Stack allocated
│ head: usize           (8 bytes)          │
│ len: usize            (8 bytes)          │
│ mask: usize           (8 bytes) = 255    │ ← For bitwise AND
│ sum: f64              (8 bytes)          │ ← Running sum
│ sum_sq: f64           (8 bytes)          │ ← Running sum of squares
└─────────────────────────────────────────┘
Total: 2088 bytes (fits in 32 cache lines)

CrossCorrelator<256>:
┌─────────────────────────────────────────┐
│ buf_a: RingBuffer<256> (2088 bytes)     │
│ buf_b: RingBuffer<256> (2088 bytes)     │
│ sum_ab: f64           (8 bytes)         │ ← Running cross-sum
│ epsilon: f64          (8 bytes) = 1e-12 │
└─────────────────────────────────────────┘
Total: 4192 bytes (fits in 65 cache lines)
```

## Key Functions

### `RingBuffer::push(val) -> Option<f64>`
- **Input**: New value to insert
- **Output**: Dropped value (if buffer was full)
- **Side effects**: Updates `sum`, `sum_sq`, advances `head`
- **Complexity**: O(1)

### `CrossCorrelator::push(price_a, price_b)`
- **Input**: Price pair from both exchanges
- **Output**: None (updates internal state)
- **Side effects**: Updates all 5 running sums
- **Complexity**: O(1)

### `CrossCorrelator::correlation() -> f64`
- **Input**: None (reads internal state)
- **Output**: Pearson R in [-1.0, 1.0]
- **Side effects**: None (pure function)
- **Complexity**: O(1)
- **Formula**: Numerically stable mean-subtraction: `Σxy - (n * mean_x * mean_y)` / `sqrt(var_x * var_y)`
- **Fix**: Original naïve formula `(N*Σxy - Σx*Σy) / sqrt(...)` caused catastrophic cancellation with large prices (~60,000)

### `CrossCorrelator::find_best_lag(min, max) -> (i32, f64)`
- **Input**: Lag search range
- **Output**: (best_lag, best_correlation)
- **Side effects**: None
- **Complexity**: O(k) where k = max - min + 1

## State Machine Transitions

See [OVERVIEW.md](../OVERVIEW.md#hysteresis-state-machine) for full diagram.

```
UNDETERMINED → A_LEADS (first update)
A_LEADS → A_LEADS (A still dominant)
A_LEADS → B_CANDIDATE (B exceeds threshold)
B_CANDIDATE → B_CANDIDATE (B still dominant, streak++)
B_CANDIDATE → B_LEADS (streak >= min_consecutive)
B_CANDIDATE → A_LEADS (A breaks streak)