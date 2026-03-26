# Hot Path Module

## Objective
Process incoming ticks from both exchanges, compute cross-correlation, detect lead-lag relationship, and generate trade signals. Must execute in <10µs with zero heap allocations.

## Latency Profile

| Operation | O(n) | Cycles | Notes |
|-----------|------|--------|-------|
| Ring buffer push | O(1) | ~50 | Bitwise mask, no modulo |
| Running sum update | O(1) | ~20 | 5 additions/subtractions |
| Correlation calc | O(1) | ~100 | 1 sqrt, 5 multiplications |
| Lag search (21 lags) | O(k) | ~2100 | k=21, could SIMD |
| Hysteresis update | O(1) | ~30 | Branch comparison |
| **Total** | **O(k)** | **~2300** | **~3µs @ 3GHz** |

## Invariants

1. **Power-of-2 size**: `RingBuffer<N>` where N must be `2^k`
2. **No allocation**: No `Vec::push`, `Box::new`, or `String::from` in hot path
3. **No locks**: No `Mutex`, `RwLock`, or `await` in hot path
4. **Defensive math**: All divisions guarded by epsilon, results clamped
5. **Single consumer**: One thread reads from crossbeam channel

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