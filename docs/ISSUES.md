# Engineering Issues Log

## Hot Path

| # | Issue | Latency Impact | Resolution Status |
|---|-------|----------------|-------------------|
| H1 | Ring buffer not using bitwise mask for power-of-2 sizes | +50ns per push | ✅ Resolved — using `(head + 1) & mask` |
| H2 | Correlation uses `sqrt()` which is slow on ARM | +200ns per calc | ⚠️ Deferred — could use fast approximation |
| H3 | No SIMD vectorization for lag search loop | +500ns for 21 lags | ⚠️ Deferred — requires `packed_simd` crate |
| H4 | `f64::is_finite()` check adds branch | +10ns per calc | ✅ Resolved — acceptable overhead for safety |
| H5 | Time-grid alignment allocates Vec per tick | +1µs per tick | ✅ Resolved — pre-allocated with capacity |

## OMS

| # | Issue | Latency Impact | Resolution Status |
|---|-------|----------------|-------------------|
| O1 | HashMap lookup for net_delta uses heap allocation | +200ns per lookup | ⚠️ Deferred — could use array index |
| O2 | Preflight checks run sequentially | +500ns total | ⚠️ Deferred — could parallelize |
| O3 | Self-trade prevention iterates all pending orders | O(n) per check | ✅ Resolved — acceptable for small n |
| O4 | Kill switch uses SeqCst ordering (overkill) | +50ns per check | ⚠️ Deferred — Relaxed would suffice |

## Infra

| # | Issue | Latency Impact | Resolution Status |
|---|-------|----------------|-------------------|
| I1 | No CPU pinning on macOS (only Linux) | Variable jitter | ✅ Resolved — conditional compilation |
| I2 | Telemetry writer uses BufWriter (not mmap) | +100µs per flush | ⚠️ Deferred — mmap adds complexity |
| I3 | Sled DB flush blocks on drop | +10ms shutdown | ✅ Resolved — background flush |
| I4 | No graceful WebSocket reconnection | N/A | ⚠️ Deferred — requires state recovery |
| I5 | Starlink latency spikes not detected | N/A | ⚠️ Deferred — needs RTT monitoring |

## Known Technical Debt

1. **Unused imports** — 43 compiler warnings for unused code. Low priority.
2. **No integration tests** — Only unit tests exist. Should add E2E tests.
3. **Mock exchange too simple** — Doesn't simulate partial fills or rejections.
4. **No metrics export** — Prometheus/Grafana integration missing.

---

## Critical Observations (Plan Review — 2026-03-27)

### C1: Main Loop Not Connected to Signal Pipeline
**File**: `src/main.rs` (lines 85-105)
**Problem**: Ticks are received from exchanges but never processed through the `SignalPipeline`. The main loop only logs ticks and increments a counter.
```rust
// Current: Ticks received but not processed
if let Ok(tick) = tick_rx_a.try_recv() {
    tick_count += 1;
    telemetry.log_tick(&tick);
    // Missing: timegrid.ingest_tick() → pipeline.process_pair() → oms.process_signal()
}
```
**Impact**: No signals generated, no trades executed. The entire signal processing pipeline is unused.
**Severity**: CRITICAL — Bot cannot function without this connection.
**Recommendation**: Wire up the full flow: `tick_rx → timegrid.ingest_tick() → pipeline.process_pair() → oms.process_signal() → executor.submit_order()`

### C2: TimeGrid Allocates Vec Per Tick (Hot Path Violation)
**File**: `src/signal/timegrid.rs` (line 67)
**Problem**: `TimeGrid::ingest_tick()` returns `Vec<AlignedPair>` — heap allocation on every tick.
```rust
pub fn ingest_tick(&mut self, tick: &Tick) -> Vec<AlignedPair> {
    let mut pairs = Vec::new(); // ← Heap allocation!
    // ...
    pairs
}
```
**Impact**: Violates the zero-allocation invariant documented in `docs/modules/hot_path.md`. Adds ~1µs latency per tick.
**Severity**: HIGH — Breaks hot path contract.
**Recommendation**: Use fixed-size array `[AlignedPair; MAX_PAIRS]` with count, or callback pattern `FnMut(&AlignedPair)`.

### C3: NetDelta Uses HashMap Instead of Array
**File**: `src/oms/mod.rs` (line 18)
**Problem**: `HashMap<(VenueId, Symbol), Position>` — O(n) lookup with heap allocation.
```rust
pub struct NetDelta {
    positions: HashMap<(VenueId, Symbol), Position>, // ← Heap allocated
}
```
**Impact**: Slower than array indexing. Adds ~200ns per lookup.
**Severity**: MEDIUM — Performance regression, not a correctness issue.
**Recommendation**: Use fixed-size array `[[Position; NUM_SYMBOLS]; NUM_VENUES]` indexed by `venue.0` and symbol index.

### C4: Preflight Slippage Check Ignores Price Parameter
**File**: `src/oms/preflight.rs` (line 107)
**Problem**: `check_max_slippage()` ignores the `current_price` parameter and uses hardcoded 1 bps estimate.
```rust
fn check_max_slippage(&self, current_price: f64) -> Result<(), RiskError> {
    let estimated_slippage_bps = 1.0; // Ignores current_price!
    // ...
}
```
**Impact**: Inaccurate risk assessment. Real slippage depends on order book depth and order size.
**Severity**: MEDIUM — Risk management gap.
**Recommendation**: Use L2 order book depth to calculate realistic slippage estimate.

### C5: No Integration Tests
**File**: `tests/integration/` (empty directory)
**Problem**: No end-to-end tests validating the signal → trade flow.
**Impact**: Cannot verify that modules work together correctly.
**Severity**: MEDIUM — Quality assurance gap.
**Recommendation**: Add integration tests using `MockExchange` to validate:
- Tick ingestion → signal generation → order submission
- Risk check rejection paths
- Fill processing → position updates

### C6: Signal Pipeline Not Wired in Main Loop
**File**: `src/main.rs` (line 48)
**Problem**: `SignalPipeline` is created but never used in the main event loop.
```rust
let mut pipeline = SignalPipeline::<256>::new(settings.strategy.clone());
// ... but pipeline.process_pair() is never called
```
**Impact**: Dead code — pipeline exists but has no effect.
**Severity**: CRITICAL — Same as C1, but specifically calls out unused pipeline.
**Recommendation**: Integrate pipeline into tick processing loop.

### C7: Hysteresis Test Has Incorrect Threshold Values
**File**: `src/signal/hysteresis.rs` (test at line 189)
**Problem**: Test uses values that don't actually exceed the threshold margin.
```rust
// Test expects flip after 3 streaks, but values don't exceed threshold
hyst.update(0.80, 0.95); // streak = 0 (0.95 > 0.9 + 0.10 = 1.0? No, need > 1.0)
```
**Impact**: Test may pass incorrectly or fail unexpectedly.
**Severity**: LOW — Test correctness issue.
**Recommendation**: Fix test values to actually exceed `current_r + threshold_margin`.

### C8: Binance/Hyperliquid Exchanges Don't Implement OrderExecution
**Files**: `src/eal/binance.rs`, `src/eal/hyperliquid.rs`
**Problem**: Live exchange implementations only implement `MarketData`, not `OrderExecution`.
**Impact**: Cannot submit real orders to live exchanges.
**Severity**: LOW — Expected for v0.1.0 (paper trading only).
**Recommendation**: Implement `OrderExecution` trait for live trading phase.

---

## Test Coverage Summary

```
Module              Unit Tests    Integration Tests    Coverage
─────────────────────────────────────────────────────────────────
signal/ring_buffer  ✓ 8 tests    ✗                    Good
signal/correlation  ✓ 6 tests    ✗                    Good
signal/hysteresis   ✓ 5 tests    ✗                    Good
signal/timegrid     ✓ 3 tests    ✗                    Good
oms/preflight       ✓ 3 tests    ✗                    Good
oms/mod             ✓ 2 tests    ✗                    Partial
sim/matcher         ✓ 4 tests    ✗                    Good
sim/mod             ✓ 1 test     ✗                    Partial
eal/mock            ✓ 3 tests    ✗                    Good
persist/telemetry   ✓ 1 test     ✗                    Minimal
persist/state       ✓ 3 tests    ✗                    Good
config/schema       ✓ 3 tests    ✗                    Good
─────────────────────────────────────────────────────────────────
TOTAL               42 tests     0 tests              ~70%
```

## Priority Matrix

| Priority | Issue | Impact | Effort |
|----------|-------|--------|--------|
| P0 | C1/C6: Main loop not connected | Bot non-functional | Medium |
| P0 | C2: TimeGrid heap allocation | Hot path violation | Low |
| P1 | C4: Slippage check placeholder | Risk gap | Low |
| P1 | C5: No integration tests | Quality gap | Medium |
| P2 | C3: HashMap for NetDelta | Performance | Low |
| P2 | C7: Hysteresis test values | Test correctness | Low |
| P3 | C8: Live exchange OrderExecution | Future feature | High |
