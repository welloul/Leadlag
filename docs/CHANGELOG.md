# Changelog

All notable changes to TokioParasite are documented here.

Format: `[Version] — Date — Category`

Categories: `GC Pressure`, `Allocation Changes`, `Math Optimizations`, `Architecture`, `Bug Fix`

---

## [0.1.1] — 2026-03-28 — Plan Review Fixes

### Architecture
- **Impulse-OBI wired into main loop** — `pipeline.process_tick()` and `pipeline.process_book()` now called from main.rs for both exchanges. Previously returned `None` silently.
- **Book subscriptions added** — main.rs subscribes to L2 order book data when `active_strategy = "impulse_obi"`. Graceful fallback when live exchanges don't support it.
- **PaperSimulator replaces MockExchange** — main.rs uses `PaperSimulator` for realistic L2 matching, slippage, and fees instead of zero-cost `MockExchange`.
- **Duplicate TimeGrid removed** — `SignalPipeline` no longer has an internal `TimeGrid`. Uses `timegrid_precision_ns` field set from main.rs.

### Bug Fix
- **ImpulseDetector cross-venue tracker pollution** — Ticks were routed to BOTH `tracker_a` and `tracker_b`, corrupting delta calculations. Now correctly routes to one tracker per venue.
- **NaN/Inf in MidpriceTracker** — Division by zero possible when `prev` price is 0.0. Added `is_finite()` and `> 0.0` guards.
- **OMS swallowed execution errors** — `process_signal()` discarded the original `ExecutionError` and returned misleading `ExceedsMaxNotional { 0.0, 0.0 }`. Added `RiskError::ExecutionFailed(String)` variant.
- **Integration tests non-compiling** — All `StrategySettings` constructors missing 12 Impulse-OBI fields. Fixed across `integration_test.rs`, `signal_flow_test.rs`, and `sim/mod.rs`.
- **Hysteresis test misleading** — Renamed `test_no_flip_below_threshold_with_higher_values` to `test_no_flip_when_current_lead_reasserts` with accurate comments.

### Allocation Changes
- **Eliminated signal clone on hot path** — `ImpulseObiEngine` now stores `PendingSignal` (Copy-friendly struct with `VenueId`, `OrderSide`, `u64`) instead of cloning full `ImpulseSignal`/`ObiSignal` structs with heap-allocated `Symbol` strings.
- **`saturating_sub` for timeout** — Prevents unsigned integer underflow in timeout calculation.

### GC Pressure
- **`yield_now()` replaces `sleep(100µs)`** — Lower latency scheduling for the main loop. Reduced from 100µs sleep to cooperative yield.

### Tests
- **4 new Impulse-OBI tests**: `test_impulse_only_medium_conviction`, `test_timeout_clears_pending_signals`, `test_direction_matching_logic`, plus retained `test_spread_filter`.
- **Test count**: 137 total (132 unit + 7 integration + 4 signal flow + 1 doc test).

---

## [0.1.0] — 2026-03-26 — Initial Release

### Architecture
- Complete modular architecture with EAL, Signal Pipeline, OMS, Simulator, Persistence
- Exchange Abstraction Layer with trait-based design (`MarketData`, `OrderExecution`)
- Paper trading simulator with L2 order book matching
- Sled embedded database for state persistence
- Proto3 binary telemetry writer

### Math Optimizations
- **Incremental Pearson correlation** with O(1) running sums
- **Power-of-2 ring buffer** with bitwise mask indexing (`& mask` instead of `%`)
- **Defensive math**: epsilon guards, NaN/Inf protection, clamping to [-1, 1]
- **Lag detection**: correlation at multiple time offsets for lead identification

### Allocation Changes
- **Zero allocations on hot path**: pre-allocated ring buffers, no Vec::push
- **Stack-only hot path types**: `HotPathError` is `#[repr(u8)]` enum
- **Arc-wrapped shared data**: `Arc<Tick>` for zero-copy fan-out
- **Bounded channels**: `crossbeam_channel::bounded` for backpressure

### GC Pressure
- **No GC**: Rust's ownership model eliminates GC pressure
- **No heap allocations in hot path**: all data on stack or pre-allocated
- **Atomic operations**: `AtomicBool` for kill switch (no locks)

### Bug Fix
- Fixed `update_position` not updating size on first fill (OMS module)
- Fixed correlation lag direction (was `i + lag`, now `i - lag`)
- Fixed hysteresis test using values that don't exceed threshold margin

---

## [Unreleased]

### Planned
- SIMD vectorization for lag search loop (`packed_simd` crate)
- Fast sqrt approximation for correlation denominator
- WebSocket reconnection with state recovery
- Prometheus metrics export
- Live exchange `OrderExecution` implementations (Binance, Hyperliquid)
- Live exchange `subscribe_book()` implementations for OBI strategy
